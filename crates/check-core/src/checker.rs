use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Code, Diagnostic, Span};
use crate::types::{Show, Subst, Ty};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Coercion {
    pub to_float: bool,
    pub wraps: usize,
}

#[derive(Debug, Clone)]
pub enum Resolved {
    Construct(String),
    Variant(String, String),
    Const(String),
    Static(String),
    Method(String),
}

#[derive(Default)]
pub struct Info {
    pub coercions: HashMap<NodeId, Coercion>,
    pub wrapping: HashSet<NodeId>,
    pub signatures: HashMap<NodeId, String>,
    pub unwrapped_index: HashSet<NodeId>,
    pub resolved: HashMap<NodeId, Resolved>,
    pub variant_patterns: HashSet<NodeId>,
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
    this: Option<(Ty, bool)>,
    owner: Option<String>,
    library: bool,
}

struct ClassDef {
    is_ref: bool,
    fields: Vec<ParamSig>,
    mutable: Vec<bool>,
    private: Vec<bool>,
    readonly: Vec<bool>,
}

struct FieldInfo {
    ty: Ty,
    mutable: bool,
    is_ref: bool,
    private: bool,
    readonly: bool,
}

struct VariantDef {
    name: String,
    fields: Vec<ParamSig>,
}

enum TypeDef {
    Class(ClassDef),
    Enum(Vec<VariantDef>),
}

struct ConstEntry {
    decl: ConstDecl,
    owner: String,
    ty: Ty,
    state: FuncState,
}

type Binding = (String, Span, Ty);

enum ArgSource<'a> {
    Expr(&'a Expr),
    Spread(Ty, Span),
}

#[derive(Clone, Copy)]
enum Cell<'p> {
    Pat(&'p Pattern),
    Items(&'p [Pattern]),
}

static WILDCARD: Pattern = Pattern {
    id: 0,
    span: Span { start: 0, end: 0 },
    kind: PatternKind::Wildcard,
};

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

impl Var {
    fn binding(ty: Ty, span: Span) -> Var {
        Var {
            ty,
            mutable: false,
            kind: VarKind::Local,
            span,
            reassigned: false,
            list_len: None,
        }
    }
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
    type_names: HashSet<String>,
    types: HashMap<String, TypeDef>,
    consts: HashMap<String, ConstEntry>,
    current_type: Option<String>,
    is_bindings: HashMap<NodeId, Vec<Binding>>,
    defaults: Vec<(Expr, Ty, Option<String>)>,
    in_library: bool,
}

const RESERVED_NAMES: &[&str] = &["Maybe", "Some", "None", "Result", "Ok", "Err"];
const MUTATING: &[&str] = &["push", "pop", "removeAt"];
const BUILT_IN_TYPES: &[&str] = &["String", "Int", "Float", "Bool"];

pub fn check(program: &[Stmt], library: &[Stmt]) -> (Info, Vec<Diagnostic>) {
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
        type_names: HashSet::new(),
        types: HashMap::new(),
        consts: HashMap::new(),
        current_type: None,
        is_bindings: HashMap::new(),
        defaults: Vec::new(),
        in_library: false,
    };
    checker.program(program, library);
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

    fn program(&mut self, program: &[Stmt], library: &[Stmt]) {
        for stmt in program {
            let (name, span) = match stmt {
                Stmt::Class(c) => (&c.name, c.span),
                Stmt::Enum(e) => (&e.name, e.span),
                _ => continue,
            };
            if RESERVED_NAMES.contains(&name.as_str()) || BUILT_IN_TYPES.contains(&name.as_str()) {
                self.error(Code::redefined_builtin, span);
            } else if !self.type_names.insert(name.clone()) {
                self.error(Code::duplicate_name, span);
            }
        }
        for stmt in program {
            match stmt {
                Stmt::Class(c) => self.declare_class(c),
                Stmt::Enum(e) => self.declare_enum(e),
                _ => {}
            }
        }
        for stmt in program {
            match stmt {
                Stmt::Func(decl) => {
                    if self.type_names.contains(&decl.name) {
                        self.error(Code::duplicate_name, decl.span);
                    }
                    self.declare_func(decl, None, None);
                }
                Stmt::Let { name, .. } => {
                    self.top_level_names.insert(name.clone());
                }
                Stmt::Implement(i) => self.declare_implement(i),
                _ => {}
            }
        }
        self.in_library = true;
        for stmt in library {
            match stmt {
                Stmt::Func(decl) => self.declare_func(decl, None, None),
                Stmt::Implement(i) => self.declare_implement(i),
                _ => {}
            }
        }
        self.in_library = false;
        for (default, ty, owner) in std::mem::take(&mut self.defaults) {
            self.current_type = owner;
            let actual = self.expr(&default, Some(&ty));
            self.subsume(default.id, &actual, &ty);
        }
        self.current_type = None;
        for stmt in program {
            self.stmt(stmt);
        }
        let names: Vec<String> = self.funcs.keys().cloned().collect();
        for name in names {
            self.ensure_checked(&name, Span::default());
        }
        let consts: Vec<String> = self.consts.keys().cloned().collect();
        for key in consts {
            self.const_type(&key);
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

    fn declare_class(&mut self, c: &ClassDecl) {
        self.current_type = Some(c.name.clone());
        let fields = self.param_sigs(&c.fields);
        self.current_type = None;
        self.check_unique(c.fields.iter().map(|f| (&f.name, f.span)));
        let mutable = c.fields.iter().map(|f| f.is_var).collect();
        for f in &c.fields {
            if f.is_private && f.default.is_none() {
                self.error(Code::private_field_without_default, f.span);
            }
        }
        self.types.insert(
            c.name.clone(),
            TypeDef::Class(ClassDef {
                is_ref: c.is_ref,
                fields,
                mutable,
                private: c.fields.iter().map(|f| f.is_private).collect(),
                readonly: c.fields.iter().map(|f| f.is_readonly).collect(),
            }),
        );
        if let Some(init) = &c.init {
            self.declare_func(init, Some((Ty::Named(c.name.clone()), true)), Some(&c.name));
        }
        self.declare_members(&c.name, Ty::Named(c.name.clone()), &c.members);
    }

    fn declare_implement(&mut self, i: &ImplementDecl) {
        let this = match i.target.as_str() {
            "String" => Ty::Str,
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Bool" => Ty::Bool,
            name if self.type_names.contains(name) => Ty::Named(name.to_string()),
            _ => {
                self.error(Code::unknown_name, i.span);
                return;
            }
        };
        self.declare_members(&i.target, this, &i.members);
    }

    fn declare_enum(&mut self, e: &EnumDecl) {
        self.check_unique(e.variants.iter().map(|v| (&v.name, v.span)));
        let variants = e
            .variants
            .iter()
            .map(|v| VariantDef {
                name: v.name.clone(),
                fields: v
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, t)| ParamSig {
                        name: v
                            .field_names
                            .as_ref()
                            .map_or_else(String::new, |n| n[i].clone()),
                        ty: self.type_from(t),
                        has_default: false,
                        is_rest: false,
                    })
                    .collect(),
            })
            .collect();
        self.types.insert(e.name.clone(), TypeDef::Enum(variants));
        self.declare_members(&e.name, Ty::Named(e.name.clone()), &e.members);
    }

    fn declare_members(&mut self, owner: &str, this: Ty, members: &Members) {
        for m in &members.methods {
            self.declare_func(m, Some((this.clone(), false)), Some(owner));
        }
        for m in &members.statics {
            self.declare_func(m, None, Some(owner));
        }
        for c in &members.consts {
            let key = format!("{owner}.{}", c.name);
            if self.consts.contains_key(&key) || self.funcs.contains_key(&key) {
                self.error(Code::duplicate_name, c.span);
                continue;
            }
            self.consts.insert(
                key,
                ConstEntry {
                    decl: c.clone(),
                    owner: owner.to_string(),
                    ty: Ty::Error,
                    state: FuncState::Unchecked,
                },
            );
        }
    }

    fn check_unique<'n>(&mut self, names: impl Iterator<Item = (&'n String, Span)>) {
        let mut seen = HashSet::new();
        for (name, span) in names {
            if !seen.insert(name) {
                self.error(Code::duplicate_name, span);
            }
        }
    }

    fn declare_func(&mut self, decl: &FuncDecl, this: Option<(Ty, bool)>, owner: Option<&str>) {
        if self.funcs.contains_key(&decl.name) {
            if !self.in_library {
                self.error(Code::duplicate_name, decl.span);
            }
            return;
        }
        if decl.body.is_none() && !self.in_library {
            self.error(Code::external_outside_library, decl.span);
        }
        if RESERVED_NAMES.contains(&decl.name.as_str()) {
            self.error(Code::redefined_builtin, decl.span);
        }
        let params = self.param_sigs(&decl.params);
        let ret = decl.ret.as_ref().map(|t| self.type_from(t));
        let entry = FuncEntry {
            decl: decl.clone(),
            sig: Sig { params, ret },
            state: FuncState::Unchecked,
            this,
            owner: owner.map(str::to_string),
            library: self.in_library,
        };
        self.funcs.insert(decl.name.clone(), entry);
    }

    fn param_sigs(&mut self, decl_params: &[Param]) -> Vec<ParamSig> {
        let mut params = Vec::new();
        for p in decl_params {
            let ty = match (&p.ty, &p.default) {
                (Some(t), _) => self.type_from(t),
                (None, Some(default)) if p.is_rest => {
                    let t = self.expr(default, None);
                    self.resolve(&t)
                }
                (None, Some(_)) => self.subst.fresh(),
                (None, None) => self.error(Code::type_needed, p.span),
            };
            if let Some(default) = &p.default
                && !(p.is_rest && p.ty.is_none())
            {
                self.defaults
                    .push((default.clone(), ty.clone(), self.current_type.clone()));
            }
            let ty = match (p.is_rest, ty) {
                (true, Ty::List(elem)) => *elem,
                (true, Ty::Error) => Ty::Error,
                (true, _) => self.error(Code::type_mismatch, p.span),
                (false, ty) => ty,
            };
            params.push(ParamSig {
                name: p.name.clone(),
                ty,
                has_default: p.default.is_some(),
                is_rest: p.is_rest,
            });
        }
        params
    }

    fn const_type(&mut self, key: &str) -> Ty {
        let entry = self.consts.get_mut(key).expect("const");
        match entry.state {
            FuncState::Done => return entry.ty.clone(),
            FuncState::InProgress => {
                let span = entry.decl.span;
                return self.error(Code::const_cycle, span);
            }
            FuncState::Unchecked => entry.state = FuncState::InProgress,
        }
        let (decl, owner) = (entry.decl.clone(), entry.owner.clone());
        if let Some(bad) = self.non_constant(&decl.value, &owner) {
            self.error(Code::const_not_constant, bad.span);
        }
        let saved_scopes = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
        let saved_type = self.current_type.replace(owner);
        let declared = decl.ty.as_ref().map(|t| self.type_from(t));
        let actual = self.expr(&decl.value, declared.as_ref());
        let ty = match declared {
            Some(d) => {
                self.expect(&decl.value, &actual, &d);
                d
            }
            None => actual,
        };
        self.scopes = saved_scopes;
        self.current_type = saved_type;
        let entry = self.consts.get_mut(key).expect("const");
        entry.ty = ty.clone();
        entry.state = FuncState::Done;
        ty
    }

    fn non_constant<'e>(&self, e: &'e Expr, owner: &str) -> Option<&'e Expr> {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) => None,
            ExprKind::Str(segs) => segs.iter().find_map(|s| match s {
                StrSeg::Expr(x) => self.non_constant(x, owner),
                StrSeg::Text(_) => None,
            }),
            ExprKind::List(items) => items.iter().find_map(|x| self.non_constant(x, owner)),
            ExprKind::Unary(op, x) if *op != UnOp::Try => self.non_constant(x, owner),
            ExprKind::Binary(_, a, b) => self
                .non_constant(a, owner)
                .or_else(|| self.non_constant(b, owner)),
            ExprKind::Name(n) if self.consts.contains_key(&format!("{owner}.{n}")) => None,
            ExprKind::Field(base, n)
                if matches!(&base.kind, ExprKind::Name(t)
                    if self.consts.contains_key(&format!("{t}.{n}"))) =>
            {
                None
            }
            _ => Some(e),
        }
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
                _ if self.type_names.contains(name) => Ty::Named(name.clone()),
                _ => self.error(Code::unknown_name, *span),
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
                _ => self.error(Code::unknown_name, *span),
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
            TypeExpr::Record(names, fields, _) => {
                let fields = fields.iter().map(|f| self.type_from(f)).collect();
                Ty::Record(names.clone(), fields)
            }
        }
    }

    fn ensure_checked(&mut self, name: &str, call_span: Span) -> Option<Sig> {
        let entry = self.funcs.get_mut(name)?;
        match entry.state {
            FuncState::Done => return Some(entry.sig.clone()),
            FuncState::InProgress => {
                if entry.sig.ret.is_none() {
                    self.error(Code::recursion_without_return_type, call_span);
                    return None;
                }
                return Some(entry.sig.clone());
            }
            FuncState::Unchecked if entry.decl.body.is_none() => {
                entry.state = FuncState::Done;
                entry.sig.ret.get_or_insert(Ty::Void);
                return Some(entry.sig.clone());
            }
            FuncState::Unchecked => entry.state = FuncState::InProgress,
        }
        let decl = entry.decl.clone();
        let sig = entry.sig.clone();
        let this = entry.this.clone();
        let saved_type = std::mem::replace(&mut self.current_type, entry.owner.clone());
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
        if let Some((ty, mutable)) = this {
            scope.insert(
                "this".to_string(),
                Var {
                    ty,
                    mutable,
                    kind: VarKind::Param,
                    span: decl.span,
                    reassigned: false,
                    list_len: None,
                },
            );
        }
        self.scopes.push(scope);
        let body = decl.body.as_ref().expect("checked functions have bodies");
        let ret = self.function_body(body, sig.ret.clone());
        self.finish_scope();
        self.current_type = saved_type;
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
            let code = self.mismatch(&body_ty, &ret, Code::return_type_mismatch);
            self.error(code, body.span);
        }
        for (id, span, ty) in ctx.returns {
            if !self.subsume(id, &ty, &ret) {
                let code = self.mismatch(&ty, &ret, Code::return_type_mismatch);
                self.error(code, span);
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
            Code::unhandled_maybe
        } else {
            self.mismatch(&a, expected, Code::type_mismatch)
        };
        self.error(code, expr.span);
    }

    fn mismatch(&self, actual: &Ty, expected: &Ty, otherwise: Code) -> Code {
        let expected = match self.subst.shallow(expected) {
            Ty::Maybe(inner) => self.subst.shallow(&inner),
            e => e,
        };
        match (self.subst.shallow(actual), expected) {
            (Ty::Record(..), Ty::Named(class)) if self.class(&class).is_some() => {
                Code::record_to_class
            }
            _ => otherwise,
        }
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
            self.warn(Code::unused_var, span);
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
            self.error(Code::redefined_builtin, span);
        }
        let global = self.scopes.len() == 1;
        let user_func = self.funcs.get(name).is_some_and(|f| !f.library);
        if self.scopes.last().is_some_and(|s| s.contains_key(name)) || (global && user_func) {
            self.error(Code::duplicate_name, span);
        } else if self.lookup(name).is_some() || user_func {
            self.warn(Code::shadowed_name, span);
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
                if let (ExprKind::Record(args), Some(Ty::Named(class))) = (&value.kind, &declared)
                    && self.class(class).is_some()
                    && !args
                        .first()
                        .is_some_and(|a| matches!(a.value.kind, ExprKind::Spread(_)))
                {
                    self.warn(Code::record_beside_class, value.span);
                }
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
            Stmt::LetPattern {
                pattern,
                is_var,
                value,
                otherwise,
                ..
            } => {
                let t = self.expr(value, None);
                let mut binds = Vec::new();
                self.pattern(pattern, &t, &mut binds);
                match otherwise {
                    Some(otherwise) => {
                        let ot = self.expr(otherwise, None);
                        if self.subst.shallow(&ot) != Ty::Never {
                            self.error(Code::let_else_can_finish, otherwise.span);
                        }
                        if let PatternKind::Variant(None, name, args) = &pattern.kind
                            && name == "Some"
                            && matches!(args.as_slice(), [p] if matches!(p.kind, PatternKind::Name(_)) && !self.info.variant_patterns.contains(&p.id))
                        {
                            self.warn(Code::let_else_some, pattern.span);
                        }
                    }
                    None => self.report_missing(vec![vec![Cell::Pat(pattern)]], &t, pattern.span),
                }
                for (name, span, ty) in binds {
                    let var = Var {
                        mutable: *is_var,
                        ..Var::binding(ty, span)
                    };
                    self.declare(&name, span, var);
                }
                Ty::Void
            }
            Stmt::Func(_) | Stmt::Class(_) | Stmt::Enum(_) | Stmt::Implement(_) => Ty::Void,
            Stmt::Expr(e) => self.unused(e),
        }
    }

    fn place_name(&mut self, name: &str, span: Span) -> (Ty, Option<Code>) {
        if self.lookup(name).is_none()
            && let Some(owner) = &self.current_type
        {
            let key = format!("{owner}.{name}");
            if self.consts.contains_key(&key) {
                return (self.const_type(&key), Some(Code::assign_to_let));
            }
        }
        let Some(var) = self.lookup_mut(name) else {
            let code = if name == "this" {
                Code::this_outside_method
            } else if self.top_level_names.contains(name) {
                Code::used_before_set
            } else {
                Code::unknown_name
            };
            return (self.error(code, span), None);
        };
        if !var.mutable {
            let code = if var.kind == VarKind::Param && name != "this" {
                Code::assign_to_parameter
            } else {
                Code::assign_to_let
            };
            return (var.ty.clone(), Some(code));
        }
        var.reassigned = true;
        var.list_len = None;
        (var.ty.clone(), None)
    }

    fn place(&mut self, target: &Expr) -> (Ty, Option<Code>) {
        match &target.kind {
            ExprKind::Name(name) => self.place_name(name, target.span),
            ExprKind::This => self.place_name("this", target.span),
            ExprKind::Field(base, name) if self.type_ref(base).is_none() => {
                let (bt, base_err) = self.place(base);
                match self.subst.shallow(&bt) {
                    Ty::Named(class) => match self.visible_field(&class, name, target.span) {
                        Some(f) => {
                            let FieldInfo {
                                ty,
                                mutable,
                                is_ref,
                                ..
                            } = f;
                            let err = if !mutable {
                                Some(Code::assign_to_let)
                            } else if f.readonly && !self.inside(&class) {
                                Some(Code::readonly_field)
                            } else if is_ref {
                                None
                            } else {
                                base_err
                            };
                            (ty, err)
                        }
                        None => (self.error(Code::unknown_field, target.span), None),
                    },
                    Ty::Record(names, fields) => match record_field(&names, &fields, name) {
                        Some(ty) => (ty, Some(Code::assign_to_let)),
                        None => (self.error(Code::unknown_field, target.span), None),
                    },
                    Ty::Error => (Ty::Error, None),
                    Ty::Var(_) => (self.error(Code::type_needed, base.span), None),
                    _ => (self.error(Code::unknown_field, target.span), None),
                }
            }
            ExprKind::Index(list, index) => {
                let (lt, base_err) = self.place(list);
                let it = self.expr(index, Some(&Ty::Int));
                self.expect(index, &it, &Ty::Int);
                match self.subst.shallow(&lt) {
                    Ty::List(elem) => (*elem, base_err),
                    Ty::Error => (Ty::Error, None),
                    Ty::Var(_) => (self.error(Code::type_needed, list.span), None),
                    _ => (self.error(Code::invalid_operand_types, target.span), None),
                }
            }
            _ => (self.expr(target, None), Some(Code::assign_to_let)),
        }
    }

    fn class(&self, type_name: &str) -> Option<&ClassDef> {
        match self.types.get(type_name) {
            Some(TypeDef::Class(c)) => Some(c),
            _ => None,
        }
    }

    fn field(&self, type_name: &str, field: &str) -> Option<FieldInfo> {
        let c = self.class(type_name)?;
        let i = c.fields.iter().position(|f| f.name == field)?;
        let ty = match &c.fields[i] {
            f if f.is_rest => Ty::list(f.ty.clone()),
            f => f.ty.clone(),
        };
        Some(FieldInfo {
            ty,
            mutable: c.mutable[i],
            is_ref: c.is_ref,
            private: c.private[i],
            readonly: c.readonly[i],
        })
    }

    fn check_private_method(&mut self, key: &str, type_name: &str, span: Span) {
        if self.funcs[key].decl.is_private && !self.inside(type_name) {
            self.error(Code::private_member, span);
        }
    }

    fn inside(&self, type_name: &str) -> bool {
        self.current_type.as_deref() == Some(type_name)
    }

    /// A field read from code outside its class, which can't see private fields.
    fn visible_field(&mut self, type_name: &str, field: &str, span: Span) -> Option<FieldInfo> {
        let f = self.field(type_name, field)?;
        if f.private && !self.inside(type_name) {
            self.error(Code::private_member, span);
        }
        Some(f)
    }

    /// The constructor takes every field but the private ones.
    fn construct(&mut self, class: &str, args: &[Arg], span: Span) {
        for a in args {
            if let Some(name) = &a.name
                && self.field(class, name).is_some_and(|f| f.private)
            {
                self.error(Code::private_member, a.value.span);
            }
        }
        let c = self.class(class).expect("class");
        let params: Vec<ParamSig> = c
            .fields
            .iter()
            .zip(&c.private)
            .filter(|(_, private)| !**private)
            .map(|(f, _)| f.clone())
            .collect();
        self.match_args(&params, args, span);
    }

    fn type_ref(&self, e: &Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Name(n)
                if self.lookup(n).is_none()
                    && (self.type_names.contains(n) || BUILT_IN_TYPES.contains(&n.as_str())) =>
            {
                Some(n.clone())
            }
            _ => None,
        }
    }

    fn holds_ref(&self, ty: &Ty, seen: &mut HashSet<String>) -> bool {
        match self.subst.shallow(ty) {
            Ty::List(inner) | Ty::Maybe(inner) => self.holds_ref(&inner, seen),
            Ty::Named(name) if seen.insert(name.clone()) => match self.types.get(&name) {
                Some(TypeDef::Class(c)) => {
                    c.is_ref || c.fields.iter().any(|f| self.holds_ref(&f.ty, seen))
                }
                Some(TypeDef::Enum(vs)) => vs
                    .iter()
                    .any(|v| v.fields.iter().any(|f| self.holds_ref(&f.ty, seen))),
                None => false,
            },
            _ => false,
        }
    }

    fn is_ref_class(&self, ty: &Ty) -> bool {
        matches!(self.subst.shallow(ty), Ty::Named(n)
            if matches!(self.types.get(&n), Some(TypeDef::Class(c)) if c.is_ref))
    }

    fn bare_variant(&self, e: &Expr) -> bool {
        let name = match &e.kind {
            ExprKind::Name(n) => n,
            ExprKind::Call(callee, _) => match &callee.kind {
                ExprKind::Name(n) => n,
                _ => return false,
            },
            _ => return false,
        };
        self.lookup(name).is_none()
            && !self.funcs.contains_key(name)
            && !self.type_names.contains(name)
            && self.enum_with_variant(name)
    }

    fn variant(&self, enum_name: &str, variant: &str) -> Option<&VariantDef> {
        match self.types.get(enum_name) {
            Some(TypeDef::Enum(vs)) => vs.iter().find(|v| v.name == variant),
            _ => None,
        }
    }

    fn enum_with_variant(&self, variant: &str) -> bool {
        self.types
            .values()
            .any(|t| matches!(t, TypeDef::Enum(vs) if vs.iter().any(|v| v.name == variant)))
    }

    fn expected_variant(&self, expected: Option<&Ty>, name: &str) -> Option<String> {
        match expected.map(|e| self.subst.shallow(e)) {
            Some(Ty::Named(e)) if self.variant(&e, name).is_some() => Some(e),
            Some(Ty::Maybe(inner)) => self.expected_variant(Some(&inner), name),
            _ => None,
        }
    }

    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, span: Span) {
        let (ty, err) = self.place(target);
        if let Some(code) = err {
            self.error(code, target.span);
        }
        let actual = match op {
            None => self.expr(value, Some(&ty)),
            Some(op) => {
                let rhs = self.expr(value, None);
                let result = self.binary_type(op, target, &ty, value, &rhs, span);
                if op == BinOp::Div && self.subst.shallow(&ty) == Ty::Int {
                    self.error(Code::slash_for_int, span);
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

    // Branches whose value is never used don't need matching types (design.md, Syntax).
    fn unused(&mut self, expr: &Expr) -> Ty {
        match &expr.kind {
            ExprKind::Block(stmts) => {
                self.scopes.push(HashMap::new());
                for stmt in stmts {
                    self.stmt(stmt);
                }
                self.finish_scope();
            }
            ExprKind::If(cond, then, otherwise) => {
                let c = self.expr(cond, Some(&Ty::Bool));
                self.expect(cond, &c, &Ty::Bool);
                self.push_bindings(self.exports(cond));
                self.unused(then);
                self.scopes.pop();
                if let Some(other) = otherwise {
                    self.unused(other);
                }
            }
            ExprKind::Match(scrutinee, arms) => {
                self.match_expr(expr, scrutinee, arms, None, false);
            }
            _ => return self.expr(expr, None),
        }
        Ty::Void
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
                            self.warn(Code::interpolated_maybe, e.span);
                        }
                    }
                }
                Ty::Str
            }
            ExprKind::Name(name) => self.name(expr.id, name, expr.span, expected),
            ExprKind::This => match self.lookup("this") {
                Some(var) => var.ty.clone(),
                None => self.error(Code::this_outside_method, expr.span),
            },
            ExprKind::Field(base, name) => self.field_expr(expr.id, base, name, expr.span),
            ExprKind::Match(scrutinee, arms) => {
                self.match_expr(expr, scrutinee, arms, expected, true)
            }
            ExprKind::Is(lhs, pattern) => {
                let t = self.expr(lhs, None);
                let mut binds = Vec::new();
                self.pattern(pattern, &t, &mut binds);
                self.is_bindings.insert(expr.id, binds);
                Ty::Bool
            }
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
            ExprKind::Record(args) => self.record_literal(expr, args, expected),
            ExprKind::Spread(inner) => {
                self.expr(inner, None);
                self.error(Code::wrong_arguments, expr.span)
            }
            ExprKind::Unary(op, operand) => self.unary(*op, operand, expr.span),
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::Call(callee, args) => self.call(expr.id, callee, args, expr.span, expected),
            ExprKind::Method(recv, name, args) => self.method(expr.id, recv, name, args, expr.span),
            ExprKind::Index(list, index) => self.index(expr.id, list, index, expr.span),
            ExprKind::Lambda(params, ret, body) => {
                self.lambda(expr.id, params, ret.as_ref(), body, expected)
            }
            ExprKind::Block(stmts) => self.block(stmts, expected),
            ExprKind::If(cond, then, otherwise) => {
                self.if_expr(expr, cond, then, otherwise.as_deref(), expected)
            }
            ExprKind::While(cond, body) => {
                let c = self.expr(cond, Some(&Ty::Bool));
                self.expect(cond, &c, &Ty::Bool);
                self.push_bindings(self.exports(cond));
                let ty = self.valued_loop(expr.id, body);
                self.scopes.pop();
                ty
            }
            ExprKind::For(pattern, iter, body) => self.for_loop(expr.id, pattern, iter, body),
            ExprKind::Loop(body) => {
                self.loops.push(LoopCtx {
                    kind: LoopKind::Loop,
                    break_ty: None,
                });
                self.unused(body);
                let ctx = self.loops.pop().expect("loop context");
                ctx.break_ty.unwrap_or(Ty::Never)
            }
            ExprKind::Break(value) => {
                let ty = match value {
                    Some(v) => self.expr(v, None),
                    None => Ty::Void,
                };
                let Some(ctx) = self.loops.last() else {
                    return self.error(Code::break_outside_loop, expr.span);
                };
                match ctx.break_ty.clone() {
                    None => self.loops.last_mut().expect("loop").break_ty = Some(ty),
                    Some(prev) => {
                        let ok = match value {
                            Some(v) => self.subsume(v.id, &ty, &prev),
                            None => self.subst.unify(&prev, &Ty::Void),
                        };
                        if !ok {
                            self.error(Code::break_type_mismatch, expr.span);
                        }
                    }
                }
                Ty::Never
            }
            ExprKind::Continue => {
                if self.loops.is_empty() {
                    return self.error(Code::break_outside_loop, expr.span);
                }
                Ty::Never
            }
            ExprKind::Return(value) => {
                let declared = self.fns.last().map(|f| f.declared.clone());
                let Some(declared) = declared else {
                    return self.error(Code::return_outside_function, expr.span);
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

    fn push_bindings(&mut self, binds: Vec<Binding>) {
        let scope = binds
            .into_iter()
            .map(|(name, span, ty)| (name, Var::binding(ty, span)))
            .collect();
        self.scopes.push(scope);
    }

    fn exports(&self, e: &Expr) -> Vec<Binding> {
        match &e.kind {
            ExprKind::Is(..) => self.is_bindings.get(&e.id).cloned().unwrap_or_default(),
            ExprKind::Binary(BinOp::And, l, r) => {
                let mut binds = self.exports(l);
                binds.extend(self.exports(r));
                binds
            }
            _ => Vec::new(),
        }
    }

    fn unit_variant(&mut self, id: NodeId, enum_name: &str, name: &str, span: Span) -> Ty {
        let has_fields = self
            .variant(enum_name, name)
            .is_some_and(|v| !v.fields.is_empty());
        if has_fields {
            return self.error(Code::wrong_arguments, span);
        }
        self.info.resolved.insert(
            id,
            Resolved::Variant(enum_name.to_string(), name.to_string()),
        );
        Ty::Named(enum_name.to_string())
    }

    fn variant_call(
        &mut self,
        id: NodeId,
        enum_name: &str,
        name: &str,
        args: &[Arg],
        span: Span,
    ) -> Ty {
        let params = self
            .variant(enum_name, name)
            .map(|v| v.fields.clone())
            .unwrap_or_default();
        self.match_args(&params, args, span);
        self.info.resolved.insert(
            id,
            Resolved::Variant(enum_name.to_string(), name.to_string()),
        );
        Ty::Named(enum_name.to_string())
    }

    fn record_literal(&mut self, expr: &Expr, args: &[Arg], expected: Option<&Ty>) -> Ty {
        if let Some(ExprKind::Spread(inner)) = args.first().map(|a| &a.value.kind) {
            return self.record_copy(inner, &args[1..]);
        }
        let expected = expected.map(|e| match self.subst.shallow(e) {
            Ty::Maybe(inner) => self.subst.shallow(&inner),
            e => e,
        });
        if let Some(Ty::Named(class)) = &expected
            && self.class(class).is_some()
        {
            self.construct(class, args, expr.span);
            self.info
                .resolved
                .insert(expr.id, Resolved::Construct(class.clone()));
            return Ty::Named(class.clone());
        }
        if args.is_empty() {
            return self.error(Code::type_needed, expr.span);
        }
        let names: Vec<String> = args.iter().filter_map(|a| a.name.clone()).collect();
        self.check_unique(
            args.iter()
                .filter_map(|a| Some((a.name.as_ref()?, a.value.span))),
        );
        let hint = match expected {
            Some(Ty::Record(n, f)) if n == names && f.len() == args.len() => Some(f),
            _ => None,
        };
        let mut fields = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            if let ExprKind::Spread(inner) = &arg.value.kind {
                self.expr(inner, None);
                fields.push(self.error(Code::spread_not_first, arg.value.span));
                continue;
            }
            fields.push(match &hint {
                Some(h) => {
                    let t = self.expr(&arg.value, Some(&h[i]));
                    self.expect(&arg.value, &t, &h[i]);
                    h[i].clone()
                }
                None => self.expr(&arg.value, None),
            });
        }
        if !names.is_empty() && names.len() != args.len() {
            return self.error(Code::mixed_fields, expr.span);
        }
        Ty::Record(names, fields)
    }

    fn record_copy(&mut self, original: &Expr, changes: &[Arg]) -> Ty {
        let t = self.expr(original, None);
        let copyable = match self.subst.shallow(&t) {
            Ty::Named(class) => self.class(&class).is_some(),
            Ty::Record(..) | Ty::Error => true,
            Ty::Var(_) => {
                self.error(Code::type_needed, original.span);
                false
            }
            _ => false,
        };
        if !copyable {
            self.error(Code::type_mismatch, original.span);
        }
        let mut changed = HashSet::new();
        for change in changes {
            if let Some(name) = &change.name
                && !changed.insert(name)
            {
                self.expr(&change.value, None);
                self.error(Code::wrong_arguments, change.value.span);
                continue;
            }
            let field = match (&change.name, self.subst.shallow(&t)) {
                (Some(name), Ty::Named(class)) => match self.field(&class, name) {
                    Some(f) if (f.private || f.readonly) && !self.inside(&class) => {
                        let code = if f.private {
                            Code::private_member
                        } else {
                            Code::readonly_field
                        };
                        self.error(code, change.value.span);
                        Some(f.ty)
                    }
                    f => f.map(|f| f.ty),
                },
                (Some(name), Ty::Record(names, fields)) if !names.is_empty() => {
                    record_field(&names, &fields, name)
                }
                (_, Ty::Error) => Some(Ty::Error),
                _ => None,
            };
            let code = match (&change.value.kind, &change.name, field) {
                (ExprKind::Spread(_), ..) => Code::spread_not_first,
                (_, None, _) => Code::wrong_arguments,
                (_, Some(_), None) => Code::unknown_field,
                (_, Some(_), Some(ft)) => {
                    let v = self.expr(&change.value, Some(&ft));
                    self.expect(&change.value, &v, &ft);
                    continue;
                }
            };
            let value = match &change.value.kind {
                ExprKind::Spread(inner) => inner,
                _ => &change.value,
            };
            self.expr(value, None);
            self.error(code, change.value.span);
        }
        if copyable { t } else { Ty::Error }
    }

    fn field_expr(&mut self, id: NodeId, base: &Expr, name: &str, span: Span) -> Ty {
        if let Some(t) = self.type_ref(base) {
            let key = format!("{t}.{name}");
            if self.consts.contains_key(&key) {
                if self.consts[&key].decl.is_private && !self.inside(&t) {
                    self.error(Code::private_member, span);
                }
                self.info.resolved.insert(id, Resolved::Const(key.clone()));
                return self.const_type(&key);
            }
            if self.variant(&t, name).is_some() {
                return self.unit_variant(id, &t, name, span);
            }
            return self.error(Code::unknown_field, span);
        }
        let bt = self.expr(base, None);
        match self.subst.shallow(&bt) {
            Ty::Named(class) => match self.visible_field(&class, name, span) {
                Some(f) => f.ty,
                None => self.error(Code::unknown_field, span),
            },
            Ty::Record(names, fields) => match record_field(&names, &fields, name) {
                Some(ty) => ty,
                None => self.error(Code::unknown_field, span),
            },
            Ty::Error => Ty::Error,
            Ty::Var(_) => self.error(Code::type_needed, base.span),
            _ => self.error(Code::unknown_field, span),
        }
    }

    fn name(&mut self, id: NodeId, name: &str, span: Span, expected: Option<&Ty>) -> Ty {
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
        if let Some(owner) = &self.current_type {
            let key = format!("{owner}.{name}");
            if self.consts.contains_key(&key) {
                self.info.resolved.insert(id, Resolved::Const(key.clone()));
                return self.const_type(&key);
            }
        }
        if let Some(e) = self.expected_variant(expected, name) {
            return self.unit_variant(id, &e, name, span);
        }
        if self.enum_with_variant(name) {
            return self.error(Code::unqualified_variant, span);
        }
        let code = if self.top_level_names.contains(name) && !self.fns.is_empty() {
            Code::used_before_set
        } else if self.top_level_names.contains(name) {
            Code::used_before_declared
        } else {
            Code::unknown_name
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
                    None => (**inner).clone(),
                    Some(Some(
                        Ty::Int
                        | Ty::Float
                        | Ty::Bool
                        | Ty::Str
                        | Ty::Void
                        | Ty::List(_)
                        | Ty::Func(..)
                        | Ty::Range,
                    )) => self.error(Code::try_outside_maybe, span),
                    _ => {
                        self.fns.last_mut().expect("fn").uses_try = true;
                        (**inner).clone()
                    }
                }
            }
            (_, Ty::Var(_)) => self.error(Code::type_needed, operand.span),
            _ => self.error(Code::invalid_operand_types, span),
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        match op {
            BinOp::And | BinOp::Or => {
                let lt = self.expr(lhs, Some(&Ty::Bool));
                if !self.subst.unify(&lt, &Ty::Bool) {
                    self.error(Code::invalid_operand_types, lhs.span);
                }
                let binds = if op == BinOp::And {
                    self.exports(lhs)
                } else {
                    Vec::new()
                };
                self.push_bindings(binds);
                let rt = self.expr(rhs, Some(&Ty::Bool));
                self.scopes.pop();
                if !self.subst.unify(&rt, &Ty::Bool) {
                    self.error(Code::invalid_operand_types, rhs.span);
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
                    Ty::Var(_) => self.error(Code::type_needed, lhs.span),
                    Ty::Error => Ty::Error,
                    _ => self.error(Code::type_mismatch, lhs.span),
                }
            }
            BinOp::Eq | BinOp::Ne => {
                let (lt, rt) = if self.bare_variant(lhs) {
                    let rt = self.expr(rhs, None);
                    (self.expr(lhs, Some(&rt)), rt)
                } else {
                    let lt = self.expr(lhs, None);
                    let rt = self.expr(rhs, Some(&lt));
                    (lt, rt)
                };
                self.binary_type(op, lhs, &lt, rhs, &rt, span)
            }
            _ => {
                let lt = self.expr(lhs, None);
                let rt = self.expr(rhs, None);
                if op == BinOp::Pow
                    && self.subst.shallow(&lt) == Ty::Int
                    && let ExprKind::Unary(UnOp::Neg, inner) = &rhs.kind
                    && matches!(inner.kind, ExprKind::Int(_))
                {
                    return self.error(Code::negative_literal_exponent, rhs.span);
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
            return self.error(Code::type_needed, lhs.span);
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
                    self.error(Code::bitwise_on_bool, span)
                } else {
                    self.error(Code::invalid_operand_types, span)
                }
            }
            BinOp::Eq | BinOp::Ne => {
                if self.holds_ref(&a, &mut HashSet::new()) {
                    return self.error(Code::invalid_operand_types, span);
                }
                if numeric {
                    widen(self);
                } else if !self.subst.unify(&a, &b) {
                    return self.error(Code::invalid_operand_types, span);
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
                    self.error(Code::invalid_operand_types, span)
                }
            }
            BinOp::Range | BinOp::RangeIncl if a == Ty::Int && b == Ty::Int => {
                if let (ExprKind::Int(x), ExprKind::Int(y)) = (&lhs.kind, &rhs.kind)
                    && x > y
                {
                    self.error(Code::constant_backwards_range, span);
                }
                Ty::Range
            }
            _ => self.error(Code::invalid_operand_types, span),
        }
    }

    fn spread_args<'a>(&mut self, args: &'a [Arg]) -> Option<Vec<(Option<String>, ArgSource<'a>)>> {
        let mut out = Vec::new();
        let mut known = true;
        for (i, arg) in args.iter().enumerate() {
            let ExprKind::Spread(inner) = &arg.value.kind else {
                out.push((arg.name.clone(), ArgSource::Expr(&arg.value)));
                continue;
            };
            let t = self.expr(inner, None);
            if i > 0 {
                self.error(Code::spread_not_first, arg.value.span);
                known = false;
                continue;
            }
            match self.subst.shallow(&t) {
                Ty::Record(names, fields) => {
                    for (k, ty) in fields.into_iter().enumerate() {
                        out.push((names.get(k).cloned(), ArgSource::Spread(ty, arg.value.span)));
                    }
                }
                Ty::Error => known = false,
                _ => {
                    self.error(Code::type_mismatch, inner.span);
                    known = false;
                }
            }
        }
        if !known {
            for (_, source) in out {
                if let ArgSource::Expr(e) = source {
                    self.expr(e, None);
                }
            }
            return None;
        }
        Some(out)
    }

    fn check_arg(&mut self, source: &ArgSource, param: &Ty) {
        match source {
            ArgSource::Expr(e) => {
                let t = self.expr(e, Some(param));
                self.expect(e, &t, param);
            }
            ArgSource::Spread(t, span) => {
                if !self.subst.unify(t, param) {
                    let code = self.mismatch(t, param, Code::type_mismatch);
                    self.error(code, *span);
                }
            }
        }
    }

    fn skip_arg(&mut self, source: &ArgSource) {
        if let ArgSource::Expr(e) = source {
            self.expr(e, None);
        }
    }

    fn args_of(&mut self, args: &[Arg], span: Span, params: &[Ty]) -> bool {
        let Some(sources) = self.spread_args(args) else {
            return false;
        };
        if sources.len() != params.len() || sources.iter().any(|(name, _)| name.is_some()) {
            for (_, source) in &sources {
                self.skip_arg(source);
            }
            self.error(Code::wrong_arguments, span);
            return false;
        }
        for ((_, source), param) in sources.iter().zip(params) {
            self.check_arg(source, param);
        }
        true
    }

    fn call(
        &mut self,
        id: NodeId,
        callee: &Expr,
        args: &[Arg],
        span: Span,
        expected: Option<&Ty>,
    ) -> Ty {
        if let ExprKind::Name(name) = &callee.kind
            && self.lookup(name).is_none()
        {
            if self.funcs.contains_key(name) {
                return self.call_named(name, args, span);
            }
            match name.as_str() {
                "print" => {
                    if args.len() != 1 {
                        return self.error(Code::wrong_arguments, span);
                    }
                    self.expr(&args[0].value, None);
                    return Ty::Void;
                }
                "Some" => {
                    if args.len() != 1 {
                        return self.error(Code::wrong_arguments, span);
                    }
                    let inner = match expected.map(|e| self.subst.shallow(e)) {
                        Some(Ty::Maybe(inner)) => Some(*inner),
                        _ => None,
                    };
                    let t = self.expr(&args[0].value, inner.as_ref());
                    return Ty::maybe(t);
                }
                "same" => {
                    if args.len() != 2 {
                        return self.error(Code::wrong_arguments, span);
                    }
                    let a = self.expr(&args[0].value, None);
                    let b = self.expr(&args[1].value, None);
                    if !self.is_ref_class(&a) || !self.subst.unify(&a, &b) {
                        self.error(Code::type_mismatch, span);
                    }
                    return Ty::Bool;
                }
                _ => {}
            }
            if let Some(TypeDef::Class(_)) = self.types.get(name) {
                self.construct(name, args, span);
                self.info
                    .resolved
                    .insert(id, Resolved::Construct(name.clone()));
                return Ty::Named(name.clone());
            }
            if let Some(e) = self.expected_variant(expected, name) {
                return self.variant_call(id, &e, name, args, span);
            }
            if self.enum_with_variant(name) {
                for a in args {
                    self.expr(&a.value, None);
                }
                return self.error(Code::unqualified_variant, callee.span);
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
            _ => self.error(Code::not_callable, callee.span),
        }
    }

    fn call_named(&mut self, name: &str, args: &[Arg], span: Span) -> Ty {
        let params = self.funcs[name].sig.params.clone();
        self.match_args(&params, args, span);
        match self.ensure_checked(name, span) {
            Some(sig) => sig.ret.unwrap_or(Ty::Void),
            None => Ty::Error,
        }
    }

    fn match_args(&mut self, params: &[ParamSig], args: &[Arg], span: Span) {
        let Some(sources) = self.spread_args(args) else {
            return;
        };
        let mut filled = vec![false; params.len()];
        let positional: Vec<&ArgSource> = sources
            .iter()
            .filter(|(name, _)| name.is_none())
            .map(|(_, source)| source)
            .collect();
        let fixed: Vec<usize> = params
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_rest)
            .map(|(i, _)| i)
            .collect();
        let rest = params.iter().position(|p| p.is_rest);
        let mut ok = true;
        for (k, source) in positional.into_iter().enumerate() {
            if let Some(&i) = fixed.get(k) {
                filled[i] = true;
                self.check_arg(source, &params[i].ty);
            } else if let Some(r) = rest {
                filled[r] = true;
                self.check_arg(source, &params[r].ty);
            } else {
                self.skip_arg(source);
                ok = false;
            }
        }
        for (name, source) in &sources {
            let Some(arg_name) = name else {
                continue;
            };
            match params
                .iter()
                .position(|p| &p.name == arg_name && !p.is_rest)
            {
                Some(i) if !filled[i] => {
                    filled[i] = true;
                    self.check_arg(source, &params[i].ty);
                }
                _ => {
                    self.skip_arg(source);
                    ok = false;
                }
            }
        }
        for (i, p) in params.iter().enumerate() {
            if !filled[i] && !p.has_default && !p.is_rest {
                ok = false;
            }
        }
        if !ok {
            self.error(Code::wrong_arguments, span);
        }
    }

    fn method(&mut self, id: NodeId, recv: &Expr, name: &str, args: &[Arg], span: Span) -> Ty {
        if let Some(t) = self.type_ref(recv) {
            if self.variant(&t, name).is_some() {
                return self.variant_call(id, &t, name, args, span);
            }
            let key = format!("{t}.{name}");
            if self.funcs.get(&key).is_some_and(|f| f.this.is_none()) {
                self.check_private_method(&key, &t, span);
                self.info.resolved.insert(id, Resolved::Static(key.clone()));
                return self.call_named(&key, args, span);
            }
            for a in args {
                self.expr(&a.value, None);
            }
            return self.error(Code::unknown_method, span);
        }
        let is_place = matches!(
            recv.kind,
            ExprKind::Name(_) | ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::This
        );
        let recv_ty = if MUTATING.contains(&name) && is_place {
            let (t, err) = self.place(recv);
            if let Some(code) = err
                && matches!(self.subst.shallow(&t), Ty::List(_))
            {
                let code = if code == Code::readonly_field {
                    code
                } else {
                    Code::change_let_list
                };
                self.error(code, recv.span);
            }
            t
        } else {
            self.expr(recv, None)
        };
        let recv_ty = self.subst.shallow(&recv_ty);
        match recv_ty {
            t @ (Ty::Named(_) | Ty::Str | Ty::Int | Ty::Float | Ty::Bool) => {
                let key = format!("{}.{name}", Show(&t));
                if self.funcs.get(&key).is_some_and(|f| f.this.is_some()) {
                    self.check_private_method(&key, &Show(&t).to_string(), span);
                    self.info.resolved.insert(id, Resolved::Method(key.clone()));
                    return self.call_named(&key, args, span);
                }
                for a in args {
                    self.expr(&a.value, None);
                }
                self.error(Code::unknown_method, span)
            }
            Ty::Error => {
                for a in args {
                    self.expr(&a.value, None);
                }
                Ty::Error
            }
            Ty::Var(_) => self.error(Code::type_needed, recv.span),
            Ty::List(elem) => self.list_method(*elem, name, args, span),
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
                        return self.error(Code::flatten_without_nesting, span);
                    }
                    while let Ty::Maybe(i) = t {
                        t = self.subst.shallow(&i);
                    }
                    Ty::maybe(t)
                }
                _ => self.error(Code::unknown_method, span),
            },
            _ => self.error(Code::unknown_method, span),
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
                    return self.error(Code::unknown_method, span);
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
            _ => self.error(Code::unknown_method, span),
        }
    }

    fn index(&mut self, id: NodeId, list: &Expr, index: &Expr, span: Span) -> Ty {
        let lt = self.expr(list, None);
        let it = self.expr(index, Some(&Ty::Int));
        self.expect(index, &it, &Ty::Int);
        let elem = match self.subst.shallow(&lt) {
            Ty::List(e) => *e,
            Ty::Error => return Ty::Error,
            Ty::Var(_) => return self.error(Code::type_needed, list.span),
            _ => return self.error(Code::invalid_operand_types, span),
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

    fn lambda(
        &mut self,
        id: NodeId,
        params: &[Param],
        ret: Option<&TypeExpr>,
        body: &Expr,
        expected: Option<&Ty>,
    ) -> Ty {
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
        let declared = ret.map(|t| self.type_from(t));
        let saved_loops = std::mem::take(&mut self.loops);
        self.scopes.push(scope);
        let ret = self.function_body(body, declared);
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
        self.push_bindings(self.exports(cond));
        let tt = self.expr(then, expected);
        self.scopes.pop();
        match otherwise {
            Some(other) => {
                let ot = self.expr(other, expected);
                match self.join((then, tt), (other, ot)) {
                    Some(t) => t,
                    None => self.error(Code::branch_type_mismatch, expr.span),
                }
            }
            None => self.optional_value(expr, tt),
        }
    }

    fn match_expr(
        &mut self,
        expr: &Expr,
        scrutinee: &Expr,
        arms: &[Arm],
        expected: Option<&Ty>,
        used: bool,
    ) -> Ty {
        let st = self.expr(scrutinee, None);
        let mut bodies = Vec::new();
        for arm in arms {
            let mut binds = Vec::new();
            self.pattern(&arm.pattern, &st, &mut binds);
            self.push_bindings(binds);
            if let Some(guard) = &arm.guard {
                let g = self.expr(guard, Some(&Ty::Bool));
                self.expect(guard, &g, &Ty::Bool);
            }
            let t = if used {
                self.expr(&arm.body, expected)
            } else {
                self.unused(&arm.body)
            };
            self.scopes.pop();
            bodies.push((&arm.body, t));
        }
        let rows = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .map(|a| vec![Cell::Pat(&a.pattern)])
            .collect();
        self.report_missing(rows, &st, scrutinee.span);
        if !used {
            return Ty::Void;
        }
        let mut target = Ty::Never;
        for (_, t) in &bodies {
            target = match (self.subst.shallow(&target), self.subst.shallow(t)) {
                (Ty::Never, t) | (t, Ty::Never) => t,
                (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => Ty::Float,
                (Ty::Maybe(a), b) if !matches!(b, Ty::Maybe(_)) && self.subst.unify(&a, &b) => {
                    Ty::Maybe(a)
                }
                (a, Ty::Maybe(b)) if !matches!(a, Ty::Maybe(_)) && self.subst.unify(&a, &b) => {
                    Ty::Maybe(b)
                }
                (a, _) => a,
            };
        }
        for (body, t) in bodies {
            if !self.subsume(body.id, &t, &target) {
                return self.error(Code::branch_type_mismatch, expr.span);
            }
        }
        target
    }

    fn pattern(&mut self, pat: &Pattern, ty: &Ty, binds: &mut Vec<Binding>) {
        match &pat.kind {
            PatternKind::Wildcard => {}
            PatternKind::Name(name) => {
                let enum_name = match self.subst.shallow(ty) {
                    Ty::Named(e) if self.variant(&e, name).is_some() => Some(e),
                    _ => None,
                };
                if let Some(e) = enum_name {
                    if self.variant(&e, name).is_some_and(|v| !v.fields.is_empty()) {
                        self.error(Code::wrong_arguments, pat.span);
                    }
                    self.info.variant_patterns.insert(pat.id);
                } else if name == "None" {
                    let fresh = self.subst.fresh();
                    if !self.subst.unify(ty, &Ty::maybe(fresh)) {
                        self.error(Code::type_mismatch, pat.span);
                    }
                    self.info.variant_patterns.insert(pat.id);
                } else if binds.iter().any(|b| &b.0 == name) {
                    self.error(Code::duplicate_name, pat.span);
                } else {
                    binds.push((name.clone(), pat.span, ty.clone()));
                }
            }
            PatternKind::Literal(lit) => {
                let t = self.expr(lit, Some(ty));
                self.expect(lit, &t, ty);
            }
            PatternKind::Range(lo, hi, _) => {
                for end in [lo, hi] {
                    let t = self.expr(end, Some(ty));
                    self.expect(end, &t, ty);
                }
                if !matches!(self.subst.shallow(ty), Ty::Int | Ty::Float | Ty::Error) {
                    self.error(Code::type_mismatch, pat.span);
                }
            }
            PatternKind::Variant(None, name, args) if name == "Some" => {
                let inner = self.subst.fresh();
                if !self.subst.unify(ty, &Ty::maybe(inner.clone())) {
                    self.error(Code::type_mismatch, pat.span);
                }
                match args.as_slice() {
                    [arg] => self.pattern(arg, &inner, binds),
                    _ => {
                        self.error(Code::wrong_arguments, pat.span);
                    }
                }
            }
            PatternKind::Variant(None, name, args) if self.class(name).is_some() => {
                if !self.subst.unify(ty, &Ty::Named(name.clone())) {
                    self.error(Code::type_mismatch, pat.span);
                    return;
                }
                self.class_pattern(name, args, pat.span, binds);
            }
            PatternKind::Variant(qualifier, name, args) => {
                let enum_name = match (qualifier, self.subst.shallow(ty)) {
                    (Some(q), t) => {
                        if !self.subst.unify(&t, &Ty::Named(q.clone())) {
                            self.error(Code::type_mismatch, pat.span);
                            return;
                        }
                        q.clone()
                    }
                    (None, Ty::Named(e)) => e,
                    (None, Ty::Error) => return,
                    (None, Ty::Var(_)) => {
                        self.error(Code::type_needed, pat.span);
                        return;
                    }
                    (None, _) => {
                        self.error(Code::type_mismatch, pat.span);
                        return;
                    }
                };
                let Some(fields) = self
                    .variant(&enum_name, name)
                    .map(|v| v.fields.iter().map(|f| f.ty.clone()).collect::<Vec<_>>())
                else {
                    self.error(Code::unknown_name, pat.span);
                    return;
                };
                if fields.len() != args.len() {
                    self.error(Code::wrong_arguments, pat.span);
                    return;
                }
                for (arg, field) in args.iter().zip(&fields) {
                    self.pattern(arg, field, binds);
                }
            }
            PatternKind::Or(alternatives) => {
                let mut first: Option<Vec<Binding>> = None;
                for alt in alternatives {
                    let mut alt_binds = Vec::new();
                    self.pattern(alt, ty, &mut alt_binds);
                    match &first {
                        None => first = Some(alt_binds),
                        Some(expected) => {
                            let same = expected.len() == alt_binds.len()
                                && expected.iter().all(|(n, _, t)| {
                                    alt_binds
                                        .iter()
                                        .find(|b| &b.0 == n)
                                        .is_some_and(|b| self.subst.unify(t, &b.2))
                                });
                            if !same {
                                self.error(Code::or_pattern_bindings, alt.span);
                            }
                        }
                    }
                }
                for bind in first.unwrap_or_default() {
                    if binds.iter().any(|b| b.0 == bind.0) {
                        self.error(Code::duplicate_name, pat.span);
                    }
                    binds.push(bind);
                }
            }
            PatternKind::Record(items) => match self.subst.shallow(ty) {
                Ty::Record(_, fields) => self.positional_patterns(items, &fields, pat.span, binds),
                Ty::Named(class) if self.rest_field(&class).is_some() => {
                    self.error(Code::wrong_arguments, pat.span);
                }
                Ty::Named(class) if self.class(&class).is_some() => {
                    self.class_pattern(&class, items, pat.span, binds);
                }
                Ty::Error => {}
                Ty::Var(_) => {
                    self.error(Code::type_needed, pat.span);
                }
                _ => {
                    self.error(Code::type_mismatch, pat.span);
                }
            },
            PatternKind::List(items) => {
                let elem = self.subst.fresh();
                if !self.subst.unify(ty, &Ty::list(elem.clone())) {
                    self.error(Code::type_mismatch, pat.span);
                    return;
                }
                self.list_pattern(items, &elem, binds);
            }
            PatternKind::Rest(_) => {
                self.error(Code::wrong_arguments, pat.span);
            }
        }
    }

    fn class_pattern(
        &mut self,
        class: &str,
        items: &[Pattern],
        span: Span,
        binds: &mut Vec<Binding>,
    ) {
        let fields = self
            .class(class)
            .map(|c| c.fields.clone())
            .unwrap_or_default();
        let tys: Vec<Ty> = fields.iter().map(|f| f.ty.clone()).collect();
        match fields.iter().position(|f| f.is_rest) {
            Some(r) if items.len() >= r => {
                self.positional_patterns(&items[..r], &tys[..r], span, binds);
                self.list_pattern(&items[r..], &tys[r], binds);
            }
            Some(_) => {
                self.error(Code::wrong_arguments, span);
            }
            None => self.positional_patterns(items, &tys, span, binds),
        }
    }

    fn positional_patterns(
        &mut self,
        items: &[Pattern],
        tys: &[Ty],
        span: Span,
        binds: &mut Vec<Binding>,
    ) {
        if items.len() != tys.len() {
            self.error(Code::wrong_arguments, span);
            return;
        }
        for (item, ty) in items.iter().zip(tys) {
            self.pattern(item, ty, binds);
        }
    }

    fn list_pattern(&mut self, items: &[Pattern], elem: &Ty, binds: &mut Vec<Binding>) {
        for item in items {
            match &item.kind {
                PatternKind::Rest(name) if name == "_" => {}
                PatternKind::Rest(name) if binds.iter().any(|b| &b.0 == name) => {
                    self.error(Code::duplicate_name, item.span);
                }
                PatternKind::Rest(name) => {
                    binds.push((name.clone(), item.span, Ty::list(elem.clone())));
                }
                _ => self.pattern(item, elem, binds),
            }
        }
    }

    fn rest_field(&self, class: &str) -> Option<usize> {
        self.class(class)?.fields.iter().position(|f| f.is_rest)
    }

    fn constructors(&self, ty: &Ty) -> Option<Vec<(String, Vec<Ty>)>> {
        match self.subst.shallow(ty) {
            Ty::Bool => Some(vec![
                ("true".to_string(), Vec::new()),
                ("false".to_string(), Vec::new()),
            ]),
            Ty::Maybe(inner) => Some(vec![
                ("Some".to_string(), vec![*inner]),
                ("None".to_string(), Vec::new()),
            ]),
            Ty::Record(_, fields) => Some(vec![(String::new(), fields)]),
            Ty::Named(e) => match self.types.get(&e) {
                Some(TypeDef::Enum(vs)) => Some(
                    vs.iter()
                        .map(|v| {
                            (
                                v.name.clone(),
                                v.fields.iter().map(|f| f.ty.clone()).collect(),
                            )
                        })
                        .collect(),
                ),
                Some(TypeDef::Class(c)) => {
                    let mut fields = Vec::new();
                    for f in &c.fields {
                        if f.is_rest {
                            fields.push(Ty::list(f.ty.clone()));
                            break;
                        }
                        fields.push(f.ty.clone());
                    }
                    Some(vec![(e, fields)])
                }
                None => None,
            },
            _ => None,
        }
    }

    fn irrefutable(&self, pat: &Pattern) -> bool {
        match &pat.kind {
            PatternKind::Wildcard => true,
            PatternKind::Name(_) => !self.info.variant_patterns.contains(&pat.id),
            PatternKind::Record(items) => items.iter().all(|p| self.irrefutable(p)),
            PatternKind::Variant(None, name, items) if self.class(name).is_some() => self
                .class_cells(name, items)
                .iter()
                .all(|c| self.irrefutable_cell(*c)),
            _ => false,
        }
    }

    fn irrefutable_cell(&self, cell: Cell) -> bool {
        match cell {
            Cell::Pat(p) => self.irrefutable(p),
            Cell::Items(items) => matches!(items, [p] if matches!(p.kind, PatternKind::Rest(_))),
        }
    }

    fn class_cells<'p>(&self, class: &str, items: &'p [Pattern]) -> Vec<Cell<'p>> {
        match self.rest_field(class) {
            Some(r) if items.len() >= r => {
                let mut cells: Vec<Cell> = items[..r].iter().map(Cell::Pat).collect();
                cells.push(Cell::Items(&items[r..]));
                cells
            }
            _ => items.iter().map(Cell::Pat).collect(),
        }
    }

    fn specialize<'p>(&self, head: Cell<'p>, name: &str, arity: usize) -> Option<Vec<Cell<'p>>> {
        if self.irrefutable_cell(head) {
            return Some(vec![Cell::Pat(&WILDCARD); arity]);
        }
        let Cell::Pat(p) = head else {
            return None;
        };
        let cells = match &p.kind {
            PatternKind::Variant(_, n, args) if *n == name => self.class_cells(n, args),
            PatternKind::Record(items) => items.iter().map(Cell::Pat).collect(),
            PatternKind::Name(n) if *n == name => Vec::new(),
            PatternKind::Literal(lit) if matches!(lit.kind, ExprKind::Bool(b) if b.to_string() == name) => {
                Vec::new()
            }
            _ => return None,
        };
        if cells.len() != arity {
            return Some(vec![Cell::Pat(&WILDCARD); arity]);
        }
        Some(cells)
    }

    fn head_cell(p: &Pattern) -> Cell<'_> {
        match &p.kind {
            PatternKind::List(items) => Cell::Items(items),
            _ => Cell::Pat(p),
        }
    }

    fn report_missing(&mut self, rows: Vec<Vec<Cell>>, ty: &Ty, span: Span) {
        if let Some(witness) = self.missing_case(rows, std::slice::from_ref(ty)) {
            let mut d = Diagnostic::error(Code::non_exhaustive_match, span);
            d.detail = witness.into_iter().next();
            self.diags.push(d);
        }
    }

    fn missing_case(&self, rows: Vec<Vec<Cell>>, tys: &[Ty]) -> Option<Vec<String>> {
        let Some((ty, rest_tys)) = tys.split_first() else {
            return rows.is_empty().then(Vec::new);
        };
        let mut expanded = Vec::new();
        for row in rows {
            match row[0] {
                Cell::Pat(Pattern {
                    kind: PatternKind::Or(alts),
                    ..
                }) => {
                    for alt in alts {
                        let mut r = vec![Self::head_cell(alt)];
                        r.extend(&row[1..]);
                        expanded.push(r);
                    }
                }
                Cell::Pat(p) => {
                    let mut r = vec![Self::head_cell(p)];
                    r.extend(&row[1..]);
                    expanded.push(r);
                }
                _ => expanded.push(row),
            }
        }
        match self.subst.shallow(ty) {
            Ty::Error => return None,
            Ty::List(elem) => return self.missing_list_case(expanded, &elem, rest_tys),
            _ => {}
        }
        let Some(ctors) = self.constructors(ty) else {
            let rest = expanded
                .into_iter()
                .filter(|r| self.irrefutable_cell(r[0]))
                .map(|r| r[1..].to_vec())
                .collect();
            return self.missing_case(rest, rest_tys).map(|mut w| {
                w.insert(0, "_".to_string());
                w
            });
        };
        for (name, fields) in ctors {
            let arity = fields.len();
            let specialized = expanded
                .iter()
                .filter_map(|row| {
                    let mut sub = self.specialize(row[0], &name, arity)?;
                    sub.extend(&row[1..]);
                    Some(sub)
                })
                .collect();
            let mut sub_tys = fields;
            sub_tys.extend_from_slice(rest_tys);
            if let Some(mut w) = self.missing_case(specialized, &sub_tys) {
                let rest = w.split_off(arity);
                let head = if arity == 0 {
                    name
                } else {
                    format!("{name}({})", w.join(", "))
                };
                let mut out = vec![head];
                out.extend(rest);
                return Some(out);
            }
        }
        None
    }

    fn missing_list_case(
        &self,
        rows: Vec<Vec<Cell>>,
        elem: &Ty,
        rest_tys: &[Ty],
    ) -> Option<Vec<String>> {
        let fixed_len = |items: &[Pattern]| {
            items
                .iter()
                .filter(|p| !matches!(p.kind, PatternKind::Rest(_)))
                .count()
        };
        let longest = rows
            .iter()
            .filter_map(|r| match r[0] {
                Cell::Items(items) => Some(fixed_len(items)),
                Cell::Pat(_) => None,
            })
            .max()
            .unwrap_or(0);
        for len in 0..=longest + 1 {
            let specialized = rows
                .iter()
                .filter_map(|row| {
                    let mut sub: Vec<Cell> = match row[0] {
                        head if self.irrefutable_cell(head) => vec![Cell::Pat(&WILDCARD); len],
                        Cell::Items(items) => {
                            let fixed = fixed_len(items);
                            match items
                                .iter()
                                .position(|p| matches!(p.kind, PatternKind::Rest(_)))
                            {
                                Some(k) if len >= fixed => {
                                    let mut sub: Vec<Cell> =
                                        items[..k].iter().map(Cell::Pat).collect();
                                    sub.extend(vec![Cell::Pat(&WILDCARD); len - fixed]);
                                    sub.extend(items[k + 1..].iter().map(Cell::Pat));
                                    sub
                                }
                                None if items.len() == len => items.iter().map(Cell::Pat).collect(),
                                _ => return None,
                            }
                        }
                        Cell::Pat(_) => return None,
                    };
                    sub.extend(&row[1..]);
                    Some(sub)
                })
                .collect();
            let mut sub_tys = vec![elem.clone(); len];
            sub_tys.extend_from_slice(rest_tys);
            if let Some(mut w) = self.missing_case(specialized, &sub_tys) {
                let rest = w.split_off(len);
                if len > longest {
                    w.push("...".to_string());
                }
                let mut out = vec![format!("[{}]", w.join(", "))];
                out.extend(rest);
                return Some(out);
            }
        }
        None
    }

    fn optional_value(&mut self, expr: &Expr, body: Ty) -> Ty {
        match self.subst.shallow(&body) {
            Ty::Void | Ty::Never | Ty::Error => Ty::Void,
            t => {
                if matches!(t, Ty::Maybe(_)) {
                    self.warn(Code::nested_maybe, expr.span);
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
        self.unused(body);
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

    fn for_loop(&mut self, id: NodeId, pattern: &Pattern, iter: &Expr, body: &Expr) -> Ty {
        let it = self.expr(iter, None);
        let elem = match self.subst.shallow(&it) {
            Ty::Range => Ty::Int,
            Ty::List(e) => *e,
            Ty::Error => Ty::Error,
            _ => self.error(Code::not_iterable, iter.span),
        };
        let mut binds = Vec::new();
        self.pattern(pattern, &elem, &mut binds);
        self.report_missing(vec![vec![Cell::Pat(pattern)]], &elem, pattern.span);
        let narrowed_list = match &pattern.kind {
            PatternKind::Name(name) => narrowing_list(iter)
                .filter(|list| !mutates(body, list))
                .map(|list| (list, name.clone())),
            _ => None,
        };
        if let Some(narrowed) = &narrowed_list {
            self.narrowed.push(narrowed.clone());
        }
        self.scopes.push(
            binds
                .into_iter()
                .map(|(name, span, ty)| (name, Var::binding(ty, span)))
                .collect(),
        );
        let ty = self.valued_loop(id, body);
        self.scopes.pop();
        if narrowed_list.is_some() {
            self.narrowed.pop();
        }
        ty
    }
}

fn record_field(names: &[String], fields: &[Ty], name: &str) -> Option<Ty> {
    let i = if names.is_empty() {
        name.parse().ok()?
    } else {
        names.iter().position(|n| n == name)?
    };
    fields.get(i).cloned()
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
            Stmt::LetPattern { value, .. } => mutates(value, list),
            Stmt::Func(_) | Stmt::Class(_) | Stmt::Enum(_) | Stmt::Implement(_) => false,
        }),
        ExprKind::Str(segs) => segs
            .iter()
            .any(|s| matches!(s, StrSeg::Expr(e) if mutates(e, list))),
        ExprKind::List(items) => items.iter().any(|e| mutates(e, list)),
        ExprKind::Record(args) => args.iter().any(|a| mutates(&a.value, list)),
        ExprKind::Unary(_, e)
        | ExprKind::Loop(e)
        | ExprKind::Lambda(_, _, e)
        | ExprKind::Field(e, _)
        | ExprKind::Is(e, _)
        | ExprKind::Spread(e) => mutates(e, list),
        ExprKind::Match(s, arms) => {
            mutates(s, list)
                || arms.iter().any(|a| {
                    mutates(&a.body, list) || a.guard.as_ref().is_some_and(|g| mutates(g, list))
                })
        }
        ExprKind::Binary(_, a, b)
        | ExprKind::Index(a, b)
        | ExprKind::While(a, b)
        | ExprKind::For(_, a, b) => mutates(a, list) || mutates(b, list),
        ExprKind::Call(f, args) => mutates(f, list) || args.iter().any(|a| mutates(&a.value, list)),
        ExprKind::If(c, t, o) => {
            mutates(c, list) || mutates(t, list) || o.as_ref().is_some_and(|o| mutates(o, list))
        }
        ExprKind::Break(v) | ExprKind::Return(v) => v.as_ref().is_some_and(|v| mutates(v, list)),
        _ => false,
    }
}
