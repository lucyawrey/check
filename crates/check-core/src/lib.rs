pub use check_syntax::{ast, copy, diag, lexer, parser};

pub mod checker;
pub mod host;
pub mod interp;
pub mod ir;
pub mod lower;
pub mod modules;
pub mod resolve;
pub mod tidy;
pub mod types;
pub mod value;

use std::collections::HashMap;
use std::io::{BufRead, Write};

use copy::Text;
use diag::{Code, Diagnostic, Severity, Sources};
use modules::{Loaded, MemFiles};

pub use modules::Files;

pub enum Outcome {
    Finished,
    Stopped,
    CheckFailed,
    Panicked(Code),
}

pub struct Report {
    pub outcome: Outcome,
    pub diagnostics: Vec<Diagnostic>,
    pub panic_text: Option<String>,
    pub sources: Sources,
}

pub struct Checked {
    pub program: Vec<ast::Stmt>,
    pub library: Vec<ast::Stmt>,
    pub info: checker::Info,
    pub diagnostics: Vec<Diagnostic>,
    pub sources: Sources,
}

pub fn check_program(entry: &str, files: &dyn Files, allow_private: bool) -> Checked {
    check_loaded(modules::load(entry, files, allow_private))
}

pub(crate) fn check_loaded(loaded: Loaded) -> Checked {
    let Loaded {
        program,
        library,
        sources,
        diagnostics,
    } = loaded;
    let mut checked = Checked {
        program,
        library,
        info: checker::Info::default(),
        diagnostics,
        sources,
    };
    if checked
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return checked;
    }
    let (info, check_diags) = checker::check(&checked.program, &checked.library, &checked.sources);
    checked.info = info;
    checked.diagnostics.extend(check_diags);
    checked
}

/// Reads the `.con` file at `path`, with contents `text`, as the const `name` of type
/// `expected_type`, which is named as code importing the unit at `schema` in `files` names it.
/// Also returns the class the file's value is written as, resolved (`place.Place` for
/// `Place(...)`), or `None` when it's a plain record or other value.
pub fn check_data(
    files: &dyn Files,
    schema: &str,
    name: &str,
    path: &str,
    text: &str,
    expected_type: &str,
) -> Result<(value::Value, Option<String>), Report> {
    let (loaded, id, is_call) = modules::load_data(files, schema, name, path, text, expected_type);
    let Checked {
        program,
        library,
        info,
        diagnostics,
        sources,
    } = check_loaded(loaded);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(Report {
            outcome: Outcome::CheckFailed,
            diagnostics,
            panic_text: None,
            sources,
        });
    }
    // A plain record also resolves as a construct of the expected class.
    let class = match info.resolved.get(&id) {
        Some(checker::Resolved::Construct(class)) if is_call => Some(class.clone()),
        _ => None,
    };
    let value = interp::eval_data(&program, &library, &info, id).map_err(|p| Report {
        outcome: Outcome::Panicked(p.code),
        panic_text: Some(panic_text(&p, &sources)),
        diagnostics,
        sources,
    })?;
    Ok((value, class))
}

fn one_file(path: &str, src: &str) -> MemFiles {
    MemFiles(HashMap::from([(path.to_string(), src.to_string())]))
}

pub fn check_source(path: &str, src: &str) -> Checked {
    check_program(path, &one_file(path, src), false)
}

pub fn run_source(
    path: &str,
    src: &str,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Report {
    run_program(path, &one_file(path, src), false, input, output)
}

pub fn run_program(
    entry: &str,
    files: &dyn Files,
    allow_private: bool,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Report {
    let Checked {
        program,
        library,
        info,
        diagnostics,
        sources,
    } = check_program(entry, files, allow_private);
    let (outcome, panic_text) = if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        (Outcome::CheckFailed, None)
    } else {
        match interp::run(&program, &library, &info, input, output) {
            Ok(None) => (Outcome::Finished, None),
            Ok(Some((span, err))) => {
                let (path, line, col, _) = sources.locate(span.start);
                let err = err.map(|e| format!(": {e}")).unwrap_or_default();
                let text = format!(
                    "{}{err} at {path}:{line}:{col}\n",
                    Code::try_stopped.message()
                );
                (Outcome::Stopped, Some(text))
            }
            Err(p) => (Outcome::Panicked(p.code), Some(panic_text(&p, &sources))),
        }
    };
    Report {
        outcome,
        diagnostics,
        panic_text,
        sources,
    }
}

fn panic_text(p: &interp::Panic, sources: &Sources) -> String {
    let mut spans = std::iter::once(&p.span)
        .chain(&p.stack)
        .filter(|s| !modules::is_std_file(sources.locate(s.start).0));
    let at = spans.next().unwrap_or(&p.span);
    let stack: Vec<_> = spans.collect();
    let (path, line, col, _) = sources.locate(at.start);
    let reason = p
        .reason
        .as_ref()
        .map(|r| format!(": {r}"))
        .unwrap_or_default();
    let mut text = format!(
        "panic[{}]: {}{reason}\n --> {path}:{line}:{col}\n",
        p.code,
        p.code.message()
    );
    let shown = 10;
    for (i, call) in stack.iter().enumerate() {
        if stack.len() > shown * 2 && i >= shown && i < stack.len() - shown {
            if i == shown {
                let left_out = stack.len() - shown * 2;
                let line = Text::calls_left_out
                    .get()
                    .replace("{count}", &left_out.to_string());
                text.push_str(&format!("    {line}\n"));
            }
            continue;
        }
        let (path, line, col, _) = sources.locate(call.start);
        text.push_str(&format!("    at {path}:{line}:{col}\n"));
    }
    text
}
