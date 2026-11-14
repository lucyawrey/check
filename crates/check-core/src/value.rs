use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{Expr, FuncDecl, Param};

pub enum Callable {
    Named(Rc<FuncDecl>, String),
    Lambda {
        params: Vec<Param>,
        body: Rc<Expr>,
        captured: HashMap<String, Value>,
        sig: String,
    },
}

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    List(Rc<Vec<Value>>),
    Some(Rc<Value>),
    None,
    Range(i64, i64, bool),
    Func(Rc<Callable>),
    Void,
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
                (*a as f64) == *b
            }
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Some(a), Value::Some(b)) => a == b,
            (Value::None, Value::None) | (Value::Void, Value::Void) => true,
            (Value::Range(a, b, x), Value::Range(c, d, y)) => a == c && b == d && x == y,
            (Value::Func(a), Value::Func(b)) => Rc::ptr_eq(a, b),
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

    pub fn display(&self) -> String {
        match self {
            Value::Str(s) => s.to_string(),
            other => other.code(),
        }
    }

    pub fn code(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => float_text(*f),
            Value::Bool(b) => b.to_string(),
            Value::Str(s) => quote(s),
            Value::List(items) => format!(
                "[{}]",
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
            Value::Void => String::new(),
        }
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
