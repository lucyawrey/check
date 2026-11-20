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
    arrow_before_braces => "[=> before a body in braces]",
    body_without_arrow => "[a body without braces and without =>]",
    var_in_declaration_pattern => "[var inside a let or var pattern]",
    let_in_class_header => "[let in a class header, where fields are let unless written var]",
    readonly_let => "[readonly on a let field, which is already read-only]",
    arrow_on_new_line => "[a => body that starts on the line after]",
    semicolon_in_arrow_body => "[a ; in a => body, which needs braces for more than one statement]",
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
    invalid_key_type => "[a map key or set item of a type without exact equality]",
    try_outside_failable => "[try outside a function returning a Maybe or a Result]",
    try_result_in_maybe => "[try on a Result in a function returning a Maybe]",
    try_maybe_in_result => "[try on a Maybe in a function returning a Result]",
    try_error_mismatch => "[try on a Result whose error type differs from the function's]",
    assign_to_let => "[assigning to a let]",
    assign_to_captured => "[changing a variable a lambda captured]",
    missing_ref => "[an argument for a ref parameter without ref]",
    ref_not_allowed => "[ref on an argument whose parameter isn't ref]",
    overlapping_ref => "[a ref argument that overlaps another argument of the same call]",
    ref_default => "[a ref parameter with a default]",
    ref_func_as_value => "[a function with ref parameters used as a value]",
    shared_where_value_expected => "[a Shared where its value is expected]",
    changed_in_for_ref => "[changing the list a for ref loop goes through]",
    assign_to_parameter => "[assigning to a parameter not written var]",
    change_let_collection => "[changing a let list, map or set]",
    changes_let => "[calling a method that changes its value on a let]",
    ref_binding_overlap => "[using what a ref binding points to other than through it]",
    ref_binding_captured => "[a lambda capturing a ref binding]",
    interface_changes_this => "[an interface method whose implementation changes this]",
    break_outside_loop => "[break or continue outside a loop]",
    return_outside_function => "[return outside a function]",
    loop_value_without_break => "[a loop used as a value with no break value]",
    break_type_mismatch => "[break values of different types in one loop]",
    constant_backwards_range => "[a backwards range with constant ends]",
    nested_type => "[a class or enum declared inside a function body]",
    mixed_fields => "[a variant or record with both named and positional fields]",
    record_to_class => "[a record value used where a class is expected]",
    spread_not_first => "[a ... spread that isn't first, or a second one]",
    this_outside_method => "[this outside a method]",
    unknown_field => "[a field that the type doesn't have]",
    unqualified_variant => "[a variant written without its enum where the type isn't known]",
    non_exhaustive_match => "[a match that misses a case]",
    or_pattern_bindings => "[or-pattern alternatives that bind different names]",
    let_else_can_finish => "[a let-else whose else can finish]",
    const_not_constant => "[a const whose value isn't a constant expression]",
    const_cycle => "[consts defined in terms of each other]",
    missing_import => "[an import of a file or folder that doesn't exist]",
    folder_without_slash => "[a folder import without a trailing /]",
    imported_entry_file => "[an import of a file that has imports or top-level statements]",
    overlapping_imports => "[two imports that cover the same file]",
    private_import_in_run => "[import private in a program being run, not tested]",
    ambiguous_name => "[a name found in more than one namespace]",
    private_name => "[a private name used outside its unit]",
    private_member => "[a private field or method used outside its type]",
    readonly_field => "[a readonly var set outside its class]",
    private_field_without_default => "[a private field with no default]",
    wrong_type_arguments => "[a generic type with the wrong number of type arguments]",
    not_an_interface => "[a type named after : or for that isn't an interface]",
    unmet_constraint => "[a type argument that doesn't implement its parameter's interfaces]",
    missing_implementation => "[a type missing a method or field its interface requires]",
    wrong_implementation => "[a method whose types don't match its interface's]",
    conflicting_defaults => "[two interfaces' defaults for one method, neither overridden]",
    field_requirement_needs_class => "[an interface requiring fields, implemented by a type that isn't a class]",
    implement_for_specific_arguments => "[an interface implemented for some of a generic type's arguments]",
    list_needs_type => "[a list of different types with no declared element type]",
    interface_field_extras => "[an interface field with a default, private or readonly]",
    code_in_pure_data => "[something other than a literal in a .con file]",
    missing_manifest => "[a folder run with no package.con]",
    no_entry => "[a folder run whose package.con names no entry]",
    missing_entry => "[a package.con entry file that doesn't exist]",
    misplaced_host_import => "[import host twice, or beside a package.con]",
    unknown_host => "[a host that doesn't exist]",
    unsupported_import => "[a pkg: or URL import, which isn't supported yet]",
    import_cycle => "[units that import each other in a loop]",
    data_folder => "[a folder of data files with no code, which needs Map]",
    bad_data_name => "[a data file whose name isn't a valid name]",
    not_pure_data => "[a data file a host reads that isn't a .con file]",
    namespace_outside_package => "[a namespace that doesn't start with its package's]",
    misplaced_namespace => "[a namespace line that isn't the first line of the file]",
    external_outside_library => "[an external function outside a library package]",
    try_stopped => "[a top-level try found None or an Err, so the program stopped]",
    panic_called => "[panic was called]",
    integer_overflow => "[integer overflow]",
    division_by_zero => "[integer division by zero]",
    or_panic_on_none => "[orPanic on a None]",
    or_panic_on_err => "[orPanic on an Err]",
    negative_exponent => "[a negative exponent on an Int]",
    shift_out_of_range => "[a shift amount out of range]",
    round_out_of_range => "[rounding a Float that has no Int]",
    stack_overflow => "[more than 10,000 nested calls]",
    internal_error => "[an interpreter bug: a case the checker should have ruled out]",
    out_of_steps => "[a const or data value that ran too long]",
    data_try_failed => "[a try that failed while building a data value]",
    missing_key => "[a map key that isn't there, where a value is needed]",
    index_out_of_range => "[a list index that doesn't exist, where a value is needed]",
    for_ref_element_gone => "[a for ref element whose index is gone when it's written back]",
    unused_var => "[a var that's never reassigned]",
    shared_wraps_shared_class => "[Shared of a shared class, which is already shared]",
    ref_param_of_shared_class => "[a ref parameter of a shared class, which is already shared]",
    shadowed_name => "[a local that shadows a function or type]",
    interpolated_maybe => "[interpolating a T?]",
    nested_maybe => "[an if or loop value that is a T??]",
    let_else_some => "[let Some(x) = e else, which ?? can replace]",
    repeated_namespace => "[a namespace line that only repeats its package's]",
    record_beside_class => "[a record literal with its class written right beside it]",
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// Replaces the text at `span` with `text`; empty text deletes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub span: Span,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: Code,
    pub span: Span,
    pub severity: Severity,
    pub detail: Option<String>,
    /// The tidy fix, applied all together; empty when there's no one obvious fix.
    pub fix: Vec<Edit>,
    pub quick_fix_only: bool,
}

impl Diagnostic {
    pub fn error(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            severity: Severity::Error,
            detail: None,
            fix: Vec::new(),
            quick_fix_only: false,
        }
    }

    pub fn warning(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            severity: Severity::Warning,
            detail: None,
            fix: Vec::new(),
            quick_fix_only: false,
        }
    }

    pub fn with_fix(mut self, fix: Vec<Edit>) -> Self {
        self.fix = fix;
        self
    }

    pub fn with_quick_fix(mut self, fix: Vec<Edit>) -> Self {
        self.fix = fix;
        self.quick_fix_only = true;
        self
    }

    /// `color` adds ANSI colors in rustc's scheme.
    pub fn render(&self, sources: &Sources, color: bool) -> String {
        let (path, line, col, text) = sources.locate(self.span.start);
        let width = (self
            .span
            .end
            .min(self.span.start + text.len().saturating_sub(col - 1)))
        .saturating_sub(self.span.start)
        .max(1);
        let (kind, style) = match self.severity {
            Severity::Error => ("error", "1;31"),
            Severity::Warning => ("warning", "1;33"),
        };
        let paint = |style: &str, s: String| {
            if color {
                format!("\x1b[{style}m{s}\x1b[0m")
            } else {
                s
            }
        };
        let gutter = " ".repeat(line.to_string().len());
        let detail = self
            .detail
            .as_ref()
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        format!(
            "{head}{message}\n{arrow} {path}:{line}:{col}\n{empty}\n{number} {text}\n{bar} {pad}{marks}\n",
            head = paint(style, format!("{kind}[{}]", self.code)),
            message = paint("1", format!(": {}{detail}", self.code.message())),
            arrow = paint("1;34", format!("{gutter}-->")),
            empty = paint("1;34", format!("{gutter} |")),
            number = paint("1;34", format!("{line} |")),
            bar = paint("1;34", format!("{gutter} |")),
            pad = " ".repeat(col - 1),
            marks = paint(style, "^".repeat(width)),
        )
    }
}

pub struct Source {
    pub path: String,
    pub text: String,
    /// The offset of the file's first byte in spans.
    pub base: usize,
}

/// Every file gets its own range of offsets, so a span alone says which file it's in.
#[derive(Default)]
pub struct Sources {
    files: Vec<Source>,
}

impl Sources {
    /// Returns the base offset to lex the file at.
    pub fn add(&mut self, path: &str, text: &str) -> usize {
        let base = self.files.last().map_or(0, |f| f.base + f.text.len() + 1);
        self.files.push(Source {
            path: path.to_string(),
            text: text.to_string(),
            base,
        });
        base
    }

    pub fn files(&self) -> &[Source] {
        &self.files
    }

    pub fn file(&self, offset: usize) -> &Source {
        let i = self.files.partition_point(|f| f.base <= offset);
        &self.files[i.saturating_sub(1)]
    }

    pub fn text(&self, span: Span) -> &str {
        let file = self.file(span.start);
        &file.text[span.start - file.base..span.end - file.base]
    }

    pub fn locate(&self, offset: usize) -> (&str, usize, usize, &str) {
        let file = self.file(offset);
        let (line, col, text) = locate(&file.text, offset - file.base);
        (&file.path, line, col, text)
    }
}

fn locate(src: &str, offset: usize) -> (usize, usize, &str) {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[offset..].find('\n').map_or(src.len(), |i| offset + i);
    let line = src[..offset].matches('\n').count() + 1;
    let col = src[line_start..offset].chars().count() + 1;
    (line, col, &src[line_start..line_end])
}
