use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
    ($($code:ident,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[allow(non_camel_case_types)]
        pub enum Code { $($code,)* }

        impl Code {
            pub const ALL: &[Code] = &[$(Code::$code,)*];

            pub fn name(self) -> &'static str {
                match self { $(Code::$code => stringify!($code),)* }
            }

            pub fn message(self) -> &'static str {
                crate::copy::text(self.name())
            }
        }

        impl fmt::Display for Code {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

codes! {
    unknown_character,
    unclosed_string,
    unknown_escape,
    unclosed_interpolation,
    leading_zero,
    malformed_number,
    unexpected_token,
    unclosed_bracket,
    trailing_separator,
    else_on_new_line,
    foreign_operator,
    assignment_in_condition,
    arrow_without_braces,
    arrow_before_braces,
    body_without_arrow,
    var_in_declaration_pattern,
    let_in_class_header,
    readonly_let,
    arrow_on_new_line,
    semicolon_in_arrow_body,
    unknown_name,
    duplicate_name,
    redefined_builtin,
    used_before_declared,
    used_before_set,
    unset_read,
    let_set_twice,
    top_level_without_value,
    nested_named_function,
    type_mismatch,
    type_needed,
    wrong_arguments,
    unknown_method,
    not_callable,
    invalid_operand_types,
    bitwise_on_bool,
    chained_comparison,
    slash_for_int,
    branch_type_mismatch,
    return_type_mismatch,
    recursion_without_return_type,
    literal_too_big,
    negative_literal_exponent,
    unhandled_maybe,
    doubled_maybe,
    flatten_without_nesting,
    not_iterable,
    invalid_key_type,
    try_outside_failable,
    try_result_in_maybe,
    try_maybe_in_result,
    try_error_mismatch,
    assign_to_let,
    assign_to_captured,
    overlapping_ref,
    ref_default,
    ref_func_as_value,
    shared_where_value_expected,
    changed_in_for_ref,
    assign_to_parameter,
    change_let_collection,
    changes_let,
    missing_shared,
    not_shared,
    ref_binding_overlap,
    ref_binding_captured,
    interface_changes_this,
    break_outside_loop,
    return_outside_function,
    loop_value_without_break,
    break_type_mismatch,
    constant_backwards_range,
    nested_type,
    mixed_fields,
    record_to_class,
    spread_not_first,
    this_outside_method,
    unknown_field,
    unqualified_variant,
    non_exhaustive_match,
    or_pattern_bindings,
    let_else_can_finish,
    const_not_constant,
    const_cycle,
    missing_import,
    folder_without_slash,
    imported_entry_file,
    overlapping_imports,
    private_import_in_run,
    ambiguous_name,
    private_name,
    private_member,
    readonly_field,
    private_field_without_default,
    wrong_type_arguments,
    not_an_interface,
    unmet_constraint,
    missing_implementation,
    wrong_implementation,
    conflicting_defaults,
    field_requirement_needs_class,
    implement_for_specific_arguments,
    list_needs_type,
    interface_field_extras,
    code_in_pure_data,
    missing_manifest,
    no_entry,
    missing_entry,
    misplaced_host_import,
    unknown_host,
    unsupported_import,
    import_cycle,
    data_folder,
    bad_data_name,
    not_pure_data,
    namespace_outside_package,
    misplaced_namespace,
    external_outside_library,
    try_stopped,
    panic_called,
    integer_overflow,
    division_by_zero,
    or_panic_on_none,
    or_panic_on_err,
    negative_exponent,
    shift_out_of_range,
    round_out_of_range,
    stack_overflow,
    internal_error,
    out_of_steps,
    data_try_failed,
    missing_key,
    index_out_of_range,
    for_ref_element_gone,
    unused_var,
    changed_copy,
    shared_wraps_shared_class,
    ref_param_of_shared_class,
    shadowed_name,
    interpolated_maybe,
    nested_maybe,
    let_else_some,
    repeated_namespace,
    set_in_every_branch,
    nested_try,
    compound_on_missing_index,
    ref_changed_elsewhere,
    ref_global_written,
    record_beside_class,
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
