use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Code, Diagnostic, Edit, Sources, Span};
use crate::types::{Show, Subst, Ty};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Coercion {
    pub to_float: bool,
    pub maybe_to_float: bool,
    pub wraps: usize,
    /// Wraps the value in `Ok` after any `Some`s, for a function that infers a `Result`.
    pub ok: bool,
}

#[derive(Debug, Clone)]
pub enum Resolved {
    Construct(String),
    Variant(String, String),
    Const(String),
    Static(String),
    Method(String),
    /// A method found at run time from the receiver's type, for interface and `T` receivers.
    Dynamic(String),
}

#[derive(Default)]
pub struct Info {
    pub coercions: HashMap<NodeId, Coercion>,
    pub try_unwraps: HashMap<NodeId, Span>,
    pub plain_tries: HashSet<NodeId>,
    pub coalesce_to_maybe: HashSet<NodeId>,
    pub wrapping: HashSet<NodeId>,
    pub signatures: HashMap<NodeId, String>,
    /// Each lambda's captured variables: what its body, or a lambda inside it, reads from outside.
    pub captures: HashMap<NodeId, Vec<String>>,
    pub unwrapped_index: HashSet<NodeId>,
    pub resolved: HashMap<NodeId, Resolved>,
    pub variant_patterns: HashSet<NodeId>,
    /// `Circle.describe` to the interface default it runs, `Shape.describe`.
    pub aliases: HashMap<String, String>,
    /// Each type's interfaces, their parents included.
    pub implements: HashMap<String, HashSet<String>>,
    /// Each `ref` binding, by its target, and how many statements after it its last use is.
    pub ref_ends: HashMap<NodeId, usize>,
    /// Methods whose body changes `this`, so calls write it back, with where the body starts.
    pub changes_this: HashMap<String, Span>,
    /// Arguments passed to a `ref` parameter, which the call writes back.
    pub ref_args: HashMap<NodeId, Span>,
    /// Shared names passed to a `shared` parameter or field, which pass their box.
    pub shared_args: HashMap<NodeId, Span>,
}

#[derive(Clone)]
struct ParamSig {
    name: String,
    ty: Ty,
    has_default: bool,
    is_rest: bool,
    is_ref: bool,
    sharing: Sharing,
}

/// What a `shared` parameter or field takes: a field also takes a plain value, in a new box.
#[derive(Clone, Copy, PartialEq)]
enum Sharing {
    No,
    Param,
    Field,
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
    changes_this: bool,
    owner: Option<String>,
    library: bool,
    /// The type parameters it's generic over, its owner's included, and their bounds.
    bounds: HashMap<String, Vec<Ty>>,
    renamed: HashMap<String, String>,
}

struct InterfaceDef {
    params: Vec<String>,
    parents: Vec<Ty>,
    /// Name, type and whether it's a `var`.
    fields: Vec<(String, Ty, bool)>,
    /// Name, signature and whether it has a default body.
    methods: Vec<(String, Sig, bool)>,
}

struct ClassDef {
    is_shared: bool,
    fields: Vec<ParamSig>,
    mutable: Vec<bool>,
    private: Vec<bool>,
    readonly: Vec<bool>,
}

struct FieldInfo {
    ty: Ty,
    mutable: bool,
    /// Its class is a `shared class`.
    is_shared: bool,
    /// It's a `shared` field, held in a box that copies of the object share.
    boxed: bool,
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
    /// The type a member const belongs to, or `None` for a data file.
    owner: Option<String>,
    /// For a data file, whether it's a `.con` file, which holds only literals.
    pure: bool,
    ty: Ty,
    state: FuncState,
}

/// A name a pattern binds, where, its type and whether it's marked `var`.
type Binding = (String, Span, Ty, bool);

/// A type's own method key, with each interface's signature for it and what that
/// interface's parameters stand for.
type OwnMethod = (String, Vec<(Sig, HashMap<String, Ty>)>);

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
    is_var: false,
};

#[derive(Clone, Copy, PartialEq)]
enum VarKind {
    Local,
    Param,
    /// A `shared` local or parameter: its value is in a box, so writes go through.
    Shared,
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

/// What the `try`s in a function pass up.
#[derive(Clone)]
enum Fails {
    Maybe,
    /// A `Result`, with its error type.
    Result(Ty),
}

struct FnCtx {
    declared: Option<Ty>,
    fails: Option<Fails>,
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
    break_flow: Option<Flow>,
}

#[derive(Clone, Default)]
struct Flow {
    maybe_unset: HashSet<Span>,
    maybe_set: HashSet<Span>,
    dead: bool,
}

impl Flow {
    fn join(self, other: Flow) -> Flow {
        if self.dead {
            return other;
        }
        if other.dead {
            return self;
        }
        Flow {
            maybe_unset: &self.maybe_unset | &other.maybe_unset,
            maybe_set: &self.maybe_set | &other.maybe_set,
            dead: false,
        }
    }
}

pub struct Checker<'s> {
    sources: &'s Sources,
    subst: Subst,
    pub diags: Vec<Diagnostic>,
    pub info: Info,
    scopes: Vec<HashMap<String, Var>>,
    funcs: HashMap<String, FuncEntry>,
    fns: Vec<FnCtx>,
    loops: Vec<LoopCtx>,
    flow: Flow,
    in_try: bool,
    try_points: usize,
    lets_without_value: HashMap<Span, usize>,
    /// The lambdas being checked, innermost last: each one's first scope (variables below it are
    /// captured) and what it has captured so far.
    lambdas: Vec<(usize, BTreeSet<String>)>,
    /// Collection and key names whose index can't miss, and the innermost scope they
    /// may come from, so a shadowing name isn't narrowed.
    narrowed: Vec<(String, String, usize)>,
    /// Names of functions and methods with a `ref` parameter, for checks that run before calls
    /// are resolved.
    ref_callees: HashSet<String>,
    /// Field reads of a `shared` field.
    boxed_fields: HashSet<NodeId>,
    top_level_names: HashSet<String>,
    lambda_types: Vec<(NodeId, Ty)>,
    type_names: HashSet<String>,
    types: HashMap<String, TypeDef>,
    consts: HashMap<String, ConstEntry>,
    current_type: Option<String>,
    is_bindings: HashMap<NodeId, Vec<Binding>>,
    defaults: Vec<(Expr, Ty, Option<String>)>,
    /// The `var` keyword of each local declared with one, by its name's start.
    var_keywords: HashMap<usize, Span>,
    in_library: bool,
    /// Each generic type's parameter names.
    generics: HashMap<String, Vec<String>>,
    interfaces: HashMap<String, InterfaceDef>,
    /// The interfaces each type names, in terms of its own parameters.
    impls: HashMap<String, Vec<Ty>>,
    /// The type parameters in scope and their bounds.
    bounds: HashMap<String, Vec<Ty>>,
    /// Each generic class's and enum's parameter bounds, checked where a value is made.
    type_bounds: HashMap<String, HashMap<String, Vec<Ty>>>,
    /// A call's type arguments and a bound each must meet, if any, checked once inference
    /// is done.
    pending_bounds: Vec<(Ty, Option<Ty>, Span)>,
    /// Where each type first names an interface, for errors about its implementation.
    impl_spans: HashMap<String, Span>,
    /// Parameter names an `implement` uses for its type's own (`U` for `T`).
    renamed: HashMap<String, String>,
    current_method: Option<String>,
    /// Calls on `this` to a method still being checked, as (caller, callee): the caller changes
    /// `this` as soon as the callee does.
    pending_changes: Vec<(String, String)>,
    /// Set while checking a write path: whether it went into a `Shared` or `shared class`.
    through_shared: bool,
    /// Set while checking a write path: its variable, when that is a `var`.
    place_root: Option<String>,
}

pub(crate) const RESERVED_NAMES: &[&str] = &["Maybe", "Some", "None", "Result", "Ok", "Err"];
const MUTATING: &[&str] = &["push", "pop", "removeAt", "add", "remove"];
pub(crate) const BUILT_IN_TYPES: &[&str] = &["String", "Int", "Float", "Bool"];
pub(crate) const MAYBE: &str = "check.std.Maybe";
pub(crate) const MAP: &str = "Map";
pub(crate) const SET: &str = "Set";
pub(crate) const SHARED: &str = "Shared";
/// The bound a map key or set item's type needs; not a real interface.
const KEY: &str = "check.std.Key";
pub(crate) const RESULT: &str = "check.std.Result";
pub(crate) const PRINTABLE: &str = "check.std.Printable";
pub(crate) const EQ: &str = "check.std.Eq";

pub fn check(program: &[Stmt], library: &[Stmt], sources: &Sources) -> (Info, Vec<Diagnostic>) {
    let mut checker = Checker {
        sources,
        subst: Subst::default(),
        diags: Vec::new(),
        info: Info::default(),
        scopes: vec![HashMap::new()],
        funcs: HashMap::new(),
        fns: Vec::new(),
        loops: Vec::new(),
        flow: Flow::default(),
        in_try: false,
        try_points: 0,
        lets_without_value: HashMap::new(),
        lambdas: Vec::new(),
        narrowed: Vec::new(),
        ref_callees: HashSet::new(),
        boxed_fields: HashSet::new(),
        top_level_names: HashSet::new(),
        lambda_types: Vec::new(),
        type_names: HashSet::new(),
        types: HashMap::new(),
        consts: HashMap::new(),
        current_type: None,
        is_bindings: HashMap::new(),
        defaults: Vec::new(),
        var_keywords: HashMap::new(),
        in_library: false,
        generics: HashMap::new(),
        interfaces: HashMap::new(),
        impls: HashMap::new(),
        bounds: HashMap::new(),
        type_bounds: HashMap::new(),
        pending_bounds: Vec::new(),
        impl_spans: HashMap::new(),
        renamed: HashMap::new(),
        current_method: None,
        pending_changes: Vec::new(),
        through_shared: false,
        place_root: None,
    };
    checker.program(program, library);
    (checker.info, checker.diags)
}

impl Checker<'_> {
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
        for stmt in library.iter().chain(program) {
            let (name, span, params) = match stmt {
                Stmt::Class(c) => (&c.name, c.span, &c.type_params),
                Stmt::Enum(e) => (&e.name, e.span, &e.type_params),
                Stmt::Interface(i) => (&i.name, i.span, &i.type_params),
                _ => continue,
            };
            if RESERVED_NAMES.contains(&name.as_str())
                || BUILT_IN_TYPES.contains(&name.as_str())
                || [MAP, SET].contains(&name.as_str())
            {
                self.error(Code::redefined_builtin, span);
            } else if !self.type_names.insert(name.clone()) {
                self.error(Code::duplicate_name, span);
            }
            let names = params.iter().map(|p| p.name.clone()).collect();
            self.generics.insert(name.clone(), names);
        }
        for stmt in library.iter().chain(program) {
            if let Stmt::Interface(i) = stmt {
                self.declare_interface(i);
            }
        }
        for stmt in library.iter().chain(program) {
            match stmt {
                Stmt::Class(c) => self.declare_class(c),
                Stmt::Enum(e) if e.name != MAYBE => self.declare_enum(e),
                _ => {}
            }
        }
        for stmt in program {
            match stmt {
                Stmt::Func(decl) => {
                    if self.type_names.contains(&decl.name) || self.consts.contains_key(&decl.name)
                    {
                        self.error(Code::duplicate_name, decl.span);
                    }
                    self.declare_func(decl, None, None);
                }
                Stmt::Let { name, .. } => {
                    self.top_level_names.insert(name.clone());
                }
                Stmt::Implement(i) => self.declare_implement(i),
                Stmt::Data(d) => self.declare_data(d),
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
        // Defaults are linked before any body is checked, since bodies can call them.
        let own_methods = self.check_impls();
        for (default, ty, owner) in std::mem::take(&mut self.defaults) {
            self.current_type = owner;
            let actual = self.expr(&default, Some(&ty));
            self.subsume(default.id, &actual, &ty);
        }
        self.current_type = None;
        let interface_methods = self.check_signatures(own_methods);
        self.ref_callees = (self.funcs.iter())
            .filter(|(_, f)| f.sig.params.iter().any(|p| p.is_ref))
            .map(|(key, _)| key.rsplit('.').next().unwrap_or(key).to_string())
            .collect();
        for (i, stmt) in program.iter().enumerate() {
            self.ref_binding(stmt, &program[i + 1..]);
            self.stmt(stmt);
        }
        self.check_bounds(0);
        let names: Vec<String> = self.funcs.keys().cloned().collect();
        for name in names {
            self.ensure_checked(&name, Span::default());
        }
        self.report_changes(interface_methods);
        self.changed_copies(program);
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

    /// Brings a declaration's type parameters into scope, returning the ones before.
    fn enter_params(&mut self, params: &[TypeParam]) -> HashMap<String, Vec<Ty>> {
        let saved = self.bounds.clone();
        for p in params {
            self.bounds.insert(p.name.clone(), Vec::new());
        }
        for p in params {
            let bounds = p.bounds.iter().map(|b| self.type_from(b)).collect();
            self.bounds.insert(p.name.clone(), bounds);
        }
        saved
    }

    /// The type a generic type's own declaration sees: `Box<T>` inside `Box`.
    fn own_type(&self, name: &str) -> Ty {
        let params = self.generics.get(name).cloned().unwrap_or_default();
        Ty::Named(
            name.to_string(),
            params.into_iter().map(Ty::Param).collect(),
        )
    }

    /// The type `name` with a fresh type variable for each parameter.
    fn fresh_type(&mut self, name: &str) -> Ty {
        let params = self.generics.get(name).cloned().unwrap_or_default();
        let args = params.iter().map(|_| self.subst.fresh()).collect();
        Ty::Named(name.to_string(), args)
    }

    /// What each of type `name`'s parameters stands for in `args`.
    fn args_map(&self, name: &str, args: &[Ty]) -> HashMap<String, Ty> {
        let params = self.generics.get(name).cloned().unwrap_or_default();
        params.into_iter().zip(args.iter().cloned()).collect()
    }

    fn add_impls(&mut self, owner: &str, interfaces: &[TypeExpr]) {
        for t in interfaces {
            let ty = self.type_from(t);
            let span = type_span(t);
            match &ty {
                Ty::Named(n, _) if self.interfaces.contains_key(n) => {}
                Ty::Error => continue,
                _ => {
                    self.error(Code::not_an_interface, span);
                    continue;
                }
            }
            self.impl_spans.entry(owner.to_string()).or_insert(span);
            let list = self.impls.entry(owner.to_string()).or_default();
            if list.contains(&ty) {
                self.error(Code::duplicate_name, span);
            } else {
                list.push(ty);
            }
        }
    }

    fn declare_interface(&mut self, i: &InterfaceDecl) {
        let saved = self.enter_params(&i.type_params);
        let parents = i.parents.iter().map(|p| self.type_from(p)).collect();
        let fields = i
            .fields
            .iter()
            .map(|f| {
                if f.default.is_some() || f.is_private || f.is_readonly {
                    self.error(Code::interface_field_extras, f.span);
                }
                let ty = match &f.ty {
                    Some(t) => self.type_from(t),
                    None => self.error(Code::type_needed, f.span),
                };
                (f.name.clone(), ty, f.is_var)
            })
            .collect();
        self.check_unique(i.fields.iter().map(|f| (&f.name, f.span)));
        let this = self.own_type(&i.name);
        let mut methods = Vec::new();
        for m in &i.members.methods {
            let short = m.name.rsplit('.').next().unwrap_or(&m.name).to_string();
            let inner = self.enter_params(&m.type_params);
            let params = self.param_sigs(&m.params);
            let ret = Some(m.ret.as_ref().map_or(Ty::Void, |t| self.type_from(t)));
            self.bounds = inner;
            methods.push((short, Sig { params, ret }, m.body.is_some()));
            if m.body.is_some() {
                self.declare_func(m, Some((this.clone(), false)), Some(&i.name));
            }
        }
        if !i.members.statics.is_empty() || !i.members.consts.is_empty() {
            self.error(Code::unexpected_token, i.span);
        }
        self.bounds = saved;
        self.interfaces.insert(
            i.name.clone(),
            InterfaceDef {
                params: i.type_params.iter().map(|p| p.name.clone()).collect(),
                parents,
                fields,
                methods,
            },
        );
    }

    fn declare_class(&mut self, c: &ClassDecl) {
        let saved = self.enter_params(&c.type_params);
        self.current_type = Some(c.name.clone());
        let mut fields = self.param_sigs(&c.fields);
        for f in &mut fields {
            if f.sharing == Sharing::Param {
                f.sharing = Sharing::Field;
            }
        }
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
                is_shared: c.is_shared,
                fields,
                mutable,
                private: c.fields.iter().map(|f| f.is_private).collect(),
                readonly: c.fields.iter().map(|f| f.is_readonly).collect(),
            }),
        );
        let this = self.own_type(&c.name);
        if let Some(init) = &c.init {
            self.declare_func(init, Some((this.clone(), true)), Some(&c.name));
            // Building an object can't fail, so `init` returns nothing and `try` can't pass a failure up.
            if let Some(f) = self.funcs.get_mut(&init.name) {
                f.sig.ret = Some(Ty::Void);
            }
        }
        self.declare_members(&c.name, this, &c.members);
        self.add_impls(&c.name, &c.interfaces);
        self.save_type_bounds(&c.name, &c.type_params, saved);
    }

    fn declare_implement(&mut self, i: &ImplementDecl) {
        let arity = self.generics.get(&i.target).map_or(0, Vec::len);
        let known = BUILT_IN_TYPES.contains(&i.target.as_str())
            || (self.type_names.contains(&i.target) && !self.interfaces.contains_key(&i.target));
        if !known {
            self.error(Code::unknown_name, i.span);
            return;
        }
        if !i.target_args.is_empty() && i.target_args.len() != arity {
            self.error(Code::wrong_type_arguments, i.span);
            return;
        }
        // A name in `Box<U>` that isn't a type introduces a parameter, which stands for the
        // type's own (`T`) so every implementation reads alike.
        let saved = (self.bounds.clone(), self.renamed.clone());
        let own = self.generics.get(&i.target).cloned().unwrap_or_default();
        let mut whole = true;
        for (a, p) in i.target_args.iter().zip(&own) {
            match a {
                TypeExpr::Name(n, _)
                    if !self.type_names.contains(n)
                        && !BUILT_IN_TYPES.contains(&n.as_str())
                        && !self.renamed.contains_key(n) =>
                {
                    self.renamed.insert(n.clone(), p.clone());
                    self.bounds.insert(p.clone(), Vec::new());
                }
                _ => whole = false,
            }
        }
        let args: Vec<Ty> = i.target_args.iter().map(|a| self.type_from(a)).collect();
        let this = match i.target.as_str() {
            "String" => Ty::Str,
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Bool" => Ty::Bool,
            MAYBE => Ty::maybe(args.first().cloned().unwrap_or(Ty::Error)),
            name => Ty::Named(name.to_string(), args.clone()),
        };
        if let Some(iface) = &i.interface {
            if !whole || (i.target_args.is_empty() && arity > 0) {
                self.error(Code::implement_for_specific_arguments, i.span);
            } else {
                self.add_impls(&i.target, std::slice::from_ref(iface));
            }
        }
        if i.target_args.is_empty() && arity > 0 && !i.members.methods.is_empty() {
            self.error(Code::wrong_type_arguments, i.span);
        }
        self.declare_members(&i.target, this, &i.members);
        (self.bounds, self.renamed) = saved;
    }

    fn declare_enum(&mut self, e: &EnumDecl) {
        let saved = self.enter_params(&e.type_params);
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
                        is_ref: false,
                        sharing: Sharing::No,
                    })
                    .collect(),
            })
            .collect();
        self.types.insert(e.name.clone(), TypeDef::Enum(variants));
        let this = self.own_type(&e.name);
        self.declare_members(&e.name, this, &e.members);
        self.add_impls(&e.name, &e.interfaces);
        self.save_type_bounds(&e.name, &e.type_params, saved);
    }

    /// Keeps a type's parameter bounds, its fields' key bounds included, and restores `saved`.
    fn save_type_bounds(
        &mut self,
        name: &str,
        params: &[TypeParam],
        saved: HashMap<String, Vec<Ty>>,
    ) {
        let bounds = params
            .iter()
            .map(|p| (p.name.clone(), self.bounds[&p.name].clone()))
            .collect();
        self.type_bounds.insert(name.to_string(), bounds);
        self.bounds = saved;
    }

    /// Checks, once inference is done, that a value of `ty`, a generic type, meets its bounds.
    fn require_type_bounds(&mut self, ty: &Ty, span: Span) {
        let Ty::Named(name, args) = ty else {
            return;
        };
        let map = self.args_map(name, args);
        for (p, bounds) in self.type_bounds.get(name).cloned().unwrap_or_default() {
            for b in bounds {
                self.pending_bounds
                    .push((map[&p].clone(), Some(b.substitute(&map)), span));
            }
        }
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
                    owner: Some(owner.to_string()),
                    pure: false,
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
        let saved = self.enter_params(&decl.type_params);
        let params = self.param_sigs(&decl.params);
        let ret = decl.ret.as_ref().map(|t| self.type_from(t));
        let bounds = std::mem::replace(&mut self.bounds, saved);
        let entry = FuncEntry {
            decl: decl.clone(),
            sig: Sig { params, ret },
            state: FuncState::Unchecked,
            this,
            changes_this: false,
            owner: owner.map(str::to_string),
            library: self.in_library,
            bounds,
            renamed: self.renamed.clone(),
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
            if p.is_ref && p.default.is_some() {
                self.error(Code::ref_default, p.span);
            }
            if p.is_ref && self.is_shared_class(&ty) {
                self.warn(Code::ref_param_of_shared_class, p.span);
            }
            params.push(ParamSig {
                name: p.name.clone(),
                ty,
                has_default: p.default.is_some(),
                is_rest: p.is_rest,
                is_ref: p.is_ref,
                sharing: if p.is_shared {
                    Sharing::Param
                } else {
                    Sharing::No
                },
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
        let (decl, owner, pure) = (entry.decl.clone(), entry.owner.clone(), entry.pure);
        if let Some(owner) = &owner
            && let Some(bad) = self.non_constant(&decl.value, owner)
        {
            self.error(Code::const_not_constant, bad.span);
        }
        let saved_scopes = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
        let saved_type = std::mem::replace(&mut self.current_type, owner);
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
        // Runs after typing, which records what each call and name resolved to.
        if pure && let Some(bad) = self.non_literal(&decl.value) {
            self.error(Code::code_in_pure_data, bad.span);
        }
        let entry = self.consts.get_mut(key).expect("const");
        entry.ty = ty.clone();
        entry.state = FuncState::Done;
        ty
    }

    fn declare_data(&mut self, d: &DataDecl) {
        if self.funcs.contains_key(&d.name)
            || self.type_names.contains(&d.name)
            || self.consts.contains_key(&d.name)
        {
            self.error(Code::duplicate_name, d.span);
            return;
        }
        let decl = ConstDecl {
            name: d.name.clone(),
            span: d.span,
            ty: d.ty.clone(),
            value: d.value.clone(),
            is_private: d.is_private,
        };
        self.consts.insert(
            d.name.clone(),
            ConstEntry {
                decl,
                owner: None,
                pure: d.is_pure,
                ty: Ty::Error,
                state: FuncState::Unchecked,
            },
        );
    }

    /// The first part of a `.con` value that isn't a literal: only constructor and variant
    /// calls, record literals, variants, lists, maps, sets, strings, numbers and
    /// `Bool` are.
    fn non_literal<'e>(&self, e: &'e Expr) -> Option<&'e Expr> {
        let made = matches!(
            self.info.resolved.get(&e.id),
            Some(Resolved::Construct(_) | Resolved::Variant(..))
        );
        let args = |args: &'e [Arg]| args.iter().find_map(|a| self.non_literal(&a.value));
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) => None,
            ExprKind::Str(_) => None,
            ExprKind::Unary(UnOp::Neg, x)
                if matches!(x.kind, ExprKind::Int(_) | ExprKind::Float(_)) =>
            {
                None
            }
            ExprKind::List(items) => items.iter().find_map(|x| self.non_literal(x)),
            ExprKind::Map(entries) => entries
                .iter()
                .find_map(|(k, v)| self.non_literal(k).or_else(|| self.non_literal(v))),
            ExprKind::Record(a) => args(a),
            ExprKind::Call(callee, a)
                if made || matches!(&callee.kind, ExprKind::Name(n) if n == "Some" || n == SET) =>
            {
                args(a)
            }
            ExprKind::Method(_, _, a) if made => args(a),
            ExprKind::Name(n) if made || n == "None" => None,
            ExprKind::Field(..) if made => None,
            _ => Some(e),
        }
    }

    fn non_constant<'e>(&self, e: &'e Expr, owner: &str) -> Option<&'e Expr> {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) => None,
            ExprKind::Str(segs) => segs.iter().find_map(|s| match s {
                StrSeg::Expr(x) => self.non_constant(x, owner),
                StrSeg::Text(_) => None,
            }),
            ExprKind::List(items) => items.iter().find_map(|x| self.non_constant(x, owner)),
            ExprKind::Map(entries) => entries.iter().find_map(|(k, v)| {
                self.non_constant(k, owner)
                    .or_else(|| self.non_constant(v, owner))
            }),
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
        if let TypeExpr::Name(n, _) = t
            && let Some(p) = self.renamed.get(n)
        {
            return Ty::Param(p.clone());
        }
        match t {
            TypeExpr::Name(name, span) => match name.as_str() {
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "Bool" => Ty::Bool,
                "String" => Ty::Str,
                "Void" => Ty::Void,
                "Never" => Ty::Never,
                _ if self.bounds.contains_key(name) => Ty::Param(name.clone()),
                _ if self.generics.get(name).is_some_and(|g| !g.is_empty()) => {
                    self.error(Code::wrong_type_arguments, *span)
                }
                _ if self.type_names.contains(name) => Ty::named(name),
                _ => self.error(Code::unknown_name, *span),
            },
            TypeExpr::Generic(name, args, span) => {
                let args: Vec<Ty> = args.iter().map(|a| self.type_from(a)).collect();
                match (name.as_str(), args.as_slice()) {
                    ("List", [elem]) => Ty::list(elem.clone()),
                    (MAP, [k, _]) | (SET, [k]) => {
                        self.require_key(k, *span);
                        Ty::Named(name.clone(), args)
                    }
                    (MAYBE, [inner]) => Ty::maybe(inner.clone()),
                    (SHARED, [_]) => Ty::Named(name.clone(), args),
                    _ if !self.type_names.contains(name) => self.error(Code::unknown_name, *span),
                    _ if self.generics.get(name).map_or(0, Vec::len) != args.len() => {
                        self.error(Code::wrong_type_arguments, *span)
                    }
                    _ => Ty::Named(name.clone(), args),
                }
            }
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
        let saved_bounds = std::mem::replace(&mut self.bounds, entry.bounds.clone());
        let saved_renamed = std::mem::replace(&mut self.renamed, entry.renamed.clone());
        let saved_type = std::mem::replace(&mut self.current_type, entry.owner.clone());
        let saved_method = std::mem::replace(
            &mut self.current_method,
            this.is_some().then(|| name.to_string()),
        );
        let saved_scopes = self.scopes.split_off(1);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_flow = std::mem::take(&mut self.flow);
        let saved_try = std::mem::take(&mut self.in_try);
        let saved_lambdas = std::mem::take(&mut self.lambdas);
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
                    mutable: p.is_var || p.is_ref,
                    kind: if p.is_shared {
                        VarKind::Shared
                    } else {
                        VarKind::Param
                    },
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
        let bounds = std::mem::replace(&mut self.bounds, saved_bounds);
        self.renamed = saved_renamed;
        self.current_type = saved_type;
        self.current_method = saved_method;
        self.scopes.truncate(1);
        self.scopes.extend(saved_scopes);
        self.loops = saved_loops;
        self.flow = saved_flow;
        self.in_try = saved_try;
        self.lambdas = saved_lambdas;
        self.narrowed = saved_narrowed;
        let entry = self.funcs.get_mut(name)?;
        entry.sig.ret = Some(ret);
        entry.bounds = bounds;
        entry.state = FuncState::Done;
        Some(entry.sig.clone())
    }

    fn function_body(&mut self, body: &Expr, declared: Option<Ty>) -> Ty {
        let pending = self.pending_bounds.len();
        self.fns.push(FnCtx {
            declared: declared.clone(),
            fails: None,
            returns: Vec::new(),
        });
        let body_ty = self.expr(body, declared.as_ref());
        let ctx = self.fns.pop().expect("function context");
        let ret = match declared {
            Some(t) => t,
            None => {
                if let Some(Fails::Result(e)) = ctx.fails {
                    let ret = self.ok_wrapped(body, body_ty, ctx.returns, e);
                    self.check_bounds(pending);
                    return ret;
                }
                let base = match self.subst.shallow(&body_ty) {
                    Ty::Never => ctx
                        .returns
                        .first()
                        .map_or(Ty::Never, |r| self.subst.shallow(&r.2)),
                    other => other,
                };
                if ctx.fails.is_some() && !matches!(self.subst.shallow(&base), Ty::Maybe(_)) {
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
        self.check_bounds(pending);
        ret
    }

    /// The `Result<T, E>` a function without `->` returns when its `try`s pass up `E`s: each
    /// value it ends with or returns is wrapped in `Ok`, unless it's already a `Result`.
    fn ok_wrapped(
        &mut self,
        body: &Expr,
        body_ty: Ty,
        returns: Vec<(NodeId, Span, Ty)>,
        e: Ty,
    ) -> Ty {
        let ok = self.subst.fresh();
        let ret = Ty::Named(RESULT.to_string(), vec![ok.clone(), e]);
        let tail = match &body.kind {
            ExprKind::Block(stmts) => match stmts.last() {
                Some(Stmt::Expr(e)) => e.span,
                _ => body.span,
            },
            _ => body.span,
        };
        for (id, span, ty) in std::iter::once((body.id, tail, body_ty)).chain(returns) {
            let ty = self.subst.shallow(&ty);
            let fits = match &ty {
                Ty::Never => true,
                Ty::Named(n, _) if n == RESULT => {
                    if !self.subsume(id, &ty, &ret) {
                        self.error(Code::try_error_mismatch, span);
                    }
                    true
                }
                _ => {
                    let fits = self.subsume(id, &ty, &ok);
                    self.info.coercions.entry(id).or_default().ok = true;
                    fits
                }
            };
            if !fits {
                let code = self.mismatch(&ty, &ret, Code::return_type_mismatch);
                self.error(code, span);
            }
        }
        ret
    }

    /// Checks the bounds calls added since `from`, now that their types are known.
    fn check_bounds(&mut self, from: usize) {
        let mut reported = HashSet::new();
        for (ty, bound, span) in self.pending_bounds.split_off(from) {
            let ty = self.resolve(&ty);
            let is_key = bound == Some(Ty::named(KEY));
            if matches!(ty, Ty::Var(_)) {
                // An empty map or set whose types are never used, like an empty list.
                if !is_key && reported.insert(span.start) {
                    self.error(Code::type_needed, span);
                }
            } else if let Some(bound) = bound
                && !self.implements(&ty, &bound)
            {
                if !is_key {
                    self.error(Code::unmet_constraint, span);
                } else if reported.insert(span.start) {
                    self.error(Code::invalid_key_type, span);
                }
            }
        }
    }

    /// The name types are known by at run time: `Int`, `List`, a class's key.
    fn tag(&self, ty: &Ty) -> Option<String> {
        Some(match ty {
            Ty::Int => "Int".to_string(),
            Ty::Float => "Float".to_string(),
            Ty::Bool => "Bool".to_string(),
            Ty::Str => "String".to_string(),
            Ty::List(_) => "List".to_string(),
            Ty::Maybe(_) => MAYBE.to_string(),
            Ty::Named(n, _) if !self.interfaces.contains_key(n) => n.clone(),
            _ => return None,
        })
    }

    fn type_args(ty: &Ty) -> Vec<Ty> {
        match ty {
            Ty::Named(_, args) => args.clone(),
            Ty::List(e) | Ty::Maybe(e) => vec![(**e).clone()],
            _ => Vec::new(),
        }
    }

    /// The interfaces `ty` is known to implement, with their parents.
    fn interfaces_of(&self, ty: &Ty) -> Vec<Ty> {
        let mut stack = match self.subst.shallow(ty) {
            Ty::Param(p) => self.bounds.get(&p).cloned().unwrap_or_default(),
            t @ Ty::Named(..) if self.tag(&t).is_none() => vec![t],
            t => match self.tag(&t) {
                Some(tag) => {
                    let map = self.args_map(&tag, &Self::type_args(&t));
                    let list = self.impls.get(&tag).cloned().unwrap_or_default();
                    list.iter().map(|i| i.substitute(&map)).collect()
                }
                None => Vec::new(),
            },
        };
        let mut all = Vec::new();
        while let Some(i) = stack.pop() {
            let Ty::Named(name, args) = &i else {
                continue;
            };
            if let Some(def) = self.interfaces.get(name) {
                let map: HashMap<String, Ty> = def
                    .params
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect();
                stack.extend(def.parents.iter().map(|p| p.substitute(&map)));
            }
            if !all.contains(&i) {
                all.push(i);
            }
        }
        all
    }

    fn implements(&mut self, ty: &Ty, iface: &Ty) -> bool {
        if matches!(self.subst.shallow(ty), Ty::Error | Ty::Never) {
            return true;
        }
        let Ty::Named(want, want_args) = self.subst.shallow(iface) else {
            return false;
        };
        let ty = self.subst.shallow(ty);
        if want == KEY {
            return self.is_key(&ty, &mut HashSet::new());
        }
        // Every type `==` works on is `Eq`; a `shared class` has to say so.
        if want == EQ
            && !matches!(ty, Ty::Param(_))
            && !self.is_interface(&ty)
            && !self.is_shared_class(&ty)
        {
            return !self.lacks_eq(&ty, &mut HashSet::new());
        }
        for i in self.interfaces_of(&ty) {
            if let Ty::Named(name, args) = &i
                && *name == want
            {
                return args
                    .iter()
                    .zip(&want_args)
                    .all(|(a, b)| self.subst.unify(a, b));
            }
        }
        false
    }

    fn is_interface(&self, ty: &Ty) -> bool {
        matches!(self.subst.shallow(ty), Ty::Named(n, _) if self.interfaces.contains_key(&n))
    }

    /// Whether a value of type `a` can be used as an `e`: unifying them, but letting a type
    /// stand for an interface it implements, in lists and `Maybe`s too.
    fn conforms(&mut self, a: &Ty, e: &Ty) -> bool {
        let (a, e) = (self.subst.shallow(a), self.subst.shallow(e));
        match (&a, &e) {
            (Ty::List(x), Ty::List(y)) | (Ty::Maybe(x), Ty::Maybe(y)) => self.conforms(x, y),
            (Ty::Var(_), _) => self.subst.unify(&a, &e),
            _ if self.is_interface(&e) && a != e => self.implements(&a, &e),
            _ => self.subst.unify(&a, &e),
        }
    }

    /// Checks each type against the interfaces it names, and points interface defaults it
    /// doesn't override at the type. Returns the type's own methods that interfaces declare,
    /// with each interface's signature, for `check_signatures`.
    fn check_impls(&mut self) -> Vec<OwnMethod> {
        let mut own_methods = Vec::new();
        let mut owners: Vec<String> = self.impls.keys().cloned().collect();
        owners.sort();
        for owner in owners {
            let this = self.own_type(&owner);
            let this = match owner.as_str() {
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "Bool" => Ty::Bool,
                "String" => Ty::Str,
                MAYBE => Ty::maybe(Ty::Param(self.generics[MAYBE][0].clone())),
                _ => this,
            };
            let span = self.impl_spans[&owner];
            let all = self.interfaces_of(&this);
            let mut tags = HashSet::new();
            let mut methods: Vec<String> = Vec::new();
            for i in &all {
                let Ty::Named(name, args) = i else { continue };
                tags.insert(name.clone());
                let def = &self.interfaces[name];
                let map: HashMap<String, Ty> = def
                    .params
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect();
                let fields: Vec<(String, Ty, bool)> = def
                    .fields
                    .iter()
                    .map(|(n, t, v)| (n.clone(), t.substitute(&map), *v))
                    .collect();
                let method_names: Vec<String> = def.methods.iter().map(|m| m.0.clone()).collect();
                for (field, ty, is_var) in fields {
                    let found = match &this {
                        Ty::Named(class, args) if self.class(class).is_some() => {
                            self.field(class, args, &field).filter(|f| !f.private)
                        }
                        _ => {
                            self.error(Code::field_requirement_needs_class, span);
                            continue;
                        }
                    };
                    let ok = found
                        .is_some_and(|f| (f.mutable || !is_var) && self.subst.unify(&f.ty, &ty));
                    if !ok {
                        self.error(Code::missing_implementation, span);
                    }
                }
                for m in method_names {
                    if !methods.contains(&m) {
                        methods.push(m);
                    }
                }
            }
            for m in methods {
                let key = format!("{owner}.{m}");
                let own = self.funcs.get(&key).is_some_and(|f| f.this.is_some());
                // Each interface's version of the method, and whether it has a body.
                let mut versions = Vec::new();
                for i in &all {
                    let Ty::Named(name, args) = i else { continue };
                    let def = &self.interfaces[name];
                    if let Some((_, sig, has_body)) = def.methods.iter().find(|(n, ..)| *n == m) {
                        let map: HashMap<String, Ty> = def
                            .params
                            .iter()
                            .cloned()
                            .zip(args.iter().cloned())
                            .collect();
                        versions.push((name.clone(), sig.clone(), *has_body, map));
                    }
                }
                if own {
                    let wants = versions.into_iter().map(|v| (v.1, v.3)).collect();
                    own_methods.push((key, wants));
                    continue;
                }
                // A default overridden by a more specific interface's default doesn't count.
                let defaults: Vec<String> = versions
                    .iter()
                    .filter(|v| v.2)
                    .map(|v| v.0.clone())
                    .collect();
                let most_specific: Vec<&String> = defaults
                    .iter()
                    .filter(|d| {
                        !defaults.iter().any(|other| {
                            other != *d
                                && self
                                    .interfaces_of(&Ty::named(other))
                                    .iter()
                                    .any(|p| matches!(p, Ty::Named(n, _) if n == *d))
                        })
                    })
                    .collect();
                match most_specific.as_slice() {
                    [one] => {
                        let target = format!("{one}.{m}");
                        self.info.aliases.insert(key, target);
                    }
                    [] => {
                        self.error(Code::missing_implementation, span);
                    }
                    _ => {
                        self.error(Code::conflicting_defaults, span);
                    }
                }
            }
            self.info.implements.insert(owner, tags);
        }
        own_methods
    }

    /// Methods that change `this` through a call to one still being checked, and the rule that
    /// interface methods never change it (generic calls on a `let` rely on it).
    fn report_changes(&mut self, interface_methods: Vec<String>) {
        for key in interface_methods {
            if self.funcs[&key].changes_this {
                self.error(Code::interface_changes_this, self.funcs[&key].decl.span);
            }
        }
        self.info.changes_this = (self.funcs.iter())
            .filter(|(_, f)| f.changes_this)
            .map(|(k, f)| {
                let at = f.decl.body.as_ref().map_or(f.decl.span, |b| b.span).start;
                (k.clone(), Span::new(at, at))
            })
            .collect();
    }

    /// Checks a type's own methods against the signatures its interfaces declare.
    fn check_signatures(&mut self, own_methods: Vec<OwnMethod>) -> Vec<String> {
        let mut keys = Vec::new();
        for (key, wants) in own_methods {
            keys.push(key.clone());
            let span = self.funcs[&key].decl.span;
            let Some(sig) = self.ensure_checked(&key, span) else {
                continue;
            };
            for (want, map) in &wants {
                let ok = sig.params.len() == want.params.len()
                    && sig
                        .params
                        .iter()
                        .zip(&want.params)
                        .all(|(a, b)| self.subst.unify(&a.ty, &b.ty.substitute(map)))
                    && self.conforms(
                        sig.ret.as_ref().unwrap_or(&Ty::Void),
                        &want.ret.clone().unwrap_or(Ty::Void).substitute(map),
                    );
                if !ok {
                    self.error(Code::wrong_implementation, span);
                }
            }
        }
        keys
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
            (Ty::Maybe(x), Ty::Maybe(y))
                if self.subst.shallow(x) == Ty::Int && self.subst.shallow(y) == Ty::Float =>
            {
                self.info.coercions.entry(id).or_default().maybe_to_float = true;
                true
            }
            (Ty::Maybe(_), Ty::Maybe(_)) => self.conforms(&a, &e),
            (_, Ty::Maybe(inner)) => {
                let inner = (**inner).clone();
                if self.subsume(id, &a, &inner) {
                    self.add_coercion(id, false, 1);
                    true
                } else {
                    false
                }
            }
            _ => self.conforms(&a, &e),
        }
    }

    fn expect(&mut self, expr: &Expr, actual: &Ty, expected: &Ty) {
        if self.subsume(expr.id, actual, expected) {
            return;
        }
        let a = self.subst.shallow(actual);
        if self.in_try && self.try_unwrap(expr, &a, expected) {
            return;
        }
        if let Ty::Named(n, args) = &a
            && n == SHARED
            && self.subst.unify(&args[0], expected)
        {
            self.ref_value_hint(expr, false);
            return;
        }
        let code =
            if matches!(a, Ty::Maybe(_)) && !matches!(self.subst.shallow(expected), Ty::Maybe(_)) {
                Code::unhandled_maybe
            } else {
                self.mismatch(&a, expected, Code::type_mismatch)
            };
        self.error(code, expr.span);
    }

    /// For an operator on a `Shared` local, the hint also suggests declaring it `shared`.
    fn ref_value_hint(&mut self, e: &Expr, operator: bool) {
        let fixed = format!("{}.value", self.operand_text(e));
        let mut d =
            Diagnostic::error(Code::shared_where_value_expected, e.span).with_quick_fix(vec![
                Edit {
                    span: e.span,
                    text: fixed.clone(),
                },
            ]);
        let mut detail = fixed;
        if operator
            && let ExprKind::Name(n) = &e.kind
            && self.lookup(n).is_some_and(|v| v.kind == VarKind::Local)
            && !self.is_captured(n)
        {
            detail.push_str(" [or declare it with shared]");
        }
        d.detail = Some(detail);
        self.diags.push(d);
    }

    /// The source of `e` on one line, in parentheses unless a method call on it would bind to all of it.
    fn operand_text(&self, e: &Expr) -> String {
        let text = self.sources.text(e.span);
        let text = text.lines().map(str::trim).collect::<Vec<_>>().join(" ");
        match e.kind {
            ExprKind::Name(_)
            | ExprKind::This
            | ExprKind::Call(..)
            | ExprKind::Method(..)
            | ExprKind::Field(..)
            | ExprKind::Index(..) => text,
            _ => format!("({text})"),
        }
    }

    fn mismatch(&self, actual: &Ty, expected: &Ty, otherwise: Code) -> Code {
        let expected = match self.subst.shallow(expected) {
            Ty::Maybe(inner) => self.subst.shallow(&inner),
            e => e,
        };
        match (self.subst.shallow(actual), expected) {
            (Ty::Record(..), Ty::Named(class, _)) if self.class(&class).is_some() => {
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
            let fix = self.var_keywords.get(&span.start).map(|&keyword| Edit {
                span: keyword,
                text: "let".to_string(),
            });
            let d = Diagnostic::warning(Code::unused_var, span);
            self.diags.push(d.with_fix(fix.into_iter().collect()));
        }
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    /// Records a read of `name` in every lambda it comes from outside of.
    fn capture(&mut self, name: &str) {
        let Some(i) = self.scope_of(name) else {
            return;
        };
        for (base, captures) in &mut self.lambdas {
            if i < *base {
                captures.insert(name.to_string());
            }
        }
    }

    fn scope_of(&self, name: &str) -> Option<usize> {
        self.scopes.iter().rposition(|s| s.contains_key(name))
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
                keyword,
                name,
                span,
                is_var,
                is_shared,
                ty,
                value,
                ..
            } => {
                if *is_var && !is_shared {
                    self.var_keywords.insert(span.start, *keyword);
                }
                let declared = ty.as_ref().map(|t| self.type_from(t));
                let Some(value) = value else {
                    if self.fns.is_empty() && self.scopes.len() == 1 {
                        self.error(Code::top_level_without_value, *span);
                    }
                    let ty = declared.unwrap_or_else(|| self.subst.fresh());
                    self.flow.maybe_unset.insert(*span);
                    self.flow.maybe_set.remove(span);
                    if !is_var {
                        self.lets_without_value.insert(*span, self.loops.len());
                    }
                    let var = Var {
                        ty,
                        mutable: *is_var,
                        kind: VarKind::Local,
                        span: *span,
                        reassigned: false,
                        list_len: None,
                    };
                    self.declare(name, *span, var);
                    return Ty::Void;
                };
                if let (ExprKind::Record(args), Some(Ty::Named(class, _))) =
                    (&value.kind, &declared)
                    && self.class(class).is_some()
                    && !args
                        .first()
                        .is_some_and(|a| matches!(a.value.kind, ExprKind::Spread(_)))
                {
                    let mut d = Diagnostic::warning(Code::record_beside_class, value.span);
                    if let Some(TypeExpr::Name(class, type_span)) = ty {
                        d = d.with_fix(vec![
                            Edit {
                                span: Span::new(span.end, type_span.end),
                                text: String::new(),
                            },
                            Edit {
                                span: Span::new(value.span.start, value.span.start),
                                text: class.clone(),
                            },
                        ]);
                    }
                    self.diags.push(d);
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
                let kind = if *is_shared {
                    VarKind::Shared
                } else {
                    VarKind::Local
                };
                self.declare(
                    name,
                    *span,
                    Var {
                        ty,
                        mutable: *is_var,
                        kind,
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
            Stmt::RefLet { name, span, target } => {
                let (ty, err) = self.place(target);
                if let Some(code) = err {
                    self.error(code, target.span);
                }
                let var = Var {
                    ty,
                    mutable: true,
                    kind: VarKind::Local,
                    span: *span,
                    reassigned: true,
                    list_len: None,
                };
                self.declare(name, *span, var);
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
                            && let [p] = args.as_slice()
                            && let PatternKind::Name(bound) = &p.kind
                            && !self.info.variant_patterns.contains(&p.id)
                        {
                            let d = Diagnostic::warning(Code::let_else_some, pattern.span);
                            let fix = self.let_else_some_fix(bound, pattern, value, otherwise);
                            self.diags.push(d.with_fix(fix));
                        }
                    }
                    None => self.report_missing(vec![vec![Cell::Pat(pattern)]], &t, pattern.span),
                }
                for (name, span, ty, _) in binds {
                    let var = Var {
                        mutable: *is_var,
                        ..Var::binding(ty, span)
                    };
                    self.declare(&name, span, var);
                }
                Ty::Void
            }
            Stmt::Func(_)
            | Stmt::Class(_)
            | Stmt::Enum(_)
            | Stmt::Implement(_)
            | Stmt::Interface(_)
            | Stmt::Data(_) => Ty::Void,
            Stmt::Expr(e) => self.unused(e),
        }
    }

    /// `let Some(x) = e else { jump }` → `let x = e ?? jump`, when the else body is one
    /// expression. `e` gets parentheses unless it binds tighter than `??`.
    fn let_else_some_fix(
        &self,
        bound: &str,
        pattern: &Pattern,
        value: &Expr,
        otherwise: &Expr,
    ) -> Vec<Edit> {
        let ExprKind::Block(stmts) = &otherwise.kind else {
            return Vec::new();
        };
        let [Stmt::Expr(body)] = stmts.as_slice() else {
            return Vec::new();
        };
        let value_text = self.sources.text(value.span);
        let value_text = match value.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Name(_)
            | ExprKind::This
            | ExprKind::Call(..)
            | ExprKind::Method(..)
            | ExprKind::Field(..)
            | ExprKind::Index(..) => value_text.to_string(),
            _ => format!("({value_text})"),
        };
        vec![Edit {
            span: Span::new(pattern.span.start, otherwise.span.end),
            text: format!("{bound} = {value_text} ?? {}", self.sources.text(body.span)),
        }]
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
        if self.is_unset(name) {
            return (self.error(Code::unset_read, span), None);
        }
        let captured = self.is_captured(name);
        // A write into a `Shared` or `shared class` it captured goes through the captured copy.
        self.capture(name);
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
        if var.kind == VarKind::Shared {
            let ty = var.ty.clone();
            self.through_shared = true;
            return (ty, None);
        }
        if !var.mutable {
            let (ty, kind) = (var.ty.clone(), var.kind);
            // `changes_let` here only marks a write rooted at a value's `this`: `place` takes it.
            let code = if name != "this" && kind == VarKind::Param {
                Code::assign_to_parameter
            } else if name != "this" || self.is_shared_class(&ty) {
                Code::assign_to_let
            } else if captured {
                Code::assign_to_captured
            } else {
                Code::changes_let
            };
            return (ty, Some(code));
        }
        let ty = var.ty.clone();
        if !captured {
            var.list_len = None;
        }
        self.place_root = Some(name.to_string());
        (ty, captured.then_some(Code::assign_to_captured))
    }

    fn is_unset(&self, name: &str) -> bool {
        !self.flow.dead
            && self
                .lookup(name)
                .is_some_and(|v| self.flow.maybe_unset.contains(&v.span))
    }

    /// Whether `name` comes from outside the lambda being checked.
    fn is_captured(&self, name: &str) -> bool {
        self.lambdas.last().is_some_and(|&(base, _)| {
            self.scopes
                .iter()
                .rposition(|s| s.contains_key(name))
                .is_none_or(|i| i < base)
        })
    }

    /// A place's type and error, whether it goes through a `Shared` or `shared class`, and its
    /// root variable, without marking anything changed.
    fn place_parts(&mut self, target: &Expr) -> (Ty, Option<Code>, bool, Option<String>) {
        let saved = (
            std::mem::take(&mut self.through_shared),
            self.place_root.take(),
        );
        let (ty, err) = self.place_path(target);
        let through = std::mem::replace(&mut self.through_shared, saved.0);
        let root = std::mem::replace(&mut self.place_root, saved.1);
        (ty, err, through, root)
    }

    fn place(&mut self, target: &Expr) -> (Ty, Option<Code>) {
        let (ty, mut err, through, root) = self.place_parts(target);
        if err == Some(Code::changes_let) {
            err = None;
            if let Some(key) = self.current_method.clone() {
                self.mark_changes_this(key);
            }
        }
        // A change through a `Shared` or `shared class` leaves the variable itself as it was.
        if !through && let Some(var) = root.and_then(|n| self.lookup_mut(&n)) {
            var.reassigned = true;
        }
        (ty, err)
    }

    /// `shared x`: the box behind a shared local, parameter or field.
    fn shared_of(&mut self, x: &Expr) -> Ty {
        let t = match &x.kind {
            ExprKind::Name(n) => self.name(x.id, n, x.span, None),
            _ => self.expr(x, None),
        };
        if !self.is_boxed(x) {
            return self.error(Code::not_shared, x.span);
        }
        Ty::Named(SHARED.to_string(), vec![t])
    }

    /// A shared local, parameter or field, already checked as an expression.
    fn is_boxed(&self, x: &Expr) -> bool {
        match &x.kind {
            ExprKind::Name(n) => self.lookup(n).is_some_and(|v| v.kind == VarKind::Shared),
            ExprKind::Field(..) => self.boxed_fields.contains(&x.id),
            _ => false,
        }
    }

    /// Marked as soon as it's known, so calls checked later in the same cycle of methods see it.
    fn mark_changes_this(&mut self, key: String) {
        let mut work = vec![key];
        while let Some(key) = work.pop() {
            let Some(f) = self.funcs.get_mut(&key).filter(|f| !f.changes_this) else {
                continue;
            };
            f.changes_this = true;
            work.extend(
                (self.pending_changes.iter())
                    .filter(|(_, callee)| *callee == key)
                    .map(|(caller, _)| caller.clone()),
            );
        }
    }

    fn changing_method(&mut self, ty: &Ty, name: &str) -> bool {
        let ty = self.subst.shallow(ty);
        let Some(tag) = self.tag(&ty) else {
            return false;
        };
        let key = self.method_key(&tag, name, &ty);
        self.funcs.get(&key).is_some_and(|f| f.this.is_some())
            && self.ensure_checked(&key, Span::default()).is_some()
            && self.funcs[&key].changes_this
    }

    /// A receiver its call changes, already checked as an expression, must be a place.
    fn changed_receiver(&mut self, recv: &Expr, code: Code) {
        let before = self.diags.len();
        let (_, err) = self.place(recv);
        self.diags.truncate(before);
        if let Some(err) = err {
            let code = match err {
                Code::readonly_field | Code::assign_to_captured => err,
                _ => code,
            };
            self.error(code, recv.span);
        }
    }

    fn place_path(&mut self, target: &Expr) -> (Ty, Option<Code>) {
        match &target.kind {
            ExprKind::Name(name) => self.place_name(name, target.span),
            ExprKind::This => self.place_name("this", target.span),
            ExprKind::Field(base, name) if self.type_ref(base).is_none() => {
                let (bt, base_err) = self.place_path(base);
                let (bt, base_err) = match self.subst.shallow(&bt) {
                    Ty::Named(n, mut a) if n == SHARED && name == "value" => {
                        self.through_shared = true;
                        return (a.remove(0), None);
                    }
                    Ty::Named(n, mut a) if n == SHARED => {
                        self.through_shared = true;
                        (a.remove(0), None)
                    }
                    bt => (bt, base_err),
                };
                match self.subst.shallow(&bt) {
                    t if self.is_interface(&t) || matches!(t, Ty::Param(_)) => {
                        match self.required_field(&t, name) {
                            Some((ty, true)) => (ty, base_err),
                            Some((ty, false)) => (ty, Some(Code::assign_to_let)),
                            None => (self.error(Code::unknown_field, target.span), None),
                        }
                    }
                    Ty::Named(class, args) => {
                        match self.visible_field(&class, &args, name, target.span) {
                            Some(f) => {
                                let FieldInfo {
                                    ty,
                                    mutable,
                                    is_shared,
                                    boxed,
                                    ..
                                } = f;
                                let err = if boxed {
                                    self.through_shared = true;
                                    None
                                } else if !mutable {
                                    Some(Code::assign_to_let)
                                } else if f.readonly && !self.inside(&class) {
                                    Some(Code::readonly_field)
                                } else if is_shared {
                                    self.through_shared = true;
                                    None
                                } else {
                                    base_err
                                };
                                (ty, err)
                            }
                            None => (self.error(Code::unknown_field, target.span), None),
                        }
                    }
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
                let (lt, base_err) = self.place_path(list);
                let (lt, base_err) = match self.subst.shallow(&lt) {
                    Ty::Named(n, mut a) if n == SHARED => {
                        self.through_shared = true;
                        (a.remove(0), None)
                    }
                    lt => (lt, base_err),
                };
                let (key, value) = match self.subst.shallow(&lt) {
                    Ty::Named(n, mut a) if n == MAP => (a.remove(0), Some(a.remove(0))),
                    _ => (Ty::Int, None),
                };
                let it = self.expr(index, Some(&key));
                self.expect(index, &it, &key);
                if let Some(value) = value {
                    return (value, base_err);
                }
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

    fn field(&self, type_name: &str, args: &[Ty], field: &str) -> Option<FieldInfo> {
        let c = self.class(type_name)?;
        let i = c.fields.iter().position(|f| f.name == field)?;
        let ty = match &c.fields[i] {
            f if f.is_rest => Ty::list(f.ty.clone()),
            f => f.ty.clone(),
        };
        let ty = ty.substitute(&self.args_map(type_name, args));
        Some(FieldInfo {
            ty,
            mutable: c.mutable[i],
            is_shared: c.is_shared,
            boxed: c.fields[i].sharing == Sharing::Field,
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
    fn visible_field(
        &mut self,
        type_name: &str,
        args: &[Ty],
        field: &str,
        span: Span,
    ) -> Option<FieldInfo> {
        let f = self.field(type_name, args, field)?;
        if f.private && !self.inside(type_name) {
            self.error(Code::private_member, span);
        }
        Some(f)
    }

    /// The type of a field an interface or a `T`'s bounds require, and whether it's a `var`.
    fn required_field(&self, ty: &Ty, field: &str) -> Option<(Ty, bool)> {
        self.interfaces_of(ty).into_iter().find_map(|i| {
            let Ty::Named(name, args) = i else {
                return None;
            };
            let def = &self.interfaces[&name];
            let map: HashMap<String, Ty> = def.params.iter().cloned().zip(args).collect();
            let (_, t, is_var) = def.fields.iter().find(|f| f.0 == field)?;
            Some((t.substitute(&map), *is_var))
        })
    }

    /// The constructor takes every field but the private ones.
    fn construct(&mut self, id: NodeId, class: &str, args: &[Arg], span: Span) -> Ty {
        for a in args {
            if let Some(name) = &a.name
                && self.field(class, &[], name).is_some_and(|f| f.private)
            {
                self.error(Code::private_member, a.value.span);
            }
        }
        let ty = self.fresh_type(class);
        self.require_type_bounds(&ty, span);
        let map = self.args_map(class, &Self::type_args(&ty));
        let c = self.class(class).expect("class");
        let params: Vec<ParamSig> = c
            .fields
            .iter()
            .zip(&c.private)
            .filter(|(_, private)| !**private)
            .map(|(f, _)| ParamSig {
                ty: f.ty.substitute(&map),
                ..f.clone()
            })
            .collect();
        self.match_args(&params, args, span);
        self.info
            .resolved
            .insert(id, Resolved::Construct(class.to_string()));
        ty
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

    /// Whether `==` can't compare `ty`: it holds a `shared class`, interface or `T` that isn't `Eq`.
    fn lacks_eq(&mut self, ty: &Ty, seen: &mut HashSet<String>) -> bool {
        let ty = self.subst.shallow(ty);
        match &ty {
            Ty::List(inner) | Ty::Maybe(inner) => self.lacks_eq(inner, seen),
            Ty::Param(_) => !self.implements(&ty, &Ty::named(EQ)),
            Ty::Named(..) if self.is_interface(&ty) => !self.implements(&ty, &Ty::named(EQ)),
            Ty::Named(name, args) if name == MAP || name == SET => {
                args.iter().any(|a| self.lacks_eq(a, seen))
            }
            Ty::Named(name, _) if name == SHARED => true,
            Ty::Func(..) => true,
            Ty::Record(_, fields) => fields.iter().any(|f| self.lacks_eq(f, seen)),
            Ty::Named(name, args) if seen.insert(Show(&self.resolve(&ty)).to_string()) => {
                if self.is_shared_class(&ty) && !self.implements(&ty, &Ty::named(EQ)) {
                    return true;
                }
                let fields = self.field_types(name, args);
                fields.iter().any(|f| self.lacks_eq(f, seen))
            }
            _ => false,
        }
    }

    /// The types of a class's fields, or of all an enum's variant fields.
    fn field_types(&self, name: &str, args: &[Ty]) -> Vec<Ty> {
        let map = self.args_map(name, args);
        match self.types.get(name) {
            Some(TypeDef::Class(c)) => c.fields.iter().map(|f| f.ty.substitute(&map)).collect(),
            Some(TypeDef::Enum(vs)) => vs
                .iter()
                .flat_map(|v| v.fields.iter().map(|f| f.ty.substitute(&map)))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Whether `ty` can be a map key or set item: its `==` is exact, so no `Float`,
    /// function, collection or shared value anywhere inside it.
    fn is_key(&mut self, ty: &Ty, seen: &mut HashSet<String>) -> bool {
        let ty = self.subst.shallow(ty);
        match &ty {
            Ty::Int | Ty::Str | Ty::Bool | Ty::Error => true,
            // A type parameter used as a key becomes a key type, checked where it's filled in.
            Ty::Param(p) => match self.bounds.get_mut(p) {
                Some(bounds) => {
                    if !bounds.contains(&Ty::named(KEY)) {
                        bounds.push(Ty::named(KEY));
                    }
                    true
                }
                None => false,
            },
            Ty::Record(_, fields) => fields.iter().all(|f| self.is_key(f, seen)),
            Ty::Maybe(inner) => self.is_key(inner, seen),
            Ty::Named(name, _) if name == MAP || name == SET || name == SHARED => false,
            Ty::Named(..) if self.is_interface(&ty) || self.is_shared_class(&ty) => false,
            Ty::Named(name, _)
                if self
                    .class(name)
                    .is_some_and(|c| c.fields.iter().any(|f| f.sharing == Sharing::Field)) =>
            {
                false
            }
            Ty::Named(name, args) => {
                if !seen.insert(Show(&self.resolve(&ty)).to_string()) {
                    return true;
                }
                let fields = self.field_types(name, args);
                fields.iter().all(|f| self.is_key(f, seen))
            }
            _ => false,
        }
    }

    /// Checks `ty` can be a key once inference is done. A type parameter gets the
    /// requirement as a bound, checked where a call fills it in.
    fn require_key(&mut self, ty: &Ty, span: Span) {
        if let Ty::Param(p) = self.subst.shallow(ty) {
            let bounds = self.bounds.entry(p).or_default();
            if !bounds.contains(&Ty::named(KEY)) {
                bounds.push(Ty::named(KEY));
            }
        } else {
            self.pending_bounds
                .push((ty.clone(), Some(Ty::named(KEY)), span));
        }
    }

    fn is_collection(&self, ty: &Ty) -> bool {
        match self.subst.shallow(ty) {
            Ty::List(_) => true,
            Ty::Named(n, _) => n == MAP || n == SET,
            _ => false,
        }
    }

    fn unref(&self, ty: &Ty) -> Ty {
        match self.subst.shallow(ty) {
            Ty::Named(n, a) if n == SHARED => self.subst.shallow(&a[0]),
            t => t,
        }
    }

    fn is_shared_class(&self, ty: &Ty) -> bool {
        matches!(self.subst.shallow(ty), Ty::Named(n, _)
            if matches!(self.types.get(&n), Some(TypeDef::Class(c)) if c.is_shared))
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

    /// The enum a bare variant name belongs to: the expected type's, or `Result`'s, which
    /// are usable anywhere.
    fn expected_variant(&self, expected: Option<&Ty>, name: &str) -> Option<String> {
        match expected.map(|e| self.subst.shallow(e)) {
            Some(Ty::Named(e, _)) if self.variant(&e, name).is_some() => Some(e),
            Some(Ty::Maybe(inner)) => self.expected_variant(Some(&inner), name),
            _ if self.variant(RESULT, name).is_some() => Some(RESULT.to_string()),
            _ => None,
        }
    }

    /// A variant's field types, for the enum type `ty`.
    fn variant_fields(&self, enum_name: &str, args: &[Ty], name: &str) -> Option<Vec<ParamSig>> {
        let map = self.args_map(enum_name, args);
        let v = self.variant(enum_name, name)?;
        Some(
            v.fields
                .iter()
                .map(|f| ParamSig {
                    ty: f.ty.substitute(&map),
                    ..f.clone()
                })
                .collect(),
        )
    }

    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, span: Span) {
        if op.is_none()
            && let ExprKind::Name(n) = &target.kind
            && let Some(var) = self.lookup(n)
            && (self.flow.maybe_unset.contains(&var.span)
                || self.flow.maybe_set.contains(&var.span))
            && !self.is_captured(n)
        {
            let (decl, ty) = (var.span, var.ty.clone());
            let again = self.flow.maybe_set.contains(&decl)
                || self
                    .lets_without_value
                    .get(&decl)
                    .is_some_and(|&d| self.loops.len() > d);
            if again && self.lets_without_value.contains_key(&decl) {
                self.error(Code::let_set_twice, target.span);
            } else if again && let Some(var) = self.lookup_mut(n) {
                var.reassigned = true;
            }
            let actual = self.expr(value, Some(&ty));
            self.expect(value, &actual, &ty);
            self.flow.maybe_unset.remove(&decl);
            self.flow.maybe_set.insert(decl);
            return;
        }
        let (ty, err) = self.place(target);
        if let Some(code) = err {
            self.error(code, target.span);
        }
        if op.is_some()
            && let ExprKind::Index(list, index) = &target.kind
            && !self.index_narrowed(list, index)
        {
            self.compound_on_missing_index(target, &ty, value);
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
                if matches!(self.subst.shallow(&ty), Ty::Named(n, _) if n == SHARED) {
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
            self.ref_binding(stmt, &stmts[i + 1..]);
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
            if self.subst.shallow(&last) == Ty::Never {
                self.flow.dead = true;
                if !is_last {
                    last = Ty::Void;
                }
            }
        }
        self.changed_copies(stmts);
        self.set_in_branches(stmts);
        self.finish_scope();
        last
    }

    // Branches whose value is never used don't need matching types (design.md, Syntax).
    fn unused(&mut self, expr: &Expr) -> Ty {
        match &expr.kind {
            ExprKind::Block(stmts) => {
                self.scopes.push(HashMap::new());
                for (i, stmt) in stmts.iter().enumerate() {
                    self.ref_binding(stmt, &stmts[i + 1..]);
                    let t = self.stmt(stmt);
                    if self.subst.shallow(&t) == Ty::Never {
                        self.flow.dead = true;
                    }
                }
                self.changed_copies(stmts);
                self.set_in_branches(stmts);
                self.finish_scope();
            }
            ExprKind::If(cond, then, otherwise) => {
                let before = self.flow.clone();
                self.then_branch(cond, then, |c| c.unused(then));
                let after_then = std::mem::replace(&mut self.flow, before);
                if let Some(other) = otherwise {
                    self.unused(other);
                }
                self.flow = after_then.join(std::mem::take(&mut self.flow));
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
            ExprKind::SharedOf(x) => self.shared_of(x),
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
                Some(var) => {
                    let ty = var.ty.clone();
                    self.capture("this");
                    ty
                }
                None => self.error(Code::this_outside_method, expr.span),
            },
            ExprKind::Field(base, name) => {
                let before = self.diags.len();
                let ty = self.field_expr(expr.id, base, name, expr.span);
                let shared_name = name == "value" && self.is_boxed(base);
                let missing = (self.diags[before..].iter_mut())
                    .find(|d| d.code == Code::unknown_field && d.span == expr.span);
                if let Some(d) = missing.filter(|_| shared_name) {
                    d.detail = Some("[it's shared: it already reads as its value]".to_string());
                    d.fix = vec![Edit {
                        span: Span::new(base.span.end, expr.span.end),
                        text: String::new(),
                    }];
                    d.quick_fix_only = true;
                }
                ty
            }
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
                let expected = match expected.map(|e| self.subst.shallow(e)) {
                    Some(Ty::Maybe(inner)) => Some(self.subst.shallow(&inner)),
                    e => e,
                };
                let (mut elem, declared) = match expected {
                    Some(Ty::List(e)) => (*e, true),
                    _ => (self.subst.fresh(), false),
                };
                let mut typed = Vec::new();
                for item in items {
                    let t = self.expr(item, Some(&elem));
                    if !declared && matches!(self.subst.shallow(&elem), Ty::Var(_)) {
                        self.subst.unify(&elem, &t);
                    } else if !declared && let Some(widened) = self.widen_to_maybe(&elem, &t) {
                        elem = widened;
                    }
                    typed.push((item, t));
                }
                for (item, t) in typed {
                    let different_types = !declared
                        && matches!(
                            (self.subst.shallow(&t), self.subst.shallow(&elem)),
                            (Ty::Named(a, _), Ty::Named(b, _)) if a != b
                        );
                    if different_types {
                        self.error(Code::list_needs_type, item.span);
                    } else {
                        self.expect(item, &t, &elem);
                    }
                }
                Ty::list(elem)
            }
            ExprKind::Map(entries) => {
                let expected = match expected.map(|e| self.subst.shallow(e)) {
                    Some(Ty::Maybe(inner)) => Some(self.subst.shallow(&inner)),
                    e => e,
                };
                let (key, value) = match expected {
                    Some(Ty::Named(n, mut a)) if n == MAP => (a.remove(0), a.remove(0)),
                    _ => {
                        let key = self.subst.fresh();
                        self.require_key(&key, expr.span);
                        (key, self.subst.fresh())
                    }
                };
                for (k, v) in entries {
                    let kt = self.expr(k, Some(&key));
                    self.expect(k, &kt, &key);
                    let vt = self.expr(v, Some(&value));
                    self.expect(v, &vt, &value);
                }
                Ty::Named(MAP.to_string(), vec![key, value])
            }
            ExprKind::Record(args) => self.record_literal(expr, args, expected),
            ExprKind::Spread(inner) => {
                self.expr(inner, None);
                self.error(Code::wrong_arguments, expr.span)
            }
            ExprKind::Unary(op, operand) => self.unary(expr.id, *op, operand, expr.span),
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::Call(callee, args) => {
                let ty = self.call(expr.id, callee, args, expr.span, expected);
                self.check_overlap(None, args);
                ty
            }
            ExprKind::Method(recv, name, args) => {
                let ty = self.method(expr.id, recv, name, args, expr.span);
                self.check_overlap(Some(recv), args);
                ty
            }
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
                self.finish_scope();
                ty
            }
            ExprKind::For(pattern, iter, body, false) => {
                self.for_loop(expr.id, pattern, iter, body)
            }
            ExprKind::For(pattern, iter, body, true) => self.for_ref(expr.id, pattern, iter, body),
            ExprKind::Loop(body) => {
                self.loops.push(LoopCtx {
                    kind: LoopKind::Loop,
                    break_ty: None,
                    break_flow: None,
                });
                self.unused(body);
                let ctx = self.loops.pop().expect("loop context");
                self.flow = ctx.break_flow.clone().unwrap_or(Flow {
                    dead: true,
                    ..Flow::default()
                });
                ctx.break_ty.unwrap_or(Ty::Never)
            }
            ExprKind::Break(value) => {
                let ty = match value {
                    Some(v) => self.expr(v, None),
                    None => Ty::Void,
                };
                let flow = std::mem::replace(
                    &mut self.flow,
                    Flow {
                        dead: true,
                        ..Flow::default()
                    },
                );
                let Some(ctx) = self.loops.last_mut() else {
                    return self.error(Code::break_outside_loop, expr.span);
                };
                ctx.break_flow = Some(match ctx.break_flow.take() {
                    Some(b) => b.join(flow),
                    None => flow,
                });
                let ctx = self.loops.last().expect("loop");
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
                self.flow.dead = true;
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
                self.flow.dead = true;
                Ty::Never
            }
        }
    }

    fn push_bindings(&mut self, binds: Vec<Binding>) {
        let scope = binds
            .into_iter()
            .map(|(name, span, ty, mutable)| {
                let var = Var {
                    mutable,
                    ..Var::binding(ty, span)
                };
                (name, var)
            })
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
        let ty = self.fresh_type(enum_name);
        self.require_type_bounds(&ty, span);
        ty
    }

    fn variant_call(
        &mut self,
        id: NodeId,
        enum_name: &str,
        name: &str,
        args: &[Arg],
        span: Span,
        expected: Option<&Ty>,
    ) -> Ty {
        let ty = self.fresh_type(enum_name);
        self.require_type_bounds(&ty, span);
        let expected = expected.map(|e| match self.subst.shallow(e) {
            Ty::Maybe(inner) => self.subst.shallow(&inner),
            e => e,
        });
        if let Some(e @ Ty::Named(n, _)) = &expected
            && n == enum_name
        {
            self.subst.unify(&ty, e);
        }
        let params = self
            .variant_fields(enum_name, &Self::type_args(&ty), name)
            .unwrap_or_default();
        self.match_args(&params, args, span);
        self.info.resolved.insert(
            id,
            Resolved::Variant(enum_name.to_string(), name.to_string()),
        );
        ty
    }

    fn record_literal(&mut self, expr: &Expr, args: &[Arg], expected: Option<&Ty>) -> Ty {
        if let Some(ExprKind::Spread(inner)) = args.first().map(|a| &a.value.kind) {
            return self.record_copy(inner, &args[1..]);
        }
        let expected = expected.map(|e| match self.subst.shallow(e) {
            Ty::Maybe(inner) => self.subst.shallow(&inner),
            e => e,
        });
        if let Some(Ty::Named(class, _)) = &expected
            && self.class(class).is_some()
        {
            return self.construct(expr.id, class, args, expr.span);
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
            Ty::Named(class, _) => self.class(&class).is_some(),
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
                (Some(name), Ty::Named(class, args)) => match self.field(&class, &args, name) {
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
        let bt = self.try_operand(base, bt);
        let bt = match self.subst.shallow(&bt) {
            Ty::Named(n, mut a) if n == SHARED && name == "value" => return a.remove(0),
            Ty::Named(n, mut a) if n == SHARED => a.remove(0),
            bt => bt,
        };
        match self.subst.shallow(&bt) {
            t if self.is_interface(&t) || matches!(t, Ty::Param(_)) => {
                match self.required_field(&t, name) {
                    Some((ty, _)) => ty,
                    None => self.error(Code::unknown_field, span),
                }
            }
            Ty::Named(class, args) => match self.visible_field(&class, &args, name, span) {
                Some(f) => {
                    if f.boxed {
                        self.boxed_fields.insert(id);
                    }
                    f.ty
                }
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
        if self.is_unset(name) {
            self.error(Code::unset_read, span);
        }
        if let Some(var) = self.lookup(name) {
            let ty = var.ty.clone();
            self.capture(name);
            return ty;
        }
        if self.funcs.contains_key(name) {
            let Some(sig) = self.ensure_checked(name, span) else {
                return Ty::Error;
            };
            if (sig.params.iter()).any(|p| p.is_ref || p.sharing != Sharing::No) {
                return self.error(Code::ref_func_as_value, span);
            }
            let map = self.instantiate(name, span);
            let params = sig.params.iter().map(|p| p.ty.substitute(&map)).collect();
            let ret = sig.ret.unwrap_or(Ty::Void).substitute(&map);
            return Ty::Func(params, Box::new(ret));
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
        if self.consts.get(name).is_some_and(|c| c.owner.is_none()) {
            self.info
                .resolved
                .insert(id, Resolved::Const(name.to_string()));
            return self.const_type(name);
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

    fn unary(&mut self, id: NodeId, op: UnOp, operand: &Expr, span: Span) -> Ty {
        let points = self.try_points;
        let t = if op == UnOp::Try {
            if self.in_try {
                let fix = vec![Edit {
                    span: Span::new(span.start, operand.span.start),
                    text: String::new(),
                }];
                self.diags
                    .push(Diagnostic::warning(Code::nested_try, span).with_fix(fix));
            }
            let outer = std::mem::replace(&mut self.in_try, true);
            let t = self.expr(operand, None);
            self.in_try = outer;
            t
        } else {
            let t = self.expr(operand, None);
            self.try_operand(operand, t)
        };
        let t = self.subst.shallow(&t);
        let inner_fails = self.try_points > points;
        if op == UnOp::Try {
            self.try_points += 1;
        }
        match (op, &t) {
            (_, Ty::Error) => Ty::Error,
            (UnOp::Neg, Ty::Int | Ty::Float) => t,
            (UnOp::BitNot, Ty::Int) => Ty::Int,
            (UnOp::Not, Ty::Bool) => Ty::Bool,
            (UnOp::Try, Ty::Maybe(inner)) => {
                let inner = (**inner).clone();
                if self.try_fails(Fails::Maybe, operand, span) {
                    inner
                } else {
                    Ty::Error
                }
            }
            (UnOp::Try, Ty::Named(n, args)) if n == RESULT => {
                let (ok, e) = (args[0].clone(), args[1].clone());
                if self.try_fails(Fails::Result(e), operand, span) {
                    ok
                } else {
                    Ty::Error
                }
            }
            (UnOp::Try, _) if inner_fails => {
                self.info.plain_tries.insert(id);
                t
            }
            (_, Ty::Var(_)) => self.error(Code::type_needed, operand.span),
            _ => self.error(Code::invalid_operand_types, span),
        }
    }

    fn try_unwrap(&mut self, expr: &Expr, actual: &Ty, expected: &Ty) -> bool {
        let e = self.subst.shallow(expected);
        let failable =
            |t: &Ty| matches!(t, Ty::Maybe(_)) || matches!(t, Ty::Named(n, _) if n == RESULT);
        if matches!(e, Ty::Var(_)) || failable(&e) {
            return false;
        }
        let Some((inner, fails)) = failure_of(actual) else {
            return false;
        };
        if !self.subsume(expr.id, &inner, &e) {
            return false;
        }
        self.record_unwrap(expr, fails);
        true
    }

    fn try_operand(&mut self, expr: &Expr, ty: Ty) -> Ty {
        let Some((inner, fails)) = failure_of(&self.subst.shallow(&ty)).filter(|_| self.in_try)
        else {
            return ty;
        };
        self.record_unwrap(expr, fails);
        inner
    }

    fn record_unwrap(&mut self, expr: &Expr, fails: Fails) {
        if self.try_fails(fails, expr, expr.span) {
            self.info.try_unwraps.insert(expr.id, expr.span);
            self.try_points += 1;
        }
    }

    /// Records what a `try` passes up, or reports why the function can't pass it up.
    fn try_fails(&mut self, fails: Fails, operand: &Expr, span: Span) -> bool {
        let Some(ctx) = self.fns.last() else {
            return true;
        };
        let declared = ctx.declared.as_ref().map(|d| self.subst.shallow(d));
        let passes = match declared {
            Some(Ty::Maybe(_)) => Some(Fails::Maybe),
            Some(Ty::Named(n, args)) if n == RESULT => Some(Fails::Result(args[1].clone())),
            Some(Ty::Error) => return false,
            Some(_) => {
                self.error(Code::try_outside_failable, span);
                return false;
            }
            None => ctx.fails.clone(),
        };
        let (code, fix) = match (&fails, passes) {
            (_, None) => {
                self.fns.last_mut().expect("fn").fails = Some(fails);
                return true;
            }
            (Fails::Maybe, Some(Fails::Maybe)) => return true,
            (Fails::Result(e1), Some(Fails::Result(e2))) => {
                if self.subst.unify(e1, &e2) {
                    return true;
                }
                self.error(Code::try_error_mismatch, operand.span);
                return false;
            }
            (Fails::Result(_), Some(Fails::Maybe)) => (Code::try_result_in_maybe, "ok()"),
            (Fails::Maybe, Some(Fails::Result(_))) => (Code::try_maybe_in_result, "orErr(...)"),
        };
        let mut d = Diagnostic::error(code, span);
        let prefix = if span == operand.span { "" } else { "try " };
        d.detail = Some(format!("{prefix}{}.{fix}", self.operand_text(operand)));
        self.diags.push(d);
        false
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        match op {
            BinOp::And | BinOp::Or => {
                let lt = self.expr(lhs, Some(&Ty::Bool));
                let lt = self.try_operand(lhs, lt);
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
                let rt = self.try_operand(rhs, rt);
                self.scopes.pop();
                if !self.subst.unify(&rt, &Ty::Bool) {
                    self.error(Code::invalid_operand_types, rhs.span);
                }
                Ty::Bool
            }
            BinOp::Coalesce => {
                let lt = self.expr(lhs, None);
                let inner = match self.subst.shallow(&lt) {
                    Ty::Maybe(inner) => *inner,
                    Ty::Named(n, mut args) if n == RESULT => args.swap_remove(0),
                    Ty::Var(_) => return self.error(Code::type_needed, lhs.span),
                    Ty::Error => return Ty::Error,
                    _ => return self.error(Code::type_mismatch, lhs.span),
                };
                let rt = self.expr(rhs, Some(&inner));
                let maybe = Ty::maybe(inner.clone());
                if matches!(self.subst.shallow(&rt), Ty::Maybe(_))
                    && self.subsume(rhs.id, &rt, &maybe)
                {
                    self.info.coalesce_to_maybe.insert(lhs.id);
                    return maybe;
                }
                self.expect(rhs, &rt, &inner);
                inner
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
                let (lt, rt) = self.compare_maybe_with_value(lhs, lt, rhs, rt);
                self.binary_type(op, lhs, &lt, rhs, &rt, span)
            }
            _ => {
                let lt = self.expr(lhs, None);
                let lt = self.try_operand(lhs, lt);
                let rt = self.expr(rhs, None);
                let rt = self.try_operand(rhs, rt);
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

    fn compare_maybe_with_value(&mut self, lhs: &Expr, lt: Ty, rhs: &Expr, rt: Ty) -> (Ty, Ty) {
        let (a, b) = (self.subst.shallow(&lt), self.subst.shallow(&rt));
        let plain = |t: &Ty| !matches!(t, Ty::Maybe(_) | Ty::Var(_));
        match (&a, &b) {
            (Ty::Maybe(_), b) if plain(b) && self.subsume(rhs.id, &rt, &lt) => (lt.clone(), lt),
            (a, Ty::Maybe(_)) if plain(a) && self.subsume(lhs.id, &lt, &rt) => (rt.clone(), rt),
            _ => (lt, rt),
        }
    }

    fn widen_to_maybe(&mut self, elem: &Ty, item: &Ty) -> Option<Ty> {
        match (self.subst.shallow(elem), self.subst.shallow(item)) {
            (e, Ty::Maybe(x))
                if !matches!(e, Ty::Maybe(_) | Ty::Var(_)) && self.subst.unify(&e, &x) =>
            {
                Some(Ty::Maybe(x))
            }
            _ => None,
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
        let inner = |t: &Ty| match t {
            Ty::Named(n, args) if n == SHARED => Some(args[0].clone()),
            _ => None,
        };
        match (inner(&a), inner(&b)) {
            (Some(t), None) => {
                self.ref_value_hint(lhs, true);
                a = self.subst.shallow(&t);
            }
            (None, Some(t)) => {
                self.ref_value_hint(rhs, true);
                b = self.subst.shallow(&t);
            }
            _ => {}
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
                if self.lacks_eq(&a, &mut HashSet::new()) {
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

    fn check_arg(&mut self, source: &ArgSource, param: &Ty, ref_param: bool, sharing: Sharing) {
        if sharing != Sharing::No
            && let ArgSource::Expr(e) = source
        {
            let t = self.expr(e, Some(param));
            let boxed = Ty::Named(SHARED.to_string(), vec![param.clone()]);
            if self.is_boxed(e) {
                self.info.shared_args.insert(e.id, e.span);
                if !self.subst.unify(&t, param) {
                    let code = self.mismatch(&t, param, Code::type_mismatch);
                    self.error(code, e.span);
                }
            } else if matches!(self.subst.shallow(&t), Ty::Named(n, _) if n == SHARED) {
                self.expect(e, &t, &boxed);
            } else if sharing == Sharing::Field {
                self.expect(e, &t, param);
            } else {
                self.error(Code::missing_shared, e.span);
            }
            return;
        }
        match source {
            ArgSource::Expr(e) if ref_param => {
                self.info.ref_args.insert(e.id, e.span);
                let (t, err) = self.place(e);
                let fits = self.subst.unify(&t, param);
                let boxed = !fits
                    && matches!(self.subst.shallow(&t), Ty::Named(n, a)
                        if n == SHARED && self.subst.unify(&a[0], param));
                match err {
                    _ if boxed => self.ref_value_hint(e, false),
                    Some(code) => {
                        self.error(code, e.span);
                    }
                    None if !fits => {
                        let code = self.mismatch(&t, param, Code::type_mismatch);
                        self.error(code, e.span);
                    }
                    None => {}
                }
            }
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
            self.check_arg(source, param, false, Sharing::No);
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
                SET => {
                    let item = match expected.map(|e| self.subst.shallow(e)) {
                        Some(Ty::Named(n, mut a)) if n == SET => a.remove(0),
                        _ => {
                            let item = self.subst.fresh();
                            self.require_key(&item, span);
                            item
                        }
                    };
                    self.args_of(args, span, &[Ty::list(item.clone())]);
                    return Ty::Named(SET.to_string(), vec![item]);
                }
                SHARED => {
                    if args.len() != 1 {
                        return self.error(Code::wrong_arguments, span);
                    }
                    let inner = match expected.map(|e| self.subst.shallow(e)) {
                        Some(Ty::Named(n, mut a)) if n == SHARED => a.remove(0),
                        _ => self.subst.fresh(),
                    };
                    let t = self.expr(&args[0].value, Some(&inner));
                    self.expect(&args[0].value, &t, &inner);
                    if self.is_shared_class(&inner) {
                        self.warn(Code::shared_wraps_shared_class, span);
                    }
                    return Ty::Named(SHARED.to_string(), vec![inner]);
                }
                "same" => {
                    if args.len() != 2 {
                        return self.error(Code::wrong_arguments, span);
                    }
                    let a = self.expr(&args[0].value, None);
                    let b = self.expr(&args[1].value, None);
                    let shared = self.is_shared_class(&a)
                        || matches!(self.subst.shallow(&a), Ty::Named(n, _) if n == SHARED);
                    if !shared || !self.subst.unify(&a, &b) {
                        self.error(Code::type_mismatch, span);
                    }
                    return Ty::Bool;
                }
                _ => {}
            }
            if let Some(TypeDef::Class(_)) = self.types.get(name) {
                return self.construct(id, name, args, span);
            }
            if let Some(e) = self.expected_variant(expected, name) {
                return self.variant_call(id, &e, name, args, span, expected);
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
        self.call_entry(name, None, args, span)
    }

    /// Fresh type variables for a function's type parameters, whose bounds are checked later.
    fn instantiate(&mut self, name: &str, span: Span) -> HashMap<String, Ty> {
        let bounds = self.funcs[name].bounds.clone();
        let map: HashMap<String, Ty> = bounds
            .keys()
            .map(|g| (g.clone(), self.subst.fresh()))
            .collect();
        for (g, bs) in bounds {
            self.pending_bounds.push((map[&g].clone(), None, span));
            for b in bs {
                self.pending_bounds
                    .push((map[&g].clone(), Some(b.substitute(&map)), span));
            }
        }
        map
    }

    /// Calls function `name`, a method of `recv` when given.
    fn call_entry(&mut self, name: &str, recv: Option<&Ty>, args: &[Arg], span: Span) -> Ty {
        // Checking the body first lets it add key bounds to the type parameters.
        let sig = self.ensure_checked(name, span);
        let map = self.instantiate(name, span);
        if let Some(recv) = recv
            && let Some((this, _)) = self.funcs[name].this.clone()
            && !self.conforms(recv, &this.substitute(&map))
        {
            return self.unknown_method(args, span);
        }
        let params: Vec<ParamSig> = self.funcs[name]
            .sig
            .params
            .iter()
            .map(|p| ParamSig {
                ty: p.ty.substitute(&map),
                ..p.clone()
            })
            .collect();
        self.match_args(&params, args, span);
        match sig {
            Some(sig) => sig.ret.unwrap_or(Ty::Void).substitute(&map),
            None => Ty::Error,
        }
    }

    /// The method `name` of a `recv`, whose run-time type is `tag`: one added for its exact type
    /// arguments (`Box<Int>.get`) first, then the type's own or an interface default, then
    /// the only one added for some arguments, which the call checks against `recv`.
    fn method_key(&self, tag: &str, name: &str, recv: &Ty) -> String {
        let exact = format!("{}.{name}", Show(&self.resolve(recv)));
        if self.funcs.contains_key(&exact) {
            return exact;
        }
        let own = format!("{tag}.{name}");
        let own = self.info.aliases.get(&own).cloned().unwrap_or(own);
        if self.funcs.contains_key(&own) {
            return own;
        }
        let prefix = format!("{tag}<");
        let suffix = format!(">.{name}");
        let mut added = self
            .funcs
            .keys()
            .filter(|k| k.starts_with(&prefix) && k.ends_with(&suffix));
        match (added.next(), added.next()) {
            (Some(only), None) => only.clone(),
            _ => own,
        }
    }

    /// A method an interface, or a `T`'s bounds, declare: where to find it and its signature.
    fn interface_method(&self, ty: &Ty, name: &str) -> Option<Sig> {
        self.interfaces_of(ty).into_iter().find_map(|i| {
            let Ty::Named(iface, args) = i else {
                return None;
            };
            let def = &self.interfaces[&iface];
            let map: HashMap<String, Ty> = def.params.iter().cloned().zip(args).collect();
            let (_, sig, _) = def.methods.iter().find(|m| m.0 == name)?;
            Some(Sig {
                params: sig
                    .params
                    .iter()
                    .map(|p| ParamSig {
                        ty: p.ty.substitute(&map),
                        ..p.clone()
                    })
                    .collect(),
                ret: sig.ret.as_ref().map(|r| r.substitute(&map)),
            })
        })
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
                self.check_arg(source, &params[i].ty, params[i].is_ref, params[i].sharing);
            } else if let Some(r) = rest {
                filled[r] = true;
                self.check_arg(source, &params[r].ty, false, Sharing::No);
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
                    self.check_arg(source, &params[i].ty, params[i].is_ref, params[i].sharing);
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
                return self.variant_call(id, &t, name, args, span, None);
            }
            let key = format!("{t}.{name}");
            if self.funcs.get(&key).is_some_and(|f| f.this.is_none()) {
                self.check_private_method(&key, &t, span);
                self.info.resolved.insert(id, Resolved::Static(key.clone()));
                return self.call_named(&key, args, span);
            }
            return self.unknown_method(args, span);
        }
        let recv_ty = if MUTATING.contains(&name) {
            // Typed as a write path (`grid[i]` is a list, not a maybe), but only a collection's
            // method changes it.
            let (t, _) = self.place_path(recv);
            (self.through_shared, self.place_root) = (false, None);
            if self.is_collection(&t) {
                self.changed_receiver(recv, Code::change_let_collection);
            }
            t
        } else {
            self.expr(recv, None)
        };
        let recv_ty = if matches!(name, "orPanic" | "orErr" | "ok" | "flatten") {
            recv_ty
        } else {
            self.try_operand(recv, recv_ty)
        };
        let boxed = matches!(self.subst.shallow(&recv_ty), Ty::Named(n, _) if n == SHARED);
        let mut recv_ty = self.unref(&recv_ty);
        // A method that changes an element is a write through the index, as `xs[i].n = 1` is.
        if let (ExprKind::Index(..), Ty::Maybe(inner)) = (&recv.kind, self.subst.shallow(&recv_ty))
            && self.changing_method(&inner, name)
        {
            recv_ty = self.subst.shallow(&inner);
        }
        if self.is_interface(&recv_ty) || matches!(recv_ty, Ty::Param(_)) {
            let Some(sig) = self.interface_method(&recv_ty, name) else {
                return self.unknown_method(args, span);
            };
            self.match_args(&sig.params, args, span);
            self.info
                .resolved
                .insert(id, Resolved::Dynamic(name.to_string()));
            return sig.ret.unwrap_or(Ty::Void);
        }
        if let Some(tag) = self.tag(&recv_ty) {
            let key = self.method_key(&tag, name, &recv_ty);
            if self.funcs.get(&key).is_some_and(|f| f.this.is_some()) {
                self.check_private_method(&key, &tag, span);
                self.info.resolved.insert(id, Resolved::Method(key.clone()));
                let ret = self.call_entry(&key, Some(&recv_ty), args, span);
                let f = &self.funcs[&key];
                if f.changes_this {
                    if !boxed {
                        self.changed_receiver(recv, Code::changes_let);
                    }
                } else if f.state == FuncState::InProgress
                    && !boxed
                    && let Some(caller) = self.current_method.clone()
                {
                    let before = self.diags.len();
                    let (_, _, through, _) = self.place_parts(recv);
                    self.diags.truncate(before);
                    if !through && ref_path(recv).is_some_and(|(root, _)| root == "this") {
                        self.pending_changes.push((caller, key));
                    }
                }
                return ret;
            }
        }
        match recv_ty {
            Ty::Named(n, targs) if n == MAP => self.map_method(targs, name, args, span),
            Ty::Named(n, mut targs) if n == SET => {
                self.set_method(targs.remove(0), name, args, span)
            }
            Ty::Named(n, mut targs) if n == RESULT => match name {
                "orPanic" => {
                    if !args.is_empty() {
                        self.args_of(args, span, &[Ty::Str]);
                    }
                    targs.swap_remove(0)
                }
                "ok" => {
                    self.args_of(args, span, &[]);
                    Ty::maybe(targs.swap_remove(0))
                }
                _ => self.unknown_method(args, span),
            },
            Ty::Named(..) | Ty::Str | Ty::Int | Ty::Float | Ty::Bool => {
                self.unknown_method(args, span)
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
                "orErr" => {
                    let e = self.subst.fresh();
                    self.args_of(args, span, std::slice::from_ref(&e));
                    Ty::Named(RESULT.to_string(), vec![*inner, e])
                }
                _ => self.unknown_method(args, span),
            },
            _ => self.unknown_method(args, span),
        }
    }

    fn map_method(&mut self, targs: Vec<Ty>, name: &str, args: &[Arg], span: Span) -> Ty {
        let (key, value) = (targs[0].clone(), targs[1].clone());
        match name {
            "contains" => {
                self.args_of(args, span, &[key]);
                Ty::Bool
            }
            "remove" => {
                self.args_of(args, span, &[key]);
                Ty::maybe(value)
            }
            "keys" => {
                self.args_of(args, span, &[]);
                Ty::list(key)
            }
            "values" => {
                self.args_of(args, span, &[]);
                Ty::list(value)
            }
            "length" => {
                self.args_of(args, span, &[]);
                Ty::Int
            }
            "isEmpty" => {
                self.args_of(args, span, &[]);
                Ty::Bool
            }
            _ => self.unknown_method(args, span),
        }
    }

    fn set_method(&mut self, item: Ty, name: &str, args: &[Arg], span: Span) -> Ty {
        match name {
            "add" => {
                self.args_of(args, span, &[item]);
                Ty::Void
            }
            "remove" | "contains" => {
                self.args_of(args, span, &[item]);
                Ty::Bool
            }
            "length" => {
                self.args_of(args, span, &[]);
                Ty::Int
            }
            "isEmpty" => {
                self.args_of(args, span, &[]);
                Ty::Bool
            }
            _ => self.unknown_method(args, span),
        }
    }

    /// Checks the arguments anyway, so their own errors show too.
    fn unknown_method(&mut self, args: &[Arg], span: Span) -> Ty {
        for a in args {
            self.expr(&a.value, None);
        }
        self.error(Code::unknown_method, span)
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
            "enumerate" => {
                self.args_of(args, span, &[]);
                Ty::list(Ty::Record(Vec::new(), vec![Ty::Int, elem]))
            }
            "join" => {
                if !self.subst.unify(&elem, &Ty::Str) {
                    return self.unknown_method(args, span);
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
            _ => self.unknown_method(args, span),
        }
    }

    fn index(&mut self, id: NodeId, list: &Expr, index: &Expr, span: Span) -> Ty {
        let lt = self.expr(list, None);
        let lt = self.unref(&lt);
        let key = match &lt {
            Ty::Named(n, a) if n == MAP => a[0].clone(),
            _ => Ty::Int,
        };
        let it = self.expr(index, Some(&key));
        self.expect(index, &it, &key);
        let elem = match lt {
            Ty::List(e) => *e,
            Ty::Named(n, mut a) if n == MAP => a.remove(1),
            Ty::Error => return Ty::Error,
            Ty::Var(_) => return self.error(Code::type_needed, list.span),
            _ => return self.error(Code::invalid_operand_types, span),
        };
        if self.index_narrowed(list, index) {
            self.info.unwrapped_index.insert(id);
            return elem;
        }
        Ty::maybe(elem)
    }

    fn index_narrowed(&self, list: &Expr, index: &Expr) -> bool {
        let ExprKind::Name(list_name) = &list.kind else {
            return false;
        };
        let known_length = |k: i64| {
            self.lookup(list_name)
                .and_then(|v| v.list_len)
                .is_some_and(|n| k >= 0 && (k as usize) < n)
        };
        match &index.kind {
            ExprKind::Int(k) => known_length(*k),
            ExprKind::Name(index_name) => self.narrowed.iter().any(|(l, i, depth)| {
                l == list_name
                    && i == index_name
                    && [l, i]
                        .iter()
                        .all(|n| self.scope_of(n).is_some_and(|s| s <= *depth))
            }),
            _ => false,
        }
    }

    fn compound_on_missing_index(&mut self, target: &Expr, ty: &Ty, value: &Expr) {
        let mut d = Diagnostic::error(Code::compound_on_missing_index, target.span);
        let zero = match self.subst.shallow(ty) {
            Ty::Int => Some("0"),
            Ty::Float => Some("0.0"),
            _ => None,
        };
        if let Some(zero) = zero {
            let place = self.sources.text(target.span);
            let op = self
                .sources
                .text(Span::new(target.span.end, value.span.start));
            let op = op.trim().trim_end_matches('=');
            let value_text = self.sources.text(value.span);
            d = d.with_quick_fix(vec![Edit {
                span: Span::new(target.span.start, value.span.end),
                text: format!("{place} = ({place} ?? {zero}) {op} {value_text}"),
            }]);
        }
        self.diags.push(d);
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
                    mutable: p.is_var || p.is_ref,
                    kind: VarKind::Param,
                    span: p.span,
                    reassigned: false,
                    list_len: None,
                },
            );
        }
        let expected_maybe = match expected.map(|e| self.subst.shallow(e)) {
            Some(Ty::Func(_, r)) if matches!(self.subst.shallow(&r), Ty::Maybe(_)) => Some(*r),
            _ => None,
        };
        let declared = ret.map(|t| self.type_from(t)).or(expected_maybe);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_flow = self.flow.clone();
        let saved_try = std::mem::take(&mut self.in_try);
        self.flow.dead = false;
        self.lambdas.push((self.scopes.len(), BTreeSet::new()));
        self.scopes.push(scope);
        let ret = self.function_body(body, declared);
        self.finish_scope();
        self.loops = saved_loops;
        self.flow = saved_flow;
        self.in_try = saved_try;
        let (_, captures) = self.lambdas.pop().expect("lambda");
        self.info
            .captures
            .insert(id, captures.into_iter().collect());
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
        let before = self.flow.clone();
        let tt = self.then_branch(cond, then, |c| c.expr(then, expected));
        let after_then = std::mem::replace(&mut self.flow, before);
        let ot = otherwise.map(|other| self.expr(other, expected));
        self.flow = after_then.join(std::mem::take(&mut self.flow));
        match (otherwise, ot) {
            (Some(other), Some(ot)) => match self.join((then, tt), (other, ot)) {
                Some(t) => t,
                None => self.error(Code::branch_type_mismatch, expr.span),
            },
            _ => self.optional_value(expr, tt),
        }
    }

    /// Checks an `if`'s condition, then runs `f` on its `then` branch with what the
    /// condition binds and narrows.
    fn then_branch<T>(&mut self, cond: &Expr, then: &Expr, f: impl FnOnce(&mut Self) -> T) -> T {
        let c = self.expr(cond, Some(&Ty::Bool));
        self.expect(cond, &c, &Ty::Bool);
        self.push_bindings(self.exports(cond));
        let narrowed = self.contains_check(cond, then);
        if let Some(n) = &narrowed {
            self.narrowed.push(n.clone());
        }
        let t = f(self);
        if narrowed.is_some() {
            self.narrowed.pop();
        }
        self.finish_scope();
        t
    }

    /// The map and key that `if m.contains(k)` narrows `m[k]` for in `then`, when neither
    /// changes there.
    fn contains_check(&self, cond: &Expr, then: &Expr) -> Option<(String, String, usize)> {
        let ExprKind::Method(recv, method, args) = &cond.kind else {
            return None;
        };
        let (ExprKind::Name(m), [arg]) = (&recv.kind, args.as_slice()) else {
            return None;
        };
        let ExprKind::Name(k) = &arg.value.kind else {
            return None;
        };
        let is_map = self
            .lookup(m)
            .is_some_and(|v| matches!(self.subst.shallow(&v.ty), Ty::Named(n, _) if n == MAP));
        (method == "contains"
            && is_map
            && !mutates(then, m, &self.ref_callees)
            && !mutates(then, k, &self.ref_callees))
        .then(|| (m.clone(), k.clone(), self.scopes.len() - 1))
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
        let before = self.flow.clone();
        let mut after: Option<Flow> = None;
        for arm in arms {
            self.flow = before.clone();
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
            self.finish_scope();
            let flow = std::mem::take(&mut self.flow);
            after = Some(match after {
                Some(a) => a.join(flow),
                None => flow,
            });
            bodies.push((&arm.body, t));
        }
        self.flow = after.unwrap_or(before);
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
                    Ty::Named(e, _) if self.variant(&e, name).is_some() => Some(e),
                    _ => None,
                };
                if pat.is_var && (enum_name.is_some() || name == "None") {
                    self.error(Code::unexpected_token, pat.span);
                }
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
                    binds.push((name.clone(), pat.span, ty.clone(), pat.is_var));
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
                let class_ty = self.fresh_type(name);
                if !self.subst.unify(ty, &class_ty) {
                    self.error(Code::type_mismatch, pat.span);
                    return;
                }
                self.class_pattern(name, &Self::type_args(&class_ty), args, pat.span, binds);
            }
            PatternKind::Variant(qualifier, name, args) => {
                let (enum_name, enum_args) = match (qualifier, self.subst.shallow(ty)) {
                    (Some(q), t) => {
                        let enum_ty = self.fresh_type(q);
                        if !self.subst.unify(&t, &enum_ty) {
                            self.error(Code::type_mismatch, pat.span);
                            return;
                        }
                        (q.clone(), Self::type_args(&enum_ty))
                    }
                    (None, Ty::Named(e, args)) => (e, args),
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
                    .variant_fields(&enum_name, &enum_args, name)
                    .map(|fs| fs.into_iter().map(|f| f.ty).collect::<Vec<_>>())
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
                                && expected.iter().all(|(n, _, t, is_var)| {
                                    alt_binds.iter().find(|b| &b.0 == n).is_some_and(|b| {
                                        b.3 == *is_var && self.subst.unify(t, &b.2)
                                    })
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
                Ty::Named(class, _) if self.rest_field(&class).is_some() => {
                    self.error(Code::wrong_arguments, pat.span);
                }
                Ty::Named(class, args) if self.class(&class).is_some() => {
                    self.class_pattern(&class, &args, items, pat.span, binds);
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
        args: &[Ty],
        items: &[Pattern],
        span: Span,
        binds: &mut Vec<Binding>,
    ) {
        let fields = self
            .class(class)
            .map(|c| c.fields.clone())
            .unwrap_or_default();
        let map = self.args_map(class, args);
        let tys: Vec<Ty> = fields.iter().map(|f| f.ty.substitute(&map)).collect();
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
                    binds.push((name.clone(), item.span, Ty::list(elem.clone()), item.is_var));
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
            Ty::Named(e, args) => {
                let map = self.args_map(&e, &args);
                match self.types.get(&e) {
                    Some(TypeDef::Enum(vs)) => Some(
                        vs.iter()
                            .map(|v| {
                                (
                                    v.name.clone(),
                                    v.fields.iter().map(|f| f.ty.substitute(&map)).collect(),
                                )
                            })
                            .collect(),
                    ),
                    Some(TypeDef::Class(c)) => {
                        let mut fields = Vec::new();
                        for f in &c.fields {
                            if f.is_rest {
                                fields.push(Ty::list(f.ty.substitute(&map)));
                                break;
                            }
                            fields.push(f.ty.substitute(&map));
                        }
                        Some(vec![(e, fields)])
                    }
                    None => None,
                }
            }
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
        // Splitting a column no pattern looks into would never end on a recursive type.
        let no_ctors = !expanded.is_empty() && expanded.iter().all(|r| self.irrefutable_cell(r[0]));
        let ctors = if no_ctors {
            None
        } else {
            self.constructors(ty)
        };
        let Some(ctors) = ctors else {
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
            break_flow: None,
        });
        let before = self.flow.clone();
        self.unused(body);
        let ctx = self.loops.pop().expect("loop context");
        let after = std::mem::take(&mut self.flow).join(before);
        self.flow = match ctx.break_flow.clone() {
            Some(b) => after.join(b),
            None => after,
        };
        debug_assert!(ctx.kind == LoopKind::Valued);
        match ctx.break_ty.map(|t| self.subst.shallow(&t)) {
            None | Some(Ty::Void) | Some(Ty::Error) => Ty::Void,
            Some(t) => {
                self.info.wrapping.insert(id);
                Ty::maybe(t)
            }
        }
    }

    fn check_overlap(&mut self, recv: Option<&Expr>, args: &[Arg]) {
        let (refs, plain): (Vec<&Arg>, Vec<&Arg>) = args
            .iter()
            .partition(|a| self.info.ref_args.contains_key(&a.value.id));
        let refs: Vec<(&Expr, RefPath)> = refs
            .into_iter()
            .filter_map(|a| Some((&a.value, ref_path(&a.value)?)))
            .collect();
        for (i, (e, path)) in refs.iter().enumerate() {
            let mut readers: Vec<&Expr> = recv.into_iter().collect();
            readers.extend(plain.iter().map(|a| &a.value));
            for (j, (other, _)) in refs.iter().enumerate() {
                if j != i {
                    index_exprs(other, &mut readers);
                }
            }
            let reads = readers.iter().any(|r| uses_path(r, path).is_some());
            if reads || refs[..i].iter().any(|(_, p)| overlaps(p, path)) {
                self.error(Code::overlapping_ref, e.span);
            }
        }
    }

    /// Warns about a `var` copied from a field, index or map lookup whose last change is never
    /// read. Reads in a loop or branch with the change count as after it, and a var a lambda
    /// captures never warns.
    fn set_in_branches(&mut self, stmts: &[Stmt]) {
        for pair in stmts.windows(2) {
            let [
                Stmt::Let {
                    name,
                    span,
                    ty,
                    value: None,
                    ..
                },
                Stmt::Expr(e),
            ] = pair
            else {
                continue;
            };
            let mut sets = Vec::new();
            if !branch_sets(e, name, &mut sets) {
                continue;
            }
            let decl_end = ty.as_ref().map_or(span.end, |t| type_span(t).end);
            let mut fix = vec![Edit {
                span: Span::new(decl_end, e.span.start),
                text: " = ".to_string(),
            }];
            fix.extend(sets.into_iter().map(|span| Edit {
                span,
                text: String::new(),
            }));
            self.diags
                .push(Diagnostic::warning(Code::set_in_every_branch, *span).with_fix(fix));
        }
    }

    fn changed_copies(&mut self, stmts: &[Stmt]) {
        for (i, stmt) in stmts.iter().enumerate() {
            let Stmt::Let {
                keyword,
                name,
                is_var: true,
                is_shared: false,
                ty,
                value: Some(value),
                ..
            } = stmt
            else {
                continue;
            };
            let Some(copied) = copied_path(value) else {
                continue;
            };
            let shared = self
                .scopes
                .last()
                .and_then(|s| s.get(name))
                .is_none_or(|v| {
                    let t = self.subst.shallow(&v.ty);
                    self.is_shared_class(&t) || matches!(t, Ty::Named(n, _) if n == SHARED)
                });
            if shared {
                continue;
            }
            let mut unread = None;
            for s in &stmts[i + 1..] {
                let uses = self.copy_uses(name, s);
                if uses.captured {
                    unread = None;
                    break;
                }
                unread = match uses.change {
                    Some(_) if uses.read && uses.branches => None,
                    Some(change) => Some(change),
                    None if uses.read => None,
                    None => unread,
                };
            }
            let Some(change) = unread else {
                continue;
            };
            let mut d = Diagnostic::warning(Code::changed_copy, change);
            let before = self.diags.len();
            let (_, err, _, _) = self.place_parts(copied);
            self.diags.truncate(before);
            // In a method, a `ref` binding to a path through `this` is fine: the method then
            // changes `this`.
            let bindable = matches!(err, None | Some(Code::changes_let));
            if bindable && ty.is_none() && std::ptr::eq(copied, value) {
                d = d.with_quick_fix(vec![Edit {
                    span: *keyword,
                    text: "ref".to_string(),
                }]);
            }
            self.diags.push(d);
        }
    }

    /// How statement `s` uses the var `name`: its last change's span (a write into it, or a call
    /// that changes it), whether it reads it otherwise, and whether it can run its parts in
    /// another order (a loop or branch).
    fn copy_uses(&self, name: &str, s: &Stmt) -> CopyUses {
        let mut changed_roots: Vec<*const Expr> = Vec::new();
        let mut uses = CopyUses::default();
        let exprs = stmt_exprs(s);
        if let Stmt::Assign { target, .. } = s
            && let Some(root) = steps_root(target, name)
        {
            changed_roots.push(root);
            uses.change = Some(target.span);
        }
        for e in &exprs {
            find_expr(e, &mut |x, assigned| {
                match &x.kind {
                    ExprKind::Method(recv, m, _) if self.changes_receiver(x.id, m) => {
                        if let Some(root) = path_root(recv, name) {
                            changed_roots.push(root);
                            uses.change = Some(x.span);
                        }
                    }
                    ExprKind::Lambda(_, _, body) => {
                        uses.captured |= find_expr(body, &mut |y, _| is_name(y, name)).is_some();
                    }
                    ExprKind::If(..)
                    | ExprKind::Match(..)
                    | ExprKind::While(..)
                    | ExprKind::Loop(_)
                    | ExprKind::For(..) => uses.branches = true,
                    _ if assigned => {
                        if let Some(root) = steps_root(x, name) {
                            changed_roots.push(root);
                            uses.change = Some(x.span);
                        }
                    }
                    _ => {}
                }
                false
            });
        }
        for e in &exprs {
            find_expr(e, &mut |x, _| {
                uses.read |= is_name(x, name) && !changed_roots.contains(&(x as *const Expr));
                false
            });
        }
        uses
    }

    fn changes_receiver(&self, id: NodeId, method: &str) -> bool {
        match self.info.resolved.get(&id) {
            Some(Resolved::Method(key)) => self.funcs.get(key).is_some_and(|f| f.changes_this),
            _ => MUTATING.contains(&method),
        }
    }

    /// `ref name = target` lasts until its last use. Until then, the rest of its block reaches
    /// the target only through `name`, and no lambda captures `name`.
    fn ref_binding(&mut self, stmt: &Stmt, rest: &[Stmt]) {
        let Stmt::RefLet { name, target, .. } = stmt else {
            return;
        };
        let live = ref_end(name, rest).map_or(0, |k| k + 1);
        self.info.ref_ends.insert(target.id, live);
        let Some(path) = ref_path(target) else {
            return;
        };
        for e in rest[..live].iter().flat_map(stmt_exprs) {
            if let Some(hit) = uses_path(e, &path) {
                self.error(Code::ref_binding_overlap, hit.span);
            }
            let captures = find_expr(e, &mut |x, _| {
                let ExprKind::Lambda(_, _, body) = &x.kind else {
                    return false;
                };
                find_expr(
                    body,
                    &mut |y, _| matches!(&y.kind, ExprKind::Name(n) if n == name),
                )
                .is_some()
            });
            if let Some(hit) = captures {
                self.error(Code::ref_binding_captured, hit.span);
            }
        }
    }

    fn for_ref(&mut self, id: NodeId, pattern: &Pattern, iter: &Expr, body: &Expr) -> Ty {
        let (t, err) = self.place(iter);
        let shared = matches!(self.subst.shallow(&t), Ty::Named(n, _) if n == SHARED);
        if let Some(code) = err.filter(|_| !shared) {
            self.error(code, iter.span);
        }
        let elem = match self.unref(&t) {
            Ty::List(e) => *e,
            Ty::Error => Ty::Error,
            _ => self.error(Code::not_iterable, iter.span),
        };
        let PatternKind::Name(name) = &pattern.kind else {
            return self.error(Code::unexpected_token, pattern.span);
        };
        self.push_bindings(vec![(name.clone(), pattern.span, elem, true)]);
        if let Some(var) = self.scopes.last_mut().and_then(|s| s.get_mut(name)) {
            var.reassigned = true;
        }
        let ty = self.valued_loop(id, body);
        self.finish_scope();
        // After the body, so its calls are resolved.
        let changed = |call: &Expr, arg: Option<&Expr>| match (arg, &call.kind) {
            (Some(a), _) => self.info.ref_args.contains_key(&a.id),
            (None, ExprKind::Method(_, m, _)) => self.changes_receiver(call.id, m),
            (None, _) => false,
        };
        let hit = ref_path(iter).and_then(|path| {
            changes(body, &changed, &|e| {
                ref_path(e).is_some_and(|p| overlaps(&p, &path))
            })
        });
        if let Some(hit) = hit {
            self.error(Code::changed_in_for_ref, hit.span);
        }
        ty
    }

    fn for_loop(&mut self, id: NodeId, pattern: &Pattern, iter: &Expr, body: &Expr) -> Ty {
        let it = self.expr(iter, None);
        let elem = match self.subst.shallow(&it) {
            Ty::Range => Ty::Int,
            Ty::List(e) => *e,
            Ty::Named(n, a) if n == MAP => Ty::Record(Vec::new(), a),
            Ty::Named(n, mut a) if n == SET => a.remove(0),
            Ty::Error => Ty::Error,
            _ => self.error(Code::not_iterable, iter.span),
        };
        let mut binds = Vec::new();
        self.pattern(pattern, &elem, &mut binds);
        self.report_missing(vec![vec![Cell::Pat(pattern)]], &elem, pattern.span);
        let narrowed_list = match &pattern.kind {
            PatternKind::Name(name) => narrowing_list(iter)
                .filter(|list| !mutates(body, list, &self.ref_callees))
                .map(|list| (list, name.clone(), self.scopes.len())),
            _ => None,
        };
        if let Some(narrowed) = &narrowed_list {
            self.narrowed.push(narrowed.clone());
        }
        self.push_bindings(binds);
        let ty = self.valued_loop(id, body);
        self.finish_scope();
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

/// Runs before `expr` is checked, so calls are matched by name only: any argument to a function
/// or method with a `ref` parameter counts as changed.
fn mutates(expr: &Expr, list: &str, ref_callees: &HashSet<String>) -> bool {
    let changed = |call: &Expr, arg: Option<&Expr>| match (arg, &call.kind) {
        (Some(_), ExprKind::Method(_, m, _)) => ref_callees.contains(m),
        (Some(_), ExprKind::Call(f, _)) => {
            matches!(&f.kind, ExprKind::Name(n) if ref_callees.contains(n))
        }
        (None, ExprKind::Method(_, m, _)) => MUTATING.contains(&m.as_str()),
        _ => false,
    };
    changes(
        expr,
        &changed,
        &|e| matches!(&e.kind, ExprKind::Name(n) if n == list),
    )
    .is_some()
}

/// `changed(call, Some(arg))` says whether a call changes that argument, and
/// `changed(call, None)` whether it changes its receiver.
fn changes<'a>(
    expr: &'a Expr,
    changed: &dyn Fn(&Expr, Option<&Expr>) -> bool,
    is_target: &dyn Fn(&Expr) -> bool,
) -> Option<&'a Expr> {
    find_expr(expr, &mut |e, assigned| {
        let passes =
            |args: &[Arg]| (args.iter()).any(|a| is_target(&a.value) && changed(e, Some(&a.value)));
        (assigned && is_target(e))
            || match &e.kind {
                ExprKind::Method(recv, _, args) => {
                    (is_target(recv) && changed(e, None)) || passes(args)
                }
                ExprKind::Call(_, args) => passes(args),
                ExprKind::For(_, iter, _, true) => is_target(iter),
                _ => false,
            }
    })
}

fn find_expr<'a>(e: &'a Expr, hit: &mut dyn FnMut(&Expr, bool) -> bool) -> Option<&'a Expr> {
    if hit(e, false) {
        return Some(e);
    }
    let mut kids: Vec<&Expr> = Vec::new();
    match &e.kind {
        ExprKind::Method(recv, _, args) | ExprKind::Call(recv, args) => {
            kids.push(recv);
            kids.extend(args.iter().map(|a| &a.value));
        }
        ExprKind::Record(args) => kids.extend(args.iter().map(|a| &a.value)),
        ExprKind::Block(stmts) => {
            for s in stmts {
                match s {
                    Stmt::Assign { target, value, .. } => {
                        if hit(target, true) {
                            return Some(target);
                        }
                        kids.push(target);
                        kids.push(value);
                    }
                    Stmt::Let { value, .. } => kids.extend(value),
                    Stmt::LetPattern { value, .. } => kids.push(value),
                    Stmt::RefLet { target, .. } => kids.push(target),
                    Stmt::Expr(x) => kids.push(x),
                    _ => {}
                }
            }
        }
        ExprKind::Str(segs) => kids.extend(segs.iter().filter_map(|s| match s {
            StrSeg::Expr(x) => Some(x),
            _ => None,
        })),
        ExprKind::List(items) => kids.extend(items),
        ExprKind::Map(entries) => {
            for (k, v) in entries {
                kids.push(k);
                kids.push(v);
            }
        }
        ExprKind::Unary(_, x)
        | ExprKind::Loop(x)
        | ExprKind::Field(x, _)
        | ExprKind::Is(x, _)
        | ExprKind::SharedOf(x)
        | ExprKind::Spread(x) => kids.push(x),
        ExprKind::Lambda(_, _, x) => kids.push(x),
        ExprKind::Match(subject, arms) => {
            kids.push(subject);
            for a in arms {
                kids.push(&a.body);
                kids.extend(&a.guard);
            }
        }
        ExprKind::Binary(_, a, b)
        | ExprKind::Index(a, b)
        | ExprKind::While(a, b)
        | ExprKind::For(_, a, b, _) => {
            kids.push(a);
            kids.push(b);
        }
        ExprKind::If(c, t, o) => {
            kids.push(c);
            kids.push(t);
            kids.extend(o.as_deref());
        }
        ExprKind::Break(v) | ExprKind::Return(v) => kids.extend(v.as_deref()),
        _ => {}
    }
    kids.into_iter().find_map(|k| find_expr(k, hit))
}

type RefPath<'a> = (&'a str, Vec<&'a str>);

fn ref_path(e: &Expr) -> Option<RefPath<'_>> {
    match &e.kind {
        ExprKind::Name(n) => Some((n, Vec::new())),
        ExprKind::This => Some(("this", Vec::new())),
        ExprKind::Field(base, name) => {
            let (root, mut steps) = ref_path(base)?;
            steps.push(name);
            Some((root, steps))
        }
        ExprKind::Index(base, _) => {
            let (root, mut steps) = ref_path(base)?;
            steps.push("[]");
            Some((root, steps))
        }
        _ => None,
    }
}

/// The first path in `e` that overlaps `path`, not counting the shorter paths inside a path.
fn uses_path<'a>(e: &'a Expr, path: &RefPath) -> Option<&'a Expr> {
    let mut bases = Vec::new();
    find_expr(e, &mut |x, _| {
        if bases.contains(&(x as *const Expr)) {
            return false;
        }
        let Some(p) = ref_path(x) else {
            return false;
        };
        path_bases(x, &mut bases);
        overlaps(&p, path)
    })
}

/// The index in `rest` of the last statement that uses `name`, counting uses of the `ref`
/// bindings made from it, which write back into it.
fn ref_end(name: &str, rest: &[Stmt]) -> Option<usize> {
    let mut end = None;
    for (j, s) in rest.iter().enumerate() {
        let mentions = |e: &Expr| {
            find_expr(
                e,
                &mut |x, _| matches!(&x.kind, ExprKind::Name(n) if n == name),
            )
            .is_some()
        };
        if stmt_exprs(s).into_iter().any(mentions) {
            end = Some(j);
        }
        if let Stmt::RefLet {
            name: inner,
            target,
            ..
        } = s
            && ref_path(target).is_some_and(|(root, _)| root == name)
            && let Some(k) = ref_end(inner, &rest[j + 1..])
        {
            end = Some(j + 1 + k);
        }
    }
    end
}

#[derive(Default)]
struct CopyUses {
    change: Option<Span>,
    read: bool,
    branches: bool,
    captured: bool,
}

/// The field, index or map lookup `e` copies, also through `??`, `try` and `.orPanic`.
fn copied_path(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        ExprKind::Field(..) | ExprKind::Index(..) if ref_path(e).is_some() => Some(e),
        ExprKind::Binary(BinOp::Coalesce, x, _) | ExprKind::Unary(UnOp::Try, x) => copied_path(x),
        ExprKind::Method(x, m, _) if m == "orPanic" => copied_path(x),
        _ => None,
    }
}

fn failure_of(t: &Ty) -> Option<(Ty, Fails)> {
    match t {
        Ty::Maybe(inner) => Some(((**inner).clone(), Fails::Maybe)),
        Ty::Named(n, args) if n == RESULT => {
            Some((args[0].clone(), Fails::Result(args[1].clone())))
        }
        _ => None,
    }
}

fn mentions(e: &Expr, name: &str) -> bool {
    find_expr(e, &mut |x, _| is_name(x, name)).is_some()
}

fn branch_sets(e: &Expr, name: &str, sets: &mut Vec<Span>) -> bool {
    match &e.kind {
        ExprKind::If(cond, then, Some(other)) => {
            !mentions(cond, name)
                && block_sets(then, name, sets)
                && (branch_sets(other, name, sets) || block_sets(other, name, sets))
        }
        ExprKind::Match(subject, arms) => {
            !mentions(subject, name)
                && arms.iter().all(|a| {
                    a.guard.as_ref().is_none_or(|g| !mentions(g, name))
                        && block_sets(&a.body, name, sets)
                })
        }
        _ => false,
    }
}

fn block_sets(body: &Expr, name: &str, sets: &mut Vec<Span>) -> bool {
    let ExprKind::Block(stmts) = &body.kind else {
        return false;
    };
    let [
        Stmt::Assign {
            target,
            op: None,
            value,
            ..
        },
    ] = stmts.as_slice()
    else {
        return false;
    };
    let clean = is_name(target, name) && !mentions(value, name);
    if clean {
        sets.push(Span::new(target.span.start, value.span.start));
    }
    clean
}

fn is_name(e: &Expr, name: &str) -> bool {
    matches!(&e.kind, ExprKind::Name(n) if n == name)
}

/// The `name` a path starts from, if `e` is a path rooted in it.
fn path_root<'a>(e: &'a Expr, name: &str) -> Option<&'a Expr> {
    match &e.kind {
        ExprKind::Field(base, _) | ExprKind::Index(base, _) => path_root(base, name),
        _ => is_name(e, name).then_some(e),
    }
}

/// Like `path_root`, for a path with at least one field or index after the name.
fn steps_root<'a>(e: &'a Expr, name: &str) -> Option<&'a Expr> {
    match &e.kind {
        ExprKind::Field(base, _) | ExprKind::Index(base, _) => path_root(base, name),
        _ => None,
    }
}

fn stmt_exprs(s: &Stmt) -> Vec<&Expr> {
    match s {
        Stmt::Assign { target, value, .. } => vec![target, value],
        Stmt::Let { value, .. } => value.iter().collect(),
        Stmt::LetPattern {
            value, otherwise, ..
        } => std::iter::once(value).chain(otherwise).collect(),
        Stmt::RefLet { target, .. } => vec![target],
        Stmt::Expr(e) => vec![e],
        _ => Vec::new(),
    }
}

fn overlaps(a: &RefPath, b: &RefPath) -> bool {
    a.0 == b.0 && a.1.iter().zip(&b.1).all(|(x, y)| x == y)
}

/// The shorter paths inside a path, which a read of the whole path doesn't read on its own.
fn path_bases(e: &Expr, out: &mut Vec<*const Expr>) {
    if let ExprKind::Field(base, _) | ExprKind::Index(base, _) = &e.kind {
        out.push(&**base);
        path_bases(base, out);
    }
}

fn index_exprs<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    match &e.kind {
        ExprKind::Field(base, _) => index_exprs(base, out),
        ExprKind::Index(base, index) => {
            out.push(index);
            index_exprs(base, out);
        }
        _ => {}
    }
}

fn type_span(t: &TypeExpr) -> Span {
    match t {
        TypeExpr::Name(_, span) | TypeExpr::Generic(_, _, span) | TypeExpr::Record(_, _, span) => {
            *span
        }
        TypeExpr::Maybe(inner) => type_span(inner),
        TypeExpr::Func(params, _) => params.first().map_or(Span::default(), type_span),
    }
}
