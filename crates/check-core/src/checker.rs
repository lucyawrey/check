use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Code, Diagnostic, Span};
use crate::types::{Show, Subst, Ty};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Coercion {
    pub to_float: bool,
    pub wraps: usize,
}

#[derive(Default)]
pub struct Info {
    pub coercions: HashMap<NodeId, Coercion>,
    pub wrapping: HashSet<NodeId>,
    pub signatures: HashMap<NodeId, String>,
    pub unwrapped_index: HashSet<NodeId>,
}

#[derive(Clone)]
struct ParamSig {
    name: String,
    ty: Ty,
    has_default: bool,
    is_rest: bool,
}

#[derive(Clone)]
struct Sig {
    params: Vec<ParamSig>,
    ret: Option<Ty>,
}

#[derive(Clone, Copy, PartialEq)]
enum FuncState {
    Unchecked,
    InProgress,
    Done,
}

struct FuncEntry {
    decl: FuncDecl,
    sig: Sig,
    state: FuncState,
}

#[derive(Clone, Copy, PartialEq)]
enum VarKind {
    Local,
    Param,
}

struct Var {
    ty: Ty,
    mutable: bool,
    kind: VarKind,
    span: Span,
    reassigned: bool,
    list_len: Option<usize>,
}

struct FnCtx {
    declared: Option<Ty>,
    uses_try: bool,
    returns: Vec<(NodeId, Span, Ty)>,
}

#[derive(PartialEq)]
enum LoopKind {
    Loop,
    Valued,
}

struct LoopCtx {
    kind: LoopKind,
    break_ty: Option<Ty>,
}

pub struct Checker {
    subst: Subst,
    pub diags: Vec<Diagnostic>,
    pub info: Info,
    scopes: Vec<HashMap<String, Var>>,
    funcs: HashMap<String, FuncEntry>,
    fns: Vec<FnCtx>,
    loops: Vec<LoopCtx>,
    narrowed: Vec<(String, String)>,
    top_level_names: HashSet<String>,
    lambda_types: Vec<(NodeId, Ty)>,
}

const RESERVED_NAMES: &[&str] = &["Maybe", "Some", "None", "Result", "Ok", "Err"];
const MUTATING: &[&str] = &["push", "pop", "removeAt"];

pub fn check(program: &[Stmt]) -> (Info, Vec<Diagnostic>) {
    let mut checker = Checker {
        subst: Subst::default(),
        diags: Vec::new(),
        info: Info::default(),
        scopes: vec![HashMap::new()],
        funcs: HashMap::new(),
        fns: Vec::new(),
        loops: Vec::new(),
        narrowed: Vec::new(),
        top_level_names: HashSet::new(),
        lambda_types: Vec::new(),
    };
    checker.program(program);
    (checker.info, checker.diags)
}

impl Checker {
    fn error(&mut self, code: Code, span: Span) -> Ty {
        self.diags.push(Diagnostic::error(code, span));
        Ty::Error
    }

    fn warn(&mut self, code: Code, span: Span) {
        self.diags.push(Diagnostic::warning(code, span));
    }

    fn resolve(&self, ty: &Ty) -> Ty {
        self.subst.resolve(ty)
    }

    fn program(&mut self, program: &[Stmt]) {
        for stmt in program {
            match stmt {
                Stmt::Func(decl) => self.declare_func(decl),
                Stmt::Let { name, .. } => {
                    self.top_level_names.insert(name.clone());
                }
                _ => {}
            }
        }
        for stmt in program {
            self.stmt(stmt);
        }
        let names: Vec<String> = self.funcs.keys().cloned().collect();
        for name in names {
            self.ensure_checked(&name, Span::default());
        }
        self.finish_scope();
        self.record_signatures();
    }

    fn record_signatures(&mut self) {
        for entry in self.funcs.values() {
            let params: Vec<String> = entry
                .sig
                .params
                .iter()
                .map(|p| {
                    format!(
                        "{}{}: {}",
                        if p.is_rest { "..." } else { "" },
                        p.name,
                        Show(&self.subst.resolve(&p.ty))
                    )
                })
                .collect();
            let ret = entry
                .sig
                .ret
                .as_ref()
                .map(|r| self.subst.resolve(r))
                .unwrap_or(Ty::Void);
            let arrow = if ret == Ty::Void {
                String::new()
            } else {
                format!(" -> {}", Show(&ret))
            };
            let text = format!("func {}({}){}", entry.decl.name, params.join(", "), arrow);
            self.info.signatures.insert(entry.decl.id, text);
        }
        for (id, ty) in &self.lambda_types {
            self.info
                .signatures
                .insert(*id, Show(&self.subst.resolve(ty)).to_string());
        }
    }

    fn declare_func(&mut self, decl: &FuncDecl) {
        if self.funcs.contains_key(&decl.name) {
            self.error(Code::E0102, decl.span);
            return;
        }
        if RESERVED_NAMES.contains(&decl.name.as_str()) {
            self.error(Code::E0103, decl.span);
        }
        let mut params = Vec::new();
        for p in &decl.params {
            let ty = match (&p.ty, &p.default) {
                (Some(t), _) => self.type_from(t),
                (None, Some(default)) => {
                    let t = self.expr(default, None);
                    self.resolve(&t)
                }
                (None, None) => self.error(Code::E0202, p.span),
            };
            if let (Some(_), Some(default)) = (&p.ty, &p.default) {
                let actual = self.expr(default, Some(&ty));
                self.subsume(default.id, &actual, &ty);
            }
            let ty = match (p.is_rest, ty) {
                (true, Ty::List(elem)) => *elem,
                (true, Ty::Error) => Ty::Error,
                (true, _) => self.error(Code::E0201, p.span),
                (false, ty) => ty,
            };
            params.push(ParamSig {
                name: p.name.clone(),
                ty,
                has_default: p.default.is_some(),
                is_rest: p.is_rest,
            });
        }
        let ret = decl.ret.as_ref().map(|t| self.type_from(t));
        let entry = FuncEntry {
            decl: decl.clone(),
            sig: Sig { params, ret },
            state: FuncState::Unchecked,
        };
        self.funcs.insert(decl.name.clone(), entry);
    }

    fn type_from(&mut self, t: &TypeExpr) -> Ty {
        match t {
            TypeExpr::Name(name, span) => match name.as_str() {
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "Bool" => Ty::Bool,
                "String" => Ty::Str,
                "Void" => Ty::Void,
                "Never" => Ty::Never,
                _ => self.error(Code::E0101, *span),
            },
            TypeExpr::Generic(name, args, span) => match (name.as_str(), args.as_slice()) {
                ("List", [elem]) => {
                    let elem = self.type_from(elem);
                    Ty::list(elem)
                }
                ("Maybe", [inner]) => {
                    let inner = self.type_from(inner);
                    Ty::maybe(inner)
                }
                _ => self.error(Code::E0101, *span),
            },
            TypeExpr::Maybe(inner) => {
                let inner = self.type_from(inner);
                Ty::maybe(inner)
            }
            TypeExpr::Func(params, ret) => {
                let params = params.iter().map(|p| self.type_from(p)).collect();
                let ret = ret.as_ref().map_or(Ty::Void, |r| self.type_from(r));
                Ty::Func(params, Box::new(ret))
            }
        }
    }

    fn ensure_checked(&mut self, name: &str, call_span: Span) -> Option<Sig> {
        let entry = self.funcs.get_mut(name)?;
        match entry.state {
            FuncState::Done => return Some(entry.sig.clone()),
            FuncState::InProgress => {
                if entry.sig.ret.is_none() {
                    self.error(Code::E0212, call_span);
                    return None;
                }
                return Some(entry.sig.clone());
            }
            FuncState::Unchecked => entry.state = FuncState::InProgress,
        }
        let decl = entry.decl.clone();
        let sig = entry.sig.clone();
        let saved_scopes = self.scopes.split_off(1);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_narrowed = std::mem::take(&mut self.narrowed);
        let mut scope = HashMap::new();
        for (p, ps) in decl.params.iter().zip(&sig.params) {
            let ty = if ps.is_rest {
                Ty::list(ps.ty.clone())
            } else {
                ps.ty.clone()
            };
            scope.insert(
                p.name.clone(),
                Var {
                    ty,
                    mutable: p.is_var,
                    kind: VarKind::Param,
                    span: p.span,
                    reassigned: false,
                    list_len: None,
                },
            );
        }
        self.scopes.push(scope);
        let ret = self.function_body(&decl.body, sig.ret.clone());
        self.finish_scope();
        self.scopes.truncate(1);
        self.scopes.extend(saved_scopes);
        self.loops = saved_loops;
        self.narrowed = saved_narrowed;
        let entry = self.funcs.get_mut(name)?;
        entry.sig.ret = Some(ret);
        entry.state = FuncState::Done;
        Some(entry.sig.clone())
    }

    fn function_body(&mut self, body: &Expr, declared: Option<Ty>) -> Ty {
        self.fns.push(FnCtx {
            declared: declared.clone(),
            uses_try: false,
            returns: Vec::new(),
        });
        let body_ty = self.expr(body, declared.as_ref());
        let ctx = self.fns.pop().expect("function context");
        let ret = match declared {
            Some(t) => t,
            None => {
                let base = match self.subst.shallow(&body_ty) {
                    Ty::Never => ctx
                        .returns
                        .first()
                        .map_or(Ty::Never, |r| self.subst.shallow(&r.2)),
                    other => other,
                };
                if ctx.uses_try && !matches!(self.subst.shallow(&base), Ty::Maybe(_)) {
                    Ty::maybe(base)
                } else {
                    base
                }
            }
        };
        if !self.subsume(body.id, &body_ty, &ret) {
            self.error(Code::E0211, body.span);
        }
        for (id, span, ty) in ctx.returns {
            if !self.subsume(id, &ty, &ret) {
                self.error(Code::E0211, span);
            }
        }
        ret
    }

    fn add_coercion(&mut self, id: NodeId, to_float: bool, wraps: usize) {
        let c = self.info.coercions.entry(id).or_default();
        c.to_float |= to_float;
        c.wraps += wraps;
    }

    fn subsume(&mut self, id: NodeId, actual: &Ty, expected: &Ty) -> bool {
        let a = self.subst.shallow(actual);
        let e = self.subst.shallow(expected);
        match (&a, &e) {
            (Ty::Never, _) | (Ty::Error, _) | (_, Ty::Error) => true,
            (Ty::Var(_), _) | (_, Ty::Var(_)) => self.subst.unify(&a, &e),
            (Ty::Int, Ty::Float) => {
                self.add_coercion(id, true, 0);
                true
            }
            (Ty::Maybe(_), Ty::Maybe(_)) => self.subst.unify(&a, &e),
            (_, Ty::Maybe(inner)) => {
                let inner = (**inner).clone();
                if self.subsume(id, &a, &inner) {
                    self.add_coercion(id, false, 1);
                    true
                } else {
                    false
                }
            }
            _ => self.subst.unify(&a, &e),
        }
    }

    fn expect(&mut self, expr: &Expr, actual: &Ty, expected: &Ty) {
        if self.subsume(expr.id, actual, expected) {
            return;
        }
        let a = self.subst.shallow(actual);
        let code = if matches!(a, Ty::Maybe(_)) {
            Code::E0215
        } else {
            Code::E0201
        };
        self.error(code, expr.span);
    }

    fn join(&mut self, a: (&Expr, Ty), b: (&Expr, Ty)) -> Option<Ty> {
        let (ta, tb) = (self.subst.shallow(&a.1), self.subst.shallow(&b.1));
        if ta == Ty::Never {
            return Some(tb);
        }
        if tb == Ty::Never {
            return Some(ta);
        }
        if self.subst.unify(&ta, &tb) {
            return Some(ta);
        }
        if self.subsume(a.0.id, &ta, &tb) {
            return Some(tb);
        }
        if self.subsume(b.0.id, &tb, &ta) {
            return Some(ta);
        }
        None
    }

    fn finish_scope(&mut self) {
        let Some(scope) = self.scopes.pop() else {
            return;
        };
        let mut unused: Vec<Span> = scope
            .values()
            .filter(|v| v.mutable && !v.reassigned && v.kind == VarKind::Local)
            .map(|v| v.span)
            .collect();
        unused.sort_by_key(|s| s.start);
        for span in unused {
            self.warn(Code::W0001, span);
        }
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn lookup_mut(&mut self, name: &str) -> Option<&mut Var> {
        self.scopes.iter_mut().rev().find_map(|s| s.get_mut(name))
    }

    fn declare(&mut self, name: &str, span: Span, var: Var) {
        if RESERVED_NAMES.contains(&name) {
            self.error(Code::E0103, span);
        }
        let global = self.scopes.len() == 1;
        if self.scopes.last().is_some_and(|s| s.contains_key(name))
            || (global && self.funcs.contains_key(name))
        {
            self.error(Code::E0102, span);
        } else if self.lookup(name).is_some() || self.funcs.contains_key(name) {
            self.warn(Code::W0002, span);
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), var);
    }

    fn stmt(&mut self, stmt: &Stmt) -> Ty {
        match stmt {
            Stmt::Let {
                name,
                span,
                is_var,
                ty,
                value,
                ..
            } => {
                let declared = ty.as_ref().map(|t| self.type_from(t));
                let actual = self.expr(value, declared.as_ref());
                let ty = match declared {
                    Some(d) => {
                        self.expect(value, &actual, &d);
                        d
                    }
                    None => actual,
                };
                let list_len = match &value.kind {
                    ExprKind::List(items) if !is_var => Some(items.len()),
                    _ => None,
                };
                self.declare(
                    name,
                    *span,
                    Var {
                        ty,
                        mutable: *is_var,
                        kind: VarKind::Local,
                        span: *span,
                        reassigned: false,
                        list_len,
                    },
                );
                Ty::Void
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                self.assign(target, *op, value, *span);
                Ty::Void
            }
            Stmt::Func(_) => Ty::Void,
            Stmt::Expr(e) => self.expr(e, None),
        }
    }

    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, span: Span) {
        let ExprKind::Name(name) = &target.kind else {
            self.error(Code::E0007, target.span);
            return;
        };
        let Some(var) = self.lookup(name) else {
            let code = if self.top_level_names.contains(name) {
                Code::E0105
            } else {
                Code::E0101
            };
            self.error(code, target.span);
            return;
        };
        let (ty, mutable, kind) = (var.ty.clone(), var.mutable, var.kind);
        if !mutable {
            let code = if kind == VarKind::Param {
                Code::E0302
            } else {
                Code::E0301
            };
            self.error(code, target.span);
        }
        if let Some(v) = self.lookup_mut(name) {
            v.reassigned = true;
            v.list_len = None;
        }
        let actual = match op {
            None => self.expr(value, Some(&ty)),
            Some(op) => {
                let rhs = self.expr(value, None);
                let result = self.binary_type(op, target, &ty, value, &rhs, span);
                if op == BinOp::Div && self.subst.shallow(&ty) == Ty::Int {
                    self.error(Code::E0209, span);
                    return;
                }
                result
            }
        };
        self.expect(value, &actual, &ty);
    }

    fn block(&mut self, stmts: &[Stmt], expected: Option<&Ty>) -> Ty {
        self.scopes.push(HashMap::new());
        let mut last = Ty::Void;
        for (i, stmt) in stmts.iter().enumerate() {
            let is_last = i + 1 == stmts.len();
            last = match (stmt, is_last) {
                (Stmt::Expr(e), true) => self.expr(e, expected),
                _ => {
                    let t = self.stmt(stmt);
                    if is_last && !matches!(stmt, Stmt::Expr(_)) {
                        Ty::Void
                    } else {
                        t
                    }
                }
            };
            if self.subst.shallow(&last) == Ty::Never && !is_last {
                last = Ty::Void;
            }
        }
        self.finish_scope();
        last
    }

    pub fn expr(&mut self, expr: &Expr, expected: Option<&Ty>) -> Ty {
        match &expr.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(segs) => {
                for seg in segs {
                    if let StrSeg::Expr(e) = seg {
                        let t = self.expr(e, None);
                        if matches!(self.subst.shallow(&t), Ty::Maybe(_)) {
                            self.warn(Code::W0003, e.span);
                        }
                    }
                }
                Ty::Str
            }
            ExprKind::Name(name) => self.name(name, expr.span),
            ExprKind::List(items) => {
                let elem = match expected.map(|e| self.subst.shallow(e)) {
                    Some(Ty::List(e)) => *e,
                    _ => self.subst.fresh(),
                };
                for item in items {
                    let t = self.expr(item, Some(&elem));
                    self.expect(item, &t, &elem);
                }
                Ty::list(elem)
            }
            ExprKind::Unary(op, operand) => self.unary(*op, operand, expr.span),
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::Call(callee, args) => self.call(callee, args, expr.span),
            ExprKind::Method(recv, name, args) => self.method(recv, name, args, expr.span),
            ExprKind::Index(list, index) => self.index(expr.id, list, index, expr.span),
            ExprKind::Lambda(params, body) => self.lambda(expr.id, params, body, expected),
            ExprKind::Block(stmts) => self.block(stmts, expected),
            ExprKind::If(cond, then, otherwise) => {
                self.if_expr(expr, cond, then, otherwise.as_deref(), expected)
            }
            ExprKind::While(cond, body) => {
                let c = self.expr(cond, Some(&Ty::Bool));
                self.expect(cond, &c, &Ty::Bool);
                self.valued_loop(expr.id, body)
            }
            ExprKind::For(name, span, iter, body) => {
                self.for_loop(expr.id, name, *span, iter, body)
            }
            ExprKind::Loop(body) => {
                self.loops.push(LoopCtx {
                    kind: LoopKind::Loop,
                    break_ty: None,
                });
                self.expr(body, None);
                let ctx = self.loops.pop().expect("loop context");
                ctx.break_ty.unwrap_or(Ty::Never)
            }
            ExprKind::Break(value) => {
                let ty = match value {
                    Some(v) => self.expr(v, None),
                    None => Ty::Void,
                };
                let Some(ctx) = self.loops.last() else {
                    return self.error(Code::E0304, expr.span);
                };
                match ctx.break_ty.clone() {
                    None => self.loops.last_mut().expect("loop").break_ty = Some(ty),
                    Some(prev) => {
                        let ok = match value {
                            Some(v) => self.subsume(v.id, &ty, &prev),
                            None => self.subst.unify(&prev, &Ty::Void),
                        };
                        if !ok {
                            self.error(Code::E0307, expr.span);
                        }
                    }
                }
                Ty::Never
            }
            ExprKind::Continue => {
                if self.loops.is_empty() {
                    return self.error(Code::E0304, expr.span);
                }
                Ty::Never
            }
            ExprKind::Return(value) => {
                let declared = self.fns.last().map(|f| f.declared.clone());
                let Some(declared) = declared else {
                    return self.error(Code::E0305, expr.span);
                };
                let (id, span, ty) = match value {
                    Some(v) => (v.id, v.span, self.expr(v, declared.as_ref())),
                    None => (expr.id, expr.span, Ty::Void),
                };
                self.fns
                    .last_mut()
                    .expect("fn")
                    .returns
                    .push((id, span, ty));
                Ty::Never
            }
        }
    }

    fn name(&mut self, name: &str, span: Span) -> Ty {
        if let Some(var) = self.lookup(name) {
            return var.ty.clone();
        }
        if self.funcs.contains_key(name) {
            let Some(sig) = self.ensure_checked(name, span) else {
                return Ty::Error;
            };
            let params = sig.params.iter().map(|p| p.ty.clone()).collect();
            return Ty::Func(params, Box::new(sig.ret.unwrap_or(Ty::Void)));
        }
        if name == "None" {
            return Ty::maybe(self.subst.fresh());
        }
        let code = if self.top_level_names.contains(name) && !self.fns.is_empty() {
            Code::E0105
        } else if self.top_level_names.contains(name) {
            Code::E0104
        } else {
            Code::E0101
        };
        self.error(code, span)
    }

    fn unary(&mut self, op: UnOp, operand: &Expr, span: Span) -> Ty {
        let t = self.expr(operand, None);
        let t = self.subst.shallow(&t);
        match (op, &t) {
            (_, Ty::Error) => Ty::Error,
            (UnOp::Neg, Ty::Int | Ty::Float) => t,
            (UnOp::BitNot, Ty::Int) => Ty::Int,
            (UnOp::Not, Ty::Bool) => Ty::Bool,
            (UnOp::Try, Ty::Maybe(inner)) => {
                let declared = self
                    .fns
                    .last()
                    .map(|f| f.declared.as_ref().map(|d| self.subst.shallow(d)));
                match declared {
                    None
                    | Some(Some(
                        Ty::Int
                        | Ty::Float
                        | Ty::Bool
                        | Ty::Str
                        | Ty::Void
                        | Ty::List(_)
                        | Ty::Func(..)
                        | Ty::Range,
                    )) => self.error(Code::E0219, span),
                    _ => {
                        self.fns.last_mut().expect("fn").uses_try = true;
                        (**inner).clone()
                    }
                }
            }
            (_, Ty::Var(_)) => self.error(Code::E0202, operand.span),
            _ => self.error(Code::E0206, span),
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        match op {
            BinOp::And | BinOp::Or => {
                for side in [lhs, rhs] {
                    let t = self.expr(side, Some(&Ty::Bool));
                    if !self.subst.unify(&t, &Ty::Bool) {
                        self.error(Code::E0206, side.span);
                    }
                }
                Ty::Bool
            }
            BinOp::Coalesce => {
                let lt = self.expr(lhs, None);
                match self.subst.shallow(&lt) {
                    Ty::Maybe(inner) => {
                        let rt = self.expr(rhs, Some(&inner));
                        self.expect(rhs, &rt, &inner);
                        *inner
                    }
                    Ty::Var(_) => self.error(Code::E0202, lhs.span),
                    Ty::Error => Ty::Error,
                    _ => self.error(Code::E0201, lhs.span),
                }
            }
            _ => {
                let lt = self.expr(lhs, None);
                let rt = self.expr(rhs, None);
                if op == BinOp::Pow
                    && self.subst.shallow(&lt) == Ty::Int
                    && let ExprKind::Unary(UnOp::Neg, inner) = &rhs.kind
                    && matches!(inner.kind, ExprKind::Int(_))
                {
                    return self.error(Code::E0214, rhs.span);
                }
                self.binary_type(op, lhs, &lt, rhs, &rt, span)
            }
        }
    }

    fn binary_type(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        lt: &Ty,
        rhs: &Expr,
        rt: &Ty,
        span: Span,
    ) -> Ty {
        let (mut a, mut b) = (self.subst.shallow(lt), self.subst.shallow(rt));
        if matches!(a, Ty::Var(_)) && !matches!(b, Ty::Var(_)) {
            self.subst.unify(&a, &b);
            a = b.clone();
        } else if matches!(b, Ty::Var(_)) && !matches!(a, Ty::Var(_)) {
            self.subst.unify(&b, &a);
            b = a.clone();
        }
        if a == Ty::Error || b == Ty::Error {
            return Ty::Error;
        }
        if matches!(a, Ty::Var(_)) {
            return self.error(Code::E0202, lhs.span);
        }
        let numeric = a.is_numeric() && b.is_numeric();
        let widen = |c: &mut Self| {
            if a == Ty::Int && b == Ty::Float {
                c.add_coercion(lhs.id, true, 0);
            }
            if b == Ty::Int && a == Ty::Float {
                c.add_coercion(rhs.id, true, 0);
            }
            if a == Ty::Float || b == Ty::Float {
                Ty::Float
            } else {
                Ty::Int
            }
        };
        match op {
            BinOp::Add if a == Ty::Str && b == Ty::Str => Ty::Str,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Pow if numeric => widen(self),
            BinOp::Div if numeric => {
                widen(self);
                Ty::Float
            }
            BinOp::IntDiv | BinOp::Mod if a == Ty::Int && b == Ty::Int => Ty::Int,
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if a == Ty::Int && b == Ty::Int {
                    Ty::Int
                } else if a == Ty::Bool || b == Ty::Bool {
                    self.error(Code::E0207, span)
                } else {
                    self.error(Code::E0206, span)
                }
            }
            BinOp::Eq | BinOp::Ne => {
                if numeric {
                    widen(self);
                } else if !self.subst.unify(&a, &b) {
                    return self.error(Code::E0206, span);
                }
                Ty::Bool
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                if numeric {
                    widen(self);
                    Ty::Bool
                } else if a == Ty::Str && b == Ty::Str {
                    Ty::Bool
                } else {
                    self.error(Code::E0206, span)
                }
            }
            BinOp::Range | BinOp::RangeIncl if a == Ty::Int && b == Ty::Int => {
                if let (ExprKind::Int(x), ExprKind::Int(y)) = (&lhs.kind, &rhs.kind)
                    && x > y
                {
                    self.error(Code::E0308, span);
                }
                Ty::Range
            }
            _ => self.error(Code::E0206, span),
        }
    }

    fn args_of(&mut self, args: &[Arg], span: Span, params: &[Ty]) -> bool {
        if args.len() != params.len() || args.iter().any(|a| a.name.is_some()) {
            for a in args {
                self.expr(&a.value, None);
            }
            self.error(Code::E0203, span);
            return false;
        }
        for (arg, param) in args.iter().zip(params) {
            let t = self.expr(&arg.value, Some(param));
            self.expect(&arg.value, &t, param);
        }
        true
    }

    fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Ty {
        if let ExprKind::Name(name) = &callee.kind
            && self.lookup(name).is_none()
        {
            if self.funcs.contains_key(name) {
                return self.call_named(name, args, span);
            }
            match name.as_str() {
                "print" => {
                    if args.len() != 1 {
                        return self.error(Code::E0203, span);
                    }
                    self.expr(&args[0].value, None);
                    return Ty::Void;
                }
                "panic" => {
                    if !args.is_empty() {
                        self.args_of(args, span, &[Ty::Str]);
                    }
                    return Ty::Never;
                }
                "input" => {
                    if !args.is_empty() {
                        self.args_of(args, span, &[Ty::Str]);
                    }
                    return Ty::maybe(Ty::Str);
                }
                "Some" => {
                    if args.len() != 1 {
                        return self.error(Code::E0203, span);
                    }
                    let t = self.expr(&args[0].value, None);
                    return Ty::maybe(t);
                }
                _ => {}
            }
        }
        let callee_ty = self.expr(callee, None);
        match self.subst.shallow(&callee_ty) {
            Ty::Func(params, ret) => {
                self.args_of(args, span, &params);
                *ret
            }
            Ty::Var(_) => {
                let params: Vec<Ty> = args.iter().map(|_| self.subst.fresh()).collect();
                let ret = self.subst.fresh();
                self.subst
                    .unify(&callee_ty, &Ty::Func(params.clone(), Box::new(ret.clone())));
                self.args_of(args, span, &params);
                ret
            }
            Ty::Error => Ty::Error,
            _ => self.error(Code::E0205, callee.span),
        }
    }

    fn call_named(&mut self, name: &str, args: &[Arg], span: Span) -> Ty {
        let sig = self.funcs[name].sig.clone();
        let mut filled = vec![false; sig.params.len()];
        let positional: Vec<&Arg> = args.iter().filter(|a| a.name.is_none()).collect();
        let fixed: Vec<usize> = sig
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_rest)
            .map(|(i, _)| i)
            .collect();
        let rest = sig.params.iter().position(|p| p.is_rest);
        let mut ok = true;
        for (k, arg) in positional.iter().enumerate() {
            if let Some(&i) = fixed.get(k) {
                filled[i] = true;
                let t = self.expr(&arg.value, Some(&sig.params[i].ty));
                self.expect(&arg.value, &t, &sig.params[i].ty);
            } else if let Some(r) = rest {
                filled[r] = true;
                let t = self.expr(&arg.value, Some(&sig.params[r].ty));
                self.expect(&arg.value, &t, &sig.params[r].ty);
            } else {
                self.expr(&arg.value, None);
                ok = false;
            }
        }
        for arg in args.iter().filter(|a| a.name.is_some()) {
            let arg_name = arg.name.as_deref().unwrap_or_default();
            match sig
                .params
                .iter()
                .position(|p| p.name == arg_name && !p.is_rest)
            {
                Some(i) if !filled[i] => {
                    filled[i] = true;
                    let t = self.expr(&arg.value, Some(&sig.params[i].ty));
                    self.expect(&arg.value, &t, &sig.params[i].ty);
                }
                _ => {
                    self.expr(&arg.value, None);
                    ok = false;
                }
            }
        }
        for (i, p) in sig.params.iter().enumerate() {
            if !filled[i] && !p.has_default && !p.is_rest {
                ok = false;
            }
        }
        if !ok {
            self.error(Code::E0203, span);
        }
        match self.ensure_checked(name, span) {
            Some(sig) => sig.ret.unwrap_or(Ty::Void),
            None => Ty::Error,
        }
    }

    fn method(&mut self, recv: &Expr, name: &str, args: &[Arg], span: Span) -> Ty {
        if let ExprKind::Name(type_name) = &recv.kind
            && self.lookup(type_name).is_none()
            && (type_name == "Int" || type_name == "Float")
        {
            if name != "parse" {
                return self.error(Code::E0204, span);
            }
            self.args_of(args, span, &[Ty::Str]);
            return Ty::maybe(if type_name == "Int" {
                Ty::Int
            } else {
                Ty::Float
            });
        }
        let recv_ty = self.expr(recv, None);
        let recv_ty = self.subst.shallow(&recv_ty);
        if MUTATING.contains(&name)
            && let ExprKind::Name(var_name) = &recv.kind
            && let Some(var) = self.lookup_mut(var_name)
        {
            if var.mutable {
                var.reassigned = true;
                var.list_len = None;
            } else if matches!(recv_ty, Ty::List(_)) {
                self.error(Code::E0303, recv.span);
            }
        }
        match recv_ty {
            Ty::Error => {
                for a in args {
                    self.expr(&a.value, None);
                }
                Ty::Error
            }
            Ty::Var(_) => self.error(Code::E0202, recv.span),
            Ty::Str => self.string_method(name, args, span),
            Ty::List(elem) => self.list_method(*elem, name, args, span),
            Ty::Int if name == "toFloat" => {
                self.args_of(args, span, &[]);
                Ty::Float
            }
            Ty::Float if matches!(name, "round" | "floor" | "ceil" | "truncate") => {
                self.args_of(args, span, &[]);
                Ty::Int
            }
            Ty::Maybe(inner) => match name {
                "orPanic" => {
                    if !args.is_empty() {
                        self.args_of(args, span, &[Ty::Str]);
                    }
                    *inner
                }
                "flatten" => {
                    self.args_of(args, span, &[]);
                    let mut t = self.subst.shallow(&inner);
                    if !matches!(t, Ty::Maybe(_)) {
                        return self.error(Code::E0217, span);
                    }
                    while let Ty::Maybe(i) = t {
                        t = self.subst.shallow(&i);
                    }
                    Ty::maybe(t)
                }
                _ => self.error(Code::E0204, span),
            },
            _ => self.error(Code::E0204, span),
        }
    }

    fn string_method(&mut self, name: &str, args: &[Arg], span: Span) -> Ty {
        match name {
            "upper" | "lower" | "trim" => {
                self.args_of(args, span, &[]);
                Ty::Str
            }
            "isEmpty" | "isBlank" => {
                self.args_of(args, span, &[]);
                Ty::Bool
            }
            "contains" => {
                self.args_of(args, span, &[Ty::Str]);
                Ty::Bool
            }
            "split" => {
                if !args.is_empty() {
                    self.args_of(args, span, &[Ty::Str]);
                }
                Ty::list(Ty::Str)
            }
            _ => self.error(Code::E0204, span),
        }
    }

    fn list_method(&mut self, elem: Ty, name: &str, args: &[Arg], span: Span) -> Ty {
        match name {
            "push" => {
                self.args_of(args, span, &[elem]);
                Ty::Void
            }
            "pop" | "first" | "last" => {
                self.args_of(args, span, &[]);
                Ty::maybe(elem)
            }
            "removeAt" => {
                self.args_of(args, span, &[Ty::Int]);
                Ty::maybe(elem)
            }
            "length" => {
                self.args_of(args, span, &[]);
                Ty::Int
            }
            "isEmpty" => {
                self.args_of(args, span, &[]);
                Ty::Bool
            }
            "contains" => {
                self.args_of(args, span, &[elem]);
                Ty::Bool
            }
            "indices" => {
                self.args_of(args, span, &[]);
                Ty::Range
            }
            "join" => {
                if !self.subst.unify(&elem, &Ty::Str) {
                    return self.error(Code::E0204, span);
                }
                self.args_of(args, span, &[Ty::Str]);
                Ty::Str
            }
            "map" => {
                let out = self.subst.fresh();
                self.args_of(args, span, &[Ty::Func(vec![elem], Box::new(out.clone()))]);
                Ty::list(out)
            }
            "filter" => {
                self.args_of(
                    args,
                    span,
                    &[Ty::Func(vec![elem.clone()], Box::new(Ty::Bool))],
                );
                Ty::list(elem)
            }
            _ => self.error(Code::E0204, span),
        }
    }

    fn index(&mut self, id: NodeId, list: &Expr, index: &Expr, span: Span) -> Ty {
        let lt = self.expr(list, None);
        let it = self.expr(index, Some(&Ty::Int));
        self.expect(index, &it, &Ty::Int);
        let elem = match self.subst.shallow(&lt) {
            Ty::List(e) => *e,
            Ty::Error => return Ty::Error,
            Ty::Var(_) => return self.error(Code::E0202, list.span),
            _ => return self.error(Code::E0206, span),
        };
        if let ExprKind::Name(list_name) = &list.kind {
            if let ExprKind::Int(k) = index.kind
                && self
                    .lookup(list_name)
                    .and_then(|v| v.list_len)
                    .is_some_and(|n| k >= 0 && (k as usize) < n)
            {
                self.info.unwrapped_index.insert(id);
                return elem;
            }
            if let ExprKind::Name(index_name) = &index.kind
                && self
                    .narrowed
                    .iter()
                    .any(|(l, i)| l == list_name && i == index_name)
            {
                self.info.unwrapped_index.insert(id);
                return elem;
            }
        }
        Ty::maybe(elem)
    }

    fn lambda(&mut self, id: NodeId, params: &[Param], body: &Expr, expected: Option<&Ty>) -> Ty {
        let expected_params = match expected.map(|e| self.subst.shallow(e)) {
            Some(Ty::Func(ps, _)) if ps.len() == params.len() => Some(ps),
            _ => None,
        };
        let mut scope = HashMap::new();
        let mut param_tys = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let ty = match (&p.ty, &expected_params) {
                (Some(t), _) => self.type_from(t),
                (None, Some(ps)) => ps[i].clone(),
                (None, None) => self.subst.fresh(),
            };
            param_tys.push(ty.clone());
            scope.insert(
                p.name.clone(),
                Var {
                    ty,
                    mutable: p.is_var,
                    kind: VarKind::Param,
                    span: p.span,
                    reassigned: false,
                    list_len: None,
                },
            );
        }
        let saved_loops = std::mem::take(&mut self.loops);
        self.scopes.push(scope);
        let ret = self.function_body(body, None);
        self.finish_scope();
        self.loops = saved_loops;
        let ty = Ty::Func(param_tys, Box::new(ret));
        self.lambda_types.push((id, ty.clone()));
        ty
    }

    fn if_expr(
        &mut self,
        expr: &Expr,
        cond: &Expr,
        then: &Expr,
        otherwise: Option<&Expr>,
        expected: Option<&Ty>,
    ) -> Ty {
        let c = self.expr(cond, Some(&Ty::Bool));
        self.expect(cond, &c, &Ty::Bool);
        let tt = self.expr(then, expected);
        match otherwise {
            Some(other) => {
                let ot = self.expr(other, expected);
                match self.join((then, tt), (other, ot)) {
                    Some(t) => t,
                    None => self.error(Code::E0210, expr.span),
                }
            }
            None => self.optional_value(expr, tt),
        }
    }

    fn optional_value(&mut self, expr: &Expr, body: Ty) -> Ty {
        match self.subst.shallow(&body) {
            Ty::Void | Ty::Never | Ty::Error => Ty::Void,
            t => {
                if matches!(t, Ty::Maybe(_)) {
                    self.warn(Code::W0004, expr.span);
                }
                self.info.wrapping.insert(expr.id);
                Ty::maybe(t)
            }
        }
    }

    fn valued_loop(&mut self, id: NodeId, body: &Expr) -> Ty {
        self.loops.push(LoopCtx {
            kind: LoopKind::Valued,
            break_ty: None,
        });
        self.expr(body, None);
        let ctx = self.loops.pop().expect("loop context");
        debug_assert!(ctx.kind == LoopKind::Valued);
        match ctx.break_ty.map(|t| self.subst.shallow(&t)) {
            None | Some(Ty::Void) | Some(Ty::Error) => Ty::Void,
            Some(t) => {
                self.info.wrapping.insert(id);
                Ty::maybe(t)
            }
        }
    }

    fn for_loop(&mut self, id: NodeId, name: &str, span: Span, iter: &Expr, body: &Expr) -> Ty {
        let it = self.expr(iter, None);
        let elem = match self.subst.shallow(&it) {
            Ty::Range => Ty::Int,
            Ty::List(e) => *e,
            Ty::Error => Ty::Error,
            _ => self.error(Code::E0218, iter.span),
        };
        let narrowed_list = narrowing_list(iter).filter(|list| !mutates(body, list));
        if let Some(list) = &narrowed_list {
            self.narrowed.push((list.clone(), name.to_string()));
        }
        let mut scope = HashMap::new();
        scope.insert(
            name.to_string(),
            Var {
                ty: elem,
                mutable: false,
                kind: VarKind::Local,
                span,
                reassigned: false,
                list_len: None,
            },
        );
        self.scopes.push(scope);
        let ty = self.valued_loop(id, body);
        self.scopes.pop();
        if narrowed_list.is_some() {
            self.narrowed.pop();
        }
        ty
    }
}

fn narrowing_list(iter: &Expr) -> Option<String> {
    match &iter.kind {
        ExprKind::Method(recv, m, args) if m == "indices" && args.is_empty() => match &recv.kind {
            ExprKind::Name(n) => Some(n.clone()),
            _ => None,
        },
        ExprKind::Binary(BinOp::Range, lo, hi) if matches!(lo.kind, ExprKind::Int(0)) => match &hi
            .kind
        {
            ExprKind::Method(recv, m, args) if m == "length" && args.is_empty() => match &recv.kind
            {
                ExprKind::Name(n) => Some(n.clone()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn mutates(expr: &Expr, list: &str) -> bool {
    let is_list = |e: &Expr| matches!(&e.kind, ExprKind::Name(n) if n == list);
    match &expr.kind {
        ExprKind::Method(recv, m, args) => {
            (is_list(recv) && MUTATING.contains(&m.as_str()))
                || mutates(recv, list)
                || args.iter().any(|a| mutates(&a.value, list))
        }
        ExprKind::Block(stmts) => stmts.iter().any(|s| match s {
            Stmt::Assign { target, value, .. } => is_list(target) || mutates(value, list),
            Stmt::Let { value, .. } => mutates(value, list),
            Stmt::Expr(e) => mutates(e, list),
            Stmt::Func(_) => false,
        }),
        ExprKind::Str(segs) => segs
            .iter()
            .any(|s| matches!(s, StrSeg::Expr(e) if mutates(e, list))),
        ExprKind::List(items) => items.iter().any(|e| mutates(e, list)),
        ExprKind::Unary(_, e) | ExprKind::Loop(e) | ExprKind::Lambda(_, e) => mutates(e, list),
        ExprKind::Binary(_, a, b)
        | ExprKind::Index(a, b)
        | ExprKind::While(a, b)
        | ExprKind::For(_, _, a, b) => mutates(a, list) || mutates(b, list),
        ExprKind::Call(f, args) => mutates(f, list) || args.iter().any(|a| mutates(&a.value, list)),
        ExprKind::If(c, t, o) => {
            mutates(c, list) || mutates(t, list) || o.as_ref().is_some_and(|o| mutates(o, list))
        }
        ExprKind::Break(v) | ExprKind::Return(v) => v.as_ref().is_some_and(|v| mutates(v, list)),
        _ => false,
    }
}
