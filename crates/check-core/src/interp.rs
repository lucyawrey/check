use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::rc::Rc;

use crate::ast::*;
use crate::checker::Info;
use crate::diag::{Code, Span};
use crate::value::{Callable, Value};

pub struct Panic {
    pub code: Code,
    pub span: Span,
    pub reason: Option<String>,
}

pub enum Flow {
    Break(Value),
    Continue,
    Return(Value),
    Panic(Panic),
}

type Eval = Result<Value, Flow>;

struct Frame {
    scopes: Vec<HashMap<String, Value>>,
}

pub struct Interpreter<'a> {
    info: &'a Info,
    funcs: HashMap<String, Rc<FuncDecl>>,
    globals: HashMap<String, Value>,
    frames: Vec<Frame>,
    depth: usize,
    input: &'a mut dyn BufRead,
    output: &'a mut dyn Write,
}

const MAX_DEPTH: usize = 10_000;

pub fn run(
    program: &[Stmt],
    info: &Info,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<(), Panic> {
    let mut interp = Interpreter {
        info,
        funcs: HashMap::new(),
        globals: HashMap::new(),
        frames: vec![Frame { scopes: Vec::new() }],
        depth: 0,
        input,
        output,
    };
    for stmt in program {
        if let Stmt::Func(decl) = stmt {
            interp
                .funcs
                .insert(decl.name.clone(), Rc::new(decl.clone()));
        }
    }
    for stmt in program {
        match interp.stmt(stmt) {
            Ok(_) => {}
            Err(Flow::Panic(p)) => return Err(p),
            Err(_) => {}
        }
    }
    Ok(())
}

fn panic<T>(code: Code, span: Span) -> Result<T, Flow> {
    Err(Flow::Panic(Panic {
        code,
        span,
        reason: None,
    }))
}

impl Interpreter<'_> {
    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("frame")
    }

    fn define(&mut self, name: &str, value: Value) {
        match self.frame().scopes.last_mut() {
            Some(scope) => {
                scope.insert(name.to_string(), value);
            }
            None => {
                self.globals.insert(name.to_string(), value);
            }
        }
    }

    fn slot(&mut self, name: &str) -> Option<&mut Value> {
        let frame = self.frames.last_mut().expect("frame");
        if let Some(scope) = frame.scopes.iter_mut().rev().find(|s| s.contains_key(name)) {
            return scope.get_mut(name);
        }
        self.globals.get_mut(name)
    }

    fn lookup(&mut self, name: &str) -> Option<Value> {
        if let Some(v) = self.slot(name) {
            return Some(v.clone());
        }
        if let Some(decl) = self.funcs.get(name) {
            let sig = self
                .info
                .signatures
                .get(&decl.id)
                .cloned()
                .unwrap_or_default();
            return Some(Value::Func(Rc::new(Callable::Named(decl.clone(), sig))));
        }
        match name {
            "None" => Some(Value::None),
            _ => None,
        }
    }

    fn coerce(&self, id: NodeId, mut value: Value) -> Value {
        if let Some(c) = self.info.coercions.get(&id) {
            if c.to_float
                && let Value::Int(n) = value
            {
                value = Value::Float(n as f64);
            }
            for _ in 0..c.wraps {
                value = Value::Some(Rc::new(value));
            }
        }
        value
    }

    fn stmt(&mut self, stmt: &Stmt) -> Eval {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = self.expr(value)?;
                self.define(name, v);
                Ok(Value::Void)
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let ExprKind::Name(name) = &target.kind else {
                    return Ok(Value::Void);
                };
                let rhs = self.expr(value)?;
                let new = match op {
                    None => rhs,
                    Some(op) => {
                        let current = self.lookup(name).unwrap_or(Value::Void);
                        let current = self.coerce(target.id, current);
                        arith(*op, current, rhs, *span)?
                    }
                };
                if let Some(slot) = self.slot(name) {
                    *slot = new;
                }
                Ok(Value::Void)
            }
            Stmt::Func(_) => Ok(Value::Void),
            Stmt::Expr(e) => self.expr(e),
        }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Eval {
        self.frame().scopes.push(HashMap::new());
        let mut last = Ok(Value::Void);
        for stmt in stmts {
            last = self.stmt(stmt);
            if last.is_err() {
                break;
            }
        }
        if matches!(stmts.last(), Some(Stmt::Let { .. } | Stmt::Assign { .. })) {
            last = last.map(|_| Value::Void);
        }
        self.frame().scopes.pop();
        last
    }

    fn expr(&mut self, expr: &Expr) -> Eval {
        let value = self.raw(expr)?;
        Ok(self.coerce(expr.id, value))
    }

    fn raw(&mut self, expr: &Expr) -> Eval {
        match &expr.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Str(segs) => {
                let mut out = String::new();
                for seg in segs {
                    match seg {
                        StrSeg::Text(t) => out.push_str(t),
                        StrSeg::Expr(e) => out.push_str(&self.expr(e)?.display()),
                    }
                }
                Ok(Value::Str(out.into()))
            }
            ExprKind::Name(name) => Ok(self.lookup(name).unwrap_or(Value::Void)),
            ExprKind::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.expr(item)?);
                }
                Ok(Value::List(Rc::new(values)))
            }
            ExprKind::Unary(op, operand) => {
                let v = self.expr(operand)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(n)) => n
                        .checked_neg()
                        .map(Value::Int)
                        .map_or_else(|| panic(Code::E0902, expr.span), Ok),
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Try, Value::Some(v)) => Ok((*v).clone()),
                    (UnOp::Try, _) => Err(Flow::Return(Value::None)),
                    (_, v) => Ok(v),
                }
            }
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::Call(callee, args) => self.call(callee, args, expr.span),
            ExprKind::Method(recv, name, args) => self.method(recv, name, args, expr.span),
            ExprKind::Index(list, index) => {
                let l = self.expr(list)?;
                let i = self.expr(index)?;
                let (Value::List(items), Value::Int(i)) = (l, i) else {
                    return Ok(Value::None);
                };
                let found = usize::try_from(i).ok().and_then(|i| items.get(i).cloned());
                Ok(match found {
                    Some(v) if self.info.unwrapped_index.contains(&expr.id) => v,
                    Some(v) => Value::Some(Rc::new(v)),
                    None => Value::None,
                })
            }
            ExprKind::Lambda(params, body) => {
                let mut captured = self.globals.clone();
                for scope in &self.frames.last().expect("frame").scopes {
                    captured.extend(scope.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                let sig = self
                    .info
                    .signatures
                    .get(&expr.id)
                    .cloned()
                    .unwrap_or_default();
                Ok(Value::Func(Rc::new(Callable::Lambda {
                    params: params.clone(),
                    body: Rc::new((**body).clone()),
                    captured,
                    sig,
                })))
            }
            ExprKind::Block(stmts) => self.block(stmts),
            ExprKind::If(cond, then, otherwise) => {
                let wraps = self.info.wrapping.contains(&expr.id);
                if self.expr(cond)?.truthy() {
                    let v = self.expr(then)?;
                    Ok(if wraps {
                        Value::Some(Rc::new(v))
                    } else if otherwise.is_some() {
                        v
                    } else {
                        Value::Void
                    })
                } else if let Some(other) = otherwise {
                    self.expr(other)
                } else {
                    Ok(if wraps { Value::None } else { Value::Void })
                }
            }
            ExprKind::While(cond, body) => {
                let wraps = self.info.wrapping.contains(&expr.id);
                while self.expr(cond)?.truthy() {
                    match self.expr(body) {
                        Ok(_) | Err(Flow::Continue) => {}
                        Err(Flow::Break(v)) => {
                            return Ok(if wraps {
                                Value::Some(Rc::new(v))
                            } else {
                                Value::Void
                            });
                        }
                        Err(other) => return Err(other),
                    }
                }
                Ok(if wraps { Value::None } else { Value::Void })
            }
            ExprKind::Loop(body) => loop {
                match self.expr(body) {
                    Ok(_) | Err(Flow::Continue) => {}
                    Err(Flow::Break(v)) => return Ok(v),
                    Err(other) => return Err(other),
                }
            },
            ExprKind::For(name, _, iter, body) => {
                let wraps = self.info.wrapping.contains(&expr.id);
                let items: Box<dyn Iterator<Item = Value>> = match self.expr(iter)? {
                    Value::Range(lo, hi) => Box::new((lo..hi).map(Value::Int)),
                    Value::List(items) => Box::new((*items).clone().into_iter()),
                    _ => Box::new(std::iter::empty()),
                };
                for item in items {
                    self.frame()
                        .scopes
                        .push(HashMap::from([(name.clone(), item)]));
                    let result = self.expr(body);
                    self.frame().scopes.pop();
                    match result {
                        Ok(_) | Err(Flow::Continue) => {}
                        Err(Flow::Break(v)) => {
                            return Ok(if wraps {
                                Value::Some(Rc::new(v))
                            } else {
                                Value::Void
                            });
                        }
                        Err(other) => return Err(other),
                    }
                }
                Ok(if wraps { Value::None } else { Value::Void })
            }
            ExprKind::Break(value) => {
                let v = match value {
                    Some(v) => self.expr(v)?,
                    None => Value::Void,
                };
                Err(Flow::Break(v))
            }
            ExprKind::Continue => Err(Flow::Continue),
            ExprKind::Return(value) => {
                let v = match value {
                    Some(v) => self.expr(v)?,
                    None => self.coerce(expr.id, Value::Void),
                };
                Err(Flow::Return(v))
            }
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Eval {
        match op {
            BinOp::And => {
                let l = self.expr(lhs)?;
                if !l.truthy() {
                    return Ok(Value::Bool(false));
                }
                self.expr(rhs)
            }
            BinOp::Or => {
                let l = self.expr(lhs)?;
                if l.truthy() {
                    return Ok(Value::Bool(true));
                }
                self.expr(rhs)
            }
            BinOp::Coalesce => match self.expr(lhs)? {
                Value::Some(v) => Ok((*v).clone()),
                _ => self.expr(rhs),
            },
            _ => {
                let l = self.expr(lhs)?;
                let r = self.expr(rhs)?;
                arith(op, l, r, span)
            }
        }
    }

    fn args(&mut self, args: &[Arg]) -> Result<Vec<Value>, Flow> {
        args.iter().map(|a| self.expr(&a.value)).collect()
    }

    fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Eval {
        if let ExprKind::Name(name) = &callee.kind
            && self.slot(name).is_none()
        {
            if let Some(decl) = self.funcs.get(name).cloned() {
                return self.call_named(&decl, args, span);
            }
            match name.as_str() {
                "print" => {
                    let v = self.expr(&args[0].value)?;
                    writeln!(self.output, "{}", v.display()).ok();
                    return Ok(Value::Void);
                }
                "panic" => {
                    let reason = match args.first() {
                        Some(a) => Some(self.expr(&a.value)?.display()),
                        None => None,
                    };
                    return Err(Flow::Panic(Panic {
                        code: Code::E0901,
                        span,
                        reason,
                    }));
                }
                "input" => {
                    if let Some(a) = args.first() {
                        let prompt = self.expr(&a.value)?.display();
                        write!(self.output, "{prompt}").ok();
                    }
                    let mut line = String::new();
                    return Ok(match self.input.read_line(&mut line) {
                        Ok(0) | Err(_) => Value::None,
                        Ok(_) => {
                            let trimmed = line.strip_suffix('\n').unwrap_or(&line);
                            let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
                            Value::Some(Rc::new(Value::Str(trimmed.into())))
                        }
                    });
                }
                "Some" => {
                    let v = self.expr(&args[0].value)?;
                    return Ok(Value::Some(Rc::new(v)));
                }
                _ => {}
            }
        }
        let f = self.expr(callee)?;
        let values = self.args(args)?;
        self.call_value(&f, values, span)
    }

    fn call_named(&mut self, decl: &Rc<FuncDecl>, args: &[Arg], span: Span) -> Eval {
        let mut slots: Vec<Option<Value>> = vec![None; decl.params.len()];
        let mut rest = Vec::new();
        let fixed: Vec<usize> = decl
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_rest)
            .map(|(i, _)| i)
            .collect();
        let mut k = 0;
        for arg in args {
            let v = self.expr(&arg.value)?;
            match &arg.name {
                Some(name) => {
                    if let Some(i) = decl.params.iter().position(|p| &p.name == name) {
                        slots[i] = Some(v);
                    }
                }
                None => {
                    match fixed.get(k) {
                        Some(&i) => slots[i] = Some(v),
                        None => rest.push(v),
                    }
                    k += 1;
                }
            }
        }
        let mut scope = HashMap::new();
        for (i, p) in decl.params.iter().enumerate() {
            let v = if p.is_rest {
                Value::List(Rc::new(std::mem::take(&mut rest)))
            } else {
                match slots[i].take() {
                    Some(v) => v,
                    None => match &p.default {
                        Some(d) => self.expr(d)?,
                        None => Value::Void,
                    },
                }
            };
            scope.insert(p.name.clone(), v);
        }
        self.enter(vec![scope], &decl.body, span)
    }

    fn call_value(&mut self, f: &Value, values: Vec<Value>, span: Span) -> Eval {
        let Value::Func(callable) = f else {
            return Ok(Value::Void);
        };
        match &**callable {
            Callable::Named(decl, _) => {
                let scope = decl
                    .params
                    .iter()
                    .map(|p| p.name.clone())
                    .zip(values)
                    .collect();
                self.enter(vec![scope], &decl.body.clone(), span)
            }
            Callable::Lambda {
                params,
                body,
                captured,
                ..
            } => {
                let scope = params.iter().map(|p| p.name.clone()).zip(values).collect();
                let body = body.clone();
                self.enter(vec![captured.clone(), scope], &body, span)
            }
        }
    }

    fn enter(&mut self, scopes: Vec<HashMap<String, Value>>, body: &Expr, span: Span) -> Eval {
        if self.depth >= MAX_DEPTH {
            return panic(Code::E0908, span);
        }
        self.depth += 1;
        self.frames.push(Frame { scopes });
        let result = self.expr(body);
        self.frames.pop();
        self.depth -= 1;
        match result {
            Ok(v) => Ok(v),
            Err(Flow::Return(v)) => Ok(v),
            Err(other) => Err(other),
        }
    }

    fn method(&mut self, recv: &Expr, name: &str, args: &[Arg], span: Span) -> Eval {
        if let ExprKind::Name(type_name) = &recv.kind {
            if (type_name == "Int" || type_name == "Float") && self.slot(type_name).is_none() {
                let text = self.expr(&args[0].value)?.display();
                let text = text.trim();
                return Ok(if type_name == "Int" {
                    text.parse::<i64>()
                        .map_or(Value::None, |n| Value::Some(Rc::new(Value::Int(n))))
                } else {
                    text.parse::<f64>()
                        .map_or(Value::None, |f| Value::Some(Rc::new(Value::Float(f))))
                });
            }
            if matches!(name, "push" | "pop" | "removeAt") {
                let values = self.args(args)?;
                let Some(Value::List(list)) = self.slot(type_name) else {
                    return Ok(Value::Void);
                };
                let items = Rc::make_mut(list);
                return Ok(match (name, values.into_iter().next()) {
                    ("push", Some(v)) => {
                        items.push(v);
                        Value::Void
                    }
                    ("pop", _) => items.pop().map_or(Value::None, |v| Value::Some(Rc::new(v))),
                    ("removeAt", Some(Value::Int(i))) => match usize::try_from(i) {
                        Ok(i) if i < items.len() => Value::Some(Rc::new(items.remove(i))),
                        _ => Value::None,
                    },
                    _ => Value::Void,
                });
            }
        }
        let r = self.expr(recv)?;
        let values = self.args(args)?;
        self.builtin_method(r, name, values, span)
    }

    fn builtin_method(&mut self, recv: Value, name: &str, args: Vec<Value>, span: Span) -> Eval {
        let arg_str = |i: usize| args.get(i).map(Value::display).unwrap_or_default();
        Ok(match (recv, name) {
            (Value::Str(s), "upper") => Value::Str(s.to_uppercase().into()),
            (Value::Str(s), "lower") => Value::Str(s.to_lowercase().into()),
            (Value::Str(s), "trim") => Value::Str(s.trim().into()),
            (Value::Str(s), "isEmpty") => Value::Bool(s.is_empty()),
            (Value::Str(s), "isBlank") => Value::Bool(s.trim().is_empty()),
            (Value::Str(s), "contains") => Value::Bool(s.contains(arg_str(0).as_str())),
            (Value::Str(s), "split") => {
                let parts: Vec<Value> = if args.is_empty() {
                    s.split_whitespace().map(|p| Value::Str(p.into())).collect()
                } else {
                    s.split(arg_str(0).as_str())
                        .map(|p| Value::Str(p.into()))
                        .collect()
                };
                Value::List(Rc::new(parts))
            }
            (Value::List(items), "length") => Value::Int(items.len() as i64),
            (Value::List(items), "isEmpty") => Value::Bool(items.is_empty()),
            (Value::List(items), "contains") => {
                Value::Bool(args.first().is_some_and(|a| items.contains(a)))
            }
            (Value::List(items), "first") => items
                .first()
                .map_or(Value::None, |v| Value::Some(Rc::new(v.clone()))),
            (Value::List(items), "last") => items
                .last()
                .map_or(Value::None, |v| Value::Some(Rc::new(v.clone()))),
            (Value::List(items), "indices") => Value::Range(0, items.len() as i64),
            (Value::List(items), "join") => {
                let parts: Vec<String> = items.iter().map(Value::display).collect();
                Value::Str(parts.join(&arg_str(0)).into())
            }
            (Value::List(items), "map") => {
                let f = args.into_iter().next().unwrap_or(Value::Void);
                let mut out = Vec::with_capacity(items.len());
                for item in items.iter() {
                    out.push(self.call_value(&f, vec![item.clone()], span)?);
                }
                Value::List(Rc::new(out))
            }
            (Value::List(items), "filter") => {
                let f = args.into_iter().next().unwrap_or(Value::Void);
                let mut out = Vec::new();
                for item in items.iter() {
                    if self.call_value(&f, vec![item.clone()], span)?.truthy() {
                        out.push(item.clone());
                    }
                }
                Value::List(Rc::new(out))
            }
            (Value::Int(n), "toFloat") => Value::Float(n as f64),
            (Value::Float(f), "round" | "floor" | "ceil" | "truncate") => {
                let r = match name {
                    "round" => f.round(),
                    "floor" => f.floor(),
                    "ceil" => f.ceil(),
                    _ => f.trunc(),
                };
                if !r.is_finite() || r < i64::MIN as f64 || r >= i64::MAX as f64 {
                    return panic(Code::E0907, span);
                }
                Value::Int(r as i64)
            }
            (Value::Some(v), "orPanic") => (*v).clone(),
            (Value::None, "orPanic") => {
                let reason = args.first().map(Value::display);
                return Err(Flow::Panic(Panic {
                    code: Code::E0904,
                    span,
                    reason,
                }));
            }
            (v @ (Value::Some(_) | Value::None), "flatten") => {
                let mut v = v;
                while let Value::Some(inner) = &v {
                    match &**inner {
                        Value::Some(_) | Value::None => v = (**inner).clone(),
                        _ => break,
                    }
                }
                v
            }
            _ => Value::Void,
        })
    }
}

fn floor_div(a: i64, b: i64) -> Option<i64> {
    let q = a.checked_div(b)?;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        Some(q - 1)
    } else {
        Some(q)
    }
}

fn arith(op: BinOp, l: Value, r: Value, span: Span) -> Eval {
    use Value::{Float as F, Int as I};
    let overflow = |v: Option<i64>| v.map(I).map_or_else(|| panic(Code::E0902, span), Ok);
    let (l, r) = match (l, r) {
        (I(a), F(b)) => (F(a as f64), F(b)),
        (F(a), I(b)) if op != BinOp::Pow => (F(a), F(b as f64)),
        pair => pair,
    };
    Ok(match (op, l, r) {
        (BinOp::Add, I(a), I(b)) => return overflow(a.checked_add(b)),
        (BinOp::Sub, I(a), I(b)) => return overflow(a.checked_sub(b)),
        (BinOp::Mul, I(a), I(b)) => return overflow(a.checked_mul(b)),
        (BinOp::Div, I(a), I(b)) => {
            if b == 0 {
                return panic(Code::E0903, span);
            }
            F(a as f64 / b as f64)
        }
        (BinOp::IntDiv | BinOp::Mod, I(_), I(0)) => return panic(Code::E0903, span),
        (BinOp::IntDiv, I(a), I(b)) => return overflow(floor_div(a, b)),
        (BinOp::Mod, I(a), I(b)) => {
            return overflow(
                floor_div(a, b)
                    .and_then(|q| q.checked_mul(b))
                    .and_then(|m| a.checked_sub(m)),
            );
        }
        (BinOp::Pow, I(a), I(b)) => {
            if b < 0 {
                return panic(Code::E0905, span);
            }
            let exp = u32::try_from(b).ok();
            return overflow(exp.and_then(|e| a.checked_pow(e)));
        }
        (BinOp::Pow, F(a), I(b)) => F(a.powi(b as i32)),
        (BinOp::Add, F(a), F(b)) => F(a + b),
        (BinOp::Sub, F(a), F(b)) => F(a - b),
        (BinOp::Mul, F(a), F(b)) => F(a * b),
        (BinOp::Div, F(a), F(b)) => F(a / b),
        (BinOp::Pow, F(a), F(b)) => F(a.powf(b)),
        (BinOp::BitAnd, I(a), I(b)) => I(a & b),
        (BinOp::BitOr, I(a), I(b)) => I(a | b),
        (BinOp::BitXor, I(a), I(b)) => I(a ^ b),
        (BinOp::Shl | BinOp::Shr, I(_), I(b)) if !(0..64).contains(&b) => {
            return panic(Code::E0906, span);
        }
        (BinOp::Shl, I(a), I(b)) => I(a.wrapping_shl(b as u32)),
        (BinOp::Shr, I(a), I(b)) => I(a >> b),
        (BinOp::Range, I(a), I(b)) => Value::Range(a, b),
        (BinOp::RangeIncl, I(a), I(b)) => match b.checked_add(1) {
            Some(end) => Value::Range(a, end),
            None => return panic(Code::E0902, span),
        },
        (BinOp::Add, Value::Str(a), Value::Str(b)) => Value::Str(format!("{a}{b}").into()),
        (BinOp::Eq, a, b) => Value::Bool(a == b),
        (BinOp::Ne, a, b) => Value::Bool(a != b),
        (BinOp::Lt, a, b) => Value::Bool(a.compare(&b).is_lt()),
        (BinOp::Gt, a, b) => Value::Bool(a.compare(&b).is_gt()),
        (BinOp::Le, a, b) => Value::Bool(a.compare(&b).is_le()),
        (BinOp::Ge, a, b) => Value::Bool(a.compare(&b).is_ge()),
        _ => Value::Void,
    })
}
