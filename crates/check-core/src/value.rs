use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use crate::ast::{Expr, FuncDecl, Param};
use crate::checker::{MAP, MAYBE, SET};

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
    /// A `ref class` that implements `Eq`, so `==` compares its fields.
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
                Value::Ref(inner) => {
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

    fn code(&self, always_parens: bool) -> String {
        match self.code_with(always_parens, |v| Ok::<_, Infallible>(v.code())) {
            Ok(text) => text,
            Err(never) => match never {},
        }
    }

    /// The record as code, with `field` giving each field's text.
    pub fn code_with<E>(
        &self,
        always_parens: bool,
        mut field: impl FnMut(&Value) -> Result<String, E>,
    ) -> Result<String, E> {
        if self.fields.is_empty() && !always_parens {
            return Ok(self.name.to_string());
        }
        let mut fields = Vec::with_capacity(self.fields.len());
        for (i, v) in self.fields.iter().enumerate() {
            let text = field(v)?;
            fields.push(match &self.field_names {
                Some(names) => format!("{} = {text}", names[i]),
                None => text,
            });
        }
        Ok(format!("{}({})", self.name, fields.join(", ")))
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
    /// The `ref class` objects `==` is comparing, by address.
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
    Ref(Rc<RefCell<Value>>),
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
            (Value::Ref(a), Value::Ref(b)) => Rc::ptr_eq(a, b),
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
            Value::Ref(_) => "Ref".into(),
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
        crate::interp::grow(|| self.code_inner())
    }

    fn code_inner(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => float_text(*f),
            Value::Bool(b) => b.to_string(),
            Value::Str(s) => quote(s),
            Value::List(items) => format!(
                "[{}]",
                items.iter().map(Value::code).collect::<Vec<_>>().join(", ")
            ),
            Value::Map(entries) => map_code(entries.iter().map(|(k, v)| (k.code(), v.code()))),
            Value::Set(items) => format!(
                "Set([{}])",
                items.iter().map(Value::code).collect::<Vec<_>>().join(", ")
            ),
            Value::Some(v) => match &**v {
                Value::Void => "Some()".to_string(),
                inner => format!("Some({})", inner.code()),
            },
            Value::None => "None".to_string(),
            Value::Range(a, b, inclusive) => {
                format!("{a}..{}{b}", if *inclusive { "=" } else { "" })
            }
            Value::Func(f) => match &**f {
                Callable::Named(_, sig) | Callable::Lambda { sig, .. } => sig.clone(),
            },
            Value::Object(r) => r.code(true),
            Value::Shared(r) => match r.try_borrow_mut() {
                Ok(record) => record.code(true),
                Err(_) => "...".to_string(),
            },
            Value::Ref(v) => match v.try_borrow_mut() {
                Ok(v) => format!("Ref({})", v.code()),
                Err(_) => "Ref(...)".to_string(),
            },
            Value::Variant(r) => r.code(false),
            Value::Record(r) => r.code(true),
            Value::Void => String::new(),
        }
    }
}

pub fn map_code(entries: impl Iterator<Item = (String, String)>) -> String {
    let parts: Vec<String> = entries.map(|(k, v)| format!("{k} = {v}")).collect();
    if parts.is_empty() {
        "[=]".to_string()
    } else {
        format!("[{}]", parts.join(", "))
    }
}

pub fn float_text(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        return format!("{f:e}");
    }
    let text = f.to_string();
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '{' => out.push_str("\\{"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:X}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
