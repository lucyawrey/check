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
        #[allow(non_camel_case_types)]
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
    unknown_character => "[a character that isn't part of Check]",
    unclosed_string => "[a string with no closing quote]",
    unknown_escape => "[an unknown escape in a string]",
    unclosed_interpolation => "[a { in a string that never closes or holds no expression]",
    leading_zero => "[a number starting with 0 followed by digits]",
    malformed_number => "[a malformed number]",
    unexpected_token => "[an unexpected token]",
    unclosed_bracket => "[a bracket or brace that never closes]",
    trailing_separator => "[a , or ; at a line end or before a closing bracket]",
    else_on_new_line => "[else on the line after }]",
    foreign_operator => "[an operator from another language]",
    assignment_in_condition => "[= in a condition]",
    arrow_without_braces => "[a function with -> and no braces]",
    statement_body_without_braces => "[a body without braces that is a statement]",
    unknown_name => "[a name that doesn't exist]",
    duplicate_name => "[a name defined twice in the same block or file]",
    redefined_builtin => "[redefining or shadowing a reserved name]",
    used_before_declared => "[a variable used above its own line]",
    used_before_set => "[a function run before a top-level variable it uses is set]",
    nested_named_function => "[a named function declared inside a function body]",
    type_mismatch => "[a type mismatch]",
    type_needed => "[a type that can't be worked out and needs writing]",
    wrong_arguments => "[the wrong number of arguments, or an unknown named one]",
    unknown_method => "[a method that the type doesn't have]",
    not_callable => "[calling something that isn't a function]",
    invalid_operand_types => "[an operator used on types it doesn't work on]",
    bitwise_on_bool => "[a bitwise operator on Bools]",
    chained_comparison => "[chained comparisons]",
    slash_for_int => "[/ where an Int is expected]",
    branch_type_mismatch => "[if and else branches of different types]",
    return_type_mismatch => "[return paths of different types]",
    recursion_without_return_type => "[a recursive function without ->]",
    literal_too_big => "[a number literal too big for its type]",
    negative_literal_exponent => "[a negative literal exponent on an Int]",
    unhandled_maybe => "[a T? used where a T is needed]",
    doubled_maybe => "[a T? body where a plain T? is expected]",
    flatten_without_nesting => "[flatten() on a T? with nothing nested]",
    not_iterable => "[for over something that isn't a list or range]",
    try_outside_maybe => "[try outside a function returning a Maybe]",
    assign_to_let => "[assigning to a let]",
    assign_to_parameter => "[assigning to a parameter not written var]",
    change_let_list => "[changing a let list]",
    break_outside_loop => "[break or continue outside a loop]",
    return_outside_function => "[return outside a function]",
    loop_value_without_break => "[a loop used as a value with no break value]",
    break_type_mismatch => "[break values of different types in one loop]",
    constant_backwards_range => "[a backwards range with constant ends]",
    nested_type => "[a class or enum declared inside a function body]",
    mixed_fields => "[a variant with both named and positional fields]",
    this_outside_method => "[this outside a method]",
    unknown_field => "[a field that the type doesn't have]",
    unqualified_variant => "[a variant written without its enum where the type isn't known]",
    non_exhaustive_match => "[a match that misses a case]",
    or_pattern_bindings => "[or-pattern alternatives that bind different names]",
    let_else_can_finish => "[a let-else whose else can finish]",
    const_not_constant => "[a const whose value isn't a constant expression]",
    const_cycle => "[consts defined in terms of each other]",
    external_outside_library => "[an external function outside a library package]",
    try_stopped => "[a top-level try found no value, so the program stopped]",
    panic_called => "[panic was called]",
    integer_overflow => "[integer overflow]",
    division_by_zero => "[integer division by zero]",
    or_panic_on_none => "[orPanic on a None]",
    negative_exponent => "[a negative exponent on an Int]",
    shift_out_of_range => "[a shift amount out of range]",
    round_out_of_range => "[rounding a Float that has no Int]",
    stack_overflow => "[more than 10,000 nested calls]",
    index_out_of_range => "[writing to a list index that doesn't exist]",
    unused_var => "[a var that's never reassigned]",
    shadowed_name => "[a local that shadows a function or type]",
    interpolated_maybe => "[interpolating a T?]",
    nested_maybe => "[an if or loop value that is a T??]",
    let_else_some => "[let Some(x) = e else, which ?? can replace]",
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
    pub detail: Option<String>,
}

impl Diagnostic {
    pub fn error(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            severity: Severity::Error,
            detail: None,
        }
    }

    pub fn warning(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            severity: Severity::Warning,
            detail: None,
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
            "{kind}[{code}]: {message}{detail}\n{gutter}--> {path}:{line}:{col}\n{gutter} |\n{line} | {text}\n{gutter} | {pad}{marks}\n",
            code = self.code,
            message = self.code.message(),
            detail = self
                .detail
                .as_ref()
                .map(|d| format!(": {d}"))
                .unwrap_or_default(),
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
