use std::collections::BTreeMap;

use check_core::ast::{BinOp, UnOp};
use check_core::diag::{Code, Diagnostic, Span};
use check_core::ir::{Expr, ExprKind, Func, Program};
use check_core::types::Ty;
use wasm_encoder::{BlockType, InstructionSink, ValType};

use crate::Module;

type Compiled<T> = Result<T, Diagnostic>;

const STRING_HEADER: i32 = 8;

enum Label {
    Plain,
    Break(usize),
    Continue(usize),
}

struct Literal {
    global: u32,
    segment: u32,
}

struct Gen {
    m: Module,
    base: u32,
    globals: Vec<u32>,
    literals: BTreeMap<String, Literal>,
    body: Vec<u8>,
    locals: Vec<ValType>,
    free: Vec<u32>,
    owned_params: Vec<u32>,
    scopes: Vec<Vec<u32>>,
    labels: Vec<Label>,
}

pub fn compile(program: &Program, runtime: &[u8]) -> Compiled<Vec<u8>> {
    let mut m = Module::new(runtime);
    let base = m.next_func();
    let globals = program
        .globals
        .iter()
        .map(|ty| m.global(slot(ty)))
        .collect();
    let mut g = Gen {
        m,
        base,
        globals,
        literals: BTreeMap::new(),
        body: Vec::new(),
        locals: Vec::new(),
        free: Vec::new(),
        owned_params: Vec::new(),
        scopes: Vec::new(),
        labels: Vec::new(),
    };
    for func in &program.funcs {
        g.func(func)?;
    }
    g.start(base + program.main as u32);
    Ok(g.m.finish())
}

fn unsupported<T>(span: Span) -> Compiled<T> {
    Err(Diagnostic::error(Code::wasm_unsupported, span))
}

fn val(ty: &Ty) -> Option<ValType> {
    match ty {
        Ty::Int => Some(ValType::I64),
        Ty::Float => Some(ValType::F64),
        Ty::Bool | Ty::Str => Some(ValType::I32),
        _ => None,
    }
}

fn slot(ty: &Ty) -> ValType {
    val(ty).expect("a variable holds a value")
}

fn block_type(ty: &Ty) -> BlockType {
    val(ty).map_or(BlockType::Empty, BlockType::Result)
}

impl Gen {
    fn f(&mut self) -> InstructionSink<'_> {
        InstructionSink::new(&mut self.body)
    }

    fn rt(&self, name: &str) -> u32 {
        self.m.runtime_func(name).expect("the runtime exports it")
    }

    fn call_rt(&mut self, name: &str) {
        let func = self.rt(name);
        self.f().call(func);
    }

    fn at(&mut self, span: Span) {
        self.f().i32_const(span.start as i32);
    }

    fn temp(&mut self, ty: ValType) -> u32 {
        match self
            .free
            .iter()
            .position(|&t| self.locals[t as usize] == ty)
        {
            Some(i) => self.free.swap_remove(i),
            None => {
                self.locals.push(ty);
                (self.locals.len() - 1) as u32
            }
        }
    }

    fn func(&mut self, func: &Func) -> Compiled<()> {
        self.locals = func.locals.iter().map(slot).collect();
        self.owned_params = (0..func.params)
            .filter(|&i| func.locals[i] == Ty::Str)
            .map(|i| i as u32)
            .collect();
        self.value_or_discard(&func.body, &func.ret)?;
        self.release_params();
        let results: Vec<ValType> = val(&func.ret).into_iter().collect();
        let body = std::mem::take(&mut self.body);
        let (params, locals) = self.locals.split_at(func.params);
        self.m.func(params, &results, locals, &body);
        self.free.clear();
        Ok(())
    }

    fn start(&mut self, main: u32) {
        let new_string = self.rt("check_new_string");
        let mut body = Vec::new();
        let mut f = InstructionSink::new(&mut body);
        for (text, l) in &self.literals {
            let len = text.len() as i32;
            f.i32_const(len).call(new_string).global_set(l.global);
            f.global_get(l.global).i32_const(STRING_HEADER).i32_add();
            f.i32_const(0).i32_const(len).memory_init(0, l.segment);
        }
        f.call(main);
        let start = self.m.func(&[], &[], &[], &body);
        self.m.export_func("_start", start);
    }

    fn literal(&mut self, text: &str) -> u32 {
        if let Some(l) = self.literals.get(text) {
            return l.global;
        }
        let global = self.m.global(ValType::I32);
        let segment = self.m.passive_data(text.as_bytes());
        self.literals
            .insert(text.to_string(), Literal { global, segment });
        global
    }

    fn leave_scopes(&mut self, from: usize) {
        let release = self.rt("check_release");
        let mut f = InstructionSink::new(&mut self.body);
        for &l in self.scopes[from..].iter().flatten() {
            f.local_get(l).call(release).i32_const(0).local_set(l);
        }
    }

    fn release_params(&mut self) {
        let release = self.rt("check_release");
        let mut f = InstructionSink::new(&mut self.body);
        for &p in &self.owned_params {
            f.local_get(p).call(release);
        }
    }

    fn retain_if_str(&mut self, ty: &Ty) {
        if *ty == Ty::Str {
            self.call_rt("check_retain");
        }
    }

    fn release_old_if_str(&mut self, ty: &Ty, get_old: impl FnOnce(&mut InstructionSink)) {
        if *ty == Ty::Str {
            get_old(&mut self.f());
            self.call_rt("check_release");
        }
    }

    fn borrowing(&mut self, func: u32, args: &[Ty]) {
        if !args.contains(&Ty::Str) {
            self.f().call(func);
            return;
        }
        let temps: Vec<u32> = args.iter().map(|ty| self.temp(slot(ty))).collect();
        for &t in temps.iter().rev() {
            self.f().local_set(t);
        }
        for &t in &temps {
            self.f().local_get(t);
        }
        self.f().call(func);
        for (t, ty) in temps.into_iter().zip(args) {
            if *ty == Ty::Str {
                self.f().local_get(t);
                self.call_rt("check_release");
            }
            self.free.push(t);
        }
    }

    fn borrowing_rt(&mut self, name: &str, args: &[Ty]) {
        let func = self.rt(name);
        self.borrowing(func, args);
    }

    fn expr(&mut self, e: &Expr) -> Compiled<()> {
        self.kind(e)?;
        if e.ty == Ty::Never {
            self.f().unreachable();
        }
        Ok(())
    }

    fn discard(&mut self, e: &Expr) -> Compiled<()> {
        self.expr(e)?;
        match e.ty {
            Ty::Str => self.call_rt("check_release"),
            Ty::Void | Ty::Never => {}
            _ => {
                self.f().drop();
            }
        }
        Ok(())
    }

    fn value_or_discard(&mut self, e: &Expr, ty: &Ty) -> Compiled<()> {
        if *ty == Ty::Void {
            self.discard(e)
        } else {
            self.expr(e)
        }
    }

    fn kind(&mut self, e: &Expr) -> Compiled<()> {
        match &e.kind {
            ExprKind::Int(n) => {
                self.f().i64_const(*n);
            }
            ExprKind::Float(x) => {
                self.f().f64_const((*x).into());
            }
            ExprKind::Bool(b) => {
                self.f().i32_const(*b as i32);
            }
            ExprKind::Str(text) => {
                let global = self.literal(text);
                self.f().global_get(global);
                self.call_rt("check_retain");
            }
            ExprKind::Local(i) => {
                self.f().local_get(*i as u32);
                self.retain_if_str(&e.ty);
            }
            ExprKind::Global(i) => {
                let global = self.globals[*i];
                self.f().global_get(global);
                self.retain_if_str(&e.ty);
            }
            ExprKind::Let(i, value) => {
                self.expr(value)?;
                self.f().local_set(*i as u32);
                if value.ty == Ty::Str {
                    let scope = self.scopes.last_mut().expect("a let is in a block");
                    scope.push(*i as u32);
                }
            }
            ExprKind::SetLocal(i, value) => {
                let local = *i as u32;
                self.expr(value)?;
                self.release_old_if_str(&value.ty, |f| {
                    f.local_get(local);
                });
                self.f().local_set(local);
            }
            ExprKind::SetGlobal(i, value) => {
                let global = self.globals[*i];
                self.expr(value)?;
                self.release_old_if_str(&value.ty, |f| {
                    f.global_get(global);
                });
                self.f().global_set(global);
            }
            ExprKind::ToFloat(x) => {
                self.expr(x)?;
                self.f().f64_convert_i64_s();
            }
            ExprKind::Text(x) => {
                self.expr(x)?;
                match x.ty {
                    Ty::Int => self.call_rt("check_int_text"),
                    Ty::Float => self.call_rt("check_float_text"),
                    Ty::Bool => self.call_rt("check_bool_text"),
                    _ => return unsupported(x.span),
                }
            }
            ExprKind::Concat(parts) => {
                for (i, part) in parts.iter().enumerate() {
                    self.expr(part)?;
                    if i > 0 {
                        self.borrowing_rt("check_concat", &[Ty::Str, Ty::Str]);
                    }
                }
            }
            ExprKind::Unary(op, x) => {
                self.expr(x)?;
                match (op, &x.ty) {
                    (UnOp::Neg, Ty::Int) => {
                        self.at(e.span);
                        self.call_rt("check_int_neg");
                    }
                    (UnOp::Neg, Ty::Float) => {
                        self.f().f64_neg();
                    }
                    (UnOp::BitNot, Ty::Int) => {
                        self.f().i64_const(-1).i64_xor();
                    }
                    (UnOp::Not, Ty::Bool) => {
                        self.f().i32_eqz();
                    }
                    _ => return unsupported(e.span),
                }
            }
            ExprKind::Binary(BinOp::And, l, r) => {
                self.expr(l)?;
                self.branch(
                    BlockType::Result(ValType::I32),
                    |g| g.expr(r),
                    |g| {
                        g.f().i32_const(0);
                        Ok(())
                    },
                )?;
            }
            ExprKind::Binary(BinOp::Or, l, r) => {
                self.expr(l)?;
                self.branch(
                    BlockType::Result(ValType::I32),
                    |g| {
                        g.f().i32_const(1);
                        Ok(())
                    },
                    |g| g.expr(r),
                )?;
            }
            ExprKind::Binary(op, l, r) => {
                self.expr(l)?;
                self.expr(r)?;
                self.binary(*op, &l.ty, e.span)?;
            }
            ExprKind::Call(id, args) => {
                for arg in args {
                    self.expr(arg)?;
                }
                let func = self.base + *id as u32;
                self.f().call(func);
            }
            ExprKind::External(name, args) => {
                let Some(func) = self.m.runtime_func(&format!("check_{name}")) else {
                    return unsupported(e.span);
                };
                for arg in args {
                    self.expr(arg)?;
                }
                let tys: Vec<Ty> = args.iter().map(|a| a.ty.clone()).collect();
                self.borrowing(func, &tys);
            }
            ExprKind::Print(x) => {
                self.expr(x)?;
                self.borrowing_rt("check_print", &[Ty::Str]);
            }
            ExprKind::Block(items) => {
                self.scopes.push(Vec::new());
                for (i, item) in items.iter().enumerate() {
                    if i + 1 == items.len() {
                        self.value_or_discard(item, &e.ty)?;
                    } else {
                        self.discard(item)?;
                    }
                }
                self.leave_scopes(self.scopes.len() - 1);
                self.scopes.pop();
            }
            ExprKind::If(cond, then, otherwise) => {
                self.expr(cond)?;
                let ty = &e.ty;
                match otherwise {
                    Some(o) => self.branch(
                        block_type(ty),
                        |g| g.value_or_discard(then, ty),
                        |g| g.value_or_discard(o, ty),
                    )?,
                    None => {
                        self.f().if_(BlockType::Empty);
                        self.labels.push(Label::Plain);
                        self.discard(then)?;
                        self.labels.pop();
                        self.f().end();
                    }
                }
            }
            ExprKind::While(cond, body) => {
                let depth = self.scopes.len();
                self.f().block(BlockType::Empty).loop_(BlockType::Empty);
                self.labels.push(Label::Break(depth));
                self.labels.push(Label::Continue(depth));
                self.expr(cond)?;
                self.f().i32_eqz().br_if(1);
                self.discard(body)?;
                self.f().br(0).end().end();
                self.labels.truncate(self.labels.len() - 2);
            }
            ExprKind::ForRange {
                var,
                start,
                end,
                inclusive,
                body,
            } => {
                let (i, n) = (self.temp(ValType::I64), self.temp(ValType::I64));
                self.expr(start)?;
                self.f().local_set(i);
                self.expr(end)?;
                self.f().local_set(n);
                let depth = self.scopes.len();
                let mut f = self.f();
                f.block(BlockType::Empty).loop_(BlockType::Empty);
                f.local_get(i).local_get(n);
                if *inclusive {
                    f.i64_gt_s();
                } else {
                    f.i64_ge_s();
                }
                f.br_if(1).local_get(i).local_set(*var as u32);
                f.block(BlockType::Empty);
                self.labels.push(Label::Break(depth));
                self.labels.push(Label::Plain);
                self.labels.push(Label::Continue(depth));
                self.discard(body)?;
                let mut f = self.f();
                f.end();
                if *inclusive {
                    f.local_get(i).local_get(n).i64_eq().br_if(1);
                }
                f.local_get(i).i64_const(1).i64_add().local_set(i);
                f.br(0).end().end();
                self.labels.truncate(self.labels.len() - 3);
                self.free.extend([i, n]);
            }
            ExprKind::Break => self.jump(true),
            ExprKind::Continue => self.jump(false),
            ExprKind::Return(value) => {
                if let Some(v) = value {
                    self.expr(v)?;
                }
                self.leave_scopes(0);
                self.release_params();
                self.f().return_();
            }
        }
        Ok(())
    }

    fn branch(
        &mut self,
        ty: BlockType,
        then: impl FnOnce(&mut Gen) -> Compiled<()>,
        otherwise: impl FnOnce(&mut Gen) -> Compiled<()>,
    ) -> Compiled<()> {
        self.f().if_(ty);
        self.labels.push(Label::Plain);
        then(self)?;
        self.f().else_();
        otherwise(self)?;
        self.labels.pop();
        self.f().end();
        Ok(())
    }

    fn jump(&mut self, to_break: bool) {
        let (pos, depth) = (self.labels.iter().enumerate().rev())
            .find_map(|(i, label)| match (label, to_break) {
                (Label::Break(d), true) | (Label::Continue(d), false) => Some((i, *d)),
                _ => None,
            })
            .expect("break and continue are in a loop");
        self.leave_scopes(depth);
        let relative = (self.labels.len() - 1 - pos) as u32;
        self.f().br(relative);
    }

    fn binary(&mut self, op: BinOp, ty: &Ty, span: Span) -> Compiled<()> {
        use BinOp::*;
        let int_op = match op {
            Add => Some("check_int_add"),
            Sub => Some("check_int_sub"),
            Mul => Some("check_int_mul"),
            Div => Some("check_int_div"),
            IntDiv => Some("check_int_floor_div"),
            Mod => Some("check_int_mod"),
            Pow => Some("check_int_pow"),
            Shl => Some("check_int_shl"),
            Shr => Some("check_int_shr"),
            _ => None,
        };
        if let (Ty::Int, Some(name)) = (ty, int_op) {
            self.at(span);
            self.call_rt(name);
            return Ok(());
        }
        match (ty, op) {
            (Ty::Int, BitAnd) => _ = self.f().i64_and(),
            (Ty::Int, BitOr) => _ = self.f().i64_or(),
            (Ty::Int, BitXor) => _ = self.f().i64_xor(),
            (Ty::Int, Eq) => _ = self.f().i64_eq(),
            (Ty::Int, Ne) => _ = self.f().i64_ne(),
            (Ty::Int, Lt) => _ = self.f().i64_lt_s(),
            (Ty::Int, Gt) => _ = self.f().i64_gt_s(),
            (Ty::Int, Le) => _ = self.f().i64_le_s(),
            (Ty::Int, Ge) => _ = self.f().i64_ge_s(),
            (Ty::Float, Add) => _ = self.f().f64_add(),
            (Ty::Float, Sub) => _ = self.f().f64_sub(),
            (Ty::Float, Mul) => _ = self.f().f64_mul(),
            (Ty::Float, Div) => _ = self.f().f64_div(),
            (Ty::Float, Pow) => self.call_rt("check_float_pow"),
            (Ty::Float, Eq) => _ = self.f().f64_eq(),
            (Ty::Float, Ne) => _ = self.f().f64_ne(),
            (Ty::Float, Lt) => _ = self.f().f64_lt(),
            (Ty::Float, Gt) => _ = self.f().f64_gt(),
            (Ty::Float, Le) => _ = self.f().f64_le(),
            (Ty::Float, Ge) => _ = self.f().f64_ge(),
            (Ty::Bool, Eq) => _ = self.f().i32_eq(),
            (Ty::Bool, Ne) => _ = self.f().i32_ne(),
            (Ty::Str, Add) => self.borrowing_rt("check_concat", &[Ty::Str, Ty::Str]),
            (Ty::Str, Eq | Ne) => {
                self.borrowing_rt("check_str_eq", &[Ty::Str, Ty::Str]);
                if op == Ne {
                    self.f().i32_eqz();
                }
            }
            (Ty::Str, Lt | Gt | Le | Ge) => {
                self.borrowing_rt("check_str_cmp", &[Ty::Str, Ty::Str]);
                let mut f = self.f();
                f.i32_const(0);
                match op {
                    Lt => f.i32_lt_s(),
                    Gt => f.i32_gt_s(),
                    Le => f.i32_le_s(),
                    _ => f.i32_ge_s(),
                };
            }
            _ => return unsupported(span),
        }
        Ok(())
    }
}
