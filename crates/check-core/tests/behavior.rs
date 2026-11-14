use std::io::Cursor;

use check_core::diag::Severity;
use check_core::{Outcome, run_source};

fn output(src: &str) -> String {
    let mut out = Vec::new();
    let report = run_source("test.check", src, &mut Cursor::new(Vec::new()), &mut out);
    let errors: Vec<String> = report
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .collect();
    assert!(errors.is_empty(), "unexpected errors {errors:?} in {src}");
    assert!(
        matches!(report.outcome, Outcome::Finished),
        "{src} didn't finish"
    );
    String::from_utf8(out)
        .expect("utf-8")
        .trim_end()
        .to_string()
}

fn prints(src: &str, expected: &str) {
    assert_eq!(output(&format!("print({src})")), expected, "print({src})");
}

fn panics_with(src: &str, code: &str) {
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    match report.outcome {
        Outcome::Panicked(c) => assert_eq!(c.to_string(), code, "{src}"),
        _ => panic!("{src} didn't panic"),
    }
}

#[test]
fn power_binds_tighter_than_unary_minus() {
    prints("-2 ** 2", "-4");
    prints("2 ** 3 ** 2", "512");
    prints("2.0 ** -1", "0.5");
}

#[test]
fn bitwise_binds_tighter_than_comparison_and_not_looser() {
    prints("6 & 3 == 2", "true");
    prints("not 1 == 2", "true");
    prints("true or false and false", "true");
}

#[test]
fn coalesce_binds_tighter_than_comparison() {
    prints("Int.parse(\"x\") ?? 7 == 7", "true");
}

#[test]
fn a_line_continues_after_an_operator_or_before_a_dot() {
    assert_eq!(output("let a = 1 +\n    2\nprint(a)"), "3");
    assert_eq!(output("let s = \"hi\"\n    .upper()\nprint(s)"), "HI");
}

#[test]
fn a_line_break_ends_a_statement() {
    assert_eq!(output("var a = 5\na\n-1\nprint(a)"), "5");
}

#[test]
fn interpolation_holds_any_expression_including_strings() {
    assert_eq!(
        output("let xs = [\"a\", \"b\"]\nprint(\"{xs.join(\", \")}!\")"),
        "a, b!"
    );
    prints("\"\\{x} {1 + 1}\"", "{x} 2");
}

#[test]
fn whole_number_division_rounds_down() {
    prints("-7 div 2", "-4");
    prints("7 div -2", "-4");
    prints("-7 mod 3", "2");
    prints("7 mod -3", "-2");
}

#[test]
fn integer_overflow_and_division_by_zero_panic() {
    panics_with("print(9223372036854775807 * 2)", "E0902");
    panics_with("print(-9223372036854775807 - 2)", "E0902");
    panics_with("print(1 mod 0)", "E0903");
    panics_with("print(1 / 0)", "E0903");
    prints("1.0 / 0.0", "Infinity");
}

#[test]
fn floats_print_like_check_literals() {
    prints("1.0", "1.0");
    prints("0.1 + 0.2", "0.30000000000000004");
    prints("1e21", "1e21");
    prints("1e20", "100000000000000000000.0");
    prints("0.000001", "0.000001");
    prints("1.5e-7", "1.5e-7");
    prints("-0.0", "-0.0");
    prints("0.0 / 0.0", "NaN");
}

#[test]
fn values_print_like_code_except_a_plain_string() {
    prints("\"a\\nb\"", "a\nb");
    prints("[\"a\\nb\", \"\\{\"]", "[\"a\\nb\", \"\\{\"]");
    prints("Some(Some(\"x\"))", "Some(Some(\"x\"))");
}

#[test]
fn a_copied_list_never_changes_the_original() {
    assert_eq!(
        output("let xs = [1]\nvar ys = xs\nys.push(2)\nprint(xs)"),
        "[1]"
    );
}

#[test]
fn a_lambda_keeps_the_values_it_captured() {
    assert_eq!(
        output("var n = 1\nlet f = func() n\nn = 2\nprint(f())"),
        "1"
    );
}

#[test]
fn try_makes_an_inferred_return_type_a_maybe() {
    assert_eq!(
        output("func first() {\n    let a = try Int.parse(\"x\")\n    a\n}\nprint(first())"),
        "None"
    );
}
