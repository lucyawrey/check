use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::*;
use crate::checker::{BUILT_IN_TYPES, THIS};
use crate::diag::{Code, Diagnostic, Span};

pub struct SourceFile {
    pub unit: usize,
    pub is_library: bool,
    pub namespace: String,
    pub stmts: Vec<Stmt>,
}

#[derive(Clone)]
struct Decl {
    /// The name the checker and interpreter know it by.
    key: String,
    unit: usize,
    is_private: bool,
    is_type: bool,
    is_library: bool,
}

/// `access[u]` lists the units code in unit `u` sees, whether it sees their private names, and
/// the unit that passed each one on, if any. `labels[u]` is how unit `u` was imported.
pub fn resolve(
    files: &mut [SourceFile],
    access: &[Vec<(usize, bool, Option<usize>)>],
    labels: &[String],
    diags: &mut Vec<Diagnostic>,
) {
    let mut decls: HashMap<String, Vec<Decl>> = HashMap::new();
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut namespaces = HashSet::new();
    let mut roots: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut variants: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for file in files.iter() {
        if let Some(root) = file.namespace.split('.').next().filter(|r| !r.is_empty())
            && !file.is_library
        {
            roots.entry(root.to_string()).or_default().insert(file.unit);
        }
        let mut ns = file.namespace.as_str();
        while !ns.is_empty() {
            namespaces.insert(ns.to_string());
            ns = ns.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        for stmt in &file.stmts {
            let (name, is_private, is_type) = match stmt {
                Stmt::Func(d) => (&d.name, d.is_private, false),
                Stmt::Class(d) => (&d.name, d.is_private, true),
                Stmt::Interface(d) => (&d.name, d.is_private, true),
                Stmt::Data(d) => (&d.name, d.is_private, false),
                Stmt::Enum(d) => {
                    for v in &d.variants {
                        let list = variants.entry(file.namespace.clone()).or_default();
                        list.push((file.unit, v.name.clone()));
                    }
                    (&d.name, d.is_private, true)
                }
                _ => continue,
            };
            let key = qualify(&file.namespace, name);
            let same = decls.entry(key.clone()).or_default();
            if same.is_empty() {
                by_name.entry(name.clone()).or_default().push(key.clone());
            }
            same.push(Decl {
                key: key.clone(),
                unit: file.unit,
                is_private,
                is_type,
                is_library: file.is_library,
            });
        }
    }
    // Private names never clash across units, so each one that shares a name gets its own key.
    for same in decls.values_mut() {
        if same.len() > 1 {
            for d in same.iter_mut().filter(|d| d.is_private) {
                d.key = format!("{}@{}", d.key, d.unit);
            }
        }
    }
    for file in files.iter_mut() {
        let sees_unit = |u: &usize| access[file.unit].iter().any(|(v, ..)| v == u);
        let own_variants = variants
            .get(&file.namespace)
            .into_iter()
            .flatten()
            .filter(|(u, _)| sees_unit(u))
            .map(|(_, v)| v.clone())
            .collect();
        let user_roots = roots
            .iter()
            .filter(|(_, units)| !file.is_library && units.iter().any(sees_unit))
            .map(|(r, _)| r.clone())
            .collect();
        let mut r = Resolver {
            decls: &decls,
            by_name: &by_name,
            namespaces: &namespaces,
            user_roots,
            own_variants,
            access: &access[file.unit],
            labels,
            unit: file.unit,
            namespace: &file.namespace,
            scopes: vec![top_level_names(&file.stmts)],
            type_params: HashSet::new(),
            diags,
        };
        for stmt in &mut file.stmts {
            r.declaration(stmt);
        }
    }
}

/// A type as the checker shows it, such as `Box<Int>` or `Int?`.
fn type_text(t: &TypeExpr) -> String {
    let list = |ts: &[TypeExpr]| ts.iter().map(type_text).collect::<Vec<_>>().join(", ");
    match t {
        TypeExpr::Name(n, _) => n.clone(),
        TypeExpr::Generic(n, args, _) => format!("{n}<{}>", list(args)),
        TypeExpr::Maybe(inner) => format!("{}?", type_text(inner)),
        TypeExpr::Func(params, ret) => {
            let ret = ret.as_ref().map(|r| format!(" -> {}", type_text(r)));
            format!("func({}){}", list(params), ret.unwrap_or_default())
        }
        TypeExpr::Record(names, types, _) => {
            let fields: Vec<String> = types
                .iter()
                .enumerate()
                .map(|(i, t)| match names.get(i) {
                    Some(n) => format!("{n}: {}", type_text(t)),
                    None => type_text(t),
                })
                .collect();
            format!("({})", fields.join(", "))
        }
    }
}

fn qualify(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}.{name}")
    }
}

fn top_level_names(stmts: &[Stmt]) -> HashSet<String> {
    let mut names = HashSet::new();
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, .. } | Stmt::RefLet { name, .. } => {
                names.insert(name.clone());
            }
            Stmt::LetPattern { pattern, .. } => bound_names(pattern, &mut names),
            _ => {}
        }
    }
    names
}

fn bound_names(pat: &Pattern, names: &mut HashSet<String>) {
    match &pat.kind {
        PatternKind::Name(n) | PatternKind::Rest(n) => {
            names.insert(n.clone());
        }
        PatternKind::Variant(_, _, subs)
        | PatternKind::Or(subs)
        | PatternKind::Record(subs)
        | PatternKind::List(subs) => {
            for sub in subs {
                bound_names(sub, names);
            }
        }
        PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::Range(..) => {}
    }
}

/// The dotted names of `a.b.c`, if `e` is only names and field reads.
fn path(e: &Expr) -> Option<Vec<&str>> {
    match &e.kind {
        ExprKind::Name(n) => Some(vec![n.as_str()]),
        ExprKind::Field(base, f) => {
            let mut p = path(base)?;
            p.push(f);
            Some(p)
        }
        _ => None,
    }
}

struct Resolver<'a> {
    decls: &'a HashMap<String, Vec<Decl>>,
    by_name: &'a HashMap<String, Vec<String>>,
    namespaces: &'a HashSet<String>,
    /// Namespace roots from user code this file sees, which a local of the same name hides.
    user_roots: HashSet<String>,
    /// Variants of enums in this file's namespace, which count as its own names.
    own_variants: HashSet<String>,
    access: &'a [(usize, bool, Option<usize>)],
    labels: &'a [String],
    unit: usize,
    namespace: &'a str,
    scopes: Vec<HashSet<String>>,
    /// Type parameters in scope, which hide types of the same name.
    type_params: HashSet<String>,
    diags: &'a mut Vec<Diagnostic>,
}

impl Resolver<'_> {
    fn error(&mut self, code: Code, span: Span, detail: Option<String>) {
        let mut d = Diagnostic::error(code, span);
        d.detail = detail;
        self.diags.push(d);
    }

    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    fn bind(&mut self, name: &str, span: Span) {
        let shadows = self
            .lookup(name, false, None)
            .is_some_and(|d| d.key != name && !d.is_library);
        if shadows || self.user_roots.contains(name) {
            self.diags
                .push(Diagnostic::warning(Code::shadowed_name, span));
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string());
    }

    fn bind_pattern(&mut self, pat: &mut Pattern) {
        self.pattern(pat);
        let mut names = HashSet::new();
        bound_names(pat, &mut names);
        for name in names {
            self.bind(&name, pat.span);
        }
    }

    /// The declaration named `name` that this file sees, and whether it sees it if private.
    fn sees(&self, name: &str, types_only: bool) -> Option<(Decl, bool)> {
        let mut found = None;
        for d in self
            .decls
            .get(name)?
            .iter()
            .filter(|d| !types_only || d.is_type)
        {
            let Some((_, private, _)) = self.access.iter().find(|(u, ..)| *u == d.unit) else {
                continue;
            };
            let usable = !d.is_private || *private;
            if usable || found.is_none() {
                found = Some((d.clone(), usable));
            }
        }
        found
    }

    /// What `name` means here. Reports errors only when `span` is given.
    fn lookup(&mut self, name: &str, types_only: bool, span: Option<Span>) -> Option<Decl> {
        let own = qualify(self.namespace, name);
        match self.sees(&own, types_only) {
            Some((d, true)) => return Some(d),
            Some((_, false)) => {
                if let Some(span) = span {
                    self.error(Code::private_name, span, None);
                }
                return None;
            }
            None if self.own_variants.contains(name) => return None,
            None => {}
        }
        let mut found = Vec::new();
        let mut hidden = false;
        for key in self.by_name.get(name).into_iter().flatten() {
            if *key == own {
                continue;
            }
            match self.sees(key, types_only) {
                Some((d, true)) => found.push(d),
                Some((_, false)) => hidden = true,
                None => {}
            }
        }
        if found.len() == 1 {
            return found.pop();
        }
        if let Some(span) = span {
            if found.len() > 1 {
                let mut names: Vec<String> = found.iter().map(|d| self.described(d)).collect();
                names.sort();
                self.error(Code::ambiguous_name, span, Some(names.join(", ")));
            } else if hidden {
                self.error(Code::private_name, span, None);
            } else if self.decls.contains_key(&own) {
                self.error(Code::unknown_name, span, None);
            }
        }
        None
    }

    /// A declaration's name, and the import that passed its unit on, if one did.
    fn described(&self, d: &Decl) -> String {
        let via = self
            .access
            .iter()
            .find(|(u, ..)| *u == d.unit)
            .and_then(|a| a.2);
        match via {
            Some(v) => format!("{} (via {})", d.key, self.labels[v]),
            None => d.key.clone(),
        }
    }

    /// The key of a qualified name (`geo.Point`), checked against what this file sees.
    fn qualified(&mut self, name: &str, span: Span) -> String {
        match self.sees(name, false) {
            Some((d, true)) => return d.key,
            Some((_, false)) => self.error(Code::private_name, span, None),
            None => self.error(Code::unknown_name, span, None),
        }
        name.to_string()
    }

    /// The key of this file's own declaration of `name`.
    fn own_key(&self, name: &str) -> String {
        let key = qualify(self.namespace, name);
        self.decls
            .get(&key)
            .and_then(|ds| ds.iter().find(|d| d.unit == self.unit))
            .map_or(key.clone(), |d| d.key.clone())
    }

    /// Rewrites `a.b.c` to the name `a.b.c` when `a.b` is a namespace and `a` isn't a local.
    fn namespace_path(&mut self, base: &Expr, last: &str, span: Span) -> Option<String> {
        let segs = path(base)?;
        if self.is_local(segs[0]) {
            return None;
        }
        let ns = segs.join(".");
        if !self.namespaces.contains(&ns) {
            return None;
        }
        Some(self.qualified(&format!("{ns}.{last}"), span))
    }

    fn declaration(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Func(d) => {
                d.name = self.own_key(&d.name);
                self.func(d, &HashSet::new());
            }
            Stmt::Class(c) => {
                let key = self.own_key(&c.name);
                let saved = self.enter_type_params(&mut c.type_params);
                self.type_exprs(&mut c.interfaces);
                let consts = c.members.consts.iter().map(|k| k.name.clone()).collect();
                self.scopes.push(consts);
                for field in &mut c.fields {
                    self.param(field);
                }
                if let Some(init) = &mut c.init {
                    init.name = format!("{key}.init");
                    self.func(init, &HashSet::new());
                }
                self.members(&c.name, &key, &mut c.members);
                self.scopes.pop();
                self.type_params = saved;
                c.name = key;
            }
            Stmt::Interface(i) => {
                let key = self.own_key(&i.name);
                let saved = self.enter_type_params(&mut i.type_params);
                self.type_params.insert(THIS.to_string());
                self.type_exprs(&mut i.parents);
                for field in &mut i.fields {
                    self.param(field);
                }
                self.members(&i.name, &key, &mut i.members);
                self.type_params = saved;
                i.name = key;
            }
            Stmt::Enum(e) => {
                let key = self.own_key(&e.name);
                let saved = self.enter_type_params(&mut e.type_params);
                self.type_exprs(&mut e.interfaces);
                for v in &mut e.variants {
                    for t in &mut v.fields {
                        self.type_expr(t);
                    }
                }
                let consts = e.members.consts.iter().map(|k| k.name.clone()).collect();
                self.scopes.push(consts);
                self.members(&e.name, &key, &mut e.members);
                self.scopes.pop();
                self.type_params = saved;
                e.name = key;
            }
            Stmt::Data(d) => {
                d.name = self.own_key(&d.name);
                if let Some(t) = &mut d.ty {
                    self.type_expr(t);
                }
                self.expr(&mut d.value);
            }
            Stmt::Implement(i) => {
                let key = self
                    .lookup(&i.target, true, Some(i.span))
                    .map_or_else(|| i.target.clone(), |d| d.key);
                if let Some(iface) = &mut i.interface {
                    self.type_expr(iface);
                }
                // A name in `Box<T>` that isn't a type introduces a parameter.
                let saved = self.type_params.clone();
                for a in &i.target_args {
                    self.introduce_params(a);
                }
                i.params = self.type_params.difference(&saved).cloned().collect();
                self.type_exprs(&mut i.target_args);
                for b in &mut i.bounds {
                    self.type_exprs(&mut b.bounds);
                }
                // Methods for some type arguments only are keyed by them: `Box<Int>.get`.
                let specific = i
                    .target_args
                    .iter()
                    .any(|a| !matches!(a, TypeExpr::Name(n, _) if self.type_params.contains(n)));
                let owner = if specific {
                    let args: Vec<String> = i.target_args.iter().map(type_text).collect();
                    format!("{key}<{}>", args.join(", "))
                } else {
                    key.clone()
                };
                let consts = i.members.consts.iter().map(|k| k.name.clone()).collect();
                self.scopes.push(consts);
                self.members(&i.target.clone(), &owner, &mut i.members);
                self.scopes.pop();
                self.type_params = saved;
                i.target = key;
            }
            _ => self.stmt(stmt),
        }
    }

    fn members(&mut self, owner: &str, key: &str, members: &mut Members) {
        for f in members.methods.iter_mut().chain(&mut members.statics) {
            if let Some(rest) = f.name.strip_prefix(owner) {
                f.name = format!("{key}{rest}");
            }
            let this = HashSet::from(["this".to_string()]);
            self.func(f, &this);
        }
        for c in &mut members.consts {
            if let Some(t) = &mut c.ty {
                self.type_expr(t);
            }
            self.expr(&mut c.value);
        }
    }

    /// Resolves the bounds of `params` and brings them into scope, returning the ones before.
    fn enter_type_params(&mut self, params: &mut [TypeParam]) -> HashSet<String> {
        let saved = self.type_params.clone();
        self.type_params
            .extend(params.iter().map(|p| p.name.clone()));
        for p in params {
            self.type_exprs(&mut p.bounds);
        }
        saved
    }

    fn introduce_params(&mut self, t: &TypeExpr) {
        match t {
            TypeExpr::Name(n, _)
                if !BUILT_IN_TYPES.contains(&n.as_str())
                    && self.lookup(n, true, None).is_none() =>
            {
                self.type_params.insert(n.clone());
            }
            TypeExpr::Generic(_, args, _) => {
                for a in args {
                    self.introduce_params(a);
                }
            }
            TypeExpr::Maybe(inner) => self.introduce_params(inner),
            _ => {}
        }
    }

    fn type_exprs(&mut self, types: &mut [TypeExpr]) {
        for t in types {
            self.type_expr(t);
        }
    }

    fn func(&mut self, d: &mut FuncDecl, extra: &HashSet<String>) {
        let saved = self.enter_type_params(&mut d.type_params);
        self.scopes.push(extra.clone());
        for p in &mut d.params {
            self.param(p);
            self.bind(&p.name, p.span);
        }
        if let Some(t) = &mut d.ret {
            self.type_expr(t);
        }
        if let Some(body) = &mut d.body {
            self.expr(body);
        }
        self.scopes.pop();
        self.type_params = saved;
    }

    fn param(&mut self, p: &mut Param) {
        if let Some(t) = &mut p.ty {
            self.type_expr(t);
        }
        if let Some(d) = &mut p.default {
            self.expr(d);
        }
    }

    fn type_expr(&mut self, t: &mut TypeExpr) {
        match t {
            TypeExpr::Name(n, _) if self.type_params.contains(n) => {}
            TypeExpr::Name(n, span) | TypeExpr::Generic(n, _, span) => {
                if n.contains('.') {
                    *n = self.qualified(&n.clone(), *span);
                } else if let Some(d) = self.lookup(n, true, Some(*span)) {
                    *n = d.key;
                }
                if let TypeExpr::Generic(_, args, _) = t {
                    for a in args {
                        self.type_expr(a);
                    }
                }
            }
            TypeExpr::Maybe(inner) => self.type_expr(inner),
            TypeExpr::Func(params, ret) => {
                for p in params {
                    self.type_expr(p);
                }
                if let Some(r) = ret {
                    self.type_expr(r);
                }
            }
            TypeExpr::Record(_, types, _) => {
                for t in types {
                    self.type_expr(t);
                }
            }
        }
    }

    fn pattern(&mut self, pat: &mut Pattern) {
        match &mut pat.kind {
            PatternKind::Variant(qualifier, name, subs) => {
                match qualifier {
                    // `geo.Point(x, y)` is a class pattern, not a variant of a type `geo`.
                    Some(q) if self.namespaces.contains(q.as_str()) && !self.is_local(q) => {
                        *name = self.qualified(&format!("{q}.{name}"), pat.span);
                        *qualifier = None;
                    }
                    Some(q) => {
                        if let Some(d) = self.lookup(q, true, Some(pat.span)) {
                            *q = d.key;
                        }
                    }
                    None => {
                        if let Some(d) = self.lookup(name, true, Some(pat.span)) {
                            *name = d.key;
                        }
                    }
                }
                for sub in subs {
                    self.pattern(sub);
                }
            }
            PatternKind::Or(subs) | PatternKind::Record(subs) | PatternKind::List(subs) => {
                for sub in subs {
                    self.pattern(sub);
                }
            }
            PatternKind::Literal(e) => self.expr(e),
            PatternKind::Range(a, b, _) => {
                self.expr(a);
                self.expr(b);
            }
            PatternKind::Wildcard | PatternKind::Name(_) | PatternKind::Rest(_) => {}
        }
    }

    fn stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Let {
                name,
                span,
                ty,
                value,
                ..
            } => {
                if let Some(t) = ty {
                    self.type_expr(t);
                }
                if let Some(value) = value {
                    self.expr(value);
                }
                let (name, span) = (name.clone(), *span);
                self.bind(&name, span);
            }
            Stmt::LetPattern {
                pattern,
                value,
                otherwise,
                ..
            } => {
                self.expr(value);
                if let Some(o) = otherwise {
                    self.expr(o);
                }
                self.bind_pattern(pattern);
            }
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::RefLet { name, span, target } => {
                self.expr(target);
                let (name, span) = (name.clone(), *span);
                self.bind(&name, span);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Func(_)
            | Stmt::Class(_)
            | Stmt::Enum(_)
            | Stmt::Implement(_)
            | Stmt::Interface(_)
            | Stmt::Data(_) => self.declaration(stmt),
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(HashSet::new());
        f(self);
        self.scopes.pop();
    }

    fn args(&mut self, args: &mut [Arg]) {
        for a in args {
            self.expr(&mut a.value);
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        let span = e.span;
        match &mut e.kind {
            ExprKind::Name(n) => {
                if !self.is_local(n) {
                    let n = n.clone();
                    if let Some(d) = self.lookup(&n, false, Some(span)) {
                        e.kind = ExprKind::Name(d.key);
                    }
                }
            }
            ExprKind::Field(base, f) => {
                let f = f.clone();
                match self.namespace_path(base, &f, span) {
                    Some(key) => e.kind = ExprKind::Name(key),
                    None => self.expr(base),
                }
            }
            ExprKind::Method(base, m, args) => {
                let m = m.clone();
                self.args(args);
                if let Some(key) = self.namespace_path(base, &m, span) {
                    let callee = Expr {
                        id: base.id,
                        span,
                        kind: ExprKind::Name(key),
                    };
                    let args = std::mem::take(args);
                    e.kind = ExprKind::Call(Box::new(callee), args);
                } else {
                    self.expr(base);
                }
            }
            ExprKind::Str(segs) => {
                for seg in segs {
                    if let StrSeg::Expr(x) = seg {
                        self.expr(x);
                    }
                }
            }
            ExprKind::List(items) => {
                for x in items {
                    self.expr(x);
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Record(args) => self.args(args),
            ExprKind::Call(callee, args) => {
                self.expr(callee);
                self.args(args);
            }
            ExprKind::Spread(x)
            | ExprKind::Opt(x)
            | ExprKind::SharedOf(x)
            | ExprKind::Unary(_, x)
            | ExprKind::Loop(x) => self.expr(x),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Lambda(params, ret, body) => self.scoped(|r| {
                for p in params.iter_mut() {
                    r.param(p);
                    r.bind(&p.name, p.span);
                }
                if let Some(t) = ret {
                    r.type_expr(t);
                }
                // Freshly parsed, so the body isn't shared yet and this doesn't copy it.
                r.expr(Arc::make_mut(body));
            }),
            ExprKind::Block(stmts) => self.scoped(|r| {
                for s in stmts.iter_mut() {
                    r.stmt(s);
                }
            }),
            ExprKind::If(cond, then, otherwise) => self.scoped(|r| {
                r.expr(cond);
                r.expr(then);
                if let Some(o) = otherwise {
                    r.expr(o);
                }
            }),
            ExprKind::While(cond, body) => self.scoped(|r| {
                r.expr(cond);
                r.expr(body);
            }),
            ExprKind::Match(scrutinee, arms) => {
                self.expr(scrutinee);
                for arm in arms {
                    self.scoped(|r| {
                        r.bind_pattern(&mut arm.pattern);
                        if let Some(g) = &mut arm.guard {
                            r.expr(g);
                        }
                        r.expr(&mut arm.body);
                    });
                }
            }
            ExprKind::Is(x, pat) => {
                self.expr(x);
                self.bind_pattern(pat);
            }
            ExprKind::For(pat, iter, body, _) => {
                self.expr(iter);
                self.scoped(|r| {
                    r.bind_pattern(pat);
                    r.expr(body);
                });
            }
            ExprKind::Break(x) | ExprKind::Return(x) => {
                if let Some(x) = x {
                    self.expr(x);
                }
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::This
            | ExprKind::Continue => {}
        }
    }
}
