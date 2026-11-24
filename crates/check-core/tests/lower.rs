use check_core::diag::{Code, Diagnostic, Severity};
use check_core::ir::Program;
use check_core::{Checked, check_source, lower::lower};

fn lowered(src: &str) -> (Result<Program, Diagnostic>, Checked) {
    let checked = check_source("test.check", src);
    let errors = (checked.diagnostics.iter()).filter(|d| d.severity == Severity::Error);
    assert_eq!(errors.count(), 0, "{src} has errors");
    (
        lower(&checked.program, &checked.library, &checked.info),
        checked,
    )
}

fn unsupported_text(src: &str) -> String {
    let (result, checked) = lowered(src);
    let d = result.err().expect("an unsupported error");
    assert_eq!(d.code, Code::wasm_unsupported);
    checked.sources.text(d.span).to_string()
}

#[test]
fn lowers_only_the_functions_the_program_reaches() {
    let src = "func unused() => [1, 2]\nfunc twice(n: Int) => n * 2\nfunc four() => twice(2)\nprint(four())";
    let program = lowered(src).0.expect("lowers");
    let names: Vec<&str> = program.funcs.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["main", "four", "twice"]);
}

#[test]
fn points_at_the_first_unsupported_expression() {
    assert_eq!(unsupported_text("let x = 1\nprint([x, 2])"), "[x, 2]");
    assert_eq!(unsupported_text("let xs = [1]\nprint(1)"), "[1]");
    assert_eq!(
        unsupported_text("func f(ref n: Int) { n += 1 }\nvar a = 1\nf(a)"),
        "f"
    );
}
