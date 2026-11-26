#[cfg(feature = "exports")]
mod wasm;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panic {
    IntegerOverflow,
    DivisionByZero,
    NegativeExponent,
    ShiftOutOfRange,
    IndexOutOfRange,
    OrPanicOnNone,
}

impl Panic {
    pub const ALL: [Panic; 6] = [
        Panic::IntegerOverflow,
        Panic::DivisionByZero,
        Panic::NegativeExponent,
        Panic::ShiftOutOfRange,
        Panic::IndexOutOfRange,
        Panic::OrPanicOnNone,
    ];
}

fn overflow(n: Option<i64>) -> Result<i64, Panic> {
    n.ok_or(Panic::IntegerOverflow)
}

pub fn add(a: i64, b: i64) -> Result<i64, Panic> {
    overflow(a.checked_add(b))
}

pub fn sub(a: i64, b: i64) -> Result<i64, Panic> {
    overflow(a.checked_sub(b))
}

pub fn mul(a: i64, b: i64) -> Result<i64, Panic> {
    overflow(a.checked_mul(b))
}

pub fn neg(a: i64) -> Result<i64, Panic> {
    overflow(a.checked_neg())
}

pub fn div(a: i64, b: i64) -> Result<f64, Panic> {
    if b == 0 {
        return Err(Panic::DivisionByZero);
    }
    Ok(a as f64 / b as f64)
}

fn floored(a: i64, b: i64) -> Option<i64> {
    let q = a.checked_div(b)?;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        Some(q - 1)
    } else {
        Some(q)
    }
}

pub fn floor_div(a: i64, b: i64) -> Result<i64, Panic> {
    if b == 0 {
        return Err(Panic::DivisionByZero);
    }
    overflow(floored(a, b))
}

pub fn modulo(a: i64, b: i64) -> Result<i64, Panic> {
    match b {
        0 => Err(Panic::DivisionByZero),
        -1 => Ok(0),
        _ => overflow(
            floored(a, b)
                .and_then(|q| q.checked_mul(b))
                .and_then(|m| a.checked_sub(m)),
        ),
    }
}

pub fn pow(a: i64, b: i64) -> Result<i64, Panic> {
    if b < 0 {
        return Err(Panic::NegativeExponent);
    }
    match a {
        0 | 1 => Ok(if b == 0 { 1 } else { a }),
        -1 => Ok(if b % 2 == 0 { 1 } else { -1 }),
        _ => overflow(u32::try_from(b).ok().and_then(|e| a.checked_pow(e))),
    }
}

fn shift(b: i64) -> Result<u32, Panic> {
    match b {
        0..64 => Ok(b as u32),
        _ => Err(Panic::ShiftOutOfRange),
    }
}

pub fn shl(a: i64, b: i64) -> Result<i64, Panic> {
    Ok(a.wrapping_shl(shift(b)?))
}

pub fn shr(a: i64, b: i64) -> Result<i64, Panic> {
    Ok(a >> shift(b)?)
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

pub fn quote(s: &str) -> String {
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
