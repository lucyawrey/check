pub mod ast;
pub mod checker;
pub mod diag;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod types;
pub mod value;

use std::io::{BufRead, Write};

use diag::{Code, Diagnostic, Severity};

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
}

pub fn check_source(src: &str) -> (Vec<ast::Stmt>, checker::Info, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let tokens = lexer::lex(src, &mut diags);
    let mut next_id = 0;
    let program = parser::parse_program(tokens, &mut next_id, &mut diags);
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return (program, checker::Info::default(), diags);
    }
    let (info, check_diags) = checker::check(&program);
    diags.extend(check_diags);
    (program, info, diags)
}

pub fn run_source(
    path: &str,
    src: &str,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Report {
    let (program, info, diagnostics) = check_source(src);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Report {
            outcome: Outcome::CheckFailed,
            diagnostics,
            panic_text: None,
        };
    }
    match interp::run(&program, &info, input, output) {
        Ok(None) => Report {
            outcome: Outcome::Finished,
            diagnostics,
            panic_text: None,
        },
        Ok(Some(span)) => {
            let (line, col, _) = diag::locate(src, span.start);
            let text = format!("{} at {path}:{line}:{col}\n", Code::try_stopped.message());
            Report {
                outcome: Outcome::Stopped,
                diagnostics,
                panic_text: Some(text),
            }
        }
        Err(p) => {
            let (line, col, _) = diag::locate(src, p.span.start);
            let reason = p.reason.map(|r| format!(": {r}")).unwrap_or_default();
            let mut text = format!(
                "panic[{}]: {}{reason}\n --> {path}:{line}:{col}\n",
                p.code,
                p.code.message()
            );
            for call in &p.stack {
                let (line, col, _) = diag::locate(src, call.start);
                text.push_str(&format!("    at {path}:{line}:{col}\n"));
            }
            Report {
                outcome: Outcome::Panicked(p.code),
                diagnostics,
                panic_text: Some(text),
            }
        }
    }
}
