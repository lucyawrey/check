use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use crate::ast::{Expr, FuncDecl, Param};
use crate::checker::{DYN, MAP, MAYBE, SET};

pub enum Callable {
    Named(Rc<FuncDecl>, String),
    Lambda {
        params: Vec<Param>,
        body: Arc<Expr>,
        /// What the lambda read from outside, as it was when the lambda was made.
        captured: Rc<HashMap<String, Value>>,
        sig: String,
    },
}

#[derive(Clone, PartialEq)]
pub struct Record {
    pub name: Rc<str>,
    /// The key of the record's class or enum, empty for a plain record.
    pub ty: Rc<str>,
    /// A `shared class` that implements `Eq`, so `==` compares its fields.
    pub by_fields: bool,
    pub field_names: Option<Rc<[Rc<str>]>>,
    pub fields: Vec<Value>,
}

impl Drop for Record {
    /// Drops nested values with a loop, since dropping a long chain (a list built from enum
    /// variants) one level per call would overflow the stack.
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.fields);
        while let Some(v) = pending.pop() {
            match v {
                Value::Object(r) | Value::Variant(r) | Value::Record(r) => {
                    if let Ok(mut r) = Rc::try_unwrap(r) {
                        pending.append(&mut r.fields);
                    }
                }
                Value::Shared(r) => {
                    if let Ok(r) = Rc::try_unwrap(r) {
                        pending.append(&mut r.into_inner().fields);
                    }
                }
                Value::List(items) => {
                    if let Ok(items) = Rc::try_unwrap(items) {
                        pending.extend(items);
                    }
                }
                Value::Set(items) => {
                    if let Ok(items) = Rc::try_unwrap(items) {
                        pending.extend(items);
                    }
                }
                Value::Map(entries) => {
                    if let Ok(entries) = Rc::try_unwrap(entries) {
                        pending.extend(entries.into_iter().flat_map(|(k, v)| [k, v]));
                    }
                }
                Value::Some(inner) => {
                    if let Ok(inner) = Rc::try_unwrap(inner) {
                        pending.push(inner);
                    }
                }
                Value::SharedBox(inner) | Value::Cell(inner) => {
                    if let Ok(inner) = Rc::try_unwrap(inner) {
                        pending.push(inner.into_inner());
                    }
                }
                _ => {}
            }
        }
    }
}

impl Record {
    pub fn index(&self, field: &str) -> Option<usize> {
        match &self.field_names {
            Some(names) => names.iter().position(|n| &**n == field),
            None => field.parse().ok(),
        }
    }

    /// Writes the record as code, with `field` writing each field's text.
    pub fn write_with<E>(
        &self,
        always_parens: bool,
        out: &mut String,
        mut field: impl FnMut(&Value, &mut String) -> Result<(), E>,
    ) -> Result<(), E> {
        out.push_str(&self.name);
        if self.fields.is_empty() && !always_parens {
            return Ok(());
        }
        out.push('(');
        for (i, v) in self.fields.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            if let Some(names) = &self.field_names {
                out.push_str(&names[i]);
                out.push_str(" = ");
            }
            field(v, out)?;
        }
        out.push(')');
        Ok(())
    }

    pub fn unnamed(field_names: Option<Rc<[Rc<str>]>>, fields: Vec<Value>) -> Record {
        Record {
            name: "".into(),
            ty: "".into(),
            by_fields: false,
            field_names,
            fields,
        }
    }
}

thread_local! {
    /// The `shared class` objects `==` is comparing, by address.
    static COMPARING: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    List(Rc<Vec<Value>>),
    /// Entries in insertion order.
    Map(Rc<IndexMap<Value, Value>>),
    /// Items in insertion order.
    Set(Rc<IndexSet<Value>>),
    Some(Rc<Value>),
    None,
    Range(i64, i64, bool),
    Func(Rc<Callable>),
    Object(Rc<Record>),
    Shared(Rc<RefCell<Record>>),
    SharedBox(Rc<RefCell<Value>>),
    /// The box behind a `shared` local, parameter or field. Reading the name reads the value,
    /// and printing, `==` and data see only the value.
    Cell(Rc<RefCell<Value>>),
    Variant(Rc<Record>),
    Record(Rc<Record>),
    Void,
}

// `==` is exact for the types a map key or set item can have (`invalid_key_type` rules out
// `Float` and everything else whose `==` is looser), so it's an equivalence for those.
impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Value::Int(n) => n.hash(state),
            Value::Bool(b) => b.hash(state),
            Value::Str(s) => s.hash(state),
            Value::Some(v) => v.hash(state),
            Value::Cell(v) => v.borrow().hash(state),
            Value::Object(r) | Value::Variant(r) | Value::Record(r) => {
                r.name.hash(state);
                r.fields.hash(state);
            }
            // Never a key, so one hash for all is correct, if slow.
            _ => {}
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        crate::interp::grow(|| self.eq_inner(other))
    }
}

impl Value {
    fn eq_inner(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
                (*a as f64) == *b
            }
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a == b,
            (Value::Some(a), Value::Some(b)) => a == b,
            (Value::None, Value::None) | (Value::Void, Value::Void) => true,
            (Value::Range(a, b, x), Value::Range(c, d, y)) => a == c && b == d && x == y,
            (Value::Func(a), Value::Func(b)) => Rc::ptr_eq(a, b),
            (Value::SharedBox(a), Value::SharedBox(b)) => Rc::ptr_eq(a, b),
            (Value::Cell(a), Value::Cell(b)) => Rc::ptr_eq(a, b) || *a.borrow() == *b.borrow(),
            (Value::Object(a), Value::Object(b))
            | (Value::Variant(a), Value::Variant(b))
            | (Value::Record(a), Value::Record(b)) => a == b,
            (Value::Shared(a), Value::Shared(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                if !a.borrow().by_fields {
                    return false;
                }
                // Objects that reach themselves are equal if nothing else differs.
                let pair = (Rc::as_ptr(a) as usize, Rc::as_ptr(b) as usize);
                if COMPARING.with(|c| c.borrow().contains(&pair)) {
                    return true;
                }
                COMPARING.with(|c| c.borrow_mut().push(pair));
                let same = *a.borrow() == *b.borrow();
                COMPARING.with(|c| c.borrow_mut().pop());
                same
            }
            _ => false,
        }
    }
}

impl Value {
    pub fn is_dyn(&self) -> bool {
        matches!(self, Value::Variant(r) if &*r.ty == DYN)
    }

    pub fn truthy(&self) -> bool {
        matches!(self, Value::Bool(true))
    }

    pub fn compare(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    /// The type key its methods are found under.
    pub fn tag(&self) -> Rc<str> {
        match self {
            Value::Int(_) => "Int".into(),
            Value::Float(_) => "Float".into(),
            Value::Bool(_) => "Bool".into(),
            Value::Str(_) => "String".into(),
            Value::List(_) => "List".into(),
            Value::Map(_) => MAP.into(),
            Value::Set(_) => SET.into(),
            Value::Some(_) | Value::None => MAYBE.into(),
            Value::Shared(r) => r.borrow().ty.clone(),
            Value::SharedBox(_) => "Shared".into(),
            Value::Cell(v) => v.borrow().tag(),
            Value::Object(r) | Value::Variant(r) | Value::Record(r) => r.ty.clone(),
            Value::Range(..) | Value::Func(_) | Value::Void => "".into(),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Value::Str(s) => s.to_string(),
            other => other.code(),
        }
    }

    pub fn code(&self) -> String {
        let mut out = String::new();
        self.write_code(&mut out);
        out
    }

    /// Writes the value as code into `out`; nested values write into the same buffer, so deep
    /// values take time in proportion to their size.
    pub fn write_code(&self, out: &mut String) {
        let field = |v: &Value, out: &mut String| {
            v.write_code(out);
            Ok::<_, Infallible>(())
        };
        crate::interp::grow(|| match self {
            Value::Int(n) => out.push_str(&n.to_string()),
            Value::Float(f) => out.push_str(&check_runtime::float_text(*f)),
            Value::Bool(b) => out.push_str(&b.to_string()),
            Value::Str(s) => out.push_str(&check_runtime::quote(s)),
            Value::List(items) => done(write_items(out, "[", items.iter(), "]", field)),
            Value::Map(entries) => done(write_map(out, entries.iter(), field)),
            Value::Set(items) => done(write_items(out, "Set([", items.iter(), "])", field)),
            Value::Some(v) => done(write_items(out, "Some(", [&**v].into_iter(), ")", field)),
            Value::None => out.push_str("None"),
            Value::Range(a, b, inclusive) => {
                let eq = if *inclusive { "=" } else { "" };
                out.push_str(&format!("{a}..{eq}{b}"))
            }
            Value::Func(f) => match &**f {
                Callable::Named(_, sig) | Callable::Lambda { sig, .. } => out.push_str(sig),
            },
            Value::Object(r) | Value::Record(r) => done(r.write_with(true, out, field)),
            Value::Shared(r) => match r.try_borrow_mut() {
                Ok(record) => done(record.write_with(true, out, field)),
                Err(_) => out.push_str("..."),
            },
            Value::SharedBox(v) => match v.try_borrow_mut() {
                Ok(v) => done(write_items(out, "Shared(", [&*v].into_iter(), ")", field)),
                Err(_) => out.push_str("Shared(...)"),
            },
            Value::Cell(v) => match v.try_borrow_mut() {
                Ok(v) => v.write_code(out),
                Err(_) => out.push_str("..."),
            },
            Value::Variant(r) => done(r.write_with(false, out, field)),
            Value::Void => {}
        });
    }
}

fn done(written: Result<(), Infallible>) {
    let Ok(()) = written;
}

/// Writes `items` between `open` and `close`, separated by commas, with `item` writing each.
pub fn write_items<'v, E>(
    out: &mut String,
    open: &str,
    items: impl Iterator<Item = &'v Value>,
    close: &str,
    mut item: impl FnMut(&Value, &mut String) -> Result<(), E>,
) -> Result<(), E> {
    out.push_str(open);
    for (i, v) in items.enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        item(v, out)?;
    }
    out.push_str(close);
    Ok(())
}

/// Writes a map literal, `[=]` when it's empty, with `item` writing each key and value.
pub fn write_map<'v, E>(
    out: &mut String,
    entries: impl ExactSizeIterator<Item = (&'v Value, &'v Value)>,
    mut item: impl FnMut(&Value, &mut String) -> Result<(), E>,
) -> Result<(), E> {
    if entries.len() == 0 {
        out.push_str("[=]");
        return Ok(());
    }
    out.push('[');
    for (i, (k, v)) in entries.enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        item(k, out)?;
        out.push_str(" = ");
        item(v, out)?;
    }
    out.push(']');
    Ok(())
}
