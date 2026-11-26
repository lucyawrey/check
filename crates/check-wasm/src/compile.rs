use std::collections::{BTreeMap, HashMap};

use check_core::ast::{BinOp, UnOp};
use check_core::diag::{Code, Diagnostic, Span};
use check_core::ir::{Arg, Expr, ExprKind, Func, ListOp, Place, Program, Root, Step};
use check_core::types::{Show, Ty};
use check_runtime::Panic;
use wasm_encoder::{BlockType, InstructionSink, MemArg, MemArgTarget, ValType};

use crate::Module;

type Compiled<T> = Result<T, Diagnostic>;

const STRING_HEADER: i32 = 8;
const BLOCK_HEADER: u32 = 8;
const LIST_HEADER: u32 = 16;
const BOX_SIZE: i32 = 16;

enum Label {
    Plain,
    Break(usize),
    Continue(usize),
}

struct Literal {
    global: u32,
    segment: u32,
}

#[derive(Clone)]
enum Helper {
    Release(Ty),
    Unique(Ty),
    Text(Ty),
    Eq(Ty),
}

#[derive(Clone, Copy)]
enum Slot {
    Local(u32),
    Global(u32),
    Mem(u32, u32),
}

struct Gen<'p> {
    m: Module,
    program: &'p Program,
    base: u32,
    globals: Vec<u32>,
    literals: BTreeMap<String, Literal>,
    helpers: HashMap<String, u32>,
    queue: Vec<Helper>,
    body: Vec<u8>,
    locals: Vec<ValType>,
    free: Vec<u32>,
    owned_params: Vec<(u32, Ty)>,
    refs: Vec<u32>,
    scopes: Vec<Vec<(u32, Ty)>>,
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
        program,
        base,
        globals,
        literals: BTreeMap::new(),
        helpers: HashMap::new(),
        queue: Vec::new(),
        body: Vec::new(),
        locals: Vec::new(),
        free: Vec::new(),
        owned_params: Vec::new(),
        refs: Vec::new(),
        scopes: Vec::new(),
        labels: Vec::new(),
    };
    for func in &program.funcs {
        g.func(func)?;
    }
    let mut next = 0;
    while next < g.queue.len() {
        let helper = g.queue[next].clone();
        g.helper(&helper);
        next += 1;
    }
    g.start(base + program.main as u32);
    Ok(g.m.finish())
}

fn unsupported<T>(span: Span) -> Compiled<T> {
    Err(Diagnostic::error(Code::wasm_unsupported, span))
}

fn is_ptr(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Str | Ty::Maybe(_) | Ty::List(_) | Ty::Record(..) | Ty::Named(..) | Ty::Range
    )
}

fn boxed(inner: &Ty) -> bool {
    !is_ptr(inner) || matches!(inner, Ty::Maybe(_))
}

fn val(ty: &Ty) -> Option<ValType> {
    match ty {
        Ty::Int => Some(ValType::I64),
        Ty::Float => Some(ValType::F64),
        Ty::Bool => Some(ValType::I32),
        ty if is_ptr(ty) => Some(ValType::I32),
        _ => None,
    }
}

fn slot(ty: &Ty) -> ValType {
    val(ty).expect("a variable holds a value")
}

fn block_type(ty: &Ty) -> BlockType {
    val(ty).map_or(BlockType::Empty, BlockType::Result)
}

fn stride(ty: &Ty) -> u32 {
    match slot(ty) {
        ValType::I32 => 4,
        _ => 8,
    }
}

fn field_offset(i: usize) -> u32 {
    BLOCK_HEADER + 8 * i as u32
}

fn mem(ty: &Ty, offset: u32) -> MemArg {
    MemArg {
        offset: offset.into(),
        align: stride(ty).trailing_zeros(),
        target: MemArgTarget::Memory(0),
        ordering: None,
    }
}

fn word(offset: u32) -> MemArg {
    mem(&Ty::Bool, offset)
}

fn elem(ty: &Ty) -> &Ty {
    match ty {
        Ty::List(elem) | Ty::Maybe(elem) => elem,
        _ => unreachable!("a list or maybe"),
    }
}

impl Gen<'_> {
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

    fn set_temp(&mut self, ty: &Ty) -> u32 {
        let t = self.temp(slot(ty));
        self.f().local_set(t);
        t
    }

    fn fields(&self, ty: &Ty) -> Vec<(Option<String>, Ty)> {
        match ty {
            Ty::Record(names, fields) => (fields.iter().enumerate())
                .map(|(i, f)| (names.get(i).cloned(), f.clone()))
                .collect(),
            Ty::Range => vec![(None, Ty::Int), (None, Ty::Int)],
            Ty::Named(name, args) => {
                let class = &self.program.classes[name];
                let map = class.params.iter().cloned().zip(args.clone()).collect();
                (class.fields.iter())
                    .map(|(n, f)| (Some(n.clone()), f.substitute(&map)))
                    .collect()
            }
            _ => unreachable!("a record or class"),
        }
    }

    fn helper_func(&mut self, helper: Helper) -> u32 {
        let ty = match &helper {
            Helper::Release(ty) | Helper::Unique(ty) | Helper::Text(ty) | Helper::Eq(ty) => ty,
        };
        let kind = match &helper {
            Helper::Release(_) => "release",
            Helper::Unique(_) => "unique",
            Helper::Text(_) => "text",
            Helper::Eq(_) => "eq",
        };
        let key = format!("{kind} {}", Show(ty));
        if let Some(&func) = self.helpers.get(&key) {
            return func;
        }
        let func = self.base + (self.program.funcs.len() + self.queue.len()) as u32;
        self.helpers.insert(key, func);
        self.queue.push(helper);
        func
    }

    fn call_helper(&mut self, helper: Helper) {
        let func = self.helper_func(helper);
        self.f().call(func);
    }

    fn func(&mut self, func: &Func) -> Compiled<()> {
        self.locals = func.locals.iter().map(slot).collect();
        self.refs = func.refs.iter().map(|&r| r as u32).collect();
        self.owned_params = (0..func.params)
            .filter(|&i| is_ptr(&func.locals[i]) && !func.refs.contains(&i))
            .map(|i| (i as u32, func.locals[i].clone()))
            .collect();
        self.value_or_discard(&func.body, &func.ret)?;
        self.release_params();
        self.push_refs();
        let mut results: Vec<ValType> = val(&func.ret).into_iter().collect();
        results.extend(func.refs.iter().map(|&r| slot(&func.locals[r])));
        let body = std::mem::take(&mut self.body);
        let (params, locals) = self.locals.split_at(func.params);
        self.m.func(params, &results, locals, &body);
        self.free.clear();
        Ok(())
    }

    fn helper(&mut self, helper: &Helper) {
        let (params, results): (&[ValType], &[ValType]) = match helper {
            Helper::Release(_) => (&[ValType::I32], &[]),
            Helper::Unique(_) | Helper::Text(_) => (&[ValType::I32], &[ValType::I32]),
            Helper::Eq(_) => (&[ValType::I32, ValType::I32], &[ValType::I32]),
        };
        self.locals = params.to_vec();
        match helper {
            Helper::Release(ty) => self.release_body(ty),
            Helper::Unique(ty) => self.unique_body(ty),
            Helper::Text(ty) => self.text_body(ty),
            Helper::Eq(ty) => self.eq_body(ty),
        }
        let body = std::mem::take(&mut self.body);
        let (params, locals) = self.locals.split_at(params.len());
        self.m.func(params, results, locals, &body);
        self.free.clear();
    }

    fn each_item(&mut self, list: u32, mut body: impl FnMut(&mut Self, u32)) {
        let i = self.temp(ValType::I64);
        self.f().i64_const(0).local_set(i);
        self.f().block(BlockType::Empty).loop_(BlockType::Empty);
        self.in_range(list, i, false);
        self.f().i32_eqz().br_if(1);
        body(self, i);
        let mut f = self.f();
        f.local_get(i).i64_const(1).i64_add().local_set(i);
        f.br(0).end().end();
        self.free.push(i);
    }

    fn release_body(&mut self, ty: &Ty) {
        let mut f = self.f();
        f.local_get(0)
            .i32_eqz()
            .if_(BlockType::Empty)
            .return_()
            .end();
        f.local_get(0).i32_load(word(0)).i32_const(1).i32_gt_u();
        f.if_(BlockType::Empty);
        f.local_get(0)
            .local_get(0)
            .i32_load(word(0))
            .i32_const(1)
            .i32_sub();
        f.i32_store(word(0)).return_().end();
        match ty {
            Ty::List(elem) => {
                if is_ptr(elem) {
                    self.each_item(0, |g, i| {
                        g.item_addr(0, i, elem);
                        g.load(elem, LIST_HEADER);
                        g.release(elem);
                    });
                }
                self.f().local_get(0).i32_const(stride(elem) as i32);
                self.call_rt("check_list_free");
            }
            Ty::Maybe(inner) => {
                if is_ptr(inner) {
                    self.f().local_get(0);
                    self.load(inner, BLOCK_HEADER);
                    self.release(inner);
                }
                self.f().local_get(0).i32_const(BOX_SIZE);
                self.call_rt("check_free");
            }
            _ => {
                let fields = self.fields(ty);
                for (i, (_, field)) in fields.iter().enumerate() {
                    if is_ptr(field) {
                        self.f().local_get(0);
                        self.load(field, field_offset(i));
                        self.release(field);
                    }
                }
                self.f()
                    .local_get(0)
                    .i32_const(field_offset(fields.len()) as i32);
                self.call_rt("check_free");
            }
        }
    }

    fn unique_body(&mut self, ty: &Ty) {
        let mut f = self.f();
        f.local_get(0).i32_load(word(0)).i32_const(1).i32_eq();
        f.if_(BlockType::Empty).local_get(0).return_().end();
        let copy = self.temp(ValType::I32);
        match ty {
            Ty::List(elem) => {
                self.f().local_get(0).i32_const(stride(elem) as i32);
                self.call_rt("check_list_copy");
                self.f().local_set(copy);
                if is_ptr(elem) {
                    self.each_item(copy, |g, i| {
                        g.item_addr(copy, i, elem);
                        g.load(elem, LIST_HEADER);
                        g.call_rt("check_retain");
                        g.f().drop();
                    });
                }
            }
            _ => {
                let fields = self.fields(ty);
                self.f()
                    .local_get(0)
                    .i32_const(field_offset(fields.len()) as i32);
                self.call_rt("check_copy");
                self.f().local_set(copy);
                for (i, (_, field)) in fields.iter().enumerate() {
                    if is_ptr(field) {
                        self.f().local_get(copy);
                        self.load(field, field_offset(i));
                        self.call_rt("check_retain");
                        self.f().drop();
                    }
                }
            }
        }
        self.f().local_get(copy);
    }

    fn append(&mut self, acc: u32) {
        let piece = self.set_temp(&Ty::Str);
        self.f().local_get(acc).local_get(piece);
        self.borrowing_rt("check_concat", &[Ty::Str, Ty::Str]);
        self.f().local_set(acc);
        self.free.push(piece);
    }

    fn append_text(&mut self, acc: u32, text: &str) {
        self.literal(text);
        self.append(acc);
    }

    fn text_body(&mut self, ty: &Ty) {
        let acc = self.temp(ValType::I32);
        match ty {
            Ty::Maybe(inner) => {
                self.f().local_get(0).i32_eqz().if_(BlockType::Empty);
                self.literal("None");
                self.f().return_().end();
                self.literal("Some(");
                self.f().local_set(acc).local_get(0);
                if boxed(inner) {
                    self.load(inner, BLOCK_HEADER);
                }
                self.text_code(inner);
                self.append(acc);
                self.append_text(acc, ")");
            }
            Ty::List(elem) => {
                self.literal("[");
                self.f().local_set(acc);
                self.each_item(0, |g, i| {
                    g.f().local_get(i).i64_eqz().i32_eqz().if_(BlockType::Empty);
                    g.append_text(acc, ", ");
                    g.f().end();
                    g.item_addr(0, i, elem);
                    g.load(elem, LIST_HEADER);
                    g.text_code(elem);
                    g.append(acc);
                });
                self.append_text(acc, "]");
            }
            _ => {
                let name = match ty {
                    Ty::Named(name, _) => name.as_str(),
                    _ => "",
                };
                self.literal(&format!("{name}("));
                self.f().local_set(acc);
                for (i, (field_name, field)) in self.fields(ty).iter().enumerate() {
                    let sep = if i > 0 { ", " } else { "" };
                    let label = match field_name {
                        Some(n) => format!("{sep}{n} = "),
                        None => sep.to_string(),
                    };
                    if !label.is_empty() {
                        self.append_text(acc, &label);
                    }
                    self.f().local_get(0);
                    self.load(field, field_offset(i));
                    self.text_code(field);
                    self.append(acc);
                }
                self.append_text(acc, ")");
            }
        }
        self.f().local_get(acc);
    }

    fn eq_body(&mut self, ty: &Ty) {
        let unequal = |g: &mut Self| {
            g.f()
                .i32_eqz()
                .if_(BlockType::Empty)
                .i32_const(0)
                .return_()
                .end();
        };
        match ty {
            Ty::Maybe(inner) => {
                let mut f = self.f();
                f.local_get(0).i32_eqz().local_get(1).i32_eqz().i32_or();
                f.if_(BlockType::Empty);
                f.local_get(0).local_get(1).i32_eq().return_().end();
                for p in [0, 1] {
                    self.f().local_get(p);
                    if boxed(inner) {
                        self.load(inner, BLOCK_HEADER);
                    }
                }
                self.eq_value(inner);
                return;
            }
            Ty::List(elem) => {
                self.len(0);
                self.len(1);
                self.f().i32_eq();
                unequal(self);
                self.each_item(0, |g, i| {
                    for p in [0, 1] {
                        g.item_addr(p, i, elem);
                        g.load(elem, LIST_HEADER);
                    }
                    g.eq_value(elem);
                    unequal(g);
                });
            }
            _ => {
                for (i, (_, field)) in self.fields(ty).iter().enumerate() {
                    for p in [0, 1] {
                        self.f().local_get(p);
                        self.load(field, field_offset(i));
                    }
                    self.eq_value(field);
                    unequal(self);
                }
            }
        }
        self.f().i32_const(1);
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

    fn literal(&mut self, text: &str) {
        let global = match self.literals.get(text) {
            Some(l) => l.global,
            None => {
                let global = self.m.global(ValType::I32);
                let segment = self.m.passive_data(text.as_bytes());
                self.literals
                    .insert(text.to_string(), Literal { global, segment });
                global
            }
        };
        self.f().global_get(global);
        self.call_rt("check_retain");
    }

    fn retain(&mut self, ty: &Ty) {
        if is_ptr(ty) {
            self.call_rt("check_retain");
        }
    }

    fn release(&mut self, ty: &Ty) {
        match ty {
            Ty::Str => self.call_rt("check_release"),
            Ty::Maybe(inner) if !boxed(inner) => self.release(inner),
            ty if is_ptr(ty) => self.call_helper(Helper::Release(ty.clone())),
            _ => _ = self.f().drop(),
        }
    }

    fn release_local(&mut self, local: u32, ty: &Ty) {
        if is_ptr(ty) {
            self.f().local_get(local);
            self.release(ty);
        }
    }

    fn leave_scopes(&mut self, from: usize) {
        let held: Vec<(u32, Ty)> = self.scopes[from..].iter().flatten().cloned().collect();
        for (l, ty) in held.iter().filter(|(_, ty)| is_ptr(ty)) {
            self.release_local(*l, ty);
            self.f().i32_const(0).local_set(*l);
        }
    }

    fn release_params(&mut self) {
        for (p, ty) in self.owned_params.clone() {
            self.release_local(p, &ty);
        }
    }

    fn push_refs(&mut self) {
        for r in self.refs.clone() {
            self.f().local_get(r);
        }
    }

    fn borrowing(&mut self, func: u32, args: &[Ty]) {
        if !args.iter().any(is_ptr) {
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
            self.release_local(t, ty);
            self.free.push(t);
        }
    }

    fn borrowing_rt(&mut self, name: &str, args: &[Ty]) {
        let func = self.rt(name);
        self.borrowing(func, args);
    }

    fn load(&mut self, ty: &Ty, offset: u32) {
        let arg = mem(ty, offset);
        match slot(ty) {
            ValType::I64 => self.f().i64_load(arg),
            ValType::F64 => self.f().f64_load(arg),
            _ => self.f().i32_load(arg),
        };
    }

    fn store(&mut self, ty: &Ty, offset: u32) {
        let arg = mem(ty, offset);
        match slot(ty) {
            ValType::I64 => self.f().i64_store(arg),
            ValType::F64 => self.f().f64_store(arg),
            _ => self.f().i32_store(arg),
        };
    }

    fn panic(&mut self, p: Panic, span: Span) {
        self.f().i32_const(p as i32);
        self.at(span);
        self.f().i32_const(0);
        self.call_rt("check_panic");
        self.f().unreachable();
    }

    fn some(&mut self, inner: &Ty) {
        if !boxed(inner) {
            return;
        }
        let value = self.set_temp(inner);
        let b = self.temp(ValType::I32);
        self.f().i32_const(BOX_SIZE);
        self.call_rt("check_alloc");
        self.f().local_tee(b).local_get(value);
        self.store(inner, BLOCK_HEADER);
        self.f().local_get(b);
        self.free.extend([value, b]);
    }

    fn unwrap(&mut self, inner: &Ty) {
        if !boxed(inner) {
            return;
        }
        let b = self.set_temp(&Ty::Bool);
        self.f().local_get(b);
        self.load(inner, BLOCK_HEADER);
        self.retain(inner);
        self.f().local_get(b);
        self.call_helper(Helper::Release(Ty::maybe(inner.clone())));
        self.free.push(b);
    }

    fn item_addr(&mut self, list: u32, index: u32, elem: &Ty) {
        self.f().local_get(list).local_get(index);
        let mut f = self.f();
        f.i32_wrap_i64()
            .i32_const(stride(elem) as i32)
            .i32_mul()
            .i32_add();
    }

    fn len(&mut self, list: u32) {
        self.f().local_get(list).i32_load(word(4));
    }

    fn in_range(&mut self, list: u32, index: u32, inclusive: bool) {
        self.f().local_get(index);
        self.len(list);
        self.f().i64_extend_i32_u();
        if inclusive {
            self.f().i64_le_u();
        } else {
            self.f().i64_lt_u();
        }
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
        if !matches!(e.ty, Ty::Void | Ty::Never) {
            self.release(&e.ty);
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

    fn text_code(&mut self, ty: &Ty) {
        match ty {
            Ty::Int => self.call_rt("check_int_text"),
            Ty::Float => self.call_rt("check_float_text"),
            Ty::Bool => self.call_rt("check_bool_text"),
            Ty::Str => self.call_rt("check_str_quote"),
            _ => self.call_helper(Helper::Text(ty.clone())),
        }
    }

    fn eq_value(&mut self, ty: &Ty) {
        match ty {
            Ty::Int => _ = self.f().i64_eq(),
            Ty::Float => _ = self.f().f64_eq(),
            Ty::Bool => _ = self.f().i32_eq(),
            Ty::Str => self.call_rt("check_str_eq"),
            _ => self.call_helper(Helper::Eq(ty.clone())),
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
            ExprKind::Str(text) => self.literal(text),
            ExprKind::Local(i) => {
                self.f().local_get(*i as u32);
                self.retain(&e.ty);
            }
            ExprKind::Global(i) => {
                let global = self.globals[*i];
                self.f().global_get(global);
                self.retain(&e.ty);
            }
            ExprKind::Let(i, value) => {
                self.expr(value)?;
                self.f().local_set(*i as u32);
                if is_ptr(&value.ty) {
                    let scope = self.scopes.last_mut().expect("a let is in a block");
                    scope.push((*i as u32, value.ty.clone()));
                }
            }
            ExprKind::SetLocal(i, value) => {
                self.expr(value)?;
                self.store_slot(Slot::Local(*i as u32), &value.ty);
            }
            ExprKind::SetGlobal(i, value) => {
                self.expr(value)?;
                self.store_slot(Slot::Global(self.globals[*i]), &value.ty);
            }
            ExprKind::ToFloat(x) => {
                self.expr(x)?;
                self.f().f64_convert_i64_s();
            }
            ExprKind::Text(x) => {
                self.expr(x)?;
                match &x.ty {
                    Ty::Int => self.call_rt("check_int_text"),
                    Ty::Float => self.call_rt("check_float_text"),
                    Ty::Bool => self.call_rt("check_bool_text"),
                    ty if is_ptr(ty) => {
                        let func = self.helper_func(Helper::Text(ty.clone()));
                        self.borrowing(func, std::slice::from_ref(ty));
                    }
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
            ExprKind::Call(id, args) => self.call(*id, args)?,
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
            ExprKind::ForList { var, list, body } => {
                let elem = elem(&list.ty).clone();
                self.expr(list)?;
                let l = self.temp(ValType::I32);
                self.f().local_set(l);
                self.scopes.push(vec![(l, list.ty.clone())]);
                let i = self.temp(ValType::I64);
                self.f().i64_const(0).local_set(i);
                let var = *var as u32;
                let depth = self.scopes.len();
                self.f().block(BlockType::Empty).loop_(BlockType::Empty);
                self.in_range(l, i, false);
                self.f().i32_eqz().br_if(1);
                self.item_addr(l, i, &elem);
                self.load(&elem, LIST_HEADER);
                self.retain(&elem);
                self.f().local_set(var);
                self.scopes.push(vec![(var, elem.clone())]);
                self.f().block(BlockType::Empty);
                self.labels.push(Label::Break(depth));
                self.labels.push(Label::Plain);
                self.labels.push(Label::Continue(depth));
                self.discard(body)?;
                self.f().end();
                self.leave_scopes(depth);
                self.scopes.pop();
                let mut f = self.f();
                f.local_get(i).i64_const(1).i64_add().local_set(i);
                f.br(0).end().end();
                self.labels.truncate(self.labels.len() - 3);
                self.leave_scopes(depth - 1);
                self.scopes.pop();
                self.free.push(i);
            }
            ExprKind::Break => self.jump(true),
            ExprKind::Continue => self.jump(false),
            ExprKind::Return(value) => {
                if let Some(v) = value {
                    self.expr(v)?;
                }
                self.leave_scopes(0);
                self.release_params();
                self.push_refs();
                self.f().return_();
            }
            ExprKind::Some(x) => {
                self.expr(x)?;
                self.some(&x.ty);
            }
            ExprKind::None => {
                self.f().i32_const(0);
            }
            ExprKind::Coalesce(l, r) => {
                self.expr(l)?;
                let t = self.set_temp(&l.ty);
                let keep = e.ty == l.ty;
                let inner = elem(&l.ty).clone();
                self.f().local_get(t);
                self.branch(
                    block_type(&e.ty),
                    |g| {
                        g.f().local_get(t);
                        if !keep {
                            g.unwrap(&inner);
                        }
                        Ok(())
                    },
                    |g| g.value_or_discard(r, &e.ty),
                )?;
                self.free.push(t);
            }
            ExprKind::Panic(reason) => {
                self.f().i32_const(Panic::PanicCalled as i32);
                self.at(e.span);
                self.expr(reason)?;
                self.call_rt("check_panic");
            }
            ExprKind::OrPanic(x, reason) => {
                self.expr(x)?;
                let t = self.set_temp(&x.ty);
                self.f().local_get(t).i32_eqz().if_(BlockType::Empty);
                self.f().i32_const(Panic::OrPanicOnNone as i32);
                self.at(e.span);
                self.expr(reason)?;
                self.call_rt("check_panic");
                self.f().unreachable().end().local_get(t);
                self.unwrap(elem(&x.ty));
                self.free.push(t);
            }
            ExprKind::Record(fields) => {
                let r = self.temp(ValType::I32);
                self.f().i32_const(field_offset(fields.len()) as i32);
                self.call_rt("check_alloc");
                self.f().local_set(r);
                for (i, field) in fields.iter().enumerate() {
                    self.f().local_get(r);
                    self.expr(field)?;
                    self.store(&field.ty, field_offset(i));
                }
                self.f().local_get(r);
                self.free.push(r);
            }
            ExprKind::Field(base, i) => {
                self.expr(base)?;
                let b = self.set_temp(&base.ty);
                self.f().local_get(b);
                self.load(&e.ty, field_offset(*i));
                self.retain(&e.ty);
                self.release_local(b, &base.ty);
                self.free.push(b);
            }
            ExprKind::List(items) => {
                let elem = elem(&e.ty).clone();
                let l = self.temp(ValType::I32);
                self.f().i32_const(stride(&elem) as i32);
                self.f().i32_const(items.len() as i32);
                self.call_rt("check_list_new");
                self.f().local_set(l);
                for (i, item) in items.iter().enumerate() {
                    self.f().local_get(l);
                    self.expr(item)?;
                    self.store(&elem, LIST_HEADER + stride(&elem) * i as u32);
                }
                self.f().local_get(l);
                self.free.push(l);
            }
            ExprKind::Index(list, index) => {
                let elem = elem(&list.ty).clone();
                self.expr(list)?;
                let l = self.set_temp(&list.ty);
                self.expr(index)?;
                let i = self.set_temp(&Ty::Int);
                self.in_range(l, i, false);
                let wrap = e.ty != elem;
                self.branch(
                    block_type(&e.ty),
                    |g| {
                        g.item_addr(l, i, &elem);
                        g.load(&elem, LIST_HEADER);
                        g.retain(&elem);
                        if wrap {
                            g.some(&elem);
                        }
                        Ok(())
                    },
                    |g| {
                        match wrap {
                            true => _ = g.f().i32_const(0),
                            false => g.panic(Panic::IndexOutOfRange, e.span),
                        }
                        Ok(())
                    },
                )?;
                self.release_local(l, &list.ty);
                self.free.extend([l, i]);
            }
            ExprKind::Length(list) => {
                self.expr(list)?;
                let l = self.set_temp(&list.ty);
                self.len(l);
                self.f().i64_extend_i32_u();
                self.release_local(l, &list.ty);
                self.free.push(l);
            }
            ExprKind::Write(place, value) => {
                self.expr(value)?;
                let v = self.set_temp(&value.ty);
                let indices = self.indices(place)?;
                let target = self.walk(place, &indices);
                self.f().local_get(v);
                self.store_slot(target, place.ty());
                self.free.push(v);
                self.free.extend(indices);
            }
            ExprKind::ListOp(op, place, args) => self.list_op(*op, place, args, e)?,
        }
        Ok(())
    }

    fn call(&mut self, id: usize, args: &[Arg]) -> Compiled<()> {
        let mut evaluated = Vec::new();
        for arg in args {
            evaluated.push(match arg {
                Arg::Value(value) => {
                    self.expr(value)?;
                    vec![self.set_temp(&value.ty)]
                }
                Arg::Ref(place) => self.indices(place)?,
            });
        }
        let mut written = Vec::new();
        for (arg, temps) in args.iter().zip(evaluated) {
            match arg {
                Arg::Value(_) => {
                    self.f().local_get(temps[0]);
                    self.free.extend(temps);
                }
                Arg::Ref(place) => {
                    let target = self.walk(place, &temps);
                    self.load_slot(target, place.ty());
                    if is_ptr(place.ty()) {
                        self.f().i32_const(0);
                        self.store_raw_from_stack(target, place.ty());
                    }
                    written.push((place, temps));
                }
            }
        }
        let func = self.base + id as u32;
        self.f().call(func);
        let mut saved = Vec::new();
        for (place, _) in written.iter().rev() {
            saved.push(self.set_temp(place.ty()));
        }
        saved.reverse();
        for ((place, indices), t) in written.into_iter().zip(saved) {
            let target = self.walk(place, &indices);
            self.f().local_get(t);
            self.store_slot(target, place.ty());
            self.free.push(t);
            self.free.extend(indices);
        }
        Ok(())
    }

    fn indices(&mut self, place: &Place) -> Compiled<Vec<u32>> {
        let mut temps = Vec::new();
        for (step, _) in &place.steps {
            if let Step::Index(index) = step {
                self.expr(index)?;
                temps.push(self.set_temp(&Ty::Int));
            }
        }
        Ok(temps)
    }

    fn walk(&mut self, place: &Place, indices: &[u32]) -> Slot {
        let mut target = match place.root {
            Root::Local(i) => Slot::Local(i as u32),
            Root::Global(i) => Slot::Global(self.globals[i]),
        };
        let mut container = place.root_ty.clone();
        let mut indices = indices.iter();
        for (step, ty) in &place.steps {
            self.load_slot(target, &container);
            self.call_helper(Helper::Unique(container.clone()));
            let c = self.temp(ValType::I32);
            self.f().local_tee(c);
            self.store_raw_from_stack(target, &container);
            target = match step {
                Step::Field(i) => Slot::Mem(c, field_offset(*i)),
                Step::Index(index) => {
                    let i = *indices.next().expect("an evaluated index");
                    self.in_range(c, i, false);
                    self.f().i32_eqz().if_(BlockType::Empty);
                    self.panic(Panic::IndexOutOfRange, index.span);
                    self.f().end();
                    let a = self.temp(ValType::I32);
                    self.item_addr(c, i, ty);
                    self.f().local_set(a);
                    Slot::Mem(a, LIST_HEADER)
                }
            };
            container = ty.clone();
        }
        target
    }

    fn load_slot(&mut self, target: Slot, ty: &Ty) {
        match target {
            Slot::Local(l) => _ = self.f().local_get(l),
            Slot::Global(g) => _ = self.f().global_get(g),
            Slot::Mem(a, offset) => {
                self.f().local_get(a);
                self.load(ty, offset);
            }
        }
    }

    fn store_raw_from_stack(&mut self, target: Slot, ty: &Ty) {
        match target {
            Slot::Local(l) => _ = self.f().local_set(l),
            Slot::Global(g) => _ = self.f().global_set(g),
            Slot::Mem(a, offset) => {
                let v = self.set_temp(ty);
                self.f().local_get(a).local_get(v);
                self.store(ty, offset);
                self.free.push(v);
            }
        }
    }

    fn store_slot(&mut self, target: Slot, ty: &Ty) {
        if is_ptr(ty) {
            self.load_slot(target, ty);
            self.release(ty);
        }
        self.store_raw_from_stack(target, ty);
    }

    fn list_op(&mut self, op: ListOp, place: &Place, args: &[Expr], e: &Expr) -> Compiled<()> {
        let elem = elem(place.ty()).clone();
        let mut values = Vec::new();
        for arg in args {
            self.expr(arg)?;
            values.push(self.set_temp(&arg.ty));
        }
        let indices = self.indices(place)?;
        let target = self.walk(place, &indices);
        self.load_slot(target, place.ty());
        self.call_helper(Helper::Unique(place.ty().clone()));
        let l = self.set_temp(place.ty());
        let s = stride(&elem) as i32;
        let i = self.temp(ValType::I64);
        match op {
            ListOp::Push | ListOp::Insert => {
                let (index, item) = match op {
                    ListOp::Push => {
                        self.len(l);
                        self.f().i64_extend_i32_u().local_set(i);
                        (i, values[0])
                    }
                    _ => {
                        self.f().local_get(values[0]).local_set(i);
                        self.in_range(l, i, true);
                        self.f().i32_eqz().if_(BlockType::Empty);
                        self.panic(Panic::IndexOutOfRange, e.span);
                        self.f().end();
                        (i, values[1])
                    }
                };
                self.f()
                    .local_get(l)
                    .i32_const(s)
                    .local_get(index)
                    .i32_wrap_i64();
                self.call_rt("check_list_open");
                self.f().local_tee(l);
                self.store_raw_from_stack(target, place.ty());
                self.item_addr(l, index, &elem);
                self.f().local_get(item);
                self.store(&elem, LIST_HEADER);
            }
            ListOp::Pop | ListOp::RemoveAt => {
                self.f().local_get(l);
                self.store_raw_from_stack(target, place.ty());
                match op {
                    ListOp::Pop => {
                        self.len(l);
                        self.f()
                            .i64_extend_i32_u()
                            .i64_const(1)
                            .i64_sub()
                            .local_set(i);
                    }
                    _ => _ = self.f().local_get(values[0]).local_set(i),
                }
                self.in_range(l, i, false);
                self.branch(
                    block_type(&e.ty),
                    |g| {
                        g.item_addr(l, i, &elem);
                        g.load(&elem, LIST_HEADER);
                        g.f().local_get(l).i32_const(s).local_get(i).i32_wrap_i64();
                        g.call_rt("check_list_close");
                        g.some(&elem);
                        Ok(())
                    },
                    |g| {
                        g.f().i32_const(0);
                        Ok(())
                    },
                )?;
            }
        }
        self.free.extend([l, i]);
        self.free.extend(values);
        self.free.extend(indices);
        Ok(())
    }

    fn branch(
        &mut self,
        ty: BlockType,
        then: impl FnOnce(&mut Self) -> Compiled<()>,
        otherwise: impl FnOnce(&mut Self) -> Compiled<()>,
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
            (ty, Eq | Ne) if is_ptr(ty) => {
                let func = match ty {
                    Ty::Str => self.rt("check_str_eq"),
                    _ => self.helper_func(Helper::Eq(ty.clone())),
                };
                self.borrowing(func, &[ty.clone(), ty.clone()]);
                if op == Ne {
                    self.f().i32_eqz();
                }
            }
            _ => return unsupported(span),
        }
        Ok(())
    }
}
