use std::cmp::Ordering;
use std::io::{BufRead, Write};
use std::rc::Rc;

use crate::checker::ORDERING;
use crate::diag::{Code, Span};
use crate::dynamic;
use crate::interp::{Flow, Panic, panic};
use crate::value::{Record, Value};

pub struct Io<'a> {
    pub input: &'a mut dyn BufRead,
    pub output: &'a mut dyn Write,
}

pub type HostFn = fn(&mut Io, Vec<Value>, Span) -> Result<Value, Flow>;

pub const FUNCTIONS: &[(&str, HostFn)] = &[
    ("check.std.panic", stop),
    ("check.cli.input", input),
    ("String.upper", |_, a, _| {
        Ok(Value::Str(text(&a[0]).to_uppercase().into()))
    }),
    ("String.lower", |_, a, _| {
        Ok(Value::Str(text(&a[0]).to_lowercase().into()))
    }),
    ("String.trim", |_, a, _| {
        Ok(Value::Str(text(&a[0]).trim().into()))
    }),
    ("String.isEmpty", |_, a, _| {
        Ok(Value::Bool(text(&a[0]).is_empty()))
    }),
    ("String.isBlank", |_, a, _| {
        Ok(Value::Bool(text(&a[0]).trim().is_empty()))
    }),
    ("String.contains", |_, a, _| {
        Ok(Value::Bool(text(&a[0]).contains(text(&a[1]))))
    }),
    ("String.split", split),
    ("Int.compare", compare),
    ("Float.compare", compare),
    ("String.compare", compare),
    ("String.replace", replace),
    ("Int.toFloat", |_, a, _| match a[0] {
        Value::Int(n) => Ok(Value::Float(n as f64)),
        _ => Ok(Value::Void),
    }),
    ("Int.parse", |_, a, _| {
        Ok(maybe(
            text(&a[0]).trim().parse::<i64>().ok().map(Value::Int),
        ))
    }),
    ("Float.parse", |_, a, _| {
        Ok(maybe(
            text(&a[0]).trim().parse::<f64>().ok().map(Value::Float),
        ))
    }),
    ("check.std.Json.parse", |_, a, _| {
        Ok(dynamic::parse_json(text(&a[0])))
    }),
    ("check.std.Data.parse", |_, a, _| {
        Ok(dynamic::parse_data(text(&a[0])))
    }),
    ("Float.round", |_, a, span| to_int(&a[0], f64::round, span)),
    ("Float.floor", |_, a, span| to_int(&a[0], f64::floor, span)),
    ("Float.ceil", |_, a, span| to_int(&a[0], f64::ceil, span)),
    ("Float.truncate", |_, a, span| {
        to_int(&a[0], f64::trunc, span)
    }),
];

fn compare(_: &mut Io, a: Vec<Value>, _: Span) -> Result<Value, Flow> {
    let order = match (&a[0], &a[1]) {
        (Value::Float(x), Value::Float(y)) => x
            .partial_cmp(y)
            .unwrap_or_else(|| x.is_nan().cmp(&y.is_nan())),
        (x, y) => x
            .compare(y)
            .expect("compare is declared on Int, Float and String"),
    };
    let name = match order {
        Ordering::Less => "Less",
        Ordering::Equal => "Equal",
        Ordering::Greater => "Greater",
    };
    Ok(Value::Variant(Rc::new(Record {
        name: name.into(),
        ty: ORDERING.into(),
        by_fields: false,
        field_names: None,
        fields: Vec::new(),
    })))
}

pub fn find(name: &str) -> Option<HostFn> {
    FUNCTIONS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

fn text(v: &Value) -> &str {
    match v {
        Value::Str(s) => s,
        _ => "",
    }
}

fn maybe(v: Option<Value>) -> Value {
    v.map_or(Value::None, |v| Value::Some(Rc::new(v)))
}

fn stop(_: &mut Io, args: Vec<Value>, span: Span) -> Result<Value, Flow> {
    let reason = match &args[0] {
        Value::Some(r) => Some(r.display()),
        _ => None,
    };
    Err(Flow::Panic(Panic {
        code: Code::panic_called,
        span,
        reason,
        stack: Vec::new(),
    }))
}

fn input(io: &mut Io, args: Vec<Value>, _: Span) -> Result<Value, Flow> {
    let prompt = text(&args[0]);
    if !prompt.is_empty() {
        write!(io.output, "{prompt}").ok();
        io.output.flush().ok();
    }
    let mut line = String::new();
    Ok(match io.input.read_line(&mut line) {
        Ok(0) | Err(_) => Value::None,
        Ok(_) => {
            let trimmed = line.strip_suffix('\n').unwrap_or(&line);
            let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
            Value::Some(Rc::new(Value::Str(trimmed.into())))
        }
    })
}

fn split(_: &mut Io, args: Vec<Value>, _: Span) -> Result<Value, Flow> {
    let s = text(&args[0]);
    let parts: Vec<Value> = match &args[1] {
        Value::Some(sep) => s.split(text(sep)).map(|p| Value::Str(p.into())).collect(),
        _ => s.split_whitespace().map(|p| Value::Str(p.into())).collect(),
    };
    Ok(Value::List(Rc::new(parts)))
}

/// A negative `count` replaces nothing.
fn replace(_: &mut Io, args: Vec<Value>, _: Span) -> Result<Value, Flow> {
    let (s, old, new) = (text(&args[0]), text(&args[1]), text(&args[2]));
    Ok(Value::Str(
        match &args[3] {
            Value::Some(n) => match **n {
                Value::Int(n) => s.replacen(old, new, usize::try_from(n).unwrap_or(0)),
                _ => s.to_string(),
            },
            _ => s.replace(old, new),
        }
        .into(),
    ))
}

fn to_int(v: &Value, round: fn(f64) -> f64, span: Span) -> Result<Value, Flow> {
    let Value::Float(f) = v else {
        return Ok(Value::Void);
    };
    let r = round(*f);
    if !r.is_finite() || r < i64::MIN as f64 || r >= i64::MAX as f64 {
        return panic(Code::round_out_of_range, span);
    }
    Ok(Value::Int(r as i64))
}
