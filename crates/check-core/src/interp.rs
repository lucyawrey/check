use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::rc::Rc;

use crate::ast::*;
use crate::checker::{Info, Resolved};
use crate::diag::{Code, Span};
use crate::host::{self, Io};
use crate::value::{Callable, Record, Value};

pub struct Panic {
    pub code: Code,
    pub span: Span,
    pub reason: Option<String>,
    pub stack: Vec<Span>,
}

pub enum Flow {
    Break(Value),
    Continue,
    Return(Value),
    Stop(Span),
    Panic(Panic),
}

type Eval = Result<Value, Flow>;

type Writer<'f> = &'f mut dyn FnMut(&mut Value) -> Result<(), Flow>;

type Descent<'s> = Result<Option<(Rc<RefCell<Record>>, &'s [Step])>, Flow>;

enum Step {
    Field(String),
    Index(i64, Span),
}

struct Frame {
    scopes: Vec<HashMap<String, Value>>,
}

pub struct Interpreter<'a> {
    info: &'a Info,
    funcs: HashMap<String, Rc<FuncDecl>>,
    classes: HashMap<String, Rc<ClassDecl>>,
    enums: HashMap<String, Rc<EnumDecl>>,
    consts: HashMap<String, ConstDecl>,
    const_values: HashMap<String, Value>,
    globals: HashMap<String, Value>,
    frames: Vec<Frame>,
    depth: usize,
    input: &'a mut dyn BufRead,
    output: &'a mut dyn Write,
}

const MAX_DEPTH: usize = 10_000;

pub fn run(
    program: &[Stmt],
    library: &[Stmt],
    info: &Info,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<Option<Span>, Panic> {
    let mut interp = Interpreter {
        info,
        funcs: HashMap::new(),
        classes: HashMap::new(),
        enums: HashMap::new(),
        consts: HashMap::new(),
        const_values: HashMap::new(),
        globals: HashMap::new(),
        frames: vec![Frame { scopes: Vec::new() }],
        depth: 0,
        input,
        output,
    };
    for stmt in library.iter().chain(program) {
        match stmt {
            Stmt::Func(decl) => interp.add_func(decl),
            Stmt::Implement(i) => interp.add_members(&i.target, &i.members),
            Stmt::Class(c) => {
                if let Some(init) = &c.init {
                    interp.add_func(init);
                }
                interp.add_members(&c.name, &c.members);
                interp.classes.insert(c.name.clone(), Rc::new(c.clone()));
            }
            Stmt::Enum(e) => {
                interp.add_members(&e.name, &e.members);
                interp.enums.insert(e.name.clone(), Rc::new(e.clone()));
            }
            _ => {}
        }
    }
    let mut consts: Vec<String> = interp.consts.keys().cloned().collect();
    consts.sort();
    for key in consts {
        if let Err(Flow::Panic(p)) = interp.resolved(&Resolved::Const(key), &[], Span::default()) {
            return Err(p);
        }
    }
    for stmt in program {
        match interp.stmt(stmt) {
            Ok(_) => {}
            Err(Flow::Panic(p)) => return Err(p),
            Err(Flow::Stop(span)) => return Ok(Some(span)),
            Err(_) => {}
        }
    }
    Ok(None)
}

pub(crate) fn panic<T>(code: Code, span: Span) -> Result<T, Flow> {
    Err(Flow::Panic(Panic {
        code,
        span,
        reason: None,
        stack: Vec::new(),
    }))
}

fn coerce(info: &Info, id: NodeId, mut value: Value) -> Value {
    if let Some(c) = info.coercions.get(&id) {
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

fn descend<'s>(v: &mut Value, steps: &'s [Step], f: Writer) -> Descent<'s> {
    let Some((step, rest)) = steps.split_first() else {
        f(v)?;
        return Ok(None);
    };
    match (v, step) {
        (Value::Shared(rc), _) => Ok(Some((rc.clone(), steps))),
        (Value::Object(rc), _) => descend_record(Rc::make_mut(rc), steps, f),
        (Value::List(items), Step::Index(i, span)) => {
            let items = Rc::make_mut(items);
            match usize::try_from(*i).ok().filter(|i| *i < items.len()) {
                Some(i) => descend(&mut items[i], rest, f),
                None => panic(Code::index_out_of_range, *span),
            }
        }
        _ => Ok(None),
    }
}

fn descend_record<'s>(r: &mut Record, steps: &'s [Step], f: Writer) -> Descent<'s> {
    let Some((Step::Field(name), rest)) = steps.split_first() else {
        return Ok(None);
    };
    let i = r.index(name);
    descend(&mut r.fields[i], rest, f)
}

impl Interpreter<'_> {
    fn add_func(&mut self, decl: &FuncDecl) {
        self.funcs.insert(decl.name.clone(), Rc::new(decl.clone()));
    }

    fn add_members(&mut self, owner: &str, members: &Members) {
        for decl in members.methods.iter().chain(&members.statics) {
            self.add_func(decl);
        }
        for c in &members.consts {
            self.consts.insert(format!("{owner}.{}", c.name), c.clone());
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self) -> Eval) -> Eval {
        self.frame().scopes.push(HashMap::new());
        let result = f(self);
        self.frame().scopes.pop();
        result
    }

    fn path(&mut self, e: &Expr, steps: &mut Vec<Step>) -> Result<String, Flow> {
        match &e.kind {
            ExprKind::Field(base, name) => {
                let root = self.path(base, steps)?;
                steps.push(Step::Field(name.clone()));
                Ok(root)
            }
            ExprKind::Index(base, index) => {
                let root = self.path(base, steps)?;
                let i = match self.expr(index)? {
                    Value::Int(i) => i,
                    _ => -1,
                };
                steps.push(Step::Index(i, e.span));
                Ok(root)
            }
            ExprKind::Name(name) => Ok(name.clone()),
            _ => Ok("this".to_string()),
        }
    }

    fn write_path(&mut self, target: &Expr, f: Writer) -> Result<(), Flow> {
        let mut steps = Vec::new();
        let root = self.path(target, &mut steps)?;
        let Some(slot) = self.slot(&root) else {
            return Ok(());
        };
        let mut next = descend(slot, &steps, f)?;
        while let Some((shared, rest)) = next {
            let mut record = shared.borrow_mut();
            next = descend_record(&mut record, rest, f)?;
        }
        Ok(())
    }

    fn resolved(&mut self, r: &Resolved, args: &[Arg], span: Span) -> Eval {
        match r {
            Resolved::Const(key) => {
                if let Some(v) = self.const_values.get(key) {
                    return Ok(v.clone());
                }
                let value = self.consts[key].value.clone();
                let v = self.expr(&value)?;
                self.const_values.insert(key.clone(), v.clone());
                Ok(v)
            }
            Resolved::Variant(enum_name, name) => {
                let decl = self.enums[enum_name].clone();
                let variant = decl.variants.iter().find(|v| &v.name == name);
                let names = variant.and_then(|v| v.field_names.clone());
                let mut fields = vec![Value::Void; variant.map_or(0, |v| v.fields.len())];
                for (k, arg) in args.iter().enumerate() {
                    let v = self.expr(&arg.value)?;
                    let i = match (&arg.name, &names) {
                        (Some(n), Some(names)) => names.iter().position(|f| f == n).unwrap_or(k),
                        _ => k,
                    };
                    fields[i] = v;
                }
                Ok(Value::Variant(Rc::new(Record {
                    name: name.as_str().into(),
                    field_names: names.map(|n| n.iter().map(|f| f.as_str().into()).collect()),
                    fields,
                })))
            }
            Resolved::Construct(class_name) => {
                if self.depth >= MAX_DEPTH {
                    return panic(Code::stack_overflow, span);
                }
                self.depth += 1;
                let result = self.construct(class_name, args, span);
                self.depth -= 1;
                result
            }
            Resolved::Static(key) | Resolved::Method(key) => {
                let decl = self.funcs[key].clone();
                self.call_named(&decl, args, span, None)
            }
        }
    }

    fn construct(&mut self, class_name: &str, args: &[Arg], span: Span) -> Eval {
        let class = self.classes[class_name].clone();
        let fields = self.bind_args(&class.fields, args)?;
        let record = Record {
            name: class_name.into(),
            field_names: Some(
                class
                    .fields
                    .iter()
                    .map(|f| f.name.as_str().into())
                    .collect(),
            ),
            fields,
        };
        let value = if class.is_ref {
            Value::Shared(Rc::new(RefCell::new(record)))
        } else {
            Value::Object(Rc::new(record))
        };
        match &class.init {
            Some(init) => self.run_init(init, value, span),
            None => Ok(value),
        }
    }

    fn run_init(&mut self, init: &FuncDecl, this: Value, span: Span) -> Eval {
        let scope = HashMap::from([("this".to_string(), this)]);
        self.frames.push(Frame {
            scopes: vec![scope],
        });
        let result = self.expr(init.body.as_ref().expect("init has a body"));
        let mut frame = self.frames.pop().expect("frame");
        match result {
            Ok(_) | Err(Flow::Return(_)) => {}
            Err(Flow::Panic(mut p)) => {
                p.stack.push(span);
                return Err(Flow::Panic(p));
            }
            Err(other) => return Err(other),
        }
        Ok(frame.scopes[0].remove("this").unwrap_or(Value::Void))
    }

    fn matches(
        &mut self,
        pat: &Pattern,
        v: &Value,
        binds: &mut Vec<(String, Value)>,
    ) -> Result<bool, Flow> {
        Ok(match &pat.kind {
            PatternKind::Wildcard => true,
            PatternKind::Name(name) if self.info.variant_patterns.contains(&pat.id) => match v {
                Value::None => name == "None",
                Value::Variant(r) => &*r.name == name,
                _ => false,
            },
            PatternKind::Name(name) => {
                binds.push((name.clone(), v.clone()));
                true
            }
            PatternKind::Literal(lit) => self.expr(lit)? == *v,
            PatternKind::Range(lo, hi, inclusive) => {
                let (lo, hi) = (self.expr(lo)?, self.expr(hi)?);
                v.compare(&lo).is_some_and(Ordering::is_ge)
                    && v.compare(&hi)
                        .is_some_and(|o| o.is_lt() || (*inclusive && o.is_eq()))
            }
            PatternKind::Variant(_, name, args) => match v {
                Value::Some(inner) if name == "Some" => self.matches(&args[0], inner, binds)?,
                Value::Variant(r) if &*r.name == name => {
                    for (arg, field) in args.iter().zip(&r.fields) {
                        if !self.matches(arg, field, binds)? {
                            return Ok(false);
                        }
                    }
                    true
                }
                _ => false,
            },
            PatternKind::Or(alternatives) => {
                for alt in alternatives {
                    let mut alt_binds = Vec::new();
                    if self.matches(alt, v, &mut alt_binds)? {
                        binds.extend(alt_binds);
                        return Ok(true);
                    }
                }
                false
            }
        })
    }

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

    fn coerce(&self, id: NodeId, value: Value) -> Value {
        coerce(self.info, id, value)
    }

    fn stmt(&mut self, stmt: &Stmt) -> Eval {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = self.scoped(|i| i.expr(value))?;
                self.define(name, v);
                Ok(Value::Void)
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let rhs = self.scoped(|i| i.expr(value))?;
                let info = self.info;
                self.write_path(target, &mut |slot| {
                    *slot = match op {
                        None => rhs.clone(),
                        Some(op) => {
                            let current = coerce(info, target.id, slot.clone());
                            arith(*op, current, rhs.clone(), *span)?
                        }
                    };
                    Ok(())
                })?;
                Ok(Value::Void)
            }
            Stmt::LetElse {
                pattern,
                value,
                otherwise,
                ..
            } => {
                let v = self.expr(value)?;
                let mut binds = Vec::new();
                if self.matches(pattern, &v, &mut binds)? {
                    for (name, v) in binds {
                        self.define(&name, v);
                    }
                    return Ok(Value::Void);
                }
                self.expr(otherwise)
            }
            Stmt::Func(_) | Stmt::Class(_) | Stmt::Enum(_) | Stmt::Implement(_) => Ok(Value::Void),
            Stmt::Expr(e) => self.scoped(|i| i.expr(e)),
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
        let info = self.info;
        if let Some(r) = info.resolved.get(&expr.id) {
            match &expr.kind {
                ExprKind::Name(_) | ExprKind::Field(..) => return self.resolved(r, &[], expr.span),
                ExprKind::Call(_, args) => return self.resolved(r, args, expr.span),
                _ => {}
            }
        }
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
            ExprKind::This => Ok(self.lookup("this").unwrap_or(Value::Void)),
            ExprKind::Field(base, name) => Ok(match self.expr(base)? {
                Value::Object(r) => r.fields[r.index(name)].clone(),
                Value::Shared(r) => {
                    let r = r.borrow();
                    r.fields[r.index(name)].clone()
                }
                _ => Value::Void,
            }),
            ExprKind::Is(lhs, pattern) => {
                let v = self.expr(lhs)?;
                let mut binds = Vec::new();
                let matched = self.matches(pattern, &v, &mut binds)?;
                for (name, v) in binds {
                    self.define(&name, v);
                }
                Ok(Value::Bool(matched))
            }
            ExprKind::Match(scrutinee, arms) => {
                let v = self.expr(scrutinee)?;
                for arm in arms {
                    let mut binds = Vec::new();
                    if !self.matches(&arm.pattern, &v, &mut binds)? {
                        continue;
                    }
                    self.frame().scopes.push(binds.into_iter().collect());
                    let result = match &arm.guard {
                        Some(g) => match self.expr(g) {
                            Ok(g) if g.truthy() => Some(self.expr(&arm.body)),
                            Ok(_) => None,
                            Err(e) => Some(Err(e)),
                        },
                        None => Some(self.expr(&arm.body)),
                    };
                    self.frame().scopes.pop();
                    if let Some(result) = result {
                        return result;
                    }
                }
                Ok(Value::Void)
            }
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
                        .map_or_else(|| panic(Code::integer_overflow, expr.span), Ok),
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Try, Value::Some(v)) => Ok((*v).clone()),
                    (UnOp::Try, _) if self.frames.len() == 1 => Err(Flow::Stop(expr.span)),
                    (UnOp::Try, _) => Err(Flow::Return(Value::None)),
                    (_, v) => Ok(v),
                }
            }
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::Call(callee, args) => self.call(callee, args, expr.span),
            ExprKind::Method(recv, name, args) => self.method(expr.id, recv, name, args, expr.span),
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
            ExprKind::Lambda(params, _, body) => {
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
                self.frame().scopes.push(HashMap::new());
                let taken = match self.expr(cond) {
                    Ok(c) if c.truthy() => Some(self.expr(then)),
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                };
                self.frame().scopes.pop();
                match taken {
                    Some(v) => {
                        let v = v?;
                        Ok(if wraps {
                            Value::Some(Rc::new(v))
                        } else if otherwise.is_some() {
                            v
                        } else {
                            Value::Void
                        })
                    }
                    None => match otherwise {
                        Some(other) => self.expr(other),
                        None => Ok(if wraps { Value::None } else { Value::Void }),
                    },
                }
            }
            ExprKind::While(cond, body) => {
                let wraps = self.info.wrapping.contains(&expr.id);
                loop {
                    self.frame().scopes.push(HashMap::new());
                    let step = match self.expr(cond) {
                        Ok(c) if c.truthy() => Some(self.expr(body)),
                        Ok(_) => None,
                        Err(e) => Some(Err(e)),
                    };
                    self.frame().scopes.pop();
                    let Some(step) = step else {
                        break;
                    };
                    match step {
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
                    Value::Range(lo, hi, false) => Box::new((lo..hi).map(Value::Int)),
                    Value::Range(lo, hi, true) => Box::new((lo..=hi).map(Value::Int)),
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
                return self.call_named(&decl, args, span, None);
            }
            match name.as_str() {
                "print" => {
                    let v = self.expr(&args[0].value)?;
                    writeln!(self.output, "{}", v.display()).ok();
                    return Ok(Value::Void);
                }
                "Some" => {
                    let v = self.expr(&args[0].value)?;
                    return Ok(Value::Some(Rc::new(v)));
                }
                "same" => {
                    let values = self.args(args)?;
                    return Ok(Value::Bool(matches!(
                        (&values[0], &values[1]),
                        (Value::Shared(a), Value::Shared(b)) if Rc::ptr_eq(a, b)
                    )));
                }
                _ => {}
            }
        }
        let f = self.expr(callee)?;
        let values = self.args(args)?;
        self.call_value(&f, values, span)
    }

    fn call_named(
        &mut self,
        decl: &Rc<FuncDecl>,
        args: &[Arg],
        span: Span,
        this: Option<Value>,
    ) -> Eval {
        let values = self.bind_args(&decl.params, args)?;
        self.run_decl(decl, values, this, span)
    }

    fn run_decl(
        &mut self,
        decl: &FuncDecl,
        mut values: Vec<Value>,
        this: Option<Value>,
        span: Span,
    ) -> Eval {
        let Some(body) = &decl.body else {
            if let Some(this) = this {
                values.insert(0, this);
            }
            let f = host::find(&decl.name).expect("every @Host declaration has a body in host.rs");
            let mut io = Io {
                input: &mut *self.input,
                output: &mut *self.output,
            };
            return f(&mut io, values, span);
        };
        let mut scope: HashMap<String, Value> = decl
            .params
            .iter()
            .map(|p| p.name.clone())
            .zip(values)
            .collect();
        if let Some(this) = this {
            scope.insert("this".to_string(), this);
        }
        self.enter(vec![scope], body, span)
    }

    fn bind_args(&mut self, params: &[Param], args: &[Arg]) -> Result<Vec<Value>, Flow> {
        let mut slots: Vec<Option<Value>> = vec![None; params.len()];
        let mut rest = Vec::new();
        let fixed: Vec<usize> = params
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
                    if let Some(i) = params.iter().position(|p| &p.name == name) {
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
        let mut values = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
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
            values.push(v);
        }
        Ok(values)
    }

    fn call_value(&mut self, f: &Value, values: Vec<Value>, span: Span) -> Eval {
        let Value::Func(callable) = f else {
            return Ok(Value::Void);
        };
        match &**callable {
            Callable::Named(decl, _) => self.run_decl(decl, values, None, span),
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
            return panic(Code::stack_overflow, span);
        }
        self.depth += 1;
        self.frames.push(Frame { scopes });
        let result = self.expr(body);
        self.frames.pop();
        self.depth -= 1;
        match result {
            Ok(v) => Ok(v),
            Err(Flow::Return(v)) => Ok(v),
            Err(Flow::Panic(mut p)) => {
                p.stack.push(span);
                Err(Flow::Panic(p))
            }
            Err(other) => Err(other),
        }
    }

    fn method(&mut self, id: NodeId, recv: &Expr, name: &str, args: &[Arg], span: Span) -> Eval {
        let info = self.info;
        match info.resolved.get(&id) {
            Some(Resolved::Method(key)) => {
                let this = self.expr(recv)?;
                let decl = self.funcs[key].clone();
                return self.call_named(&decl, args, span, Some(this));
            }
            Some(r) => return self.resolved(r, args, span),
            None => {}
        }
        let is_place = matches!(
            recv.kind,
            ExprKind::Name(_) | ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::This
        );
        if is_place && matches!(name, "push" | "pop" | "removeAt") {
            let mut arg = self.args(args)?.into_iter().next();
            let mut out = Value::Void;
            self.write_path(recv, &mut |slot| {
                let Value::List(list) = slot else {
                    return Ok(());
                };
                let items = Rc::make_mut(list);
                out = match (name, arg.take()) {
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
                };
                Ok(())
            })?;
            return Ok(out);
        }
        let r = self.expr(recv)?;
        let values = self.args(args)?;
        self.builtin_method(r, name, values, span)
    }

    fn builtin_method(&mut self, recv: Value, name: &str, args: Vec<Value>, span: Span) -> Eval {
        let arg_str = |i: usize| args.get(i).map(Value::display).unwrap_or_default();
        Ok(match (recv, name) {
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
            (Value::List(items), "indices") => Value::Range(0, items.len() as i64, false),
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
            (Value::Some(v), "orPanic") => (*v).clone(),
            (Value::None, "orPanic") => {
                let reason = args.first().map(Value::display);
                return Err(Flow::Panic(Panic {
                    code: Code::or_panic_on_none,
                    span,
                    reason,
                    stack: Vec::new(),
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
    let overflow = |v: Option<i64>| {
        v.map(I)
            .map_or_else(|| panic(Code::integer_overflow, span), Ok)
    };
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
                return panic(Code::division_by_zero, span);
            }
            F(a as f64 / b as f64)
        }
        (BinOp::IntDiv | BinOp::Mod, I(_), I(0)) => return panic(Code::division_by_zero, span),
        (BinOp::IntDiv, I(a), I(b)) => return overflow(floor_div(a, b)),
        (BinOp::Mod, I(_), I(-1)) => I(0),
        (BinOp::Mod, I(a), I(b)) => {
            return overflow(
                floor_div(a, b)
                    .and_then(|q| q.checked_mul(b))
                    .and_then(|m| a.checked_sub(m)),
            );
        }
        (BinOp::Pow, I(a), I(b)) => {
            if b < 0 {
                return panic(Code::negative_exponent, span);
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
            return panic(Code::shift_out_of_range, span);
        }
        (BinOp::Shl, I(a), I(b)) => I(a.wrapping_shl(b as u32)),
        (BinOp::Shr, I(a), I(b)) => I(a >> b),
        (BinOp::Range, I(a), I(b)) => Value::Range(a, b, false),
        (BinOp::RangeIncl, I(a), I(b)) => Value::Range(a, b, true),
        (BinOp::Add, Value::Str(a), Value::Str(b)) => Value::Str(format!("{a}{b}").into()),
        (BinOp::Eq, a, b) => Value::Bool(a == b),
        (BinOp::Ne, a, b) => Value::Bool(a != b),
        (BinOp::Lt, a, b) => Value::Bool(a.compare(&b).is_some_and(Ordering::is_lt)),
        (BinOp::Gt, a, b) => Value::Bool(a.compare(&b).is_some_and(Ordering::is_gt)),
        (BinOp::Le, a, b) => Value::Bool(a.compare(&b).is_some_and(Ordering::is_le)),
        (BinOp::Ge, a, b) => Value::Bool(a.compare(&b).is_some_and(Ordering::is_ge)),
        _ => Value::Void,
    })
}
