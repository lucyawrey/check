use std::collections::{HashMap, HashSet};

use crate::ast::{
    self, BinOp, ClassDecl, ExprKind as Ast, FuncDecl, NodeId, Param, PatternKind, Stmt, StrSeg,
};
use crate::checker::{Info, PRINTABLE, Resolved};
use crate::diag::{Code, Diagnostic, Span};
use crate::ir::{
    Arg, Class, Expr, ExprKind, Func, FuncId, ListOp, LocalId, Place, Program, Root, Step,
};
use crate::types::{Show, Ty};

type Lowered<T> = Result<T, Diagnostic>;

struct Instance<'a> {
    id: FuncId,
    name: String,
    decl: &'a FuncDecl,
    type_args: HashMap<String, Ty>,
    this: Option<Ty>,
}

struct Lowerer<'a> {
    info: &'a Info,
    decls: HashMap<&'a str, &'a FuncDecl>,
    class_decls: HashMap<&'a str, &'a ClassDecl>,
    classes: HashMap<String, Class>,
    ids: HashMap<String, FuncId>,
    pending: Vec<Instance<'a>>,
    type_args: HashMap<String, Ty>,
    globals: Vec<Ty>,
    scopes: Vec<HashMap<&'a str, Root>>,
    locals: Vec<Ty>,
    this: Option<LocalId>,
    in_main: bool,
}

pub fn lower(program: &[Stmt], library: &[Stmt], info: &Info) -> Lowered<Program> {
    let mut decls = HashMap::new();
    let mut class_decls = HashMap::new();
    for stmt in library.iter().chain(program) {
        let members = match stmt {
            Stmt::Func(decl) => {
                decls.insert(decl.name.as_str(), decl);
                continue;
            }
            Stmt::Class(c) => {
                class_decls.insert(c.name.as_str(), c);
                decls.extend(c.init.iter().map(|d| (d.name.as_str(), d)));
                &c.members
            }
            Stmt::Implement(i) => &i.members,
            _ => continue,
        };
        for decl in members.methods.iter().chain(&members.statics) {
            decls.insert(decl.name.as_str(), decl);
        }
    }
    let mut lowerer = Lowerer {
        info,
        decls,
        class_decls,
        classes: HashMap::new(),
        ids: HashMap::new(),
        pending: Vec::new(),
        type_args: HashMap::new(),
        globals: Vec::new(),
        scopes: vec![HashMap::new()],
        locals: Vec::new(),
        this: None,
        in_main: true,
    };
    let mut body = Vec::new();
    for stmt in program {
        if !is_declaration(stmt) {
            body.push(lowerer.stmt(stmt)?);
        }
    }
    let main = Func {
        name: "main".to_string(),
        params: 0,
        refs: Vec::new(),
        locals: std::mem::take(&mut lowerer.locals),
        ret: Ty::Void,
        body: expr(Ty::Void, Span::default(), ExprKind::Block(body)),
    };
    lowerer.in_main = false;
    let globals = lowerer.scopes.pop().expect("the top-level scope");
    let mut funcs = vec![Some(main)];
    while let Some(instance) = lowerer.pending.pop() {
        lowerer.scopes = vec![globals.clone()];
        funcs.resize_with(lowerer.ids.len() + 1, || None);
        let id = instance.id;
        funcs[id] = Some(lowerer.func(instance)?);
    }
    Ok(Program {
        funcs: funcs
            .into_iter()
            .map(|f| f.expect("each reached function"))
            .collect(),
        globals: lowerer.globals,
        main: 0,
        classes: lowerer.classes,
    })
}

fn is_declaration(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Func(_)
            | Stmt::Class(_)
            | Stmt::Enum(_)
            | Stmt::Implement(_)
            | Stmt::Interface(_)
            | Stmt::Data(_)
    )
}

fn unsupported<T>(span: Span) -> Lowered<T> {
    Err(Diagnostic::error(Code::wasm_unsupported, span))
}

fn expr(ty: Ty, span: Span, kind: ExprKind) -> Expr {
    Expr { ty, span, kind }
}

fn to_float(e: Expr) -> Expr {
    expr(Ty::Float, e.span, ExprKind::ToFloat(Box::new(e)))
}

fn some(e: Expr) -> Expr {
    expr(Ty::maybe(e.ty.clone()), e.span, ExprKind::Some(Box::new(e)))
}

fn binary(op: BinOp, l: Expr, r: Expr) -> ExprKind {
    let (l, r) = match (&l.ty, &r.ty) {
        (Ty::Int, Ty::Float) => (to_float(l), r),
        (Ty::Float, Ty::Int) => (l, to_float(r)),
        _ => (l, r),
    };
    ExprKind::Binary(op, Box::new(l), Box::new(r))
}

fn list_elem(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::List(elem) => Some(elem),
        _ => None,
    }
}

fn list_op(name: &str) -> Option<ListOp> {
    match name {
        "push" => Some(ListOp::Push),
        "pop" => Some(ListOp::Pop),
        "insert" => Some(ListOp::Insert),
        "removeAt" => Some(ListOp::RemoveAt),
        _ => None,
    }
}

fn type_map(params: &[String], args: &[Ty]) -> HashMap<String, Ty> {
    params.iter().cloned().zip(args.iter().cloned()).collect()
}

impl<'a> Lowerer<'a> {
    fn func(&mut self, instance: Instance<'a>) -> Lowered<Func> {
        let decl = instance.decl;
        self.type_args = instance.type_args;
        let Ty::Func(params, ret) = self.ty(decl.id) else {
            unreachable!("a function's type is a function type");
        };
        let ret = self.supported(&ret, decl.span)?;
        let changes_this = self.info.changes_this.contains(&decl.name) || self.is_init(decl);
        let mut refs = Vec::new();
        self.this = None;
        if let Some(this) = instance.this {
            self.this = Some(self.local(this));
            if changes_this {
                refs.push(0);
            }
        }
        let mut scope = HashMap::new();
        for (param, ty) in decl.params.iter().zip(&params) {
            if param.is_rest || param.is_shared {
                return unsupported(param.span);
            }
            let ty = self.supported(ty, param.span)?;
            let local = self.local(ty);
            if param.is_ref {
                refs.push(local);
            }
            scope.insert(param.name.as_str(), Root::Local(local));
        }
        self.scopes.push(scope);
        let body = self.expr(decl.body.as_ref().expect("a function with a body"))?;
        Ok(Func {
            name: instance.name,
            params: self.this.iter().count() + params.len(),
            refs,
            locals: std::mem::take(&mut self.locals),
            ret,
            body,
        })
    }

    fn is_init(&self, decl: &FuncDecl) -> bool {
        (self.class_decls.values()).any(|c| c.init.as_ref().is_some_and(|i| i.name == decl.name))
    }

    fn ty(&self, id: NodeId) -> Ty {
        self.info.types[&id].substitute(&self.type_args)
    }

    fn supported(&mut self, ty: &Ty, span: Span) -> Lowered<Ty> {
        match ty {
            Ty::Void | Ty::Never => Ok(ty.clone()),
            _ if self.stored(ty, &mut HashSet::new()) => Ok(ty.clone()),
            _ => unsupported(span),
        }
    }

    fn stored(&mut self, ty: &Ty, seen: &mut HashSet<String>) -> bool {
        match ty {
            Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Range => true,
            Ty::Maybe(inner) | Ty::List(inner) => self.stored(inner, seen),
            Ty::Record(_, fields) => fields.iter().all(|f| self.stored(f, seen)),
            Ty::Named(name, args) => {
                args.iter().all(|a| self.stored(a, seen)) && self.class(name, args, seen)
            }
            _ => false,
        }
    }

    fn class(&mut self, name: &str, args: &[Ty], seen: &mut HashSet<String>) -> bool {
        if !seen.insert(Show(&Ty::Named(name.to_string(), args.to_vec())).to_string()) {
            return true;
        }
        let (Some(decl), Some(shape)) = (self.class_decls.get(name), self.info.shapes.get(name))
        else {
            return false;
        };
        if decl.is_shared || shape.is_enum || decl.fields.iter().any(|f| f.is_shared) {
            return false;
        }
        let map = type_map(&shape.params, args);
        let fields: Vec<(String, Ty)> = (shape.variants[0].1.iter())
            .map(|f| (f.name.clone(), f.ty.clone()))
            .collect();
        if !fields
            .iter()
            .all(|(_, ty)| self.stored(&ty.substitute(&map), seen))
        {
            return false;
        }
        (self.classes)
            .entry(name.to_string())
            .or_insert_with(|| Class {
                params: shape.params.clone(),
                fields,
            });
        true
    }

    fn unprintable_inside(&self, ty: &Ty, seen: &mut HashSet<String>) -> bool {
        match ty {
            Ty::Range => true,
            Ty::Maybe(inner) | Ty::List(inner) => self.unprintable_inside(inner, seen),
            Ty::Record(_, fields) => fields.iter().any(|f| self.unprintable_inside(f, seen)),
            Ty::Named(name, args) => {
                if !seen.insert(Show(ty).to_string()) {
                    return false;
                }
                let class = &self.classes[name];
                let map = type_map(&class.params, args);
                (self.info.implements.get(name)).is_some_and(|i| i.contains(PRINTABLE))
                    || (class.fields.iter())
                        .any(|(_, f)| self.unprintable_inside(&f.substitute(&map), seen))
            }
            _ => false,
        }
    }

    fn text(&self, e: Expr) -> Lowered<Expr> {
        if e.ty == Ty::Str {
            return Ok(e);
        }
        if self.unprintable_inside(&e.ty, &mut HashSet::new()) {
            return unsupported(e.span);
        }
        Ok(expr(Ty::Str, e.span, ExprKind::Text(Box::new(e))))
    }

    fn field_index(&self, ty: &Ty, name: &str) -> Option<(usize, Ty)> {
        match ty {
            Ty::Record(names, fields) => {
                let i = match name.parse::<usize>() {
                    Ok(i) if names.is_empty() => i,
                    _ => names.iter().position(|n| n == name)?,
                };
                Some((i, fields.get(i)?.clone()))
            }
            Ty::Named(n, args) => {
                let class = self.classes.get(n)?;
                let i = class.fields.iter().position(|(f, _)| f == name)?;
                let map = type_map(&class.params, args);
                Some((i, class.fields[i].1.substitute(&map)))
            }
            _ => None,
        }
    }

    fn func_id(
        &mut self,
        decl: &'a FuncDecl,
        call: NodeId,
        this: Option<Ty>,
        span: Span,
    ) -> Lowered<(FuncId, HashMap<String, Ty>)> {
        let mut type_args = match &this {
            Some(Ty::List(elem) | Ty::Maybe(elem)) => {
                HashMap::from([("T".to_string(), (**elem).clone())])
            }
            Some(Ty::Named(name, args)) => type_map(&self.classes[name].params, args),
            _ => HashMap::new(),
        };
        for (param, ty) in self.info.type_args.get(&call).into_iter().flatten() {
            let ty = self.supported(&ty.substitute(&self.type_args), span)?;
            type_args.insert(param.clone(), ty);
        }
        if (decl.type_params.iter()).any(|p| !type_args.contains_key(&p.name)) {
            return unsupported(span);
        }
        let mut shown: Vec<String> = (type_args.iter())
            .map(|(p, ty)| format!("{p} = {}", Show(ty)))
            .collect();
        shown.sort();
        let name = match shown.is_empty() {
            true => decl.name.clone(),
            false => format!("{}<{}>", decl.name, shown.join(", ")),
        };
        if let Some(&id) = self.ids.get(&name) {
            return Ok((id, type_args));
        }
        let id = self.ids.len() + 1;
        self.ids.insert(name.clone(), id);
        self.pending.push(Instance {
            id,
            name,
            decl,
            type_args: type_args.clone(),
            this,
        });
        Ok((id, type_args))
    }

    fn lookup(&self, name: &str) -> Option<Root> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn declare(&mut self, name: &'a str, ty: Ty) -> Root {
        let root = if self.in_main && self.scopes.len() == 1 {
            self.globals.push(ty);
            Root::Global(self.globals.len() - 1)
        } else {
            Root::Local(self.local(ty))
        };
        self.scopes.last_mut().expect("a scope").insert(name, root);
        root
    }

    fn local(&mut self, ty: Ty) -> LocalId {
        self.locals.push(ty);
        self.locals.len() - 1
    }

    fn root_ty(&self, root: Root) -> Ty {
        match root {
            Root::Local(i) => self.locals[i].clone(),
            Root::Global(i) => self.globals[i].clone(),
        }
    }

    fn read(&self, root: Root, span: Span) -> Expr {
        let kind = match root {
            Root::Local(i) => ExprKind::Local(i),
            Root::Global(i) => ExprKind::Global(i),
        };
        expr(self.root_ty(root), span, kind)
    }

    fn write(root: Root, value: Expr, span: Span) -> Expr {
        let value = Box::new(value);
        let kind = match root {
            Root::Local(i) => ExprKind::SetLocal(i, value),
            Root::Global(i) => ExprKind::SetGlobal(i, value),
        };
        expr(Ty::Void, span, kind)
    }

    fn block(&mut self, stmts: &'a [Stmt]) -> Lowered<Vec<Expr>> {
        self.scopes.push(HashMap::new());
        let lowered = stmts.iter().map(|s| self.stmt(s)).collect();
        self.scopes.pop();
        lowered
    }

    fn stmt(&mut self, stmt: &'a Stmt) -> Lowered<Expr> {
        match stmt {
            Stmt::Let {
                name,
                span,
                is_shared: false,
                value: Some(value),
                ..
            } => {
                let value = self.expr(value)?;
                if matches!(value.ty, Ty::Void | Ty::Never) {
                    return unsupported(value.span);
                }
                let kind = match self.declare(name, value.ty.clone()) {
                    Root::Local(i) => ExprKind::Let(i, Box::new(value)),
                    Root::Global(i) => ExprKind::SetGlobal(i, Box::new(value)),
                };
                Ok(expr(Ty::Void, *span, kind))
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let mut value = self.expr(value)?;
                let place = self.place(target)?;
                if let Some(op) = op {
                    let current = self.place(target)?;
                    let current = self.read_place(current, target.span);
                    value = expr(current.ty.clone(), *span, binary(*op, current, value));
                }
                if place.steps.is_empty() {
                    return Ok(Self::write(place.root, value, *span));
                }
                let kind = ExprKind::Write(place, Box::new(value));
                Ok(expr(Ty::Void, *span, kind))
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Let { span, .. } | Stmt::RefLet { span, .. } | Stmt::LetPattern { span, .. } => {
                unsupported(*span)
            }
            Stmt::Func(ast::FuncDecl { span, .. }) => unsupported(*span),
            Stmt::Class(ast::ClassDecl { span, .. })
            | Stmt::Enum(ast::EnumDecl { span, .. })
            | Stmt::Implement(ast::ImplementDecl { span, .. })
            | Stmt::Interface(ast::InterfaceDecl { span, .. })
            | Stmt::Data(ast::DataDecl { span, .. }) => unsupported(*span),
        }
    }

    fn root_place(&self, root: Root) -> Place {
        Place {
            root,
            root_ty: self.root_ty(root),
            steps: Vec::new(),
        }
    }

    fn read_place(&self, place: Place, span: Span) -> Expr {
        let mut read = self.read(place.root, span);
        for (step, ty) in place.steps {
            let kind = match step {
                Step::Field(i) => ExprKind::Field(Box::new(read), i),
                Step::Index(index) => ExprKind::Index(Box::new(read), Box::new(index)),
            };
            read = expr(ty, span, kind);
        }
        read
    }

    fn place(&mut self, e: &'a ast::Expr) -> Lowered<Place> {
        match &e.kind {
            Ast::Name(name) => match self.lookup(name) {
                Some(root) => Ok(self.root_place(root)),
                None => unsupported(e.span),
            },
            Ast::This => match self.this {
                Some(this) => Ok(self.root_place(Root::Local(this))),
                None => unsupported(e.span),
            },
            Ast::Field(base, name) => {
                let mut place = self.place(base)?;
                let Some((i, ty)) = self.field_index(place.ty(), name) else {
                    return unsupported(e.span);
                };
                place.steps.push((Step::Field(i), ty));
                Ok(place)
            }
            Ast::Index(base, index) => {
                let mut place = self.place(base)?;
                let Some(elem) = list_elem(place.ty()).cloned() else {
                    return unsupported(e.span);
                };
                let mut index = self.expr(index)?;
                index.span = e.span;
                place.steps.push((Step::Index(index), elem));
                Ok(place)
            }
            _ => unsupported(e.span),
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) -> Lowered<Expr> {
        let ty = self.ty(e.id);
        let ty = self.supported(&ty, e.span)?;
        let kind = self.kind(e, &ty)?;
        let mut lowered = expr(ty, e.span, kind);
        if let Some(c) = self.info.coercions.get(&e.id) {
            if c.maybe_to_float || c.ok || c.to_dyn || c.list_to_record {
                return unsupported(e.span);
            }
            if c.to_float {
                lowered = to_float(lowered);
            }
            for _ in 0..c.wraps {
                lowered = some(lowered);
            }
        }
        Ok(lowered)
    }

    fn values(&mut self, args: &'a [ast::Arg], span: Span) -> Lowered<Vec<Expr>> {
        let mut lowered = Vec::new();
        for arg in args {
            if arg.name.is_some() {
                return unsupported(span);
            }
            lowered.push(self.expr(&arg.value)?);
        }
        Ok(lowered)
    }

    fn kind(&mut self, e: &'a ast::Expr, ty: &Ty) -> Lowered<ExprKind> {
        Ok(match &e.kind {
            Ast::Int(n) => ExprKind::Int(*n),
            Ast::Float(x) => ExprKind::Float(*x),
            Ast::Bool(b) => ExprKind::Bool(*b),
            Ast::Str(segs) => match &segs[..] {
                [] => ExprKind::Str(String::new()),
                [StrSeg::Text(s)] => ExprKind::Str(s.clone()),
                _ => {
                    let mut parts = Vec::new();
                    for seg in segs {
                        parts.push(match seg {
                            StrSeg::Text(s) => expr(Ty::Str, e.span, ExprKind::Str(s.clone())),
                            StrSeg::Expr(x) => {
                                let x = self.expr(x)?;
                                self.text(x)?
                            }
                        });
                    }
                    ExprKind::Concat(parts)
                }
            },
            Ast::Name(name) => match self.lookup(name) {
                Some(root) => self.read(root, e.span).kind,
                None if name == "None" && matches!(ty, Ty::Maybe(_)) => ExprKind::None,
                None => return unsupported(e.span),
            },
            Ast::This => match self.this {
                Some(this) => ExprKind::Local(this),
                None => return unsupported(e.span),
            },
            Ast::List(items) => ExprKind::List(
                (items.iter())
                    .map(|item| self.expr(item))
                    .collect::<Lowered<_>>()?,
            ),
            Ast::Record(args) => {
                let Ty::Record(names, _) = ty else {
                    return unsupported(e.span);
                };
                let mut fields = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let at = match &arg.name {
                        Some(name) => names.iter().position(|n| n == name),
                        None if names.is_empty() => Some(i),
                        None => None,
                    };
                    match at {
                        Some(at) => fields.push((at, self.expr(&arg.value)?)),
                        None => return unsupported(e.span),
                    }
                }
                fields.sort_by_key(|(at, _)| *at);
                ExprKind::Record(fields.into_iter().map(|(_, f)| f).collect())
            }
            Ast::Field(base, name) => {
                let base = self.expr(base)?;
                match self.field_index(&base.ty, name) {
                    Some((i, _)) => ExprKind::Field(Box::new(base), i),
                    None => return unsupported(e.span),
                }
            }
            Ast::Index(base, index) => {
                let base = self.expr(base)?;
                if list_elem(&base.ty).is_none() {
                    return unsupported(e.span);
                }
                ExprKind::Index(Box::new(base), Box::new(self.expr(index)?))
            }
            Ast::Unary(op, x) => ExprKind::Unary(*op, Box::new(self.expr(x)?)),
            Ast::Binary(BinOp::Coalesce, l, r) => {
                let l = self.expr(l)?;
                if !matches!(l.ty, Ty::Maybe(_)) {
                    return unsupported(e.span);
                }
                ExprKind::Coalesce(Box::new(l), Box::new(self.expr(r)?))
            }
            Ast::Binary(BinOp::Range, l, r) => ExprKind::Record(vec![self.expr(l)?, self.expr(r)?]),
            Ast::Binary(BinOp::RangeIncl, ..) => return unsupported(e.span),
            Ast::Binary(op, l, r) => {
                let (l, r) = (self.expr(l)?, self.expr(r)?);
                binary(*op, l, r)
            }
            Ast::Call(callee, args) => self.call(e, callee, args)?,
            Ast::Method(recv, name, args) => self.method(e, recv, name, args)?,
            Ast::Block(stmts) => ExprKind::Block(self.block(stmts)?),
            Ast::If(cond, then, otherwise) => {
                let cond = Box::new(self.expr(cond)?);
                let then = self.expr(then)?;
                match otherwise {
                    Some(o) => ExprKind::If(cond, Box::new(then), Some(Box::new(self.expr(o)?))),
                    None if matches!(ty, Ty::Maybe(_)) => ExprKind::If(
                        cond,
                        Box::new(some(then)),
                        Some(Box::new(expr(ty.clone(), e.span, ExprKind::None))),
                    ),
                    None => ExprKind::If(cond, Box::new(then), None),
                }
            }
            Ast::While(cond, body) => {
                if *ty != Ty::Void {
                    return unsupported(e.span);
                }
                ExprKind::While(Box::new(self.expr(cond)?), Box::new(self.expr(body)?))
            }
            Ast::For(pattern, iter, body, false) => {
                let PatternKind::Name(name) = &pattern.kind else {
                    return unsupported(e.span);
                };
                if *ty != Ty::Void {
                    return unsupported(e.span);
                }
                self.for_loop(name, iter, body)?
            }
            Ast::Break(None) => ExprKind::Break,
            Ast::Continue => ExprKind::Continue,
            Ast::Return(value) => ExprKind::Return(match value {
                Some(v) => Some(Box::new(self.expr(v)?)),
                None => None,
            }),
            _ => return unsupported(e.span),
        })
    }

    fn for_loop(
        &mut self,
        name: &'a str,
        iter: &'a ast::Expr,
        body: &'a ast::Expr,
    ) -> Lowered<ExprKind> {
        let mut prelude = None;
        let (range, list) = match &iter.kind {
            Ast::Binary(op @ (BinOp::Range | BinOp::RangeIncl), start, end) => {
                let range = (self.expr(start)?, self.expr(end)?, *op == BinOp::RangeIncl);
                (Some(range), None)
            }
            _ => match self.expr(iter)? {
                value if value.ty == Ty::Range => {
                    let (span, at) = (value.span, self.local(Ty::Range));
                    prelude = Some(expr(Ty::Void, span, ExprKind::Let(at, Box::new(value))));
                    let field = |i| {
                        let read = expr(Ty::Range, span, ExprKind::Local(at));
                        expr(Ty::Int, span, ExprKind::Field(Box::new(read), i))
                    };
                    (Some((field(0), field(1), false)), None)
                }
                value => (None, Some(value)),
            },
        };
        let var_ty = match &list {
            None => Ty::Int,
            Some(list) => match list_elem(&list.ty) {
                Some(elem) => elem.clone(),
                None => return unsupported(iter.span),
            },
        };
        self.scopes.push(HashMap::new());
        let Root::Local(var) = self.declare(name, var_ty) else {
            unreachable!("a loop variable is in a nested scope");
        };
        let body = self.expr(body);
        self.scopes.pop();
        let body = Box::new(body?);
        let kind = match (range, list) {
            (Some((start, end, inclusive)), _) => ExprKind::ForRange {
                var,
                start: Box::new(start),
                end: Box::new(end),
                inclusive,
                body,
            },
            (None, list) => ExprKind::ForList {
                var,
                list: Box::new(list.expect("a list to loop over")),
                body,
            },
        };
        Ok(match prelude {
            Some(prelude) => ExprKind::Block(vec![prelude, expr(Ty::Void, iter.span, kind)]),
            None => kind,
        })
    }

    fn call(
        &mut self,
        e: &'a ast::Expr,
        callee: &'a ast::Expr,
        args: &'a [ast::Arg],
    ) -> Lowered<ExprKind> {
        let Ast::Name(name) = &callee.kind else {
            return unsupported(callee.span);
        };
        if self.lookup(name).is_some() {
            return unsupported(callee.span);
        }
        match self.info.resolved.get(&e.id) {
            Some(Resolved::Construct(class)) => return self.construct(e, class, args),
            Some(_) => return unsupported(callee.span),
            None => {}
        }
        let Some(&decl) = self.decls.get(name.as_str()) else {
            let mut values = self.values(args, e.span)?;
            return match (name.as_str(), values.pop()) {
                ("Some", Some(value)) if values.is_empty() => Ok(ExprKind::Some(Box::new(value))),
                ("print", Some(value)) if values.is_empty() => {
                    Ok(ExprKind::Print(Box::new(self.text(value)?)))
                }
                _ => unsupported(callee.span),
            };
        };
        if decl.body.is_none() {
            return Ok(ExprKind::External(name.clone(), self.values(args, e.span)?));
        }
        let (id, type_args) = self.func_id(decl, e.id, None, callee.span)?;
        let args = self.args(&decl.params, args, &type_args, e.span)?;
        Ok(ExprKind::Call(id, args))
    }

    fn method(
        &mut self,
        e: &'a ast::Expr,
        recv: &'a ast::Expr,
        name: &str,
        args: &'a [ast::Arg],
    ) -> Lowered<ExprKind> {
        let key = match self.info.resolved.get(&e.id) {
            Some(Resolved::Static(key)) => {
                let decl = self.decls[key.as_str()];
                let (id, type_args) = self.func_id(decl, e.id, None, e.span)?;
                let args = self.args(&decl.params, args, &type_args, e.span)?;
                return Ok(ExprKind::Call(id, args));
            }
            Some(Resolved::Method(key)) => Some(key),
            Some(_) => return unsupported(e.span),
            None => None,
        };
        let decl = key.and_then(|k| self.decls.get(k.as_str()).copied());
        let recv_ty = match self.info.types.get(&recv.id) {
            Some(ty) => ty.substitute(&self.type_args),
            None => self.place(recv)?.ty().clone(),
        };
        let recv_ty = self.supported(&recv_ty, recv.span)?;
        let Some(decl) = decl.filter(|d| d.body.is_some()) else {
            return self.builtin(e, recv, &recv_ty, name, args, decl);
        };
        let (id, type_args) = self.func_id(decl, e.id, Some(recv_ty), e.span)?;
        let this = match self.info.changes_this.contains(&decl.name) {
            true => Arg::Ref(self.place(recv)?),
            false => Arg::Value(self.expr(recv)?),
        };
        let mut lowered = vec![this];
        lowered.extend(self.args(&decl.params, args, &type_args, e.span)?);
        Ok(ExprKind::Call(id, lowered))
    }

    fn builtin(
        &mut self,
        e: &'a ast::Expr,
        recv: &'a ast::Expr,
        recv_ty: &Ty,
        name: &str,
        args: &'a [ast::Arg],
        decl: Option<&'a FuncDecl>,
    ) -> Lowered<ExprKind> {
        Ok(match (recv_ty, name, list_op(name)) {
            (Ty::List(_), "length", _) if args.is_empty() => {
                ExprKind::Length(Box::new(self.expr(recv)?))
            }
            (Ty::List(_), _, Some(op)) => {
                let place = self.place(recv)?;
                ExprKind::ListOp(op, place, self.values(args, e.span)?)
            }
            (Ty::Maybe(inner), "orPanic", _) => {
                let Some(decl) = decl else {
                    return unsupported(e.span);
                };
                let map = HashMap::from([("T".to_string(), (**inner).clone())]);
                let recv = self.expr(recv)?;
                let Some(Arg::Value(reason)) = self.args(&decl.params, args, &map, e.span)?.pop()
                else {
                    return unsupported(e.span);
                };
                ExprKind::OrPanic(Box::new(recv), Box::new(reason))
            }
            _ => return unsupported(e.span),
        })
    }

    fn construct(
        &mut self,
        e: &'a ast::Expr,
        class: &str,
        args: &'a [ast::Arg],
    ) -> Lowered<ExprKind> {
        let ty = self.ty(e.id);
        let Ty::Named(_, type_args) = &ty else {
            return unsupported(e.span);
        };
        let decl = self.class_decls[class];
        let map = type_map(&self.classes[class].params, type_args);
        let mut fields = Vec::new();
        for arg in self.args(&decl.fields, args, &map, e.span)? {
            match arg {
                Arg::Value(v) => fields.push(v),
                Arg::Ref(_) => return unsupported(e.span),
            }
        }
        let record = expr(ty.clone(), e.span, ExprKind::Record(fields));
        let Some(init) = &decl.init else {
            return Ok(record.kind);
        };
        if !init.params.is_empty() {
            return unsupported(init.span);
        }
        let (id, _) = self.func_id(init, e.id, Some(ty.clone()), e.span)?;
        let this = self.local(ty.clone());
        let place = self.root_place(Root::Local(this));
        Ok(ExprKind::Block(vec![
            expr(Ty::Void, e.span, ExprKind::Let(this, Box::new(record))),
            expr(Ty::Void, e.span, ExprKind::Call(id, vec![Arg::Ref(place)])),
            expr(ty, e.span, ExprKind::Local(this)),
        ]))
    }

    fn args(
        &mut self,
        params: &'a [Param],
        args: &'a [ast::Arg],
        type_args: &HashMap<String, Ty>,
        span: Span,
    ) -> Lowered<Vec<Arg>> {
        let mut given: Vec<Option<&'a ast::Expr>> = vec![None; params.len()];
        let mut positional = (0..params.len()).filter(|&i| !params[i].is_private);
        for arg in args {
            let i = match &arg.name {
                Some(name) => params.iter().position(|p| p.name == *name),
                None => positional.next(),
            };
            match i {
                Some(i) if !params[i].is_rest && !params[i].is_shared => {
                    given[i] = Some(&arg.value)
                }
                _ => return unsupported(span),
            }
        }
        let mut lowered = Vec::new();
        for (param, value) in params.iter().zip(given) {
            lowered.push(match value {
                Some(value) if self.info.ref_args.contains_key(&value.id) => {
                    Arg::Ref(self.place(value)?)
                }
                Some(value) => Arg::Value(self.expr(value)?),
                None => match &param.default {
                    Some(default) => Arg::Value(self.default(default, type_args)?),
                    None => return unsupported(span),
                },
            });
        }
        Ok(lowered)
    }

    fn default(&mut self, e: &'a ast::Expr, type_args: &HashMap<String, Ty>) -> Lowered<Expr> {
        let outer = vec![self.scopes[0].clone()];
        let scopes = std::mem::replace(&mut self.scopes, outer);
        let type_args = std::mem::replace(&mut self.type_args, type_args.clone());
        let lowered = self.expr(e);
        self.scopes = scopes;
        self.type_args = type_args;
        lowered
    }
}
