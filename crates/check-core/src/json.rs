use crate::ast::{Arg, Expr, ExprKind, NodeId, StrSeg};
use crate::diag::{Code, Diagnostic, Sources, Span};
use serde_json::Value;

pub fn parse(
    text: &str,
    sources: &mut Sources,
    next_id: &mut NodeId,
    diags: &mut Vec<Diagnostic>,
) -> Option<Expr> {
    match serde_json::from_str(text) {
        Ok(value) => Some(to_expr(value, String::new(), sources, next_id)),
        Err(e) => {
            let base = sources.files().last().expect("the JSON file").base;
            let at = base + offset(text, e.line(), e.column());
            let mut d = Diagnostic::error(Code::json_syntax, Span::new(at, at + 1));
            d.detail = Some(
                e.to_string()
                    .split(" at line ")
                    .next()
                    .unwrap_or("")
                    .to_string(),
            );
            diags.push(d);
            None
        }
    }
}

fn to_expr(value: Value, path: String, sources: &mut Sources, next_id: &mut NodeId) -> Expr {
    let at = sources.add_value_path(path.clone());
    let kind = match value {
        Value::Null => ExprKind::Name("None".to_string()),
        Value::Bool(b) => ExprKind::Bool(b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => ExprKind::Int(i),
            None => ExprKind::Float(n.as_f64().expect("serde_json reads every number as f64")),
        },
        Value::String(s) => ExprKind::Str(vec![StrSeg::Text(s)]),
        Value::Array(items) => ExprKind::List(
            (items.into_iter().enumerate())
                .map(|(i, v)| to_expr(v, format!("{path}[{i}]"), sources, next_id))
                .collect(),
        ),
        Value::Object(fields) => ExprKind::Record(
            fields
                .into_iter()
                .map(|(k, v)| {
                    let inner = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    Arg {
                        value: to_expr(v, inner, sources, next_id),
                        name: Some(k),
                    }
                })
                .collect(),
        ),
    };
    *next_id += 1;
    Expr {
        id: *next_id,
        span: Span::new(at, at),
        kind,
    }
}

fn offset(text: &str, line: usize, column: usize) -> usize {
    let line_start: usize = text
        .split_inclusive('\n')
        .take(line - 1)
        .map(str::len)
        .sum();
    (line_start + column.saturating_sub(1)).min(text.len())
}
