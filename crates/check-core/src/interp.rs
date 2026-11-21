use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::rc::Rc;

use crate::ast::*;
use crate::checker::{EQ, Info, PRINTABLE, RESULT, Resolved, SET};
use crate::diag::{Code, Span};
use crate::host::{self, Io};
use indexmap::{IndexMap, IndexSet};

use crate::value::{Callable, Record, Value, write_items, write_map};

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
    /// A top-level `try` found `None`, or an `Err` with this text.
    Stop(Span, Option<String>),
    Panic(Panic),
}

type Eval = Result<Value, Flow>;

type ArgValue = (Option<Rc<str>>, Value, Option<Place>);

type Place = (Root, Vec<Step>);

type Bound = (Vec<Value>, Vec<Option<Place>>);
/// Live `ref` bindings: name, place, and the statement of its last use.
type Bindings<'s> = Vec<(&'s String, Place, usize)>;

type Writer<'f> = &'f mut dyn FnMut(&mut Value) -> Result<(), Flow>;

type Descent<'s> = Result<Option<(Holder, &'s [Step])>, Flow>;

enum Holder {
    Record(Rc<RefCell<Record>>),
    Value(Rc<RefCell<Value>>),
}

/// Where a write path starts: a variable, or a value computed once, such as a call's result,
/// which a write can only change through something shared in it.
enum Root {
    Var(String),
    Value(Value),
}

enum Step {
    Field(String),
    /// A list index or a map key.
    Index(Value, Span),
}

#[derive(Default)]
struct Frame {
    scopes: Vec<HashMap<String, Value>>,
    /// A lambda call's captured variables, read after the frame's own scopes and never written.
    captured: Option<Rc<HashMap<String, Value>>>,
}

pub struct Interpreter<'a> {
    info: &'a Info,
    funcs: HashMap<String, Rc<FuncDecl>>,
    classes: HashMap<String, Rc<ClassDecl>>,
    enums: HashMap<String, Rc<EnumDecl>>,
    consts: HashMap<String, ConstDecl>,
    const_values: HashMap<String, Value>,
    building: HashSet<String>,
    globals: HashMap<String, Value>,
    frames: Vec<Frame>,
    depth: usize,
    /// The `shared class` objects being printed, by address, to stop at a cycle.
    printing: Vec<usize>,
    /// Steps left before evaluation stops with `out_of_steps`; `None` is no limit.
    budget: Option<u64>,
    input: &'a mut dyn BufRead,
    output: &'a mut dyn Write,
}

const MAX_DEPTH: usize = 10_000;

/// The step budget for consts and data values, so loading data can't hang a host.
const DATA_BUDGET: u64 = 10_000_000;

pub fn run(
    program: &[Stmt],
    library: &[Stmt],
    info: &Info,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<Option<(Span, Option<String>)>, Panic> {
    let mut interp = match start(program, library, info, input, output) {
        Ok(interp) => interp,
        Err(Flow::Stop(span, err)) => return Ok(Some((span, err))),
        Err(Flow::Panic(p)) => return Err(p),
        Err(_) => unreachable!("start returns only stops and panics"),
    };
    interp.budget = None;
    let mut bound = Vec::new();
    for i in 0..program.len() {
        match interp.step(program, i, &mut bound) {
            Ok(_) => {}
            Err(Flow::Panic(p)) => return Err(p),
            Err(Flow::Stop(span, err)) => return Ok(Some((span, err))),
            Err(_) => {}
        }
    }
    Ok(None)
}

/// The value of the data const whose value is node `id`.
pub fn eval_data(
    program: &[Stmt],
    library: &[Stmt],
    info: &Info,
    id: NodeId,
) -> Result<Value, Panic> {
    let (mut input, mut output) = (std::io::empty(), std::io::sink());
    let mut interp = match start(program, library, info, &mut input, &mut output) {
        Ok(interp) => interp,
        Err(Flow::Stop(span, reason)) => {
            return Err(Panic {
                code: Code::data_try_failed,
                span,
                reason,
                stack: Vec::new(),
            });
        }
        Err(Flow::Panic(p)) => return Err(p),
        Err(_) => unreachable!("start returns only stops and panics"),
    };
    let name = program.iter().find_map(|s| match s {
        Stmt::Data(d) if d.value.id == id => Some(&d.name),
        _ => None,
    });
    Ok(interp.const_values.remove(name.unwrap()).unwrap())
}

/// An interpreter with the program's declarations added and its consts and data values built.
fn start<'a>(
    program: &[Stmt],
    library: &[Stmt],
    info: &'a Info,
    input: &'a mut dyn BufRead,
    output: &'a mut dyn Write,
) -> Result<Interpreter<'a>, Flow> {
    let mut interp = Interpreter {
        info,
        funcs: HashMap::new(),
        classes: HashMap::new(),
        enums: HashMap::new(),
        consts: HashMap::new(),
        const_values: HashMap::new(),
        building: HashSet::new(),
        globals: HashMap::new(),
        frames: vec![Frame::default()],
        depth: 0,
        printing: Vec::new(),
        budget: Some(DATA_BUDGET),
        input,
        output,
    };
    // Data values are built in load order, after member consts.
    let mut data = Vec::new();
    for stmt in library.iter().chain(program) {
        match stmt {
            Stmt::Func(decl) => interp.add_func(decl),
            Stmt::Implement(i) => interp.add_members(&i.target, &i.members),
            Stmt::Interface(i) => {
                for m in i.members.methods.iter().filter(|m| m.body.is_some()) {
                    interp.add_func(m);
                }
            }
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
            Stmt::Data(d) => {
                let decl = ConstDecl {
                    name: d.name.clone(),
                    span: d.span,
                    ty: None,
                    value: d.value.clone(),
                    is_private: false,
                };
                interp.consts.insert(d.name.clone(), decl);
                data.push(d.name.clone());
            }
            _ => {}
        }
    }
    let mut consts: Vec<String> = interp.consts.keys().cloned().collect();
    consts.retain(|k| !data.contains(k));
    consts.sort();
    for key in consts.into_iter().chain(data) {
        if let Err(e @ (Flow::Panic(_) | Flow::Stop(..))) =
            interp.resolved(&Resolved::Const(key), &[], Span::default())
        {
            return Err(e);
        }
    }
    Ok(interp)
}

/// A case the checker should have ruled out, returned as an error rather than a wrong value.
pub(crate) fn internal<T>(what: &str, span: Span) -> Result<T, Flow> {
    Err(Flow::Panic(Panic {
        code: Code::internal_error,
        span,
        reason: Some(what.to_string()),
        stack: Vec::new(),
    }))
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
        if c.ok {
            value = result_variant("Ok", value);
        }
    }
    value
}

fn pair(a: Value, b: Value) -> Value {
    Value::Record(Rc::new(Record::unnamed(None, vec![a, b])))
}

fn result_variant(name: &str, value: Value) -> Value {
    Value::Variant(Rc::new(Record {
        name: name.into(),
        ty: RESULT.into(),
        by_fields: false,
        field_names: None,
        fields: vec![value],
    }))
}

fn descend<'s>(v: &mut Value, steps: &'s [Step], f: Writer) -> Descent<'s> {
    if let Value::Cell(rc) = v {
        return Ok(Some((Holder::Value(rc.clone()), steps)));
    }
    let Some((step, rest)) = steps.split_first() else {
        f(v)?;
        return Ok(None);
    };
    match (v, step) {
        (Value::Shared(rc), _) => Ok(Some((Holder::Record(rc.clone()), steps))),
        (Value::SharedBox(rc), Step::Field(name)) if name == "value" => {
            Ok(Some((Holder::Value(rc.clone()), rest)))
        }
        (Value::SharedBox(rc), _) => Ok(Some((Holder::Value(rc.clone()), steps))),
        (Value::Object(rc), _) => descend_record(Rc::make_mut(rc), steps, f),
        (Value::List(items), Step::Index(i, span)) => {
            let items = Rc::make_mut(items);
            let i = match i {
                Value::Int(i) => usize::try_from(*i).ok().filter(|i| *i < items.len()),
                _ => None,
            };
            match i {
                Some(i) => descend(&mut items[i], rest, f),
                None => panic(Code::index_out_of_range, *span),
            }
        }
        (Value::Map(entries), Step::Index(key, span)) => match Rc::make_mut(entries).get_mut(key) {
            Some(v) => descend(v, rest, f),
            None => panic(Code::missing_key, *span),
        },
        _ => Ok(None),
    }
}

fn loop_end(result: Eval, wraps: bool) -> Result<Option<Value>, Flow> {
    match result {
        Ok(_) | Err(Flow::Continue) => Ok(None),
        Err(Flow::Break(v)) => Ok(Some(if wraps {
            Value::Some(Rc::new(v))
        } else {
            Value::Void
        })),
        Err(other) => Err(other),
    }
}

/// Runs `f` with room on the stack, growing it if needed, so deep programs and values reach the
/// call limit instead of overflowing the host thread's stack.
pub(crate) fn grow<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(256 * 1024, 4 * 1024 * 1024, f)
}

fn through_ref<R>(slot: &mut Value, f: impl FnOnce(&mut Value) -> R) -> R {
    match slot {
        Value::SharedBox(v) => f(&mut v.borrow_mut()),
        slot => f(slot),
    }
}

fn uncell(v: Value) -> Value {
    match v {
        Value::Cell(v) => v.borrow().clone(),
        v => v,
    }
}

fn share_new(v: Value) -> Value {
    Value::Cell(Rc::new(RefCell::new(v)))
}

/// A box for a `shared` parameter or field: the one a `Shared` value holds, else a new one.
fn share(v: Value) -> Value {
    match v {
        Value::SharedBox(rc) => Value::Cell(rc),
        v => share_new(v),
    }
}

fn deref(v: Value) -> Value {
    match v {
        Value::SharedBox(v) => v.borrow().clone(),
        v => v,
    }
}

/// A field's value, read through the box of a `shared` field.
fn field(v: &Value, name: &str, span: Span) -> Eval {
    raw_field(v, name, span).map(uncell)
}

fn raw_field(v: &Value, name: &str, span: Span) -> Eval {
    let found = match v {
        Value::Object(r) | Value::Record(r) => r.index(name).map(|i| r.fields[i].clone()),
        Value::Shared(r) => {
            let r = r.borrow();
            r.index(name).map(|i| r.fields[i].clone())
        }
        _ => None,
    };
    match found {
        Some(v) => Ok(v),
        None => internal(&format!("no field `{name}`"), span),
    }
}

fn descend_record<'s>(r: &mut Record, steps: &'s [Step], f: Writer) -> Descent<'s> {
    let Some((Step::Field(name), rest)) = steps.split_first() else {
        return Ok(None);
    };
    let Some(i) = r.index(name) else {
        return internal(&format!("no field `{name}` to write"), Span::default());
    };
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

    fn path(&mut self, e: &Expr, steps: &mut Vec<Step>) -> Result<Root, Flow> {
        match &e.kind {
            ExprKind::Field(base, name) => {
                let root = self.path(base, steps)?;
                steps.push(Step::Field(name.clone()));
                Ok(root)
            }
            ExprKind::Index(base, index) => {
                let root = self.path(base, steps)?;
                let key = self.expr(index)?;
                steps.push(Step::Index(key, e.span));
                Ok(root)
            }
            ExprKind::Name(name) => Ok(self.root(name)),
            ExprKind::This => Ok(self.root("this")),
            _ => Ok(Root::Value(self.expr(e)?)),
        }
    }

    /// A write path's variable. A lambda's captured variable is a copy that writes can only reach
    /// through (the checker allows only paths into a `Shared` or `shared class`), so it starts there.
    fn root(&self, name: &str) -> Root {
        let frame = self.frames.last().expect("frame");
        let own = frame.scopes.iter().any(|s| s.contains_key(name));
        match frame.captured.as_ref().and_then(|c| c.get(name)) {
            Some(v) if !own => Root::Value(v.clone()),
            _ => Root::Var(name.to_string()),
        }
    }

    fn write_steps(&mut self, root: &mut Root, steps: &[Step], f: Writer) -> Result<(), Flow> {
        let slot = match root {
            Root::Var(name) => match self.slot(name) {
                Some(slot) => slot,
                None => {
                    return internal(&format!("no variable `{name}` to write"), Span::default());
                }
            },
            Root::Value(v) => v,
        };
        let mut next = descend(slot, steps, f)?;
        while let Some((holder, rest)) = next {
            next = match holder {
                Holder::Record(r) => descend_record(&mut r.borrow_mut(), rest, f)?,
                Holder::Value(v) => descend(&mut v.borrow_mut(), rest, f)?,
            };
        }
        Ok(())
    }

    fn read_steps(&mut self, root: &mut Root, steps: &[Step]) -> Eval {
        let mut out = Value::Void;
        self.write_steps(root, steps, &mut |v| {
            out = v.clone();
            Ok(())
        })?;
        Ok(out)
    }

    fn resolved(&mut self, r: &Resolved, args: &[Arg], span: Span) -> Eval {
        match r {
            Resolved::Const(key) => {
                if let Some(v) = self.const_values.get(key) {
                    return Ok(v.clone());
                }
                // A data value can reach itself through a call the checker doesn't follow.
                if !self.building.insert(key.clone()) {
                    return panic(Code::const_cycle, span);
                }
                let value = self.consts[key].value.clone();
                let v = self.expr(&value);
                self.building.remove(key);
                let v = v?;
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
                    ty: enum_name.as_str().into(),
                    by_fields: false,
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
            Resolved::Dynamic(_) => unreachable!("only method calls dispatch at run time"),
            Resolved::Static(key) | Resolved::Method(key) => {
                let decl = self.funcs[key].clone();
                self.call_named(&decl, args, span, None)
            }
        }
    }

    fn implements(&self, tag: &str, iface: &str) -> bool {
        self.info
            .implements
            .get(tag)
            .is_some_and(|i| i.contains(iface))
    }

    /// The method `name` of `this`'s type, its own or an interface default.
    fn find_method(&self, this: &Value, name: &str) -> Rc<FuncDecl> {
        let key = format!("{}.{name}", this.tag());
        let key = self.info.aliases.get(&key).unwrap_or(&key);
        self.funcs[key].clone()
    }

    /// How `print` and interpolation show a value, through `toString()` where it's `Printable`.
    fn text(&mut self, v: &Value, quoted: bool, span: Span) -> Result<String, Flow> {
        let mut out = String::new();
        self.write_text(v, quoted, span, &mut out)?;
        Ok(out)
    }

    /// Writes [`Interpreter::text`] into `out`, so nested values share one buffer.
    fn write_text(
        &mut self,
        v: &Value,
        quoted: bool,
        span: Span,
        out: &mut String,
    ) -> Result<(), Flow> {
        grow(|| self.write_text_inner(v, quoted, span, out))
    }

    fn write_text_inner(
        &mut self,
        v: &Value,
        quoted: bool,
        span: Span,
        out: &mut String,
    ) -> Result<(), Flow> {
        if self.implements(&v.tag(), PRINTABLE) {
            let decl = self.find_method(v, "toString");
            let text = self
                .run_decl(&decl, Vec::new(), Some(v.clone()), span, &[])?
                .0;
            out.push_str(&text.display());
            return Ok(());
        }
        let field = |f: &Value, out: &mut String| self.write_text(f, true, span, out);
        match v {
            Value::List(items) => write_items(out, "[", items.iter(), "]", field),
            Value::Set(items) => write_items(out, "Set([", items.iter(), "])", field),
            Value::Map(entries) => write_map(out, entries.iter(), field),
            Value::Some(inner) if !matches!(**inner, Value::Void) => {
                write_items(out, "Some(", [&**inner].into_iter(), ")", field)
            }
            Value::Object(r) | Value::Record(r) => r.write_with(true, out, field),
            Value::Variant(r) => r.write_with(false, out, field),
            Value::Shared(r) => {
                let address = Rc::as_ptr(r) as usize;
                if self.printing.contains(&address) {
                    out.push_str("...");
                    return Ok(());
                }
                // A copy, so `toString()` on a field can change this object while it's printed.
                let record = r.borrow().clone();
                self.printing.push(address);
                let done =
                    record.write_with(true, out, |f, out| self.write_text(f, true, span, out));
                self.printing.pop();
                done
            }
            Value::SharedBox(r) => {
                // A copy, as for `Shared`: printing can run code that changes it.
                let inner = r.borrow().clone();
                write_items(out, "Shared(", [&inner].into_iter(), ")", field)
            }
            Value::Cell(r) => {
                let inner = r.borrow().clone();
                self.write_text(&inner, quoted, span, out)
            }
            Value::Str(_) if !quoted => {
                out.push_str(&v.display());
                Ok(())
            }
            _ => {
                v.write_code(out);
                Ok(())
            }
        }
    }

    fn construct(&mut self, class_name: &str, args: &[Arg], span: Span) -> Eval {
        let class = self.classes[class_name].clone();
        let (mut fields, _) = self.bind_args(&class.fields, args)?;
        for (value, f) in fields.iter_mut().zip(&class.fields) {
            if f.is_shared {
                *value = share(std::mem::replace(value, Value::Void));
            }
        }
        let record = Record {
            name: class_name.into(),
            ty: class_name.into(),
            by_fields: self.implements(class_name, EQ),
            field_names: Some(
                class
                    .fields
                    .iter()
                    .map(|f| f.name.as_str().into())
                    .collect(),
            ),
            fields,
        };
        let value = if class.is_shared {
            Value::Shared(Rc::new(RefCell::new(record)))
        } else {
            Value::Object(Rc::new(record))
        };
        match &class.init {
            Some(init) => self.run_init(init, value, span),
            None => Ok(value),
        }
    }

    fn copy_with_changes(&mut self, original: &Expr, changes: &[Arg], span: Span) -> Eval {
        let mut copy = self.expr(original)?;
        let mut record = match &copy {
            Value::Object(r) | Value::Record(r) => (**r).clone(),
            Value::Shared(r) => r.borrow().clone(),
            _ => return Ok(copy),
        };
        for change in changes {
            let value = self.expr(&change.value)?;
            let name = change.name.as_deref().unwrap_or_default();
            let Some(i) = record.index(name) else {
                return internal(
                    &format!("no field `{name}` to copy with"),
                    change.value.span,
                );
            };
            let class = self.classes.get(&*record.name);
            record.fields[i] = match class.is_some_and(|c| c.fields[i].is_shared) {
                true => share(value),
                false => value,
            };
        }
        let class = self.classes.get(&*record.name).cloned();
        copy = match &copy {
            Value::Shared(_) => Value::Shared(Rc::new(RefCell::new(record))),
            Value::Object(_) => Value::Object(Rc::new(record)),
            _ => Value::Record(Rc::new(record)),
        };
        match class.as_ref().and_then(|c| c.init.as_ref()) {
            Some(init) => self.run_init(init, copy, span),
            None => Ok(copy),
        }
    }

    fn run_init(&mut self, init: &FuncDecl, this: Value, span: Span) -> Eval {
        let scope = HashMap::from([("this".to_string(), this)]);
        self.frames.push(Frame {
            scopes: vec![scope],
            captured: None,
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
                Value::Variant(r) if &*r.name == name => self.match_all(args, &r.fields, binds)?,
                Value::Object(_) | Value::Shared(_) => {
                    let (class, fields) = object_fields(v);
                    *class == **name && self.match_class(name, args, &fields, binds)?
                }
                _ => false,
            },
            PatternKind::Record(items) => match v {
                Value::Record(r) => self.match_all(items, &r.fields, binds)?,
                Value::Object(_) | Value::Shared(_) => {
                    self.match_all(items, &object_fields(v).1, binds)?
                }
                _ => false,
            },
            PatternKind::List(items) => match v {
                Value::List(values) => self.match_list(items, values, binds)?,
                _ => false,
            },
            PatternKind::Rest(_) => false,
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

    fn match_all(
        &mut self,
        items: &[Pattern],
        values: &[Value],
        binds: &mut Vec<(String, Value)>,
    ) -> Result<bool, Flow> {
        for (item, value) in items.iter().zip(values) {
            if !self.matches(item, value, binds)? {
                return Ok(false);
            }
        }
        Ok(items.len() == values.len())
    }

    fn match_class(
        &mut self,
        class: &str,
        items: &[Pattern],
        fields: &[Value],
        binds: &mut Vec<(String, Value)>,
    ) -> Result<bool, Flow> {
        let rest = self.classes[class].fields.iter().position(|f| f.is_rest);
        let Some(r) = rest else {
            return self.match_all(items, fields, binds);
        };
        let Value::List(tail) = &fields[r] else {
            return Ok(false);
        };
        Ok(self.match_all(&items[..r], &fields[..r], binds)?
            && self.match_list(&items[r..], tail, binds)?)
    }

    fn match_list(
        &mut self,
        items: &[Pattern],
        values: &[Value],
        binds: &mut Vec<(String, Value)>,
    ) -> Result<bool, Flow> {
        let Some(k) = items
            .iter()
            .position(|p| matches!(p.kind, PatternKind::Rest(_)))
        else {
            return self.match_all(items, values, binds);
        };
        let after = items.len() - k - 1;
        if values.len() < k + after {
            return Ok(false);
        }
        let end = values.len() - after;
        if !self.match_all(&items[..k], &values[..k], binds)?
            || !self.match_all(&items[k + 1..], &values[end..], binds)?
        {
            return Ok(false);
        }
        if let PatternKind::Rest(name) = &items[k].kind
            && name != "_"
        {
            binds.push((name.clone(), Value::List(Rc::new(values[k..end].to_vec()))));
        }
        Ok(true)
    }

    fn spend(&mut self, steps: u64, span: Span) -> Result<(), Flow> {
        if let Some(left) = &mut self.budget {
            if *left < steps {
                return panic(Code::out_of_steps, span);
            }
            *left -= steps;
        }
        Ok(())
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

    /// A variable's value: the frame's own scopes, then a lambda's captures, then globals.
    fn variable(&self, name: &str) -> Option<&Value> {
        let frame = self.frames.last().expect("frame");
        frame
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .or_else(|| frame.captured.as_ref().and_then(|c| c.get(name)))
            .or_else(|| self.globals.get(name))
    }

    fn lookup(&mut self, name: &str) -> Option<Value> {
        if let Some(v) = self.variable(name) {
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
            Stmt::Let {
                name,
                value,
                is_shared,
                ..
            } => {
                let v = match value {
                    Some(value) => self.scoped(|i| i.expr(value))?,
                    None => Value::Void,
                };
                self.define(name, if *is_shared { share_new(v) } else { v });
                Ok(Value::Void)
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let info = self.info;
                let mut steps = Vec::new();
                let mut root = self.path(target, &mut steps)?;
                let rhs = self.scoped(|i| i.expr(value))?;
                // `m[k] = v` can add a key, so it writes to the map rather than the entry.
                if op.is_none()
                    && let Some((last @ Step::Index(key, _), init)) = steps.split_last()
                {
                    self.write_steps(&mut root, init, &mut |slot| {
                        through_ref(slot, |slot| match slot {
                            Value::Map(entries) => {
                                Rc::make_mut(entries).insert(key.clone(), rhs.clone());
                                Ok(())
                            }
                            _ => descend(slot, std::slice::from_ref(last), &mut |v| {
                                *v = rhs.clone();
                                Ok(())
                            })
                            .map(|_| ()),
                        })
                    })?;
                    return Ok(Value::Void);
                }
                self.write_steps(&mut root, &steps, &mut |slot| {
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
            Stmt::LetPattern {
                pattern,
                value,
                otherwise,
                ..
            } => {
                let v = self.expr(value)?;
                let mut binds = Vec::new();
                let matched = self.matches(pattern, &v, &mut binds)?;
                match otherwise {
                    Some(otherwise) if !matched => self.expr(otherwise),
                    _ => {
                        for (name, v) in binds {
                            self.define(&name, v);
                        }
                        Ok(Value::Void)
                    }
                }
            }
            Stmt::Func(_)
            | Stmt::Class(_)
            | Stmt::Enum(_)
            | Stmt::Implement(_)
            | Stmt::Interface(_)
            | Stmt::Data(_) => Ok(Value::Void),
            Stmt::RefLet { .. } => unreachable!("blocks and the program bind refs themselves"),
            Stmt::Expr(e) => self.scoped(|i| i.expr(e)),
        }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Eval {
        self.frame().scopes.push(HashMap::new());
        let mut bound = Vec::new();
        let mut last = Ok(Value::Void);
        for i in 0..stmts.len() {
            last = self.step(stmts, i, &mut bound);
            if last.is_err() {
                break;
            }
        }
        if matches!(
            stmts.last(),
            Some(Stmt::Let { .. } | Stmt::Assign { .. } | Stmt::RefLet { .. })
        ) {
            last = last.map(|_| Value::Void);
        }
        // Leaving early writes back the bindings still live, unless a panic ends the program.
        if !matches!(last, Err(Flow::Panic(_)))
            && let Err(flow) = self.write_refs(&mut bound, usize::MAX)
        {
            last = Err(flow);
        }
        self.frame().scopes.pop();
        last
    }

    /// Runs `stmts[i]`, then writes back the `ref` bindings whose last use it is.
    fn step<'s>(&mut self, stmts: &'s [Stmt], i: usize, bound: &mut Bindings<'s>) -> Eval {
        let value = match &stmts[i] {
            Stmt::RefLet { name, target, .. } => {
                let place = self.bind_ref(name, target)?;
                bound.push((name, place, i + self.info.ref_ends[&target.id]));
                Value::Void
            }
            stmt => self.stmt(stmt)?,
        };
        self.write_refs(bound, i)?;
        Ok(value)
    }

    /// Writes back the `ref` bindings that end by statement `i`, innermost first.
    fn write_refs(&mut self, bound: &mut Bindings, i: usize) -> Result<(), Flow> {
        for k in (0..bound.len()).rev() {
            if bound[k].2 > i {
                continue;
            }
            let (name, (mut root, steps), _) = bound.remove(k);
            let value = self.lookup(name).unwrap_or(Value::Void);
            self.write_steps(&mut root, &steps, &mut |slot| {
                *slot = value.clone();
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Copies the place `target` names into `name`, resolving it once, and returns the place.
    fn bind_ref(&mut self, name: &str, target: &Expr) -> Result<Place, Flow> {
        let mut steps = Vec::new();
        let mut root = self.path(target, &mut steps)?;
        let value = self.read_steps(&mut root, &steps)?;
        self.define(name, value);
        Ok((root, steps))
    }

    fn expr(&mut self, expr: &Expr) -> Eval {
        let value = grow(|| self.raw(expr))?;
        Ok(self.coerce(expr.id, value))
    }

    fn raw(&mut self, expr: &Expr) -> Eval {
        let info = self.info;
        if let Some(r) = info.resolved.get(&expr.id) {
            match &expr.kind {
                ExprKind::Name(_) | ExprKind::Field(..) => return self.resolved(r, &[], expr.span),
                ExprKind::Call(_, args) | ExprKind::Record(args) => {
                    return self.resolved(r, args, expr.span);
                }
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
                        StrSeg::Expr(e) => {
                            let v = self.expr(e)?;
                            out.push_str(&self.text(&v, false, e.span)?);
                        }
                    }
                }
                Ok(Value::Str(out.into()))
            }
            ExprKind::Name(name) => match self.lookup(name) {
                Some(v) => Ok(uncell(v)),
                None => internal(&format!("no variable `{name}`"), expr.span),
            },
            ExprKind::SharedOf(x) => self.shared_of(x),
            ExprKind::This => match self.lookup("this") {
                Some(v) => Ok(v),
                None => internal("no `this`", expr.span),
            },
            ExprKind::Field(base, name) => match self.expr(base)? {
                Value::SharedBox(v) if name == "value" => Ok(v.borrow().clone()),
                Value::SharedBox(v) => field(&v.borrow(), name, expr.span),
                v => field(&v, name, expr.span),
            },
            ExprKind::Is(lhs, pattern) => {
                let v = self.expr(lhs)?;
                let mut binds = Vec::new();
                let matched = self.matches(pattern, &v, &mut binds)?;
                if matched {
                    for (name, v) in binds {
                        self.define(&name, v);
                    }
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
            ExprKind::Record(args) => match args.first().map(|a| &a.value.kind) {
                Some(ExprKind::Spread(original)) => {
                    self.copy_with_changes(original, &args[1..], expr.span)
                }
                _ => {
                    let names: Option<Rc<[Rc<str>]>> = args
                        .iter()
                        .map(|a| a.name.as_deref().map(Rc::from))
                        .collect();
                    let fields = self.args(args)?;
                    Ok(Value::Record(Rc::new(Record::unnamed(names, fields))))
                }
            },
            ExprKind::Spread(inner) => self.expr(inner),
            ExprKind::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.expr(item)?);
                }
                Ok(Value::List(Rc::new(values)))
            }
            ExprKind::Map(items) => {
                let mut entries = IndexMap::with_capacity(items.len());
                for (k, v) in items {
                    let k = self.expr(k)?;
                    let v = self.expr(v)?;
                    entries.insert(k, v);
                }
                Ok(Value::Map(Rc::new(entries)))
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
                    (UnOp::Try, Value::Variant(r)) if &*r.name == "Ok" => Ok(r.fields[0].clone()),
                    (UnOp::Try, Value::Variant(r)) if self.frames.len() == 1 => {
                        let text = self.text(&r.fields[0], false, expr.span)?;
                        Err(Flow::Stop(expr.span, Some(text)))
                    }
                    (UnOp::Try, v @ Value::Variant(_)) => Err(Flow::Return(v)),
                    (UnOp::Try, _) if self.frames.len() == 1 => Err(Flow::Stop(expr.span, None)),
                    (UnOp::Try, _) => Err(Flow::Return(Value::None)),
                    (_, v) => Ok(v),
                }
            }
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::Call(callee, args) => self.call(callee, args, expr.span),
            ExprKind::Method(recv, name, args) => self.method(expr.id, recv, name, args, expr.span),
            ExprKind::Index(list, index) => {
                let l = deref(self.expr(list)?);
                let i = self.expr(index)?;
                let is_map = matches!(l, Value::Map(_));
                let found = match (l, i) {
                    (Value::List(items), Value::Int(i)) => {
                        usize::try_from(i).ok().and_then(|i| items.get(i).cloned())
                    }
                    (Value::Map(entries), key) => entries.get(&key).cloned(),
                    _ => None,
                };
                let unwrapped = self.info.unwrapped_index.contains(&expr.id);
                match found {
                    Some(v) if unwrapped => Ok(v),
                    Some(v) => Ok(Value::Some(Rc::new(v))),
                    // Narrowing can miss a function that changes the collection.
                    None if unwrapped && is_map => panic(Code::missing_key, expr.span),
                    None if unwrapped => panic(Code::index_out_of_range, expr.span),
                    None => Ok(Value::None),
                }
            }
            ExprKind::Lambda(params, _, body) => {
                let captured = info
                    .captures
                    .get(&expr.id)
                    .map_or_else(HashMap::new, |names| {
                        names
                            .iter()
                            .filter_map(|n| Some((n.clone(), self.lookup(n)?)))
                            .collect()
                    });
                let sig = self
                    .info
                    .signatures
                    .get(&expr.id)
                    .cloned()
                    .unwrap_or_default();
                Ok(Value::Func(Rc::new(Callable::Lambda {
                    params: params.clone(),
                    body: body.clone(),
                    captured: Rc::new(captured),
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
                    self.spend(1, expr.span)?;
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
                self.spend(1, expr.span)?;
                match self.expr(body) {
                    Ok(_) | Err(Flow::Continue) => {}
                    Err(Flow::Break(v)) => return Ok(v),
                    Err(other) => return Err(other),
                }
            },
            ExprKind::For(pattern, iter, body, true) => {
                let wraps = self.info.wrapping.contains(&expr.id);
                let PatternKind::Name(name) = &pattern.kind else {
                    return Ok(Value::Void);
                };
                let mut steps = Vec::new();
                let mut root = self.path(iter, &mut steps)?;
                let mut i = 0;
                loop {
                    self.spend(1, expr.span)?;
                    let item = match deref(self.read_steps(&mut root, &steps)?) {
                        Value::List(items) => items.get(i).cloned(),
                        _ => None,
                    };
                    let Some(item) = item else {
                        return Ok(if wraps { Value::None } else { Value::Void });
                    };
                    self.frame()
                        .scopes
                        .push(HashMap::from([(name.clone(), item)]));
                    let result = self.expr(body);
                    let mut scope = self.frame().scopes.pop().expect("loop scope");
                    if let Err(Flow::Panic(p)) = result {
                        return Err(Flow::Panic(p));
                    }
                    let item = scope.remove(name).unwrap_or(Value::Void);
                    let mut gone = true;
                    self.write_steps(&mut root, &steps, &mut |list| {
                        through_ref(list, |list| {
                            if let Value::List(items) = list
                                && let Some(slot) = Rc::make_mut(items).get_mut(i)
                            {
                                *slot = item.clone();
                                gone = false;
                            }
                        });
                        Ok(())
                    })?;
                    if gone {
                        return panic(Code::for_ref_element_gone, iter.span);
                    }
                    if let Some(v) = loop_end(result, wraps)? {
                        return Ok(v);
                    }
                    i += 1;
                }
            }
            ExprKind::For(pattern, iter, body, false) => {
                let wraps = self.info.wrapping.contains(&expr.id);
                // Each holds its collection's `Rc`, so the loop walks the collection as it was
                // when the loop started, without copying it.
                let items: Box<dyn Iterator<Item = Value>> = match self.expr(iter)? {
                    Value::Range(lo, hi, false) => Box::new((lo..hi).map(Value::Int)),
                    Value::Range(lo, hi, true) => Box::new((lo..=hi).map(Value::Int)),
                    Value::List(items) => Box::new((0..items.len()).map(move |i| items[i].clone())),
                    Value::Set(items) => Box::new((0..items.len()).map(move |i| items[i].clone())),
                    Value::Map(entries) => Box::new((0..entries.len()).map(move |i| {
                        let (k, v) = entries.get_index(i).expect("in range");
                        pair(k.clone(), v.clone())
                    })),
                    v => return internal(&format!("`for` over {}", v.code()), iter.span),
                };
                for item in items {
                    self.spend(1, expr.span)?;
                    let mut binds = Vec::new();
                    self.matches(pattern, &item, &mut binds)?;
                    self.frame().scopes.push(binds.into_iter().collect());
                    let result = self.expr(body);
                    self.frame().scopes.pop();
                    if let Some(v) = loop_end(result, wraps)? {
                        return Ok(v);
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
                Value::Variant(r) if &*r.name == "Ok" => Ok(r.fields[0].clone()),
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
        Ok(self
            .spread_args(args)?
            .into_iter()
            .map(|(_, v, _)| v)
            .collect())
    }

    fn shared_of(&mut self, x: &Expr) -> Eval {
        let cell = match &x.kind {
            ExprKind::Field(base, name) => raw_field(&self.expr(base)?, name, x.span)?,
            ExprKind::Name(name) => self.lookup(name).unwrap_or(Value::Void),
            _ => Value::Void,
        };
        match cell {
            Value::Cell(rc) => Ok(Value::SharedBox(rc)),
            _ => internal("`shared` of something without a box", x.span),
        }
    }

    fn spread_args(&mut self, args: &[Arg]) -> Result<Vec<ArgValue>, Flow> {
        let mut out = Vec::new();
        for arg in args {
            let name = arg.name.as_deref().map(Rc::from);
            if self.info.ref_args.contains_key(&arg.value.id) {
                let mut steps = Vec::new();
                let mut root = self.path(&arg.value, &mut steps)?;
                let value = self.read_steps(&mut root, &steps)?;
                out.push((name, value, Some((root, steps))));
                continue;
            }
            if self.info.shared_args.contains_key(&arg.value.id) {
                out.push((name, self.shared_of(&arg.value)?, None));
                continue;
            }
            let ExprKind::Spread(inner) = &arg.value.kind else {
                out.push((name, self.expr(&arg.value)?, None));
                continue;
            };
            let Value::Record(r) = self.expr(inner)? else {
                continue;
            };
            for (i, v) in r.fields.iter().enumerate() {
                let name = r.field_names.as_ref().map(|names| names[i].clone());
                out.push((name, v.clone(), None));
            }
        }
        Ok(out)
    }

    fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Eval {
        if let ExprKind::Name(name) = &callee.kind
            && self.variable(name).is_none()
        {
            if let Some(decl) = self.funcs.get(name).cloned() {
                return self.call_named(&decl, args, span, None);
            }
            match name.as_str() {
                "print" => {
                    let v = self.expr(&args[0].value)?;
                    let text = self.text(&v, false, span)?;
                    writeln!(self.output, "{text}").ok();
                    return Ok(Value::Void);
                }
                "Some" => {
                    let v = self.expr(&args[0].value)?;
                    return Ok(Value::Some(Rc::new(v)));
                }
                SET => {
                    let Value::List(items) = self.expr(&args[0].value)? else {
                        return Ok(Value::Void);
                    };
                    let set: IndexSet<Value> = items.iter().cloned().collect();
                    return Ok(Value::Set(Rc::new(set)));
                }
                "Shared" => {
                    let value = self.args(args)?.remove(0);
                    return Ok(Value::SharedBox(Rc::new(RefCell::new(value))));
                }
                "same" => {
                    let values = self.args(args)?;
                    return Ok(Value::Bool(
                        matches!(
                            (&values[0], &values[1]),
                            (Value::Shared(a), Value::Shared(b)) if Rc::ptr_eq(a, b)
                        ) || matches!(
                            (&values[0], &values[1]),
                            (Value::SharedBox(a), Value::SharedBox(b)) if Rc::ptr_eq(a, b)
                        ),
                    ));
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
        let (values, places) = self.bind_args(&decl.params, args)?;
        let names: Vec<String> = decl.params.iter().map(|p| p.name.clone()).collect();
        let (result, kept) = self.run_decl(decl, values, this, span, &names)?;
        self.write_back(places, kept)?;
        Ok(result)
    }

    /// Calls a method that changes `this`, writing it back to the receiver like a `ref` argument.
    fn call_changing(
        &mut self,
        decl: &Rc<FuncDecl>,
        args: &[Arg],
        span: Span,
        this: Value,
        (mut root, steps): Place,
    ) -> Eval {
        let (values, places) = self.bind_args(&decl.params, args)?;
        let mut names: Vec<String> = decl.params.iter().map(|p| p.name.clone()).collect();
        names.push("this".to_string());
        let (result, mut kept) = self.run_decl(decl, values, Some(this), span, &names)?;
        let this = kept.pop().expect("this is kept last");
        self.write_back(places, kept)?;
        self.write_steps(&mut root, &steps, &mut |slot| {
            through_ref(slot, |slot| *slot = this.clone());
            Ok(())
        })?;
        Ok(result)
    }

    fn write_back(&mut self, places: Vec<Option<Place>>, kept: Vec<Value>) -> Result<(), Flow> {
        for (place, value) in places.into_iter().zip(kept) {
            if let Some((mut root, steps)) = place {
                self.write_steps(&mut root, &steps, &mut |slot| {
                    *slot = value.clone();
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    fn run_decl(
        &mut self,
        decl: &FuncDecl,
        mut values: Vec<Value>,
        this: Option<Value>,
        span: Span,
        keep: &[String],
    ) -> Result<(Value, Vec<Value>), Flow> {
        let Some(body) = &decl.body else {
            if let Some(this) = this {
                values.insert(0, this);
            }
            let f =
                host::find(&decl.name).expect("every external declaration has a body in host.rs");
            let mut io = Io {
                input: &mut *self.input,
                output: &mut *self.output,
            };
            return Ok((f(&mut io, values, span)?, Vec::new()));
        };
        let mut scope: HashMap<String, Value> = decl
            .params
            .iter()
            .zip(values)
            .map(|(p, v)| (p.name.clone(), if p.is_shared { share(v) } else { v }))
            .collect();
        if let Some(this) = this {
            scope.insert("this".to_string(), this);
        }
        let frame = Frame {
            scopes: vec![scope],
            captured: None,
        };
        self.enter_keeping(frame, body, span, keep)
    }

    fn bind_args(&mut self, params: &[Param], args: &[Arg]) -> Result<Bound, Flow> {
        let mut slots: Vec<Option<Value>> = vec![None; params.len()];
        let mut places: Vec<Option<Place>> = params.iter().map(|_| None).collect();
        let mut rest = Vec::new();
        let fixed: Vec<usize> = params
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_rest && !p.is_private)
            .map(|(i, _)| i)
            .collect();
        let mut k = 0;
        for (name, v, place) in self.spread_args(args)? {
            let i = match name {
                Some(name) => params.iter().position(|p| *p.name == *name),
                None => {
                    k += 1;
                    fixed.get(k - 1).copied()
                }
            };
            match i {
                Some(i) => {
                    slots[i] = Some(v);
                    places[i] = place;
                }
                None => rest.push(v),
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
                        Some(d) => {
                            // A default sees the names where it's declared, not the caller's locals.
                            self.frames.push(Frame::default());
                            let v = self.expr(d);
                            self.frames.pop();
                            v?
                        }
                        None => Value::Void,
                    },
                }
            };
            values.push(v);
        }
        Ok((values, places))
    }

    fn call_value(&mut self, f: &Value, values: Vec<Value>, span: Span) -> Eval {
        let Value::Func(callable) = f else {
            return internal("calling a value that isn't a function", span);
        };
        match &**callable {
            Callable::Named(decl, _) => Ok(self.run_decl(decl, values, None, span, &[])?.0),
            Callable::Lambda {
                params,
                body,
                captured,
                ..
            } => {
                let frame = Frame {
                    scopes: vec![params.iter().map(|p| p.name.clone()).zip(values).collect()],
                    captured: Some(captured.clone()),
                };
                let body = body.clone();
                Ok(self.enter_keeping(frame, &body, span, &[])?.0)
            }
        }
    }

    fn enter_keeping(
        &mut self,
        frame: Frame,
        body: &Expr,
        span: Span,
        keep: &[String],
    ) -> Result<(Value, Vec<Value>), Flow> {
        if self.depth >= MAX_DEPTH {
            return panic(Code::stack_overflow, span);
        }
        self.spend(1, span)?;
        self.depth += 1;
        self.frames.push(frame);
        let result = self.expr(body);
        let mut frame = self.frames.pop().expect("frame");
        let kept = keep
            .iter()
            .map(|n| frame.scopes[0].remove(n).unwrap_or(Value::Void))
            .collect();
        self.depth -= 1;
        match result {
            Ok(v) | Err(Flow::Return(v)) => Ok((v, kept)),
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
                let decl = self.funcs[key].clone();
                if !info.changes_this.contains_key(key) {
                    let this = deref(self.expr(recv)?);
                    return self.call_named(&decl, args, span, Some(this));
                }
                let mut steps = Vec::new();
                let mut root = self.path(recv, &mut steps)?;
                let this = deref(self.read_steps(&mut root, &steps)?);
                return self.call_changing(&decl, args, span, this, (root, steps));
            }
            Some(Resolved::Dynamic(name)) => {
                let this = deref(self.expr(recv)?);
                let decl = self.find_method(&this, name);
                return self.call_named(&decl, args, span, Some(this));
            }
            Some(r) => return self.resolved(r, args, span),
            None => {}
        }
        if matches!(name, "push" | "pop" | "removeAt" | "add" | "remove") {
            let mut steps = Vec::new();
            let mut root = self.path(recv, &mut steps)?;
            let mut arg = self.args(args)?.into_iter().next();
            let mut out = Value::Void;
            self.write_steps(&mut root, &steps, &mut |slot| {
                through_ref(slot, |slot| {
                    let items = match (slot, arg.take()) {
                        (Value::List(list), a) => {
                            arg = a;
                            Rc::make_mut(list)
                        }
                        (Value::Map(entries), Some(key)) => {
                            out = match Rc::make_mut(entries).shift_remove(&key) {
                                Some(v) => Value::Some(Rc::new(v)),
                                None => Value::None,
                            };
                            return Ok(());
                        }
                        (Value::Set(set), Some(item)) => {
                            let set = Rc::make_mut(set);
                            out = if name == "add" {
                                set.insert(item);
                                Value::Void
                            } else {
                                Value::Bool(set.shift_remove(&item))
                            };
                            return Ok(());
                        }
                        _ => {
                            return internal(
                                &format!("`{name}` on a value that isn't a collection"),
                                span,
                            );
                        }
                    };
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
                        _ => return internal(&format!("`{name}` with the wrong argument"), span),
                    };
                    Ok(())
                })
            })?;
            return Ok(out);
        }
        let r = deref(self.expr(recv)?);
        let values = self.args(args)?;
        self.builtin_method(r, name, values, span)
    }

    fn builtin_method(&mut self, recv: Value, name: &str, args: Vec<Value>, span: Span) -> Eval {
        let scanned = match &recv {
            Value::List(items) if matches!(name, "contains" | "join" | "enumerate") => items.len(),
            Value::Map(entries) if matches!(name, "keys" | "values") => entries.len(),
            _ => 1,
        };
        self.spend(scanned as u64, span)?;
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
            (Value::List(items), "enumerate") => Value::List(Rc::new(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, x)| pair(Value::Int(i as i64), x.clone()))
                    .collect(),
            )),
            (Value::Map(entries), "length") => Value::Int(entries.len() as i64),
            (Value::Map(entries), "isEmpty") => Value::Bool(entries.is_empty()),
            (Value::Map(entries), "contains") => {
                Value::Bool(args.first().is_some_and(|k| entries.contains_key(k)))
            }
            (Value::Map(entries), "keys") => {
                Value::List(Rc::new(entries.iter().map(|(k, _)| k.clone()).collect()))
            }
            (Value::Map(entries), "values") => {
                Value::List(Rc::new(entries.iter().map(|(_, v)| v.clone()).collect()))
            }
            (Value::Set(items), "length") => Value::Int(items.len() as i64),
            (Value::Set(items), "isEmpty") => Value::Bool(items.is_empty()),
            (Value::Set(items), "contains") => {
                Value::Bool(args.first().is_some_and(|a| items.contains(a)))
            }
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
            (Value::Variant(r), "orPanic") if &*r.name == "Ok" => r.fields[0].clone(),
            (Value::Variant(r), "orPanic") => {
                let err = self.text(&r.fields[0], false, span)?;
                let reason = match args.first() {
                    Some(reason) => format!("{}: {err}", reason.display()),
                    None => err,
                };
                return Err(Flow::Panic(Panic {
                    code: Code::or_panic_on_err,
                    span,
                    reason: Some(reason),
                    stack: Vec::new(),
                }));
            }
            (Value::Variant(r), "ok") if &*r.name == "Ok" => {
                Value::Some(Rc::new(r.fields[0].clone()))
            }
            (Value::Variant(_), "ok") => Value::None,
            (v @ (Value::Some(_) | Value::None), "orErr") => {
                let (name, field) = match v {
                    Value::Some(v) => ("Ok", (*v).clone()),
                    _ => ("Err", args.into_iter().next().unwrap_or(Value::Void)),
                };
                result_variant(name, field)
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
            (recv, name) => {
                return internal(&format!("no method `{name}` on {}", recv.code()), span);
            }
        })
    }
}

fn object_fields(v: &Value) -> (Rc<str>, Vec<Value>) {
    match v {
        Value::Shared(r) => {
            let r = r.borrow();
            (r.name.clone(), r.fields.clone())
        }
        Value::Object(r) | Value::Variant(r) | Value::Record(r) => {
            (r.name.clone(), r.fields.clone())
        }
        _ => ("".into(), Vec::new()),
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
        (F(a), I(b)) => (F(a), F(b as f64)),
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
            return match a {
                0 | 1 => Ok(I(if b == 0 { 1 } else { a })),
                -1 => Ok(I(if b % 2 == 0 { 1 } else { -1 })),
                _ => overflow(u32::try_from(b).ok().and_then(|e| a.checked_pow(e))),
            };
        }
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
        (op, l, r) => {
            return internal(&format!("{op:?} on {} and {}", l.code(), r.code()), span);
        }
    })
}
