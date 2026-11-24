use std::collections::HashMap;

use crate::ast::{self, BinOp, ExprKind as Ast, FuncDecl, PatternKind, Stmt, StrSeg};
use crate::checker::Info;
use crate::diag::{Code, Diagnostic, Span};
use crate::ir::{Expr, ExprKind, Func, FuncId, GlobalId, LocalId, Program};
use crate::types::Ty;

type Lowered<T> = Result<T, Diagnostic>;

#[derive(Clone, Copy)]
enum Var {
    Local(LocalId),
    Global(GlobalId),
}

struct Lowerer<'a> {
    info: &'a Info,
    decls: HashMap<&'a str, &'a FuncDecl>,
    ids: HashMap<&'a str, FuncId>,
    pending: Vec<(FuncId, &'a FuncDecl)>,
    globals: Vec<Ty>,
    scopes: Vec<HashMap<&'a str, Var>>,
    locals: Vec<Ty>,
    in_main: bool,
}

pub fn lower(program: &[Stmt], library: &[Stmt], info: &Info) -> Lowered<Program> {
    let decls = (library.iter().chain(program))
        .filter_map(|s| match s {
            Stmt::Func(decl) => Some((decl.name.as_str(), decl)),
            _ => None,
        })
        .collect();
    let mut lowerer = Lowerer {
        info,
        decls,
        ids: HashMap::new(),
        pending: Vec::new(),
        globals: Vec::new(),
        scopes: vec![HashMap::new()],
        locals: Vec::new(),
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
        locals: std::mem::take(&mut lowerer.locals),
        ret: Ty::Void,
        body: Expr {
            ty: Ty::Void,
            span: Span::default(),
            kind: ExprKind::Block(body),
        },
    };
    lowerer.in_main = false;
    let globals = lowerer.scopes.pop().expect("the top-level scope");
    let mut funcs = vec![Some(main)];
    while let Some((id, decl)) = lowerer.pending.pop() {
        lowerer.scopes = vec![globals.clone()];
        funcs.resize_with(lowerer.ids.len() + 1, || None);
        funcs[id] = Some(lowerer.func(decl)?);
    }
    Ok(Program {
        funcs: funcs
            .into_iter()
            .map(|f| f.expect("each reached function"))
            .collect(),
        globals: lowerer.globals,
        main: 0,
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

fn supported(ty: &Ty, span: Span) -> Lowered<Ty> {
    match ty {
        Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Void | Ty::Never => Ok(ty.clone()),
        _ => unsupported(span),
    }
}

fn to_float(e: Expr) -> Expr {
    Expr {
        ty: Ty::Float,
        span: e.span,
        kind: ExprKind::ToFloat(Box::new(e)),
    }
}

fn text(e: Expr) -> Expr {
    if e.ty == Ty::Str {
        return e;
    }
    Expr {
        ty: Ty::Str,
        span: e.span,
        kind: ExprKind::Text(Box::new(e)),
    }
}

fn binary(op: BinOp, l: Expr, r: Expr) -> ExprKind {
    let (l, r) = match (&l.ty, &r.ty) {
        (Ty::Int, Ty::Float) => (to_float(l), r),
        (Ty::Float, Ty::Int) => (l, to_float(r)),
        _ => (l, r),
    };
    ExprKind::Binary(op, Box::new(l), Box::new(r))
}

impl<'a> Lowerer<'a> {
    fn func(&mut self, decl: &'a FuncDecl) -> Lowered<Func> {
        let Ty::Func(params, ret) = &self.info.types[&decl.id] else {
            unreachable!("a function's type is a function type");
        };
        let ret = supported(ret, decl.span)?;
        if !decl.type_params.is_empty() {
            return unsupported(decl.span);
        }
        let mut scope = HashMap::new();
        for (param, ty) in decl.params.iter().zip(params) {
            self.locals.push(supported(ty, param.span)?);
            scope.insert(param.name.as_str(), Var::Local(self.locals.len() - 1));
        }
        self.scopes.push(scope);
        let body = self.expr(decl.body.as_ref().expect("a function with a body"))?;
        Ok(Func {
            name: decl.name.clone(),
            params: params.len(),
            locals: std::mem::take(&mut self.locals),
            ret,
            body,
        })
    }

    fn func_id(&mut self, decl: &'a FuncDecl) -> FuncId {
        if let Some(&id) = self.ids.get(decl.name.as_str()) {
            return id;
        }
        let id = self.ids.len() + 1;
        self.ids.insert(&decl.name, id);
        self.pending.push((id, decl));
        id
    }

    fn lookup(&self, name: &str) -> Option<Var> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn declare(&mut self, name: &'a str, ty: Ty) -> Var {
        let var = if self.in_main && self.scopes.len() == 1 {
            self.globals.push(ty);
            Var::Global(self.globals.len() - 1)
        } else {
            self.locals.push(ty);
            Var::Local(self.locals.len() - 1)
        };
        self.scopes.last_mut().expect("a scope").insert(name, var);
        var
    }

    fn var_ty(&self, var: Var) -> Ty {
        match var {
            Var::Local(i) => self.locals[i].clone(),
            Var::Global(i) => self.globals[i].clone(),
        }
    }

    fn read(&self, var: Var, span: Span) -> Expr {
        let kind = match var {
            Var::Local(i) => ExprKind::Local(i),
            Var::Global(i) => ExprKind::Global(i),
        };
        Expr {
            ty: self.var_ty(var),
            span,
            kind,
        }
    }

    fn write(var: Var, value: Expr, span: Span) -> Expr {
        let value = Box::new(value);
        let kind = match var {
            Var::Local(i) => ExprKind::SetLocal(i, value),
            Var::Global(i) => ExprKind::SetGlobal(i, value),
        };
        Expr {
            ty: Ty::Void,
            span,
            kind,
        }
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
                let kind = match self.declare(name, value.ty.clone()) {
                    Var::Local(i) => ExprKind::Let(i, Box::new(value)),
                    Var::Global(i) => ExprKind::SetGlobal(i, Box::new(value)),
                };
                Ok(Expr {
                    ty: Ty::Void,
                    span: *span,
                    kind,
                })
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let Ast::Name(name) = &target.kind else {
                    return unsupported(target.span);
                };
                let Some(var) = self.lookup(name) else {
                    return unsupported(target.span);
                };
                let mut value = self.expr(value)?;
                if let Some(op) = op {
                    let current = self.read(var, target.span);
                    value = Expr {
                        ty: current.ty.clone(),
                        span: *span,
                        kind: binary(*op, current, value),
                    };
                }
                Ok(Self::write(var, value, *span))
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

    fn expr(&mut self, e: &'a ast::Expr) -> Lowered<Expr> {
        let ty = supported(&self.info.types[&e.id], e.span)?;
        let kind = self.kind(e)?;
        let mut lowered = Expr {
            ty,
            span: e.span,
            kind,
        };
        if let Some(c) = self.info.coercions.get(&e.id) {
            if c.maybe_to_float || c.wraps > 0 || c.ok {
                return unsupported(e.span);
            }
            if c.to_float {
                lowered = to_float(lowered);
            }
        }
        Ok(lowered)
    }

    fn kind(&mut self, e: &'a ast::Expr) -> Lowered<ExprKind> {
        Ok(match &e.kind {
            Ast::Int(n) => ExprKind::Int(*n),
            Ast::Float(x) => ExprKind::Float(*x),
            Ast::Bool(b) => ExprKind::Bool(*b),
            Ast::Str(segs) => match &segs[..] {
                [] => ExprKind::Str(String::new()),
                [StrSeg::Text(s)] => ExprKind::Str(s.clone()),
                _ => ExprKind::Concat(
                    (segs.iter())
                        .map(|seg| match seg {
                            StrSeg::Text(s) => Ok(Expr {
                                ty: Ty::Str,
                                span: e.span,
                                kind: ExprKind::Str(s.clone()),
                            }),
                            StrSeg::Expr(x) => self.expr(x).map(text),
                        })
                        .collect::<Lowered<_>>()?,
                ),
            },
            Ast::Name(name) => match self.lookup(name) {
                Some(var) => self.read(var, e.span).kind,
                None => return unsupported(e.span),
            },
            Ast::Unary(op, x) => ExprKind::Unary(*op, Box::new(self.expr(x)?)),
            Ast::Binary(op, l, r) => {
                let (l, r) = (self.expr(l)?, self.expr(r)?);
                binary(*op, l, r)
            }
            Ast::Call(callee, args) => self.call(callee, args)?,
            Ast::Block(stmts) => ExprKind::Block(self.block(stmts)?),
            Ast::If(cond, then, otherwise) => ExprKind::If(
                Box::new(self.expr(cond)?),
                Box::new(self.expr(then)?),
                match otherwise {
                    Some(o) => Some(Box::new(self.expr(o)?)),
                    None => None,
                },
            ),
            Ast::While(cond, body) => {
                ExprKind::While(Box::new(self.expr(cond)?), Box::new(self.expr(body)?))
            }
            Ast::For(pattern, iter, body, false) => {
                let (
                    PatternKind::Name(name),
                    Ast::Binary(op @ (BinOp::Range | BinOp::RangeIncl), start, end),
                ) = (&pattern.kind, &iter.kind)
                else {
                    return unsupported(e.span);
                };
                let (start, end) = (self.expr(start)?, self.expr(end)?);
                self.scopes.push(HashMap::new());
                let Var::Local(var) = self.declare(name, Ty::Int) else {
                    unreachable!("a loop variable is in a nested scope");
                };
                let body = self.expr(body);
                self.scopes.pop();
                ExprKind::ForRange {
                    var,
                    start: Box::new(start),
                    end: Box::new(end),
                    inclusive: *op == BinOp::RangeIncl,
                    body: Box::new(body?),
                }
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

    fn call(&mut self, callee: &'a ast::Expr, args: &'a [ast::Arg]) -> Lowered<ExprKind> {
        let Ast::Name(name) = &callee.kind else {
            return unsupported(callee.span);
        };
        if self.lookup(name).is_some()
            || self.info.resolved.contains_key(&callee.id)
            || args.iter().any(|a| a.name.is_some())
        {
            return unsupported(callee.span);
        }
        let decl = self.decls.get(name.as_str()).copied();
        if let Some(decl) = decl {
            let plain =
                |p: &ast::Param| !(p.is_ref || p.is_rest || p.is_shared || p.default.is_some());
            if decl.params.len() != args.len() || !decl.params.iter().all(plain) {
                return unsupported(callee.span);
            }
        }
        let mut lowered = Vec::new();
        for arg in args {
            lowered.push(self.expr(&arg.value)?);
        }
        let Some(decl) = decl else {
            return match (name.as_str(), lowered.pop()) {
                ("print", Some(arg)) => Ok(ExprKind::Print(Box::new(text(arg)))),
                _ => unsupported(callee.span),
            };
        };
        Ok(match decl.body {
            Some(_) => ExprKind::Call(self.func_id(decl), lowered),
            None => ExprKind::External(name.clone(), lowered),
        })
    }
}
