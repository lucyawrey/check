use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    pub fn to(self, other: Span) -> Span {
        Span::new(self.start, other.end.max(self.end))
    }
}

macro_rules! codes {
    ($($code:ident => $message:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Code { $($code,)* }

        impl Code {
            pub fn message(self) -> &'static str {
                match self { $(Code::$code => $message,)* }
            }
        }

        impl fmt::Display for Code {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                let name = match self { $(Code::$code => stringify!($code),)* };
                f.write_str(name)
            }
        }
    };
}

codes! {
    E0001 => "[a character that isn't part of Check]",
    E0002 => "[a string with no closing quote]",
    E0003 => "[an unknown escape in a string]",
    E0004 => "[a { in a string that never closes or holds no expression]",
    E0005 => "[a number starting with 0 followed by digits]",
    E0006 => "[a malformed number]",
    E0007 => "[an unexpected token]",
    E0008 => "[a bracket or brace that never closes]",
    E0009 => "[a , or ; at a line end or before a closing bracket]",
    E0010 => "[else on the line after }]",
    E0011 => "[an operator from another language]",
    E0012 => "[= in a condition]",
    E0013 => "[a function with -> and no braces]",
    E0014 => "[a body without braces that is a statement]",
    E0101 => "[a name that doesn't exist]",
    E0102 => "[a name defined twice in the same block or file]",
    E0103 => "[redefining or shadowing a reserved name]",
    E0104 => "[a variable used above its own line]",
    E0105 => "[a function run before a top-level variable it uses is set]",
    E0106 => "[a named function declared inside a function body]",
    E0201 => "[a type mismatch]",
    E0202 => "[a type that can't be worked out and needs writing]",
    E0203 => "[the wrong number of arguments, or an unknown named one]",
    E0204 => "[a method that the type doesn't have]",
    E0205 => "[calling something that isn't a function]",
    E0206 => "[an operator used on types it doesn't work on]",
    E0207 => "[a bitwise operator on Bools]",
    E0208 => "[chained comparisons]",
    E0209 => "[/ where an Int is expected]",
    E0210 => "[if and else branches of different types]",
    E0211 => "[return paths of different types]",
    E0212 => "[a recursive function without ->]",
    E0213 => "[a number literal too big for its type]",
    E0214 => "[a negative literal exponent on an Int]",
    E0215 => "[a T? used where a T is needed]",
    E0216 => "[a T? body where a plain T? is expected]",
    E0217 => "[flatten() on a T? with nothing nested]",
    E0218 => "[for over something that isn't a list or range]",
    E0219 => "[try outside a function returning a Maybe]",
    E0301 => "[assigning to a let]",
    E0302 => "[assigning to a parameter not written var]",
    E0303 => "[changing a let list]",
    E0304 => "[break or continue outside a loop]",
    E0305 => "[return outside a function]",
    E0306 => "[a loop used as a value with no break value]",
    E0307 => "[break values of different types in one loop]",
    E0308 => "[a backwards range with constant ends]",
    E0901 => "[panic was called]",
    E0902 => "[integer overflow]",
    E0903 => "[integer division by zero]",
    E0904 => "[orPanic on a None]",
    E0905 => "[a negative exponent on an Int]",
    E0906 => "[a shift amount out of range]",
    E0907 => "[rounding a Float that has no Int]",
    E0908 => "[more than 10,000 nested calls]",
    W0001 => "[a var that's never reassigned]",
    W0002 => "[a local that shadows a function or type]",
    W0003 => "[interpolating a T?]",
    W0004 => "[an if or loop value that is a T??]",
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: Code,
    pub span: Span,
    pub severity: Severity,
}

impl Diagnostic {
    pub fn error(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            severity: Severity::Error,
        }
    }

    pub fn warning(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            severity: Severity::Warning,
        }
    }

    pub fn render(&self, path: &str, src: &str) -> String {
        let (line, col, text) = locate(src, self.span.start);
        let width = (self
            .span
            .end
            .min(self.span.start + text.len().saturating_sub(col - 1)))
        .saturating_sub(self.span.start)
        .max(1);
        let kind = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let gutter = " ".repeat(line.to_string().len());
        format!(
            "{kind}[{code}]: {message}\n{gutter}--> {path}:{line}:{col}\n{gutter} |\n{line} | {text}\n{gutter} | {pad}{marks}\n",
            code = self.code,
            message = self.code.message(),
            pad = " ".repeat(col - 1),
            marks = "^".repeat(width),
        )
    }
}

pub fn locate(src: &str, offset: usize) -> (usize, usize, &str) {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[offset..].find('\n').map_or(src.len(), |i| offset + i);
    let line = src[..offset].matches('\n').count() + 1;
    let col = src[line_start..offset].chars().count() + 1;
    (line, col, &src[line_start..line_end])
}
