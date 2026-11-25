use std::rc::Rc;

use indexmap::IndexMap;

use crate::ast::{Arg, Expr, ExprKind, StrSeg, UnOp};
use crate::checker::{DYN, PARSE_ERROR, RESULT, SHAPE_ERROR};
use crate::diag::{Severity, Sources, Span};
use crate::value::{Record, Value};
use crate::{lexer, parser};

type Parsed = Result<Value, (usize, usize)>;

pub fn parse_json(text: &str) -> Value {
    let parsed = match serde_json::from_str(text) {
        Ok(json) => from_json(json).ok_or((1, 1)),
        Err(e) => Err((e.line().max(1), e.column().max(1))),
    };
    result(parsed)
}

pub fn parse_data(text: &str) -> Value {
    let mut sources = Sources::default();
    let base = sources.add("", text);
    let mut diags = Vec::new();
    let tokens = lexer::lex(text, base, true, &mut diags);
    let file = parser::parse_data(tokens, text, base, &mut 0, &mut diags);
    let parsed = match diags.iter().find(|d| d.severity == Severity::Error) {
        Some(d) => Err(d.span),
        None => from_data(&file.value).and_then(|v| v.ok_or(file.value.span)),
    };
    result(parsed.map_err(|span| {
        let (_, line, column, _) = sources.locate(span.start);
        (line, column)
    }))
}

fn result(parsed: Parsed) -> Value {
    match parsed {
        Ok(value) => ok(value),
        Err((line, column)) => err(object_of(
            PARSE_ERROR,
            &["line", "column"],
            vec![Value::Int(line as i64), Value::Int(column as i64)],
        )),
    }
}

pub fn ok(value: Value) -> Value {
    result_variant("Ok", value)
}

pub fn err(value: Value) -> Value {
    result_variant("Err", value)
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

pub fn shape_error(path: String, expected: String) -> Value {
    object_of(
        SHAPE_ERROR,
        &["path", "expected"],
        vec![Value::Str(path.into()), Value::Str(expected.into())],
    )
}

fn object_of(class: &str, names: &[&str], fields: Vec<Value>) -> Value {
    Value::Object(Rc::new(Record {
        name: class.into(),
        ty: class.into(),
        by_fields: false,
        field_names: Some(names.iter().map(|&n| n.into()).collect()),
        fields,
    }))
}

pub fn entry<'v>(fields: &'v IndexMap<Value, Value>, key: &str) -> Option<&'v Value> {
    fields.get(&Value::Str(key.into())).and_then(present)
}

pub fn present(item: &Value) -> Option<&Value> {
    match item {
        Value::Some(v) => Some(v),
        _ => None,
    }
}

fn maybe(v: Option<Value>) -> Value {
    v.map_or(Value::None, |v| Value::Some(Rc::new(v)))
}

pub fn get(dyn_value: &Value, key: &Value) -> Value {
    let Value::Variant(r) = dyn_value else {
        return Value::None;
    };
    match (&r.fields[0], key) {
        (Value::List(items), Value::Int(i)) => {
            (usize::try_from(*i).ok().and_then(|i| items.get(i).cloned())).unwrap_or(Value::None)
        }
        (Value::Map(fields), key) => fields.get(key).cloned().unwrap_or(Value::None),
        _ => Value::None,
    }
}

pub fn variant(name: &str, value: Value) -> Value {
    Value::Variant(Rc::new(Record {
        name: name.into(),
        ty: DYN.into(),
        by_fields: false,
        field_names: None,
        fields: vec![value],
    }))
}

fn string(s: &str) -> Value {
    variant("String", Value::Str(s.into()))
}

fn list(items: impl Iterator<Item = Option<Value>>) -> Value {
    variant("List", Value::List(Rc::new(items.map(maybe).collect())))
}

fn object(fields: impl Iterator<Item = (String, Option<Value>)>) -> Value {
    let entries: IndexMap<Value, Value> = fields
        .map(|(k, v)| (Value::Str(k.into()), maybe(v)))
        .collect();
    variant("Object", Value::Map(Rc::new(entries)))
}

pub fn from_value(value: &Value) -> Option<Value> {
    Some(match value {
        Value::Int(n) => variant("Int", Value::Int(*n)),
        Value::Float(x) => variant("Float", Value::Float(*x)),
        Value::Bool(b) => variant("Bool", Value::Bool(*b)),
        Value::Str(s) => string(s),
        Value::List(items) => list(items.iter().map(from_value)),
        Value::Set(items) => list(items.iter().map(from_value)),
        Value::Map(entries) => object(entries.iter().map(|(k, v)| (k.display(), from_value(v)))),
        Value::Some(v) => return from_value(v),
        Value::Cell(v) | Value::SharedBox(v) => return from_value(&v.borrow()),
        Value::Variant(r) if &*r.ty == DYN => value.clone(),
        Value::Variant(r) if r.fields.is_empty() => string(&r.name),
        Value::Variant(r) => {
            let mut fields = present_fields(r);
            fields.insert(0, ("type".to_string(), Some(string(&r.name))));
            object(fields.into_iter())
        }
        Value::Object(r) | Value::Record(r) if r.field_names.is_some() => {
            object(present_fields(r).into_iter())
        }
        Value::Record(r) => list(r.fields.iter().map(from_value)),
        _ => return None,
    })
}

fn present_fields(r: &Record) -> Vec<(String, Option<Value>)> {
    (r.fields.iter().enumerate())
        .filter_map(|(i, v)| {
            let name = match &r.field_names {
                Some(names) => names[i].to_string(),
                None => i.to_string(),
            };
            Some((name, Some(from_value(v)?)))
        })
        .collect()
}

fn from_json(json: serde_json::Value) -> Option<Value> {
    use serde_json::Value as Json;
    Some(match json {
        Json::Null => return None,
        Json::Bool(b) => variant("Bool", Value::Bool(b)),
        Json::Number(n) => match n.as_i64() {
            Some(i) => variant("Int", Value::Int(i)),
            None => variant("Float", Value::Float(n.as_f64()?)),
        },
        Json::String(s) => string(&s),
        Json::Array(items) => list(items.into_iter().map(from_json)),
        Json::Object(fields) => object(fields.into_iter().map(|(k, v)| (k, from_json(v)))),
    })
}

fn from_data(e: &Expr) -> Result<Option<Value>, Span> {
    let value = match &e.kind {
        ExprKind::Int(n) => variant("Int", Value::Int(*n)),
        ExprKind::Float(x) => variant("Float", Value::Float(*x)),
        ExprKind::Bool(b) => variant("Bool", Value::Bool(*b)),
        ExprKind::Unary(UnOp::Neg, x) => match x.kind {
            ExprKind::Int(n) => variant("Int", Value::Int(-n)),
            ExprKind::Float(f) => variant("Float", Value::Float(-f)),
            _ => return Err(e.span),
        },
        ExprKind::Str(segs) => match &segs[..] {
            [] => string(""),
            [StrSeg::Text(s)] => string(s),
            _ => return Err(e.span),
        },
        ExprKind::Name(n) if n == "None" => return Ok(None),
        ExprKind::Name(n) => string(n),
        ExprKind::Field(base, name) if written_name(base).is_some() => string(name),
        ExprKind::List(items) => list(
            items
                .iter()
                .map(from_data)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter(),
        ),
        ExprKind::Map(entries) => {
            let mut fields = Vec::new();
            for (k, v) in entries {
                let ExprKind::Str(segs) = &k.kind else {
                    return Err(k.span);
                };
                let key = match &segs[..] {
                    [] => String::new(),
                    [StrSeg::Text(s)] => s.clone(),
                    _ => return Err(k.span),
                };
                fields.push((key, from_data(v)?));
            }
            object(fields.into_iter())
        }
        ExprKind::Record(args) if args.iter().all(|a| a.name.is_none()) => list(
            (args.iter().map(|a| from_data(&a.value)))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter(),
        ),
        ExprKind::Record(args) => object(fields(args, e.span)?.into_iter()),
        ExprKind::Call(callee, args) => {
            let name = written_name(callee).ok_or(e.span)?;
            if name == "Some" {
                let [arg] = &args[..] else {
                    return Err(e.span);
                };
                return from_data(&arg.value);
            }
            tagged(name, args, e.span)?
        }
        ExprKind::Method(base, method, args) => {
            let base = written_name(base).ok_or(e.span)?;
            tagged(format!("{base}.{method}"), args, e.span)?
        }
        _ => return Err(e.span),
    };
    Ok(Some(value))
}

fn tagged(name: String, args: &[Arg], span: Span) -> Result<Value, Span> {
    let mut fields = fields(args, span)?;
    fields.insert(0, ("type".to_string(), Some(string(&name))));
    Ok(object(fields.into_iter()))
}

fn fields(args: &[Arg], span: Span) -> Result<Vec<(String, Option<Value>)>, Span> {
    let named = args.iter().all(|a| a.name.is_some());
    if !named && args.iter().any(|a| a.name.is_some()) {
        return Err(span);
    }
    let mut fields = Vec::new();
    for (i, a) in args.iter().enumerate() {
        let key = a.name.clone().unwrap_or_else(|| i.to_string());
        if key == "type" {
            return Err(a.value.span);
        }
        fields.push((key, from_data(&a.value)?));
    }
    Ok(fields)
}

fn written_name(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Name(n) => Some(n.clone()),
        ExprKind::Field(base, name) => Some(format!("{}.{name}", written_name(base)?)),
        _ => None,
    }
}
