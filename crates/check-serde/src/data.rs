use check_syntax::ast::{Arg, Expr, ExprKind, StrSeg, UnOp};
use check_syntax::diag::{Code, Diagnostic};

/// A value from a `.con` file, from the checker's `Value` or straight from the parse tree.
pub enum Data {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    List(Vec<Data>),
    Map(Vec<(Data, Data)>),
    Some(Box<Data>),
    None,
    /// A class value, record or variant. `name` is the class or variant, if written.
    Named {
        name: Option<String>,
        is_variant: bool,
        field_names: Option<Vec<String>>,
        fields: Vec<Data>,
    },
    /// A value a `.con` file can't hold, which the checker already refuses.
    Other,
}

#[cfg(feature = "check")]
impl From<check_core::value::Value> for Data {
    fn from(value: check_core::value::Value) -> Data {
        use check_core::value::{Record, Value};
        let named = |r: &Record, name: Option<String>, is_variant| Data::Named {
            name,
            is_variant,
            field_names: r
                .field_names
                .as_ref()
                .map(|n| n.iter().map(|s| s.to_string()).collect()),
            fields: r.fields.iter().cloned().map(Data::from).collect(),
        };
        match value {
            Value::Int(n) => Data::Int(n),
            Value::Float(n) => Data::Float(n),
            Value::Bool(b) => Data::Bool(b),
            Value::Str(s) => Data::Str(s.to_string()),
            Value::List(items) => Data::List(items.iter().cloned().map(Data::from).collect()),
            Value::Set(items) => Data::List(items.iter().cloned().map(Data::from).collect()),
            Value::Map(entries) => Data::Map(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone().into(), v.clone().into()))
                    .collect(),
            ),
            Value::Some(v) => Data::Some(Box::new((*v).clone().into())),
            Value::None => Data::None,
            Value::Record(r) => named(&r, None, false),
            Value::Object(r) => named(&r, Some(r.name.to_string()), false),
            Value::Shared(r) => {
                let r = r.borrow();
                named(&r, Some(r.name.to_string()), false)
            }
            Value::Variant(r) => named(&r, Some(r.name.to_string()), true),
            _ => Data::Other,
        }
    }
}

impl Data {
    /// Reads a parsed `.con` value, with an error for each part that isn't a literal.
    pub fn from_expr(e: &Expr, diags: &mut Vec<Diagnostic>) -> Data {
        let mut read = |e: &Expr| Data::from_expr(e, diags);
        match &e.kind {
            ExprKind::Int(n) => Data::Int(*n),
            ExprKind::Float(n) => Data::Float(*n),
            ExprKind::Bool(b) => Data::Bool(*b),
            ExprKind::Str(segs) => Data::Str(
                segs.iter()
                    .map(|s| match s {
                        StrSeg::Text(t) => t.as_str(),
                        StrSeg::Expr(_) => "",
                    })
                    .collect(),
            ),
            ExprKind::Unary(UnOp::Neg, x) => match x.kind {
                ExprKind::Int(n) => Data::Int(-n),
                ExprKind::Float(n) => Data::Float(-n),
                _ => code(e, diags),
            },
            ExprKind::List(items) => Data::List(items.iter().map(read).collect()),
            ExprKind::Map(entries) => {
                Data::Map(entries.iter().map(|(k, v)| (read(k), read(v))).collect())
            }
            ExprKind::Record(args) => named(None, args, diags),
            ExprKind::Name(n) if n == "None" => Data::None,
            ExprKind::Name(_) | ExprKind::Field(..) => match type_name(e) {
                Some(n) => Data::Named {
                    name: Some(n.clone()),
                    is_variant: true,
                    field_names: None,
                    fields: Vec::new(),
                },
                None => code(e, diags),
            },
            ExprKind::Call(callee, args) => match (&callee.kind, args.as_slice()) {
                (ExprKind::Name(n), [arg]) if n == "Some" && arg.name.is_none() => {
                    Data::Some(Box::new(read(&arg.value)))
                }
                (ExprKind::Name(n), [arg])
                    if n == "Set"
                        && arg.name.is_none()
                        && matches!(arg.value.kind, ExprKind::List(_)) =>
                {
                    read(&arg.value)
                }
                _ => match type_name(callee) {
                    Some(n) if n != "Set" => named(Some(n.clone()), args, diags),
                    _ => code(e, diags),
                },
            },
            ExprKind::Method(base, n, args)
                if is_type_name(n)
                    && matches!(&base.kind, ExprKind::Name(t) if is_type_name(t)) =>
            {
                named(Some(n.clone()), args, diags)
            }
            _ => code(e, diags),
        }
    }
}

/// A constructor or variant call, or a record literal: its fields all named or all positional.
fn named(name: Option<String>, args: &[Arg], diags: &mut Vec<Diagnostic>) -> Data {
    let names: Option<Vec<String>> = args.iter().map(|a| a.name.clone()).collect();
    if names.is_none()
        && let Some(a) = args.iter().find(|a| a.name.is_some())
    {
        diags.push(Diagnostic::error(Code::mixed_fields, a.value.span));
    }
    Data::Named {
        name,
        is_variant: false,
        field_names: names,
        fields: args
            .iter()
            .map(|a| Data::from_expr(&a.value, diags))
            .collect(),
    }
}

/// A class or variant named as a literal can: `Name`, or `Type.Variant`.
fn type_name(e: &Expr) -> Option<&String> {
    match &e.kind {
        ExprKind::Name(n) if is_type_name(n) => Some(n),
        ExprKind::Field(base, n)
            if is_type_name(n) && matches!(&base.kind, ExprKind::Name(t) if is_type_name(t)) =>
        {
            Some(n)
        }
        _ => None,
    }
}

fn is_type_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
}

fn code(e: &Expr, diags: &mut Vec<Diagnostic>) -> Data {
    diags.push(Diagnostic::error(Code::code_in_pure_data, e.span));
    Data::Other
}
