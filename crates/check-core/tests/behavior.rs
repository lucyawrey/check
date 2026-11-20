use std::collections::HashMap;
use std::io::Cursor;

use check_core::ast::Stmt;
use check_core::diag::Severity;
use check_core::modules::{MemFiles, entry_of};
use check_core::{Outcome, Report, run_program, run_source};

fn output(src: &str) -> String {
    let mut out = Vec::new();
    let report = run_source("test.check", src, &mut Cursor::new(Vec::new()), &mut out);
    finished(report, out, src)
}

fn finished(report: Report, out: Vec<u8>, src: &str) -> String {
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
    panics_with("print(9223372036854775807 * 2)", "integer_overflow");
    panics_with("print(-9223372036854775807 - 2)", "integer_overflow");
    panics_with("print(1 mod 0)", "division_by_zero");
    panics_with("print(1 / 0)", "division_by_zero");
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
        output("var n = 1\nlet f = func() => n\nn = 2\nprint(f())"),
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

#[test]
fn a_top_level_try_on_none_stops_without_a_panic() {
    let mut out = Vec::new();
    let src = "print(\"before\")\nlet line = try input()\nprint(\"after\")";
    let report = run_source("test.check", src, &mut Cursor::new(Vec::new()), &mut out);
    assert!(matches!(report.outcome, Outcome::Stopped));
    assert_eq!(String::from_utf8(out).expect("utf-8"), "before\n");
}

const PARSE: &str = "func parse(s: String) -> Result<Int, String> {\n    match Int.parse(s) { Some(n) => Ok(n), None => Err(\"bad {s}\") }\n}\n";

fn with_parse(src: &str) -> String {
    output(&format!("{PARSE}{src}"))
}

#[test]
fn try_on_a_result_gives_the_ok_value_or_passes_the_err_up() {
    let sum = "func sum(a: String, b: String) -> Result<Int, String> {\n    Ok(try parse(a) + try parse(b))\n}\n";
    assert_eq!(
        with_parse(&format!(
            "{sum}print(sum(\"1\", \"2\"))\nprint(sum(\"1\", \"x\"))"
        )),
        "Ok(3)\nErr(\"bad x\")"
    );
}

#[test]
fn try_on_a_result_makes_an_inferred_return_type_a_result() {
    let f = "func f(s: String) {\n    if s == \"\" { return 0 }\n    if s == \"-\" { return Err(\"dash\") }\n    try parse(s) + 1\n}\n";
    assert_eq!(
        with_parse(&format!(
            "{f}print(f(\"1\"))\nprint(f(\"\"))\nprint(f(\"-\"))\nprint(f(\"x\"))"
        )),
        "Ok(2)\nOk(0)\nErr(\"dash\")\nErr(\"bad x\")"
    );
    let ends_in_result = "func g(s: String) {\n    try parse(s)\n    parse(s)\n}\nprint(g(\"2\"))";
    assert_eq!(with_parse(ends_in_result), "Ok(2)");
    assert_eq!(
        with_parse(
            "let h = func(s: String) { try parse(s) * 2 }\nprint(h(\"4\"))\nprint(h(\"x\"))"
        ),
        "Ok(8)\nErr(\"bad x\")"
    );
}

#[test]
fn a_variant_inside_ok_or_err_takes_its_enum_from_the_return_type() {
    let src = "enum E { A, B }\nfunc f() -> Result<Int, E> { Err(A) }\nprint(f())";
    assert_eq!(output(src), "Err(A)");
}

#[test]
fn a_recursive_function_with_a_return_type_can_use_try() {
    let f = "func f(n: Int) -> Result<Int, String> {\n    if n == 0 { return Ok(0) }\n    Ok(try f(n - 1) + try parse(\"2\"))\n}\nprint(f(3))";
    assert_eq!(with_parse(f), "Ok(6)");
}

fn error_and_fix(src: &str) -> (String, Option<String>) {
    let src = format!("{PARSE}{src}");
    let report = run_source(
        "test.check",
        &src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    let d = report
        .diagnostics
        .into_iter()
        .find(|d| d.severity == Severity::Error)
        .expect("an error");
    (d.code.to_string(), d.detail)
}

#[test]
fn try_mixing_maybe_and_result_suggests_a_conversion() {
    assert_eq!(
        error_and_fix("func f() -> Int? { try parse(\"1\") }"),
        (
            "try_result_in_maybe".into(),
            Some("try parse(\"1\").ok()".into())
        )
    );
    assert_eq!(
        error_and_fix("func f() -> Result<Int, String> { Ok(try Int.parse(\"1\")) }"),
        (
            "try_maybe_in_result".into(),
            Some("try Int.parse(\"1\").orErr(...)".into())
        )
    );
    assert_eq!(
        error_and_fix("func f() {\n    let a = try Int.parse(\"1\")\n    try parse(\"2\")\n}"),
        (
            "try_result_in_maybe".into(),
            Some("try parse(\"2\").ok()".into())
        )
    );
    let operator = "func f() -> Result<Int, String> {\n    let b = Some(Int.parse(\"1\"))\n    Ok(try (b ?? None))\n}";
    assert_eq!(
        error_and_fix(operator).1.as_deref(),
        Some("try (b ?? None).orErr(...)")
    );
}

#[test]
fn try_needs_a_function_that_can_pass_its_failure_up() {
    let mismatch = "func f() -> Result<Int, Int> { Ok(try parse(\"1\")) }";
    assert_eq!(error_and_fix(mismatch).0, "try_error_mismatch");
    let ends_in_other = "func f() {\n    try parse(\"1\")\n    Int.parse(\"1\").orErr(0)\n}";
    assert_eq!(error_and_fix(ends_in_other).0, "try_error_mismatch");
    assert_eq!(
        error_and_fix("func f() -> Int { try parse(\"1\") }").0,
        "try_outside_failable"
    );
    let class = "class C(n: Int)\nfunc f() -> C { C(try Int.parse(\"1\")) }";
    assert_eq!(error_and_fix(class).0, "try_outside_failable");
}

#[test]
fn a_top_level_try_on_an_err_stops_and_shows_the_error() {
    let mut out = Vec::new();
    let src = format!("{PARSE}let n = try parse(\"x\")\nprint(n)");
    let report = run_source("test.check", &src, &mut Cursor::new(Vec::new()), &mut out);
    assert!(matches!(report.outcome, Outcome::Stopped));
    assert!(out.is_empty());
    let text = report.panic_text.unwrap_or_default();
    assert!(text.contains(": bad x at test.check:4:9"), "{text}");
}

#[test]
fn coalesce_and_or_panic_work_on_a_result() {
    assert_eq!(
        with_parse("print(parse(\"3\") ?? panic(\"run\"))\nprint(parse(\"x\") ?? 0)"),
        "3\n0"
    );
    panics_with(&format!("{PARSE}parse(\"x\").orPanic()"), "or_panic_on_err");
}

fn failure_text(src: &str) -> String {
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    report.panic_text.unwrap_or_default()
}

#[test]
fn a_stop_or_or_panic_on_an_err_shows_the_error_through_to_string() {
    let boom = "class Boom: Printable {\n    func toString() -> String { \"boom\" }\n}\nfunc f() -> Result<Int, Boom> { Err(Boom()) }\n";
    let text = failure_text(&format!("{boom}f().orPanic(\"needed\")"));
    assert!(text.contains(": needed: boom"), "{text}");
    let text = failure_text(&format!("{boom}let n = try f()"));
    assert!(text.contains(": boom at"), "{text}");
}

#[test]
fn an_unknown_method_still_reports_errors_in_its_arguments() {
    for recv in ["Int.parse(\"1\")", "[1]", "Ok(1)", "1", "true"] {
        let src = format!("{recv}.foo(nowhere)");
        let report = run_source(
            "test.check",
            &src,
            &mut Cursor::new(Vec::new()),
            &mut Vec::new(),
        );
        let codes: Vec<String> = report
            .diagnostics
            .iter()
            .map(|d| d.code.to_string())
            .collect();
        assert!(
            codes.contains(&"unknown_method".to_string()),
            "{src}: {codes:?}"
        );
        assert!(
            codes.contains(&"unknown_name".to_string()),
            "{src}: {codes:?}"
        );
    }
}

#[test]
fn or_err_and_ok_convert_between_maybe_and_result() {
    assert_eq!(
        output("print(Int.parse(\"1\").orErr(\"no\"))\nprint(Int.parse(\"x\").orErr(\"no\"))"),
        "Ok(1)\nErr(\"no\")"
    );
    assert_eq!(
        with_parse("print(parse(\"1\").ok())\nprint(parse(\"x\").ok())"),
        "Some(1)\nNone"
    );
}

fn diagnostics(src: &str, severity: Severity) -> Vec<String> {
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    report
        .diagnostics
        .iter()
        .filter(|d| d.severity == severity)
        .map(|d| d.code.to_string())
        .collect()
}

fn first_error(src: &str) -> String {
    diagnostics(src, Severity::Error)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// The first error's fix, as the texts it inserts.
fn first_error_fix(src: &str) -> Vec<String> {
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    let d = report
        .diagnostics
        .into_iter()
        .find(|d| d.severity == Severity::Error)
        .expect("an error");
    d.fix.into_iter().map(|e| e.text).collect()
}

#[test]
fn a_lambda_body_must_match_its_written_return_type() {
    assert_eq!(
        first_error("let f = func(x: Int) -> String { x * 2 }"),
        "return_type_mismatch"
    );
}

#[test]
fn comparisons_with_nan_are_false() {
    prints("0.0 / 0.0 <= 1.0", "false");
    prints("0.0 / 0.0 >= 1.0", "false");
    prints("0.0 / 0.0 == 0.0 / 0.0", "false");
}

#[test]
fn edge_values_that_have_an_answer_dont_overflow() {
    assert_eq!(
        output("let m = -9223372036854775807 - 1\nprint(m mod -1)"),
        "0"
    );
    assert_eq!(
        output(
            "var n = 0\nfor i in 9223372036854775806..=9223372036854775807 { n += 1 }\nprint(n)"
        ),
        "2"
    );
    prints("0..=3", "0..=3");
}

#[test]
fn a_panic_lists_the_calls_it_came_through() {
    let src = "func a() { panic(\"boom\") }\nfunc b() { a() }\nb()";
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    let text = report.panic_text.unwrap_or_default();
    assert!(text.contains("test.check:1:12"), "{text}");
    assert!(text.contains("at test.check:2:12"), "{text}");
    assert!(text.contains("at test.check:3:1"), "{text}");
}

const SHAPES: &str = "enum Shape { Square, Circle(r: Float), Rect(Float, Float) }\n";

fn missing_case(src: &str) -> Option<String> {
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    report
        .diagnostics
        .into_iter()
        .find(|d| d.code.to_string() == "non_exhaustive_match")
        .and_then(|d| d.detail)
}

#[test]
fn a_match_missing_a_case_names_it() {
    let src =
        format!("{SHAPES}let s: Shape = Square\nprint(match s {{ Square => 1, Circle(_) => 2 }})");
    assert_eq!(missing_case(&src).as_deref(), Some("Rect(_, _)"));
    let nested = "let m = Int.parse(\"1\")\nprint(match m { Some(1) => 1, None => 0 })";
    assert_eq!(missing_case(nested).as_deref(), Some("Some(_)"));
}

#[test]
fn guards_dont_count_toward_exhaustiveness() {
    let src = "let m = Int.parse(\"1\")\nprint(match m { Some(n) if n > 0 => 1, None => 0 })";
    assert_eq!(missing_case(src).as_deref(), Some("Some(_)"));
}

#[test]
fn every_or_pattern_alternative_binds_the_same_names() {
    let src = format!(
        "{SHAPES}let s: Shape = Square\nprint(match s {{ Circle(r) | Rect(r, _) => r, Rect(w, h) | Square => 0.0 }})"
    );
    assert_eq!(first_error(&src), "or_pattern_bindings");
}

#[test]
fn a_let_else_must_leave_in_its_else() {
    assert_eq!(
        first_error("let Some(x) = Int.parse(\"1\") else { print(\"no\") }"),
        "let_else_can_finish"
    );
    assert_eq!(
        output("let Some(x) = Int.parse(\"5\") else { panic() }\nprint(x)"),
        "5"
    );
}

#[test]
fn a_copied_value_class_never_changes_the_original() {
    let src = "class Point(var x: Int, var y: Int)\nclass Line(var a: Point, var b: Point)\n\
        var p = Point(1, 2)\nvar q = p\nq.x = 9\nprint(p.x)\n\
        var points = [p]\nvar copies = points\ncopies[0].y = 9\nprint(points[0])\n\
        var line = Line(p, p)\nvar other = line\nother.a.x = 5\nprint(line.a.x)";
    assert_eq!(output(src), "1\nSome(Point(x = 1, y = 2))\n1");
}

#[test]
fn a_ref_class_is_shared_even_inside_a_copied_value() {
    let src = "ref class Counter { var n: Int = 0 }\nclass Holder(c: Counter)\n\
        let h = Holder(Counter())\nlet copy = h\ncopy.c.n = 3\nprint(h.c.n)\n\
        let c = h.c\nc.n += 1\nprint(h.c.n)\nprint(same(c, copy.c))";
    assert_eq!(output(src), "3\n4\ntrue");
}

#[test]
fn fields_change_only_through_var() {
    assert_eq!(
        first_error("class Point(var x: Int)\nlet p = Point(1)\np.x = 2"),
        "assign_to_let"
    );
    assert_eq!(
        first_error("class Point(x: Int)\nvar p = Point(1)\np.x = 2"),
        "assign_to_let"
    );
    assert_eq!(
        output("ref class Box(var x: Int)\nlet b = Box(1)\nb.x = 2\nprint(b.x)"),
        "2"
    );
}

#[test]
fn is_bindings_reach_and_and_the_body_only() {
    let m = "let m = Int.parse(\"3\")\n";
    assert_eq!(
        output(&format!("{m}if m is Some(n) and n > 2 {{ print(n) }}")),
        "3"
    );
    for bad in [
        "if m is Some(n) or true { print(n) }",
        "if not (m is Some(n)) { print(n) }",
        "if m is Some(n) { } else { print(n) }",
        "let ok = m is Some(n)\nprint(n)",
    ] {
        assert_eq!(first_error(&format!("{m}{bad}")), "unknown_name", "{bad}");
    }
    let src = "var lines = [\"a\", \"b\"]\nwhile lines.pop() is Some(line) { print(line) }";
    assert_eq!(output(src), "b\na");
}

#[test]
fn an_is_binding_never_overwrites_an_outer_variable() {
    let src = "let n = 1\nif Int.parse(\"5\") is Some(n) { print(n) }\nprint(n)";
    assert_eq!(output(src), "5\n1");
}

#[test]
fn init_runs_after_the_fields_are_set() {
    let src = "class Temp {\n  var c: Float\n  var f: Float = 0.0\n  init { this.f = this.c * 9.0 / 5.0 + 32.0 }\n}\n\
        print(Temp(100.0).f)";
    assert_eq!(output(src), "212.0");
}

#[test]
fn consts_are_constant_and_never_circular() {
    assert_eq!(
        output("class Game {\n  const max = 10\n  const half = max div 2\n}\nprint(Game.half)"),
        "5"
    );
    assert_eq!(
        first_error("enum E {\n  A\n  const a = b\n  const b = a\n}"),
        "const_cycle"
    );
    assert_eq!(
        first_error("enum E {\n  A\n  const a = input()\n}"),
        "const_not_constant"
    );
}

#[test]
fn a_variant_without_its_enum_needs_a_known_type() {
    assert_eq!(
        first_error(&format!("{SHAPES}let s = Circle(1.0)")),
        "unqualified_variant"
    );
    assert_eq!(
        output(&format!(
            "{SHAPES}let s = Shape.Circle(1.0)\nlet t: Shape = Rect(1.0, 2.0)\nprint([s, t, Shape.Square])"
        )),
        "[Circle(r = 1.0), Rect(1.0, 2.0), Square]"
    );
}

#[test]
fn printing_a_ref_class_that_reaches_itself_stops() {
    let src =
        "ref class Node {\n  var next: Node? = None\n}\nlet a = Node()\na.next = Some(a)\nprint(a)";
    assert_eq!(output(src), "Node(next = Some(...))");
}

#[test]
fn a_comparison_gives_a_bare_variant_the_other_sides_type() {
    let src = format!(
        "{SHAPES}let s: Shape = Circle(2.0)\nprint(s == Circle(2.0))\nprint(Square == s)\nprint(s != Rect(1.0, 1.0))"
    );
    assert_eq!(output(&src), "true\nfalse\ntrue");
}

#[test]
fn equality_never_compares_ref_classes_even_inside_other_values() {
    for src in ["[a] == [a]", "Some(a) == Some(a)", "W(a) == W(a)"] {
        assert_eq!(
            first_error(&format!(
                "ref class R(x: Int)\nclass W(r: R)\nlet a = R(1)\nprint({src})"
            )),
            "invalid_operand_types",
            "{src}"
        );
    }
}

#[test]
fn defaults_can_use_later_types_functions_and_consts() {
    let src = "class A {\n  let b: B = B(n = size())\n}\nclass B(n: Int)\nfunc size() -> Int { 3 }\n\
        class P {\n  const maxHp = 10\n  var hp = maxHp\n}\nprint(A())\nprint(P())";
    assert_eq!(output(src), "A(b = B(n = 3))\nP(hp = 10)");
}

#[test]
fn building_an_object_counts_toward_the_call_limit() {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| {
            panics_with(
                "class A {\n  let n: Int = 0\n  init { let x = A() }\n}\nprint(A())",
                "stack_overflow",
            )
        })
        .expect("spawn")
        .join()
        .expect("test thread");
}

#[test]
fn consts_are_computed_before_the_program_runs() {
    let src = "enum E {\n  A\n  const big = 9223372036854775807 + 1\n}\nprint(\"start\")";
    panics_with(src, "integer_overflow");
}

#[test]
fn library_files_hold_only_declarations_that_match_the_rust_bodies() {
    let checked = check_core::check_source("test.check", "");
    assert!(checked.diagnostics.is_empty());
    let mut declared = Vec::new();
    for stmt in &checked.library {
        match stmt {
            Stmt::Func(f) => declared.push(f),
            Stmt::Implement(i) => {
                declared.extend(i.members.methods.iter().chain(&i.members.statics))
            }
            Stmt::Class(c) => declared.extend(c.members.methods.iter().chain(&c.members.statics)),
            Stmt::Enum(e) => declared.extend(e.members.methods.iter().chain(&e.members.statics)),
            // An interface's methods without bodies are requirements, not host functions.
            Stmt::Interface(_) => {}
            _ => panic!("a library file holds something other than a declaration"),
        }
    }
    let mut names: Vec<&str> = declared
        .iter()
        .filter(|f| f.body.is_none())
        .map(|f| f.name.as_str())
        .collect();
    let mut bodies: Vec<&str> = check_core::host::FUNCTIONS
        .iter()
        .map(|(n, _)| *n)
        .collect();
    names.sort();
    bodies.sort();
    assert_eq!(names, bodies);
}

#[test]
fn a_users_function_or_method_replaces_the_librarys() {
    let src = "func input() -> String? { Some(\"mine\") }\nimplement String {\n  func upper() -> String { \"UP\" }\n}\n\
        print(input())\nprint(\"x\".upper())";
    assert_eq!(output(src), "Some(\"mine\")\nUP");
}

#[test]
fn external_functions_belong_only_in_library_files() {
    assert_eq!(
        first_error("external func f() -> Int"),
        "external_outside_library"
    );
}

#[test]
fn implement_needs_a_class_enum_or_simple_built_in_type() {
    assert_eq!(
        first_error("implement List {\n  func f() { }\n}"),
        "unknown_name"
    );
    assert_eq!(
        first_error("implement Nope {\n  func f() { }\n}"),
        "unknown_name"
    );
    assert_eq!(
        output("implement Int {\n  func double() -> Int { this * 2 }\n}\nprint(21.double())"),
        "42"
    );
}

#[test]
fn panic_from_the_library_still_never_returns() {
    let src = "func f(x: Int) -> Int {\n  if x > 0 { return x }\n  panic(\"no\")\n}\nprint(f(1))";
    assert_eq!(output(src), "1");
    panics_with("panic()", "panic_called");
}

#[test]
fn input_and_parse_behave_as_before() {
    let mut out = Vec::new();
    let src = "print(input(\"> \"))\nprint(input())\nprint(input())\nprint(Int.parse(\" 42 \"))\nprint(Float.parse(\"x\"))";
    run_source(
        "test.check",
        src,
        &mut Cursor::new(b"a\nb\n".to_vec()),
        &mut out,
    );
    assert_eq!(
        String::from_utf8(out).expect("utf-8"),
        "> Some(\"a\")\nSome(\"b\")\nNone\nSome(42)\nNone\n"
    );
}

#[test]
fn wrong_calls_to_library_functions_are_errors() {
    assert_eq!(first_error("print(\"abc\".upper(1))"), "wrong_arguments");
    assert_eq!(first_error("panic(1)"), "type_mismatch");
    assert_eq!(first_error("print(\"abc\".shout())"), "unknown_method");
}

#[test]
fn a_users_variable_can_take_a_library_functions_name() {
    assert_eq!(output("let input = 5\nprint(input)"), "5");
    let report = run_source(
        "test.check",
        "func f() {\n  let panic = 3\n  print(panic)\n}\nf()",
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn built_in_type_names_cant_be_reused_for_classes() {
    assert_eq!(
        first_error("class Int {\n  func toFloat() -> String { \"x\" }\n}"),
        "redefined_builtin"
    );
}

#[test]
fn external_marks_only_functions() {
    assert_eq!(
        first_error("class C {\n  external const k = 1\n}"),
        "unexpected_token"
    );
    assert_eq!(output("let external = 1\nprint(external)"), "1");
}

fn warnings(src: &str) -> Vec<String> {
    diagnostics(src, Severity::Warning)
}

const POINT: &str = "class Point(x: Int, y: Int = 0)\n";

#[test]
fn records_print_compare_and_read_fields() {
    prints("(1, 2)", "(1, 2)");
    prints("(x = 1, y = \"a\")", "(x = 1, y = \"a\")");
    prints("(x = 1, y = 2).y", "2");
    prints("((1, 2), 3).0.1", "2");
    assert_eq!(first_error("print((1, 2).01)"), "leading_zero");
    assert_eq!(
        first_error("let p: (0: Int, 1: Int) = (1, 2)"),
        "unexpected_token"
    );
    prints("(1, 2) == (1, 2)", "true");
    prints("(1 + 2)", "3");
}

#[test]
fn record_types_match_by_field_names_types_and_order() {
    assert_eq!(
        output("let p: (x: Int, y: Int) = (x = 1, y = 2)\nprint(p.y)"),
        "2"
    );
    assert_eq!(output("let p: (Int, String) = (1, \"a\")\nprint(p.1)"), "a");
    assert_eq!(
        first_error("let p: (y: Int, x: Int) = (x = 1, y = 2)"),
        "type_mismatch"
    );
    assert_eq!(
        first_error("let p: (x: Int, Int) = (x = 1, y = 2)"),
        "mixed_fields"
    );
    assert_eq!(first_error("let p = (1, y = 2)"), "mixed_fields");
}

#[test]
fn a_record_literal_builds_the_expected_class_but_a_record_value_never_does() {
    let src = format!(
        "{POINT}func f(p: Point) -> Point {{ p }}\nfunc g() -> Point {{ (4, 5) }}\n\
         print(f((1, 2)))\nprint(f((x = 3)))\nprint(g())"
    );
    assert_eq!(
        output(&src),
        "Point(x = 1, y = 2)\nPoint(x = 3, y = 0)\nPoint(x = 4, y = 5)"
    );
    assert_eq!(
        output("class C(n: Int = 7)\nfunc make() -> C { () }\nprint(make())"),
        "C(n = 7)"
    );
    assert_eq!(
        output(&format!(
            "{POINT}func maybe() -> Point? {{ (1, 2) }}\nprint(maybe())"
        )),
        "Some(Point(x = 1, y = 2))"
    );
    for misuse in [
        "let t = (1, 2)\nlet p: Point = t",
        "let t = (1, 2)\nfunc f(p: Point) { }\nf(t)",
        "func h(t: (Int, Int)) -> Point { t }",
    ] {
        assert_eq!(
            first_error(&format!("{POINT}{misuse}")),
            "record_to_class",
            "{misuse}"
        );
    }
    assert_eq!(first_error("let r = ()"), "type_needed");
}

#[test]
fn record_fields_never_change() {
    assert_eq!(first_error("var t = (1, 2)\nt.0 = 3"), "assign_to_let");
    assert_eq!(first_error("var t = (x = 1)\nt.x = 3"), "assign_to_let");
}

#[test]
fn a_copy_changes_only_the_named_fields() {
    assert_eq!(
        output("let p = (x = 1, y = 2)\nprint((...p, y = 5))\nprint(p)"),
        "(x = 1, y = 5)\n(x = 1, y = 2)"
    );
    let temp = "class Temp {\n  var c: Int\n  var label = \"\"\n  init { this.label = \"{this.c}C\" }\n}\n";
    assert_eq!(
        output(&format!("{temp}print((...Temp(5), c = 7).label)")),
        "7C"
    );
    let shared = "ref class Box(var n: Int)\nlet a = Box(1)\nlet b = (...a, n = 2)\nprint(same(a, b))\nprint(a.n)";
    assert_eq!(output(shared), "false\n1");
    for (src, code) in [
        ("(...p, z = 1)", "unknown_field"),
        ("(...p, x = 5, x = 6)", "wrong_arguments"),
        ("(...p, y = \"a\")", "type_mismatch"),
        ("(...p, 1)", "wrong_arguments"),
        ("(...p, ...p)", "spread_not_first"),
        ("(y = 1, ...p)", "spread_not_first"),
    ] {
        assert_eq!(
            first_error(&format!("let p = (x = 1, y = 2)\nprint({src})")),
            code,
            "{src}"
        );
    }
}

#[test]
fn a_spread_fills_arguments_by_position_or_name() {
    let src = format!(
        "{POINT}func sum(first: Int, ...more: List<Int>) -> Int {{ first + more.length() }}\n\
         print(sum(...(1, 2, 3)))\nprint(Point(...(y = 2, x = 1)))\nprint(Point(...(5, 6)))"
    );
    assert_eq!(output(&src), "3\nPoint(x = 1, y = 2)\nPoint(x = 5, y = 6)");
    for (call, code) in [
        ("Point(1, ...(2, 3))", "spread_not_first"),
        ("Point(...(1, 2, 3))", "wrong_arguments"),
        ("Point(...(x = 1, y = 2), x = 3)", "wrong_arguments"),
        ("Point(...(\"a\", 2))", "type_mismatch"),
        ("Point(...5)", "type_mismatch"),
    ] {
        assert_eq!(
            first_error(&format!("{POINT}print({call})")),
            code,
            "{call}"
        );
    }
}

#[test]
fn patterns_take_records_classes_and_lists_apart() {
    let src = format!(
        "{POINT}func describe(words: List<String>) -> String {{\n  match words {{\n\
         \x20   [] => \"none\"\n    [\"add\", ...rest] => \"add {{rest}}\"\n    [first, ..._, last] => \"{{first}}..{{last}}\"\n    [only] => only\n  }}\n}}\n\
         print(describe([]))\nprint(describe([\"add\", \"a\", \"b\"]))\nprint(describe([\"x\", \"y\", \"z\"]))\nprint(describe([\"x\"]))\n\
         print(match Point(1, 2) {{ Point(0, _) => \"on axis\", Point(x, y) => \"{{x}},{{y}}\" }})\n\
         print(match (1, (2, 3)) {{ (a, (b, c)) => a + b + c }})"
    );
    assert_eq!(output(&src), "none\nadd [\"a\", \"b\"]\nx..z\nx\n1,2\n6");
}

#[test]
fn a_class_rest_field_matches_like_a_list() {
    let src = format!(
        "{POINT}class Polygon(name: String, ...points: List<Point>)\n\
         let tri = Polygon(\"tri\", (0, 0), (1, 0), (0, 1))\n\
         print(match tri {{ Polygon(_, first, ...rest) => \"{{first.x}} {{rest.length()}}\", Polygon(name) => name }})"
    );
    assert_eq!(output(&src), "0 2");
    let copy = format!(
        "{POINT}class Polygon(name: String, ...points: List<Point>)\nlet a = Polygon(\"a\", (0, 0))\n\
         print((...a, points = [(1, 1), (2, 2)]).points.length())"
    );
    assert_eq!(output(&copy), "2");
}

#[test]
fn exhaustiveness_covers_records_classes_and_lists() {
    let cases = [
        ("match (true, 1) { (true, _) => 1 }", Some("(false, _)")),
        ("match Point(1) { Point(0, _) => 1 }", Some("Point(_, _)")),
        ("match [1] { [] => 1, [x] => x }", Some("[_, _, ...]")),
        ("match [\"a\"] { [\"add\", ...rest] => 1 }", Some("[]")),
        ("match [1] { [] => 1, [_, ..._] => 2 }", None),
        ("match [1] { [x, ...rest] => x, [] => 0 }", None),
        ("match [1] { [] | [_, ..._] => 1 }", None),
    ];
    for (m, missing) in cases {
        assert_eq!(
            missing_case(&format!("{POINT}print({m})")).as_deref(),
            missing,
            "{m}"
        );
    }
}

#[test]
fn a_pattern_with_the_wrong_number_of_fields_is_only_an_error() {
    for src in [
        "print(match (true, true, true) { (true, true) => 1, (false, _) => 2 })",
        "enum S { A(x: Int, y: Int) }\nprint(match S.A(1, 2) { A(x) => 1 })",
    ] {
        assert_eq!(first_error(src), "wrong_arguments", "{src}");
    }
}

#[test]
fn let_var_and_for_destructure_with_patterns_that_cant_fail() {
    assert_eq!(output("let (a, b) = (1, 2)\nprint(a + b)"), "3");
    assert_eq!(
        output("var (a, b) = (1, 2)\na = 5\nb = 6\nprint(a + b)"),
        "11"
    );
    assert_eq!(first_error("let (a, b) = (1, 2)\na = 5"), "assign_to_let");
    assert_eq!(
        output("for (name, n) in [(\"a\", 1), (\"b\", 2)] { print(\"{name}{n}\") }"),
        "a1\nb2"
    );
    for refutable in [
        "let Some(x) = Int.parse(\"1\")",
        "let [x] = [1]",
        "for [x] in [[1]] { }",
    ] {
        assert_eq!(
            first_error(refutable),
            "non_exhaustive_match",
            "{refutable}"
        );
    }
}

#[test]
fn a_record_literal_beside_its_class_is_a_style_warning() {
    assert_eq!(
        warnings(&format!("{POINT}let p: Point = (1, 2)")),
        ["record_beside_class"]
    );
    for elsewhere in [
        "func f(p: Point) { }\nf((1, 2))",
        "func g() -> Point { (1, 2) }",
        "let p = Point(1)\nlet q: Point = (...p, y = 2)",
    ] {
        assert!(
            warnings(&format!("{POINT}{elsewhere}")).is_empty(),
            "{elsewhere}"
        );
    }
}

#[test]
fn a_bad_variant_field_is_an_error_not_a_hang() {
    assert_eq!(
        first_error("enum E {\n  A(ref x: Int)\n}"),
        "unexpected_token"
    );
}

#[test]
fn a_list_type_can_be_written_with_brackets_read_left_to_right() {
    assert_eq!(
        output(
            "class Bag(items: Int[] = [])\nlet m: Int?[] = [1, None]\nlet n: Int[]? = None\nlet g: Int[][] = [[1], []]\nprint(Bag().items)\nprint(m)\nprint(n)\nprint(g)"
        ),
        "[]\n[Some(1), None]\nNone\n[[1], []]"
    );
}

#[test]
fn branches_whose_value_is_unused_may_have_different_types() {
    assert_eq!(
        output(
            "enum E {\n  A\n  B\n}\nfor x in [E.A, E.B] {\n  match x {\n    A => print(\"a\")\n    _ => 5\n  }\n  if x == E.A { 1 } else { \"b\" }\n}"
        ),
        "a"
    );
    assert_eq!(
        first_error("let v = match 1 {\n  1 => \"a\"\n  _ => 5\n}"),
        "branch_type_mismatch"
    );
}

/// Runs `files[0]` as the entry file, the others beside it.
fn run_files(files: &[(&str, &str)], allow_private: bool) -> (Report, Vec<u8>) {
    let mem = MemFiles(
        files
            .iter()
            .map(|(p, s)| (p.to_string(), s.to_string()))
            .collect(),
    );
    let mut out = Vec::new();
    let report = run_program(
        files[0].0,
        &mem,
        allow_private,
        &mut Cursor::new(Vec::new()),
        &mut out,
    );
    (report, out)
}

fn files_output(files: &[(&str, &str)]) -> String {
    let (report, out) = run_files(files, false);
    finished(report, out, files[0].1)
}

fn files_error(files: &[(&str, &str)]) -> String {
    let (report, _) = run_files(files, false);
    report
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .unwrap_or_default()
}

const GEO: (&str, &str) = (
    "geo/point.check",
    "namespace geo\n\nclass Point(x: Int, y: Int)\nfunc origin() -> Point { Point(0, 0) }\nprivate func secret() -> Int { 1 }\nfunc one() -> Int { secret() }",
);
const ART: (&str, &str) = (
    "art/point.check",
    "namespace art\n\nclass Point(name: String)\nfunc origin() -> Point { Point(\"o\") }",
);

#[test]
fn an_imported_name_works_unqualified_until_it_clashes() {
    assert_eq!(
        files_output(&[
            (
                "main.check",
                "import \"geo/\"\nprint(origin())\nprint(one())"
            ),
            GEO
        ]),
        "geo.Point(x = 0, y = 0)\n1"
    );
    assert_eq!(
        files_error(&[
            (
                "main.check",
                "import \"geo/\"\nimport \"art/\"\nprint(origin())"
            ),
            GEO,
            ART
        ]),
        "ambiguous_name"
    );
    assert_eq!(
        files_output(&[
            (
                "main.check",
                "import \"geo/\"\nimport \"art/\"\nlet p: art.Point = art.origin()\nprint(p)\nprint(geo.origin() == geo.Point(0, 0))"
            ),
            GEO,
            ART
        ]),
        "art.Point(name = \"o\")\ntrue"
    );
}

#[test]
fn the_files_own_namespace_wins_a_clash() {
    assert_eq!(
        files_output(&[
            (
                "main.check",
                "import \"geo/\"\nfunc origin() -> Int { 7 }\nprint(origin())"
            ),
            GEO
        ]),
        "7"
    );
}

#[test]
fn a_private_name_is_only_visible_inside_its_unit() {
    let main = ("main.check", "import \"geo/\"\nprint(secret())");
    assert_eq!(files_error(&[main, GEO]), "private_name");
    let main = ("main.check", "import private \"geo/\"\nprint(secret())");
    assert_eq!(files_error(&[main, GEO]), "private_import_in_run");
    let (report, out) = run_files(&[main, GEO], true);
    assert_eq!(finished(report, out, main.1), "1");
}

#[test]
fn a_unit_only_sees_its_own_files_and_the_library() {
    let other = ("other/a.check", "func f() -> Int { one() }");
    let main = ("main.check", "import \"geo/\"\nimport \"other/\"\nprint(1)");
    assert_eq!(files_error(&[main, GEO, other]), "unknown_name");
}

#[test]
fn a_local_named_like_a_namespace_wins_with_a_warning() {
    let main = (
        "main.check",
        "import \"geo/\"\nlet geo = (origin = 5)\nprint(geo.origin)",
    );
    let (report, out) = run_files(&[main, GEO], false);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code.to_string() == "shadowed_name")
    );
    assert_eq!(finished(report, out, main.1), "5");
}

#[test]
fn folder_imports_are_shallow_unless_they_end_in_dots() {
    let deep = ("lib/more/b.check", "func b() -> Int { 2 }");
    let top = ("lib/a.check", "func a() -> Int { 1 }");
    let tool = ("lib/tool.check", "print(\"never part of the unit\")");
    assert_eq!(
        files_error(&[
            ("main.check", "import \"lib/\"\nprint(b())"),
            top,
            deep,
            tool
        ]),
        "unknown_name"
    );
    assert_eq!(
        files_output(&[
            ("main.check", "import \"lib/...\"\nprint(a() + b())"),
            top,
            deep,
            tool
        ]),
        "3"
    );
}

#[test]
fn imports_must_name_one_existing_unit() {
    let a = ("lib/a.check", "func a() -> Int { 1 }");
    let entry = ("lib/run.check", "print(1)");
    assert_eq!(
        files_error(&[("main.check", "import \"lib\""), a]),
        "folder_without_slash"
    );
    assert_eq!(
        files_error(&[("main.check", "import \"nope/\""), a]),
        "missing_import"
    );
    assert_eq!(
        files_error(&[("main.check", "import \"lib/run.check\""), a, entry]),
        "imported_entry_file"
    );
    assert_eq!(
        files_error(&[("main.check", "import \"lib/\"\nimport \"lib/a.check\""), a]),
        "overlapping_imports"
    );
}

#[test]
fn a_namespace_line_must_come_first() {
    assert_eq!(
        first_error("func f() {}\nnamespace late"),
        "misplaced_namespace"
    );
}

const COUNTER: &str = "ref class Counter {\n  readonly var count: Int = 0\n  private var step: Int = 1\n  private let log: String[] = []\n}\nimplement Counter {\n  func tick() { this.count += this.step }\n  private func reset() { this.count = 0 }\n}\n";

#[test]
fn a_private_field_or_method_is_only_usable_inside_its_type() {
    assert_eq!(
        output(&format!(
            "{COUNTER}var c = Counter()\nc.tick()\nc.tick()\nprint(c.count)"
        )),
        "2"
    );
    for (code, src) in [
        ("private_member", "let c = Counter()\nprint(c.step)"),
        ("private_member", "var c = Counter()\nc.reset()"),
        ("private_member", "let c = Counter(step = 2)"),
        ("private_member", "let c = (...Counter(), step = 2)"),
    ] {
        assert_eq!(first_error(&format!("{COUNTER}{src}")), code, "{src}");
    }
    assert_eq!(
        first_error("class P {\n  private let x: Int\n}"),
        "private_field_without_default"
    );
}

#[test]
fn a_readonly_var_is_set_at_construction_and_changed_only_by_its_class() {
    assert_eq!(
        output(&format!("{COUNTER}print(Counter(count = 5).count)")),
        "5"
    );
    assert_eq!(output(&format!("{COUNTER}print(Counter(5).count)")), "5");
    for src in [
        "var c = Counter()\nc.count = 3",
        "let c = (...Counter(), count = 2)",
    ] {
        assert_eq!(
            first_error(&format!("{COUNTER}{src}")),
            "readonly_field",
            "{src}"
        );
    }
}

#[test]
fn modifiers_are_ordinary_names_outside_declarations() {
    assert_eq!(
        output("let private = 1\nlet readonly = 2\nprint(private + readonly)"),
        "3"
    );
}

#[test]
fn private_names_in_two_units_dont_clash_but_public_ones_do() {
    let a = (
        "a/a.check",
        "private func helper() -> Int { 1 }\nfunc fromA() -> Int { helper() }",
    );
    let b = (
        "b/b.check",
        "private func helper() -> Int { 2 }\nfunc fromB() -> Int { helper() }",
    );
    let main = (
        "main.check",
        "import \"a/\"\nimport \"b/\"\nprint(fromA() + fromB())",
    );
    assert_eq!(files_output(&[main, a, b]), "3");
    let c = ("c/c.check", "func fromA() -> Int { 9 }");
    let main = ("main.check", "import \"a/\"\nimport \"c/\"\nprint(1)");
    assert_eq!(files_error(&[main, a, c]), "duplicate_name");
}

#[test]
fn a_variant_of_the_files_own_enum_wins_over_an_imported_type() {
    let shapes = ("shapes/s.check", "namespace shapes\n\nclass Circle(r: Int)");
    let main = (
        "main.check",
        "import \"shapes/\"\nenum Shape {\n  Circle(r: Int)\n  Dot\n}\nlet s: Shape = Circle(2)\nmatch s {\n  Circle(r) => print(r)\n  Dot => print(0)\n}\nprint(shapes.Circle(3).r)",
    );
    assert_eq!(files_output(&[main, shapes]), "2\n3");
}

#[test]
fn a_class_pattern_can_be_qualified_by_its_namespace() {
    let main = (
        "main.check",
        "import \"geo/\"\nimport \"art/\"\nmatch geo.origin() {\n  geo.Point(x, y) => print(x + y)\n}",
    );
    assert_eq!(files_output(&[main, GEO, ART]), "0");
    let main = (
        "main.check",
        "import \"geo/\"\nimport \"art/\"\nmatch geo.origin() {\n  Point(x, y) => print(x + y)\n}",
    );
    assert_eq!(files_error(&[main, GEO, ART]), "ambiguous_name");
}

#[test]
fn a_private_const_is_only_readable_inside_its_type() {
    let src = "class P {\n  let x: Int = 1\n  private const secret: Int = 42\n}\n";
    assert_eq!(
        first_error(&format!("{src}print(P.secret)")),
        "private_member"
    );
}

#[test]
fn field_modifiers_are_errors_elsewhere() {
    for src in [
        "func f(private var x: Int) -> Int { x }",
        "class P(x: Int)\nprivate implement P {\n  func f() {}\n}",
    ] {
        assert_eq!(first_error(src), "unexpected_token", "{src}");
    }
    assert_eq!(
        first_error("class P {\n  readonly let x: Int = 1\n}"),
        "readonly_let"
    );
}

#[test]
fn changing_a_readonly_list_from_outside_is_a_readonly_error() {
    let src =
        "ref class Bag {\n  readonly var items: Int[] = []\n}\nlet b = Bag()\nb.items.push(1)";
    assert_eq!(first_error(src), "readonly_field");
}

#[test]
fn an_entry_file_cant_import_itself() {
    assert_eq!(
        files_error(&[("main.check", "import \"main.check\"\nprint(1)")]),
        "imported_entry_file"
    );
}

#[test]
fn library_files_dont_warn_about_a_users_namespace() {
    let text = (
        "text/t.check",
        "namespace text\n\nfunc hi() -> String { \"hi\" }",
    );
    let (report, _) = run_files(
        &[("main.check", "import \"text/\"\nprint(hi())"), text],
        false,
    );
    assert!(report.diagnostics.is_empty());
}

const BLOCK: &str = "enum Sound {\n  Stone\n  Wood\n}\nclass BlockDef(hardness: Float, sound: Sound = Sound.Wood, drops: String[] = [])\n";

#[test]
fn a_data_file_is_a_const_named_after_the_file() {
    let settings = ("game/settings.con", "volume = 0.8\nshowFps = true");
    let stone = (
        "game/stone.con",
        "BlockDef(\n  hardness = 1.5\n  sound = Stone\n)",
    );
    let code = ("game/blocks.check", BLOCK);
    let main = (
        "main.check",
        "import \"game/\"\nprint(settings.volume)\nprint(stone)",
    );
    assert_eq!(
        files_output(&[main, settings, stone, code]),
        "0.8\nBlockDef(hardness = 1.5, sound = Stone, drops = [])"
    );
}

#[test]
fn a_con_file_holds_only_literals() {
    let code = (
        "game/blocks.check",
        "func half(x: Float) -> Float { x / 2.0 }",
    );
    for value in ["half(3.0)", "half", "[1 + 2]"] {
        let data = ("game/bad.con", value);
        let main = ("main.check", "import \"game/\"\nprint(1)");
        assert_eq!(
            files_error(&[main, data, code]),
            "code_in_pure_data",
            "{value}"
        );
    }
    let data = ("game/ok.con", "[(a = -1, b = 2.5, c = \"x\", d = true)]");
    let main = ("main.check", "import \"game/\"\nprint(ok)");
    assert_eq!(
        files_output(&[main, data, code]),
        "[(a = -1, b = 2.5, c = \"x\", d = true)]"
    );
}

#[test]
fn a_conx_file_can_compute_its_value_from_its_unit() {
    let code = (
        "game/blocks.check",
        "func half(x: Float) -> Float { x / 2.0 }",
    );
    let base = ("game/base.con", "hardness = 3.0");
    let soft = ("game/soft.conx", "hardness = half(base.hardness)");
    let main = ("main.check", "import \"game/\"\nprint(soft.hardness)");
    assert_eq!(files_output(&[main, code, base, soft]), "1.5");
}

#[test]
fn data_values_cant_depend_on_each_other_in_a_loop() {
    let code = ("game/blocks.check", "func f() -> Int { a }");
    let a = ("game/a.conx", "b + 1");
    let b = ("game/b.conx", "a + 1");
    let main = ("main.check", "import \"game/\"\nprint(1)");
    assert_eq!(files_error(&[main, code, a, b]), "const_cycle");
    let c = ("game/c.conx", "f() + 1");
    let a = ("game/a.conx", "c + 1");
    let (report, _) = run_files(&[main, code, a, c], false);
    assert!(
        matches!(report.outcome, Outcome::Panicked(_)),
        "a cycle through a call"
    );
}

#[test]
fn data_files_need_valid_names_and_code_beside_them() {
    let code = ("game/blocks.check", "func f() -> Int { 1 }");
    let main = ("main.check", "import \"game/\"\nprint(1)");
    for name in ["game/my-file.con", "game/class.con", "game/Ok.con"] {
        assert_eq!(
            files_error(&[main, code, (name, "1")]),
            "bad_data_name",
            "{name}"
        );
    }
    let only_data = ("blocks/stone.con", "1");
    let main = ("main.check", "import \"blocks/\"\nprint(1)");
    assert_eq!(files_error(&[main, only_data]), "data_folder");
    let main = ("main.check", "import \"blocks/stone.con\"\nprint(stone)");
    assert_eq!(files_output(&[main, only_data]), "1");
}

#[test]
fn a_data_file_can_be_imported_from_another_namespace() {
    let data = ("conf/settings.con", "namespace conf\n\nvolume = 3");
    let main = (
        "main.check",
        "import \"conf/settings.con\"\nprint(conf.settings.volume)\nprint(settings.volume)",
    );
    assert_eq!(files_output(&[main, data]), "3\n3");
}

const GAME_MANIFEST: (&str, &str) = (
    "game/package.con",
    "namespace game\n\nversion = \"0.1.0\"\nentry = \"main.check\"\nimports = [\"src/\", \"../vectors/\"]",
);
const VECTORS: (&str, &str) = (
    "vectors/vec.check",
    "namespace vectors\n\nclass Vec(x: Int, y: Int)\nfunc add(a: Vec, b: Vec) -> Vec { Vec(a.x + b.x, a.y + b.y) }",
);

#[test]
fn a_manifest_names_the_namespace_and_the_units_its_code_sees() {
    let src = (
        "game/src/play.check",
        "func start() -> vectors.Vec { add(Vec(1, 2), Vec(3, 4)) }\nprivate func secret() -> Int { 1 }",
    );
    let main = (
        "main.check",
        "import \"game/package.con\"\nprint(game.start())\nprint(game.package.version)",
    );
    assert_eq!(
        files_output(&[main, GAME_MANIFEST, src, VECTORS]),
        "vectors.Vec(x = 4, y = 6)\n0.1.0"
    );
    let main = ("main.check", "import \"game/package.con\"\nprint(secret())");
    assert_eq!(
        files_error(&[main, GAME_MANIFEST, src, VECTORS]),
        "private_name"
    );
}

#[test]
fn a_folder_import_passes_on_its_manifests_imports() {
    let lib = ("game/play.check", "func start() -> Int { 1 }");
    let manifest = (
        "game/package.con",
        "namespace game\n\nimports = [\"../vectors/\"]",
    );
    let main = (
        "main.check",
        "import \"game/\"\nprint(add(Vec(1, 1), Vec(2, 2)).x)\nprint(start())",
    );
    assert_eq!(files_output(&[main, manifest, lib, VECTORS]), "3\n1");
    let other = ("other/v.check", "namespace other\n\nclass Vec(z: Int)");
    let main = (
        "main.check",
        "import \"game/\"\nimport \"other/\"\nprint(Vec(1))",
    );
    let (report, _) = run_files(&[main, manifest, lib, VECTORS, other], false);
    let clash = report
        .diagnostics
        .iter()
        .find(|d| d.code.to_string() == "ambiguous_name")
        .expect("a clash");
    assert_eq!(
        clash.detail.as_deref(),
        Some("other.Vec, vectors.Vec (via game/)")
    );
}

#[test]
fn a_package_namespace_rules_its_files() {
    let manifest = ("game/package.con", "namespace game");
    let main = ("main.check", "import \"game/\"\nprint(game.f())");
    assert_eq!(
        files_output(&[main, manifest, ("game/a.check", "func f() -> Int { 1 }")]),
        "1"
    );
    assert_eq!(
        files_error(&[
            main,
            manifest,
            ("game/a.check", "namespace other\n\nfunc f() -> Int { 1 }")
        ]),
        "namespace_outside_package"
    );
    let (report, _) = run_files(
        &[
            main,
            manifest,
            ("game/a.check", "namespace game\n\nfunc f() -> Int { 1 }"),
        ],
        false,
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code.to_string() == "repeated_namespace")
    );
}

#[test]
fn manifests_are_checked_as_packages() {
    let main = ("main.check", "import \"game/\"\nprint(1)");
    let code = ("game/a.check", "func f() -> Int { 1 }");
    for (manifest, expected) in [
        ("imports = 3", "type_mismatch"),
        ("imprts = [\"x/\"]", "wrong_arguments"),
        ("imports = [1]", "type_mismatch"),
    ] {
        let m = ("game/package.con", manifest);
        assert_eq!(files_error(&[main, m, code]), expected, "{manifest}");
    }
    let m = ("game/package.con", "hostImport = \"check:playground\"");
    assert_eq!(files_error(&[main, m, code]), "unknown_host");
    let m = ("game/package.con", "imports = [\"../nope/\"]");
    assert_eq!(files_error(&[main, m, code]), "missing_import");
}

#[test]
fn units_load_once_and_cant_import_each_other_in_a_loop() {
    let a = ("a/package.con", "namespace a\n\nimports = [\"../b/\"]");
    let b = ("b/package.con", "namespace b\n\nimports = [\"../a/\"]");
    let code_a = ("a/a.check", "func fa() -> Int { 1 }");
    let code_b = ("b/b.check", "func fb() -> Int { 2 }");
    let main = ("main.check", "import \"a/\"\nprint(1)");
    assert_eq!(files_error(&[main, a, b, code_a, code_b]), "import_cycle");
    let c = ("c/package.con", "namespace c\n\nimports = [\"../d/\"]");
    let code_c = ("c/c.check", "func fc() -> Int { d.fd() }");
    let d = ("d/d.check", "namespace d\n\nfunc fd() -> Int { 4 }");
    let main = (
        "main.check",
        "import \"c/\"\nimport \"d/\"\nprint(fc() + fd())",
    );
    assert_eq!(files_output(&[main, c, code_c, d]), "8");
}

#[test]
fn packages_without_a_namespace_keep_their_package_const_to_themselves() {
    let a = ("a/package.con", "version = \"1\"");
    let b = ("b/package.con", "version = \"2\"");
    let main = (
        "main.check",
        "import \"a/\"\nimport \"b/\"\nprint(fa() + fb())",
    );
    let code_a = ("a/a.check", "func fa() -> String { package.version }");
    let code_b = ("b/b.check", "func fb() -> String { package.version }");
    assert_eq!(files_output(&[main, a, b, code_a, code_b]), "12");
}

#[test]
fn only_path_and_built_in_imports_are_supported() {
    let main = ("main.check", "import \"pkg:geometry@1.2\"\nprint(1)");
    assert_eq!(files_error(&[main]), "unsupported_import");
    let main = (
        "main.check",
        "import \"check:std\"\nimport \"check:cli\"\nprint(1)",
    );
    assert_eq!(files_output(&[main]), "1");
    let main = ("main.check", "import \"check:http\"\nprint(1)");
    assert_eq!(files_error(&[main]), "missing_import");
    let main = ("main.check", "import host \"check:cli\"\nprint(1)");
    assert_eq!(files_output(&[main]), "1");
    let main = ("main.check", "import host \"check:playground\"\nprint(1)");
    assert_eq!(files_error(&[main]), "unknown_host");
}

#[test]
fn deep_imports_skip_subfolders_with_their_own_manifest() {
    let top = ("lib/a.check", "func a() -> Int { 1 }");
    let sub = ("lib/inner/b.check", "func b() -> Int { 2 }");
    let manifest = ("lib/inner/package.con", "namespace inner");
    let main = ("main.check", "import \"lib/...\"\nprint(b())");
    assert_eq!(files_error(&[main, top, sub, manifest]), "unknown_name");
}

#[test]
fn a_folder_runs_its_manifests_entry_file() {
    let mem = |files: &[(&str, &str)]| {
        MemFiles(
            files
                .iter()
                .map(|(p, s)| (p.to_string(), s.to_string()))
                .collect(),
        )
    };
    let ok = mem(&[
        ("game/package.con", "entry = \"main.check\""),
        ("game/main.check", "print(1)"),
    ]);
    assert_eq!(entry_of("game", &ok), Ok("game/main.check".to_string()));
    let no_entry = mem(&[("game/package.con", "version = \"1\"")]);
    assert_eq!(
        entry_of("game/", &no_entry).map_err(|c| c.to_string()),
        Err("no_entry".to_string())
    );
    let missing = mem(&[("game/package.con", "entry = \"run.check\"")]);
    assert_eq!(
        entry_of("game/", &missing).map_err(|c| c.to_string()),
        Err("missing_entry".to_string())
    );
}

#[test]
fn a_manifest_at_the_root_can_list_its_own_folder() {
    let manifest = (
        "package.con",
        "entry = \"main.check\"\nimports = [\"./\", \"src/\"]",
    );
    let main = ("main.check", "import \"package.con\"\nprint(f() + g())");
    let top = ("a.check", "func f() -> Int { 1 }");
    let src = ("src/b.check", "func g() -> Int { 2 }");
    assert_eq!(files_output(&[main, manifest, top, src]), "3");
}

#[test]
fn a_listed_sub_package_manifest_is_its_own_unit() {
    let game = (
        "game/package.con",
        "namespace game\n\nimports = [\"tools/package.con\"]",
    );
    let tools = (
        "game/tools/package.con",
        "namespace tools\n\nimports = [\"./\"]",
    );
    let t = ("game/tools/t.check", "func t() -> Int { 1 }");
    let main = ("main.check", "import \"game/\"\nprint(t())");
    assert_eq!(files_output(&[main, game, tools, t]), "1");
}

#[test]
fn a_data_file_and_a_function_cant_share_a_name_in_either_order() {
    let main = ("main.check", "import \"game/\"\nprint(1)");
    let data = ("game/a.con", "1");
    let func = ("game/b.check", "func a() -> Int { 2 }");
    assert_eq!(files_error(&[main, data, func]), "duplicate_name");
    let data = ("game/z.con", "1");
    let func = ("game/b.check", "func z() -> Int { 2 }");
    assert_eq!(files_error(&[main, data, func]), "duplicate_name");
}

#[test]
fn a_types_own_const_wins_over_a_data_file_inside_the_type() {
    let data = ("game/max.con", "\"data\"");
    let code = (
        "game/b.check",
        "class Foo {\n  let x: Int = 0\n  const max = 5\n  const twice = max * 2\n}",
    );
    let main = (
        "main.check",
        "import \"game/\"\nprint(Foo.twice)\nprint(max)",
    );
    assert_eq!(files_output(&[main, data, code]), "10\ndata");
}

#[test]
fn a_con_file_cant_call_a_static_method() {
    let code = (
        "game/b.check",
        "class Item(n: Int)\nimplement Item {\n  static func make() -> Item { Item(1) }\n}",
    );
    let data = ("game/x.con", "Item.make()");
    let main = ("main.check", "import \"game/\"\nprint(1)");
    assert_eq!(files_error(&[main, code, data]), "code_in_pure_data");
}

#[test]
fn data_values_are_built_in_load_order() {
    let say = (
        "x/say.check",
        "func say(s: String) -> Int {\n  print(s)\n  1\n}",
    );
    let tell = (
        "y/tell.check",
        "func tell(s: String) -> Int {\n  print(s)\n  1\n}",
    );
    let b = ("x/b.conx", "say(\"b\")");
    let a = ("y/a.conx", "tell(\"a\")");
    let main = (
        "main.check",
        "import \"x/\"\nimport \"y/\"\nprint(\"main\")",
    );
    assert_eq!(files_output(&[main, say, b, tell, a]), "b\na\nmain");
}

#[test]
fn import_host_is_written_once_and_only_without_a_manifest() {
    let twice = (
        "main.check",
        "import host \"check:cli\"\nimport host \"check:cli\"\nprint(1)",
    );
    assert_eq!(files_error(&[twice]), "misplaced_host_import");
    let main = ("game/main.check", "import host \"check:cli\"\nprint(1)");
    let manifest = ("game/package.con", "entry = \"main.check\"");
    assert_eq!(files_error(&[main, manifest]), "misplaced_host_import");
}

#[test]
fn running_a_folder_needs_a_manifest() {
    let files = MemFiles([("game/main.check".to_string(), "print(1)".to_string())].into());
    assert_eq!(
        entry_of("game", &files).map_err(|c| c.to_string()),
        Err("missing_manifest".to_string())
    );
}

const SHAPE_INTERFACE: &str = "interface Shape {
  func area() -> Float
  func describe() -> String { \"area {this.area()}\" }
}
class Circle: Shape {
  let r: Float
  func area() -> Float { 3.0 * this.r * this.r }
}
class Square(side: Float)
implement Shape for Square {
  func area() -> Float { this.side * this.side }
  func describe() -> String { \"square\" }
}
";

fn shapes_output(src: &str) -> String {
    output(&format!("{SHAPE_INTERFACE}{src}"))
}

fn shapes_error(src: &str) -> String {
    first_error(&format!("{SHAPE_INTERFACE}{src}"))
}

#[test]
fn a_generic_function_infers_its_type_from_the_arguments_or_the_expected_type() {
    let src = "func first<T>(xs: T[]) -> T? { if xs.isEmpty() { None } else { xs[0] } }
func none<T>() -> T? { None }
print(first([\"a\", \"b\"]))
let n: Int? = none()
print(n ?? 7)";
    assert_eq!(output(src), "Some(\"a\")\n7");
    assert_eq!(
        first_error("func empty<T>() -> T[] { [] }\nlet e = empty()"),
        "type_needed"
    );
}

#[test]
fn a_generic_class_and_enum_work_through_construction_fields_methods_and_patterns() {
    let src = "class Box<T>(value: T)
implement Box<T> { func get() -> T { this.value } }
implement Box<Int> { func doubled() -> Int { this.value * 2 } }
enum Tree<T> {
  Leaf
  Node(Tree<T>, T, Tree<T>)
}
func size<T>(t: Tree<T>) -> Int {
  match t {
    Leaf => 0
    Node(l, _, r) => size(l) + 1 + size(r)
  }
}
let b = Box(Box(4))
print(b.value.get())
print(Box(5).doubled())
let Box(inner) = Box(\"hi\")
print(inner)
let boxes: Box<Int>[] = [Box(1), Box(2)]
print(boxes.map(func(x) => x.get()))
let t: Tree<String> = Node(Leaf, \"a\", Node(Leaf, \"b\", Leaf))
print(size(t))";
    assert_eq!(output(src), "4\n10\nhi\n[1, 2]\n2");
    let added = "class Box<T>(value: T)\nimplement Box<Int> { func doubled() -> Int { 2 } }\n";
    assert_eq!(
        first_error(&format!("{added}print(Box(\"a\").doubled())")),
        "unknown_method"
    );
    assert_eq!(
        first_error("class Box<T>(value: T)\nlet b: Box = Box(1)"),
        "wrong_type_arguments"
    );
}

#[test]
fn a_type_argument_must_implement_every_interface_its_parameter_names() {
    let src = "func show<T: Shape + Printable>(x: T) -> String { \"{x.toString()} {x.area()}\" }
class Dot: Shape + Printable {
  let r: Float
  func area() -> Float { 0.0 }
  func toString() -> String { \"dot\" }
}
print(show(Dot(1.0)))";
    assert_eq!(shapes_output(src), "dot 0.0");
    assert_eq!(
        shapes_error(
            "func show<T: Shape + Printable>(x: T) -> Float { x.area() }\nshow(Circle(1.0))"
        ),
        "unmet_constraint"
    );
    assert_eq!(
        first_error("func f<T>(x: T) -> Int { x.size() }"),
        "unknown_method"
    );
    assert_eq!(
        first_error("func f<T>(x: T) -> Int { T.zero() }"),
        "unknown_name"
    );
}

#[test]
fn an_interface_type_holds_different_classes_and_calls_each_ones_method() {
    let src = "let shapes: Shape[] = [Circle(1.0), Square(2.0)]
for s in shapes { print(s.describe()) }
func total(xs: Shape[]) -> Float {
  var sum = 0.0
  for x in xs { sum += x.area() }
  sum
}
let circles = [Circle(1.0), Circle(1.0)]
print(total(circles))";
    assert_eq!(shapes_output(src), "area 3.0\nsquare\n6.0");
    assert_eq!(
        shapes_error("let xs = [Circle(1.0), Square(2.0)]"),
        "list_needs_type"
    );
}

#[test]
fn a_callee_changing_an_interface_list_leaves_the_callers_list_alone() {
    let src = "func grow(var xs: Shape[]) -> Int {
  xs.push(Square(1.0))
  xs.length()
}
let circles = [Circle(1.0)]
print(grow(circles))
print(circles.length())";
    assert_eq!(shapes_output(src), "2\n1");
}

#[test]
fn an_interface_can_require_fields_that_its_defaults_use() {
    let src = "interface Named {
  let name: String
  func greet() -> String { \"hi {this.name}\" }
}
class Player: Named { let name: String }
func hello(n: Named) -> String { n.name }
print(Player(\"ana\").greet())
print(hello(Player(\"bo\")))";
    assert_eq!(output(src), "hi ana\nbo");
    let named = "interface Named { let name: String }\n";
    assert_eq!(
        first_error(&format!("{named}enum E: Named {{ A }}")),
        "field_requirement_needs_class"
    );
    assert_eq!(
        first_error(&format!(
            "{named}class P: Named {{ private let name: String = \"x\" }}"
        )),
        "missing_implementation"
    );
}

#[test]
fn a_type_must_provide_what_its_interfaces_require_with_matching_types() {
    assert_eq!(
        shapes_error("class Blob: Shape { let r: Float }"),
        "missing_implementation"
    );
    assert_eq!(
        shapes_error("class Blob: Shape {\n  let r: Int\n  func area() -> Int { this.r }\n}"),
        "wrong_implementation"
    );
    assert_eq!(
        first_error("class A(x: Int)\nclass B: A { let y: Int }"),
        "not_an_interface"
    );
    assert_eq!(
        shapes_error("interface Round: Shape {}\nimplement Shape for Round {}"),
        "unknown_name"
    );
}

#[test]
fn two_parents_defaults_conflict_until_the_type_overrides_them() {
    let parents = "interface A { func hi() -> String { \"a\" } }
interface B { func hi() -> String { \"b\" } }
";
    assert_eq!(
        first_error(&format!("{parents}class C: A + B {{ let x: Int }}")),
        "conflicting_defaults"
    );
    let src = format!(
        "{parents}class C: A + B {{\n  let x: Int\n  func hi() -> String {{ \"c\" }}\n}}\nprint(C(1).hi())"
    );
    assert_eq!(output(&src), "c");
}

#[test]
fn a_built_in_type_can_implement_an_interface() {
    let src = "implement Shape for Int { func area() -> Float { this.toFloat() } }
let shapes: Shape[] = [3, Circle(1.0)]
for s in shapes { print(s.describe()) }";
    assert_eq!(shapes_output(src), "area 3.0\narea 3.0");
}

#[test]
fn an_interface_is_implemented_only_for_a_whole_generic_type() {
    let src = "class Box<T>(v: T)
implement Shape for Box<T> { func area() -> Float { 1.5 } }
let shapes: Shape[] = [Box(1), Box(\"a\")]
for s in shapes { print(s.area()) }";
    assert_eq!(shapes_output(src), "1.5\n1.5");
    assert_eq!(
        shapes_error(
            "class Box<T>(v: T)\nimplement Shape for Box<Int> { func area() -> Float { 1.0 } }"
        ),
        "implement_for_specific_arguments"
    );
}

#[test]
fn printable_values_print_and_interpolate_through_to_string() {
    let src = "class P: Printable {
  let n: Int
  func toString() -> String { \"P{this.n}\" }
}
print(P(1))
print(\"got {P(2)}\")
print([P(3), P(4)])
let m: P? = Some(P(5))
print(m)";
    assert_eq!(output(src), "P1\ngot P2\n[P3, P4]\nSome(P5)");
}

#[test]
fn eq_lets_a_ref_class_or_a_t_use_double_equals() {
    let src = "ref class Q: Eq { let x: Int }
func same<T: Eq>(a: T, b: T) -> Bool { a == b }
print(Q(1) == Q(1))
print(Q(1) == Q(2))
print(same(Q(3), Q(3)))
print(same(\"a\", \"b\"))";
    assert_eq!(output(src), "true\nfalse\ntrue\nfalse");
    assert_eq!(
        first_error("ref class P { let x: Int }\nprint(P(1) == P(1))"),
        "invalid_operand_types"
    );
    assert_eq!(
        first_error("func f<T>(a: T, b: T) -> Bool { a == b }"),
        "invalid_operand_types"
    );
    assert_eq!(
        first_error("ref class P { let x: Int }\nfunc f<T: Eq>(a: T) -> Bool { a == a }\nf(P(1))"),
        "unmet_constraint"
    );
}

#[test]
fn maybe_and_result_are_generic_enums_from_the_standard_library() {
    let src = "func orDefault<T>(x: T?, d: T) -> T { x ?? d }
implement Maybe<T> { func isSet() -> Bool { this is Some(_) } }
func double(n: Int) -> Result<Int, String> {
  if n < 5 { Ok(n * 2) } else { Err(\"too big\") }
}
print(orDefault(None, 3))
let m: Int? = Some(4)
print(m.isSet())
for n in [4, 5] {
  match double(n) {
    Ok(v) => print(\"ok {v}\")
    Err(e) => print(\"err {e}\")
  }
}";
    assert_eq!(output(src), "3\ntrue\nok 8\nerr too big");
}

#[test]
fn one_interface_implemented_for_one_type_in_two_units_is_an_error() {
    let other = (
        "other/impl.check",
        "namespace other\n\nimplement Printable for vectors.Vec { func toString() -> String { \"a\" } }",
    );
    let manifest = (
        "other/package.con",
        "namespace other\n\nimports = [\"../vectors/\"]",
    );
    let main = (
        "main.check",
        "import \"other/\"\nimport \"vectors/\"\nimplement Printable for Vec { func toString() -> String { \"b\" } }",
    );
    assert_eq!(
        files_error(&[main, manifest, other, VECTORS]),
        "duplicate_name"
    );
}

#[test]
fn any_method_or_field_default_can_call_an_interface_default() {
    let src = "interface Named {
  func id() -> String
  func label() -> String { \"<{this.id()}>\" }
}
class A: Named {
  func id() -> String { \"a\" }
  func shown() -> String { this.label() }
}
class Holder { let text: String = A().label() }
print(A().shown())
print(Holder().text)";
    assert_eq!(output(src), "<a>\n<a>");
}

#[test]
fn an_implement_can_name_the_types_parameters_its_own_way() {
    let src = "interface Container<E> { func get() -> E }
class Box<T> { let v: T }
implement Container<U> for Box<U> { func get() -> U { this.v } }
func unwrap(c: Container<Int>) -> Int { c.get() }
print(unwrap(Box(3)))";
    assert_eq!(output(src), "3");
    let twice = "interface Container<E> { func get() -> E }
class Box<T>: Container<T> {
  let v: T
  func get() -> T { this.v }
}
implement Container<U> for Box<U> {}";
    assert_eq!(first_error(twice), "duplicate_name");
}

#[test]
fn methods_added_for_different_type_arguments_can_share_a_name() {
    let src = "class Box<T>(v: T)
implement Box<T> { func describe() -> String { \"any\" } }
implement Box<Int> { func describe() -> String { \"int {this.v}\" } }
implement Box<String> { func describe() -> String { \"text {this.v}\" } }
print(Box(1).describe())
print(Box(\"a\").describe())
print(Box(true).describe())";
    assert_eq!(output(src), "int 1\ntext a\nany");
}

#[test]
fn double_equals_looks_through_nested_generics_for_ref_classes() {
    let src = "ref class Node { let x: Int }\nclass Box<T>(v: T)\nlet n = Node(1)\n";
    assert_eq!(
        first_error(&format!("{src}print(Box(Box(n)) == Box(Box(n)))")),
        "invalid_operand_types"
    );
}

#[test]
fn eq_objects_that_reach_themselves_compare_without_looping() {
    let src = "ref class Node: Eq {
  let id: Int
  var next: Node? = None
}
let a = Node(1)
let b = Node(1)
a.next = Some(a)
b.next = Some(b)
print(a == b)
print(a == Node(1))";
    assert_eq!(output(src), "true\nfalse");
}

#[test]
fn a_declared_maybe_list_can_hold_different_classes() {
    assert_eq!(
        shapes_output("let xs: Shape[]? = [Circle(1.0), Square(2.0)]\nprint((xs ?? []).length())"),
        "2"
    );
}

#[test]
fn an_interface_field_has_only_a_name_and_a_type() {
    assert_eq!(
        first_error("interface Named { let name: String = \"x\" }"),
        "interface_field_extras"
    );
    assert_eq!(
        first_error("interface Named { private let name: String }"),
        "interface_field_extras"
    );
}

#[test]
fn printable_values_print_through_to_string_inside_other_values() {
    let src = "class P: Printable {
  let n: Int
  func toString() -> String { \"P{this.n}\" }
}
class Box(v: P)
enum E { Has(P) }
print(Box(P(1)))
print(E.Has(P(2)))
print((a = P(3), b = \"s\"))";
    assert_eq!(output(src), "Box(v = P1)\nHas(P2)\n(a = P3, b = \"s\")");
}

const AGES: &str = "var ages = [\"ana\" = 31, \"bo\" = 27]\n";

#[test]
fn a_map_literal_prints_compares_and_copies_like_a_value() {
    let src = format!(
        "{AGES}let copy = ages\nages[\"cy\"] = 40\nprint(copy)\nprint(ages)\nlet none: Map<String, Int> = [=]\nprint(none)\nprint([\"a\" = 1, \"b\" = 2] == [\"b\" = 2, \"a\" = 1])"
    );
    assert_eq!(
        output(&src),
        "[\"ana\" = 31, \"bo\" = 27]\n[\"ana\" = 31, \"bo\" = 27, \"cy\" = 40]\n[=]\ntrue"
    );
    let unused = "var m = [=]\nprint(m)\nvar s = Set([])\nprint(s)";
    assert_eq!(output(unused), "[=]\nSet([])");
}

#[test]
fn a_map_key_can_be_any_expression() {
    let src = "func f(i: Int) => i\nlet a = 1\nprint([f(1) = \"f\", (a + 1) = \"sum\"])\nprint([(a == 2) = \"no\"])";
    assert_eq!(output(src), "[1 = \"f\", 2 = \"sum\"]\n[false = \"no\"]");
}

#[test]
fn map_lookups_writes_and_removals_keep_insertion_order() {
    let src = format!(
        "{AGES}print(ages[\"cy\"])\nages[\"ana\"] = 32\nages[\"bo\"] += 1\nprint(ages.remove(\"ana\"))\nages[\"ana\"] = 1\nprint(ages.keys())\nprint(ages.values())\nprint(\"{{ages.length()}} {{ages.isEmpty()}} {{ages.contains(\"bo\")}}\")"
    );
    assert_eq!(
        output(&src),
        "None\nSome(32)\n[\"bo\", \"ana\"]\n[28, 1]\n2 false true"
    );
    panics_with(&format!("{AGES}ages[\"cy\"] += 1"), "missing_key");
}

#[test]
fn contains_narrows_a_lookup_while_neither_map_nor_key_changes() {
    let narrowed = format!("{AGES}let k = \"bo\"\nif ages.contains(k) {{ print(ages[k] + 1) }}");
    assert_eq!(output(&narrowed), "28");
    let key_changes = format!(
        "{AGES}var k = \"bo\"\nif ages.contains(k) {{\n    k = \"zz\"\n    print(ages[k] + 1)\n}}"
    );
    assert_eq!(first_error(&key_changes), "invalid_operand_types");
    let map_changes = format!(
        "{AGES}let k = \"bo\"\nif ages.contains(k) {{\n    ages.remove(k)\n    print(ages[k] + 1)\n}}"
    );
    assert_eq!(first_error(&map_changes), "invalid_operand_types");
    for shadowed in [
        "if true { let k = \"zz\"\n print(ages[k] + 1) }",
        "for k in [\"zz\"] { print(ages[k] + 1) }",
    ] {
        let src = format!("{AGES}let k = \"bo\"\nif ages.contains(k) {{ {shadowed} }}");
        assert_eq!(first_error(&src), "invalid_operand_types");
    }
}

#[test]
fn for_walks_maps_sets_and_enumerated_lists_in_order() {
    let src = format!(
        "{AGES}for (name, age) in ages {{ print(\"{{name}} {{age}}\") }}\nfor t in Set([\"b\", \"a\", \"b\"]) {{ print(t) }}\nfor (i, x) in [\"x\", \"y\"].enumerate() {{ print(\"{{i}} {{x}}\") }}"
    );
    assert_eq!(output(&src), "ana 31\nbo 27\nb\na\n0 x\n1 y");
}

#[test]
fn a_set_keeps_one_of_each_item() {
    let src = "var tags = Set([\"red\", \"blue\", \"red\"])\ntags.add(\"green\")\ntags.add(\"red\")\nprint(tags)\nprint(tags.remove(\"blue\"))\nprint(tags.remove(\"blue\"))\nprint(\"{tags.contains(\"red\")} {tags.length()} {tags.isEmpty()}\")";
    assert_eq!(
        output(src),
        "Set([\"red\", \"blue\", \"green\"])\ntrue\nfalse\ntrue 2 false"
    );
}

#[test]
fn maps_and_sets_work_as_parameters_fields_and_type_names() {
    let src = "class Inv(items: Map<String, Int>, tags: Set<String>)\nfunc total(m: Map<String, Int>) -> Int {\n    var t = 0\n    for (_, v) in m { t += v }\n    t\n}\nlet inv = Inv([\"a\" = 2, \"b\" = 3], Set([]))\nprint(total(inv.items))\nprint(inv)";
    assert_eq!(
        output(src),
        "5\nInv(items = [\"a\" = 2, \"b\" = 3], tags = Set([]))"
    );
    assert_eq!(first_error("class Set {}"), "redefined_builtin");
}

#[test]
fn map_keys_and_set_items_need_exact_equality() {
    assert_eq!(first_error("let m = [1.5 = \"a\"]"), "invalid_key_type");
    assert_eq!(
        first_error("let m: Map<Float, Int> = [=]"),
        "invalid_key_type"
    );
    assert_eq!(
        first_error("ref class R { var n = 0 }\nlet s = Set([R()])"),
        "invalid_key_type"
    );
    assert_eq!(
        first_error("enum E { A, B(Float) }\nlet s = Set([E.A])"),
        "invalid_key_type"
    );
    let generic = "func f<K, V>(m: Map<K, V>) -> Int { m.length() }\nprint(f([\"a\" = 1]))\n";
    assert_eq!(output(generic), "1");
    assert_eq!(
        first_error(&format!("{generic}print(f([1.5 = 1]))")),
        "invalid_key_type"
    );
    let record_keys = "class P(x: Int, y: Int)\nprint(Set([P(1, 2), P(1, 2)]))";
    assert_eq!(output(record_keys), "Set([P(x = 1, y = 2)])");
    for made in [
        "class Box<K>(m: Map<K, Int>)\nprint(Box([1.5 = 1]))",
        "enum E<K> { A(s: Set<K>), B }\nprint(E.A(Set([1.5])))",
        "func f<K>(k: K) -> Int {\n    let s: Set<K> = Set([k])\n    s.length()\n}\nprint(f(1.5))",
    ] {
        assert_eq!(first_error(made), "invalid_key_type");
    }
    let body_use = "func uniq<T>(xs: T[]) -> Int { Set(xs).length() }\nprint(uniq([1, 1, 2]))\n";
    assert_eq!(output(body_use), "2");
    assert_eq!(
        first_error(&format!("{body_use}print(uniq([1.5]))")),
        "invalid_key_type"
    );
}

#[test]
fn a_generic_type_checks_its_bounds_where_a_value_is_made() {
    assert_eq!(
        first_error(
            "interface Named { func name() -> String }\nclass Box<T: Named>(x: T)\nprint(Box(1).x)"
        ),
        "unmet_constraint"
    );
}

#[test]
fn a_let_map_or_set_never_changes() {
    assert_eq!(
        first_error("let s = Set([1])\ns.add(2)"),
        "change_let_collection"
    );
    assert_eq!(
        first_error("let m = [\"a\" = 1]\nprint(m.remove(\"a\"))"),
        "change_let_collection"
    );
    assert_eq!(
        first_error("let m = [\"a\" = 1]\nm[\"b\"] = 2"),
        "assign_to_let"
    );
    let own_add = "ref class Bag {\n    var n = 0\n    func add(x: Int) { this.n += x }\n}\nlet b = Bag()\nb.add(2)\nprint(b.n)";
    assert_eq!(output(own_add), "2");
    assert_eq!(first_error("let m = [\"a\" = 1, 2]"), "unexpected_token");
}

#[test]
fn a_data_file_can_hold_a_map() {
    assert_eq!(
        files_output(&[
            (
                "main.check",
                "import \"shop.con\"\nprint(shop.prices[\"pear\"])"
            ),
            ("shop.con", "prices = [\"apple\" = 1, \"pear\" = 2]"),
        ]),
        "Some(2)"
    );
}

#[test]
fn replace_changes_every_match_unless_given_a_count() {
    let src = "print(\"a-b-c\".replace(\"-\", \"+\"))\nprint(\"a-b-c\".replace(\"-\", \"+\", 1))\nprint(\"a-b-c\".replace(\"-\", \"+\", count = 0))";
    assert_eq!(output(src), "a+b+c\na+b-c\na-b-c");
}

#[test]
fn braces_in_a_con_string_are_plain_text() {
    assert_eq!(
        files_output(&[
            ("main.check", "import \"./\"\nprint(lines.hello)"),
            ("lines.con", "hello = \"Hi, {name}!\""),
        ]),
        "Hi, {name}!"
    );
    assert_eq!(
        files_output(&[
            ("main.check", "import \"./\"\nprint(lines)"),
            ("lines.conx", "\"{1 + 1}\""),
        ]),
        "2"
    );
}

#[test]
fn a_body_is_braces_or_an_arrow_and_one_line() {
    for (src, expected) in [
        ("func double(x: Int) => x * 2\nprint(double(2))", "4"),
        ("func one() -> Int { 1 }\nprint(one())", "1"),
        ("print([1, 2].map(func(x) => x * 2))", "[2, 4]"),
        ("print([1].map(func(x) -> Int { x + 1 }))", "[2]"),
        (
            "func sign(n: Int) => if n < 0 { -1 } else { 1 }\nprint(sign(-3))",
            "-1",
        ),
        (
            "func name(n: Int) => match n {\n  0 => \"zero\"\n  _ => \"many\"\n}\nprint(name(0))",
            "zero",
        ),
        (
            "func big(n: Int) => n > 1\nmatch 2 {\n  x if big(x) { print(\"big\") }\n  _ => print(\"small\")\n}",
            "big",
        ),
        (
            "let y = 2\nmatch 2 {\n  x if x == y { print(\"same\") }\n  _ => print(\"no\")\n}",
            "same",
        ),
        (
            "match 1 { 1 { print(\"one\") }, _ => print(\"other\") }",
            "one",
        ),
    ] {
        assert_eq!(output(src), expected, "{src}");
    }
    for (src, code) in [
        ("func f(x: Int) => { x }", "arrow_before_braces"),
        ("let f = func(x: Int) => { x }", "arrow_before_braces"),
        (
            "match 1 {\n  1 => { print(1) }\n  _ => print(2)\n}",
            "arrow_before_braces",
        ),
        ("func f() -> Int => 1", "arrow_without_braces"),
        ("func f() -> Int 1", "arrow_without_braces"),
        ("func f(x: Int) x * 2", "body_without_arrow"),
        (
            "enum S { Square }\nmatch S.Square { Square 1.0 }",
            "body_without_arrow",
        ),
        ("func f(x: Int)\n  => x", "arrow_on_new_line"),
        (
            "let a = 1\nlet b = 2\nif a = b { print(1) }",
            "assignment_in_condition",
        ),
        (
            "let a = 1\nmatch 1 {\n  x if x = a { print(x) }\n  _ => print(0)\n}",
            "assignment_in_condition",
        ),
    ] {
        assert_eq!(first_error(src), code, "{src}");
    }
}

#[test]
fn an_arrow_line_means_that_line_in_braces() {
    for (arrow, braces) in [
        (
            "match 1 {\n  1 => m[\"a\"] = 5\n  _ => print(0)\n}",
            "match 1 {\n  1 { m[\"a\"] = 5 }\n  _ => print(0)\n}",
        ),
        (
            "func bump() => n += 1\nbump()",
            "func bump() { n += 1 }\nbump()",
        ),
        (
            "n = [1, 2].map(func(x) => x * 10)[1] ?? 0",
            "n = [1, 2].map(func(x) { x * 10 })[1] ?? 0",
        ),
        ("func f() => let x = 1\nf()", "func f() { let x = 1 }\nf()"),
    ] {
        let start = "var n = 0\nvar m = [\"a\" = 1]\n";
        let end = "\nprint(\"{n} {m}\")";
        assert_eq!(
            output(&format!("{start}{arrow}{end}")),
            output(&format!("{start}{braces}{end}"))
        );
    }
    assert_eq!(
        first_error("match 1 {\n  1 => print(1); print(2)\n  _ => print(3)\n}"),
        "semicolon_in_arrow_body"
    );
    assert_eq!(output("let f = func(x: Int) => x * 2; print(f(1))"), "2");
}

#[test]
fn bad_arms_are_reported_and_parsing_goes_on() {
    assert_eq!(
        diagnostics(
            "match 1 {\n  1 => { print(1) }\n  2 => { print(2) }\n  _ => print(3)\n}",
            Severity::Error
        ),
        ["arrow_before_braces", "arrow_before_braces"]
    );
}

#[test]
fn var_in_a_pattern_binds_a_copy_that_can_change() {
    let src = "enum Shape { Circle(r: Float), Square }
let s = Shape.Circle(1.0)
match s {
  Circle(var r) {
    r += 1.0
    print(r)
  }
  Square => print(\"square\")
}
print(s)
let m: Int? = Some(3)
if m is Some(var v) {
  v += 1
  print(v)
}
for var x in [1, 2] {
  x *= 10
  print(x)
}
for (k, var v) in [\"a\" = 1] {
  v += 1
  print(\"{k} {v}\")
}
match [1, 2, 3] {
  [first, ...var rest] {
    rest.push(first)
    print(rest)
  }
  _ => print(\"other\")
}";
    assert_eq!(
        output(src),
        "2.0\nCircle(r = 1.0)\n4\n10\n20\na 2\n[2, 3, 1]"
    );
    for (src, code) in [
        (
            "let p = (1, 2)\nlet (var a, b) = p",
            "var_in_declaration_pattern",
        ),
        ("match 1 { var _ => print(1) }", "unexpected_token"),
        (
            "enum E { A(n: Int), B(n: Int) }\nmatch E.A(1) { A(var n) | B(n) => print(n) }",
            "or_pattern_bindings",
        ),
        (
            "let m: Int? = Some(1)\nif m is Some(v) { v = 2 }",
            "assign_to_let",
        ),
        (
            "enum C { Red, Blue }\nmatch C.Red { var Red => print(1), _ => print(2) }",
            "unexpected_token",
        ),
        (
            "let m: Int? = None\nmatch m { var None => print(1), _ => print(2) }",
            "unexpected_token",
        ),
    ] {
        assert_eq!(first_error(src), code, "{src}");
    }
    for unchanged in [
        "let m: Int? = Some(1)\nif m is Some(var v) { print(v) }",
        "let m: Int? = Some(1)\nmatch m {\n  Some(var v) => print(v)\n  None => print(0)\n}",
        "for var x in [1] { print(x) }",
    ] {
        assert_eq!(warnings(unchanged), ["unused_var"], "{unchanged}");
    }
}

const SWAP: &str = "func swap(ref a: Int, ref b: Int) {\n  let t = a\n  a = b\n  b = t\n}\n";
const SET_NINE: &str = "func setNine(ref a: Int) { a = 9 }\n";

#[test]
fn holders_of_one_ref_see_each_others_changes() {
    let src = "class P(var x: Int)\nlet a = Ref([1])\nlet b = a\nb.push(2)\na[0] = 5\nlet p = Ref(P(1))\nlet q = p\nq.x = 3\nprint(\"{a} {p.x}\")";
    assert_eq!(output(src), "Ref([5, 2]) 3");
}

#[test]
fn a_refs_value_is_a_snapshot_and_assigning_it_reaches_every_holder() {
    let src = "let a = Ref([1])\nlet b = a\nvar copy = a.value\ncopy.push(2)\nprint(\"{a}\")\nb.value = [9]\nprint(\"{a}\")";
    assert_eq!(output(src), "Ref([1])\nRef([9])");
}

#[test]
fn same_tells_one_ref_from_two_equal_ones() {
    let src = "let a = Ref(1)\nlet b = a\nprint(same(a, b))\nprint(same(a, Ref(1)))";
    assert_eq!(output(src), "true\nfalse");
    assert_eq!(
        first_error("let a = Ref(1)\nprint(a == a)"),
        "invalid_operand_types"
    );
    assert_eq!(
        first_error("let m: Map<Ref<Int>, Int> = [=]"),
        "invalid_key_type"
    );
}

#[test]
fn a_ref_prints_inside_a_list() {
    assert_eq!(output("let xs = [Ref(1)]\nprint(\"{xs}\")"), "[Ref(1)]");
}

#[test]
fn a_ref_prints_its_value_through_to_string() {
    let src = "class Boom: Printable {\n  func toString() -> String { \"boom\" }\n}\nprint(\"{Ref(Boom())}\")";
    assert_eq!(output(src), "Ref(boom)");
}

#[test]
fn a_ref_of_a_ref_class_is_a_warning() {
    let src = "ref class P(var n: Int)\nlet r = Ref(P(1))";
    assert_eq!(warnings(src), ["ref_of_ref_class"]);
}

#[test]
fn a_ref_parameter_changes_the_callers_variable_or_path() {
    let src = format!(
        "{SET_NINE}class Game(var scores: List<Int>)\nvar x = 1\nvar g = Game([1])\nvar l = [[1], [2]]\nsetNine(ref x)\nsetNine(ref g.scores[0])\nsetNine(ref l[1][0])\nprint(\"{{x}} {{g}} {{l}}\")"
    );
    assert_eq!(output(&src), "9 Game(scores = [9]) [[1], [9]]");
}

#[test]
fn a_ref_parameter_writes_back_through_return_and_further_refs() {
    let src = "func inner(ref a: Int) { a += 1 }\nfunc outer(ref a: Int) {\n  inner(ref a)\n  if a > 0 { return }\n  a = 0\n}\nvar x = 1\nouter(ref x)\nprint(x)";
    assert_eq!(output(src), "2");
}

#[test]
fn a_ref_path_is_resolved_once_and_can_go_through_a_let_ref() {
    let src = format!(
        "{SET_NINE}class P(var x: Int)\nvar n = 0\nfunc next() -> Int {{\n  n += 1\n  0\n}}\nvar xs = [1]\nsetNine(ref xs[next()])\nlet r = Ref(P(1))\nsetNine(ref r.x)\nprint(\"{{xs}} {{n}} {{r.x}}\")"
    );
    assert_eq!(output(&src), "[9] 1 9");
}

#[test]
fn a_named_ref_argument_puts_ref_after_the_name() {
    let src = "func f(n: Int, ref a: Int) { a = n }\nvar x = 1\nf(a = ref x, n = 3)\nprint(x)";
    assert_eq!(output(src), "3");
}

#[test]
fn a_ref_argument_must_be_a_var_or_a_path_in_one() {
    assert_eq!(
        first_error(&format!("{SET_NINE}let x = 1\nsetNine(ref x)")),
        "assign_to_let"
    );
    assert_eq!(
        first_error(&format!(
            "{SET_NINE}class P(x: Int)\nvar p = P(1)\nsetNine(ref p.x)"
        )),
        "assign_to_let"
    );
    assert_eq!(
        first_error(&format!("{SET_NINE}var x = 1\nsetNine(ref x + 1)")),
        "assign_to_let"
    );
    assert_eq!(
        first_error("func f(ref a: Int = 1) { a = 2 }"),
        "ref_default"
    );
    assert_eq!(first_error("func f(ref ...a: Int) {}"), "unexpected_token");
}

#[test]
fn overlapping_arguments_are_an_error_but_distinct_fields_are_fine() {
    assert_eq!(
        first_error(&format!("{SWAP}var x = 1\nswap(ref x, ref x)")),
        "overlapping_ref"
    );
    assert_eq!(
        first_error(&format!(
            "{SWAP}var xs = [0, 0]\nvar i = 0\nswap(ref xs[i], ref i)"
        )),
        "overlapping_ref"
    );
    assert_eq!(
        first_error(
            "func f(ref a: List<Int>, n: Int) { a.push(n) }\nvar xs = [1]\nf(ref xs, xs.length())"
        ),
        "overlapping_ref"
    );
    assert_eq!(
        first_error("var xs = [1]\nxs.push(ref xs[0])"),
        "overlapping_ref"
    );
    let src = format!(
        "{SWAP}class P(var x: Int, var y: Int)\nvar p = P(1, 2)\nswap(ref p.x, ref p.y)\nprint(\"{{p}}\")"
    );
    assert_eq!(output(&src), "P(x = 2, y = 1)");
    let src = "func f(ref a: Int, b: Int) { a = b }\nclass P(var x: Int, var y: Int)\nvar p = P(1, 2)\nvar xs = [5]\nf(ref p.x, p.y)\nf(ref xs[p.x - 2], p.y)\nprint(\"{p} {xs}\")";
    assert_eq!(output(src), "P(x = 2, y = 2) [2]");
}

#[test]
fn a_ref_index_out_of_range_panics_at_the_call() {
    panics_with(
        &format!("{SET_NINE}var xs = [1]\nsetNine(ref xs[5])"),
        "index_out_of_range",
    );
}

#[test]
fn ref_must_match_the_parameter() {
    assert_eq!(
        first_error(&format!("{SET_NINE}var x = 1\nsetNine(x)")),
        "missing_ref"
    );
    assert_eq!(
        first_error_fix(&format!("{SET_NINE}var x = 1\nsetNine(x)")),
        ["ref "]
    );
    assert!(first_error_fix(&format!("{SET_NINE}setNine(5)")).is_empty());
    for call in [
        "f(ref x)",
        "print(ref x)",
        "Ref(ref x)",
        "let r = (a = ref x)",
    ] {
        let src = format!("func f(a: Int) => print(a)\nvar x = 1\n{call}");
        assert_eq!(first_error(&src), "ref_not_allowed", "{call}");
        assert!(first_error_fix(&src).is_empty(), "{call}");
    }
}

#[test]
fn a_ref_where_its_value_is_expected_suggests_value() {
    let src = "func count(xs: List<Int>) -> Int { xs.length() }\nlet r = Ref([1])\ncount(r)";
    assert_eq!(
        error_and_fix(src),
        (
            "ref_where_value_expected".to_string(),
            Some("r.value".to_string())
        )
    );
    for (src, value) in [
        ("print(r + 1)", "r.value"),
        ("print(1 < r)", "r.value"),
        ("var v = r\nv += 1", "v.value"),
    ] {
        let src = format!("let r = Ref(1)\n{src}");
        assert_eq!(
            diagnostics(&src, Severity::Error),
            ["ref_where_value_expected"],
            "{src}"
        );
        assert_eq!(error_and_fix(&src).1.as_deref(), Some(value), "{src}");
    }
}

#[test]
fn a_function_with_ref_parameters_cant_be_a_value() {
    assert_eq!(
        first_error(&format!("{SET_NINE}let f = setNine")),
        "ref_func_as_value"
    );
}

#[test]
fn a_ref_parameter_of_a_ref_class_is_a_warning() {
    let src = "ref class P(var n: Int)\nfunc f(ref p: P) { p.n = 2 }";
    assert_eq!(warnings(src), ["ref_param_of_ref_class"]);
}

#[test]
fn for_ref_writes_each_element_back() {
    let src = "class T(name: String, var done: Bool)\nvar ts = [T(\"a\", false), T(\"b\", false)]\nfor ref t in ts { t.done = true }\nprint(\"{ts}\")";
    assert_eq!(
        output(src),
        "[T(name = \"a\", done = true), T(name = \"b\", done = true)]"
    );
}

#[test]
fn for_ref_keeps_the_changes_made_before_a_break() {
    let src = "var xs = [1, 2, 3]\nfor ref x in xs {\n  x *= 10\n  if x == 20 { break }\n}\nprint(\"{xs}\")";
    assert_eq!(output(src), "[10, 20, 3]");
}

#[test]
fn for_ref_cant_change_its_list_or_go_through_a_let() {
    assert_eq!(
        first_error("var xs = [1, 2]\nfor ref x in xs { xs.push(x) }"),
        "changed_in_for_ref"
    );
    assert_eq!(
        first_error("let xs = [1]\nfor ref x in xs { x = 2 }"),
        "assign_to_let"
    );
    assert_eq!(
        first_error("var xs = [1, 2]\nfor ref x in xs {\n  for ref y in xs { y += 1 }\n}"),
        "changed_in_for_ref"
    );
    assert!(warnings("var xs = [1]\nfor ref x in xs { print(x) }").is_empty());
}

#[test]
fn for_ref_goes_through_a_ref() {
    let src = "class G(var r: Ref<List<Int>>)\nlet r = Ref([1, 2])\nvar g = G(Ref([3]))\nfor ref x in r { x += 1 }\nfor ref x in g.r { x += 1 }\nprint(\"{r} {g}\")";
    assert_eq!(output(src), "Ref([2, 3]) G(r = Ref([4]))");
}

#[test]
fn for_ref_panics_when_its_element_is_gone() {
    let src = "ref class Box(var items: List<Int>)\nclass Holder(var box: Box)\nfunc empty(b: Box) { b.items = [] }\nvar h = Holder(Box([1, 2]))\nlet b = h.box\nfor ref x in h.box.items { empty(b) }";
    panics_with(src, "for_ref_element_gone");
}

#[test]
fn a_name_called_ref_still_works() {
    assert_eq!(
        output("func f(ref: Int) => ref + 1\nlet ref = 2\nprint(f(ref))"),
        "3"
    );
}

#[test]
fn a_write_through_a_call_reaches_only_shared_values() {
    let src = "ref class Box(var n: Int)\nref class Game {\n  var title: String\n  var box: Box\n  func theBox() -> Box { this.box }\n  func reset() { this.theBox().n = 0 }\n}\nlet g = Game(\"hi\", Box(9))\ng.reset()\nfunc make(b: Box) -> Box { b }\nmake(g.box).n = 7\nprint(\"{g}\")";
    assert_eq!(output(src), "Game(title = \"hi\", box = Box(n = 7))");
    let log = "var log = Ref([1])\nfunc theLog() -> Ref<List<Int>> { log }\ntheLog().push(2)\nprint(\"{log}\")";
    assert_eq!(output(log), "Ref([1, 2])");
    assert_eq!(
        first_error("class P(var n: Int)\nfunc make() -> P { P(1) }\nmake().n = 7"),
        "assign_to_let"
    );
    for change in ["xs().pop()", "[1, 2].pop()", "xs().push(3)"] {
        let src = format!("func xs() -> List<Int> {{ [1, 2] }}\nprint({change})");
        assert_eq!(first_error(&src), "change_let_collection", "{change}");
    }
}

#[test]
fn paths_run_left_to_right_and_their_base_once() {
    let src = "var log = Ref([\"\"])\nfunc note(s: String, n: Int) -> Int {\n  log.push(s)\n  n\n}\nvar xs = [0]\nvar grid = [[0]]\nxs[note(\"index\", 0)] = note(\"value\", 1)\nxs[note(\"index\", 0)] += note(\"value\", 1)\ngrid[note(\"index\", 0)].push(note(\"value\", 1))\nref class Box(var n: Int)\nlet b = Box(1)\nfunc make() -> Box {\n  log.push(\"make\")\n  b\n}\nmake().n = 7\nprint(\"{log} {xs} {grid} {b}\")";
    assert_eq!(
        output(src),
        "Ref([\"\", \"index\", \"value\", \"index\", \"value\", \"index\", \"value\", \"make\"]) [2] [[0, 1]] Box(n = 7)"
    );
}

#[test]
fn a_failed_is_binds_nothing() {
    let src = "let a = 10\nlet t = (5, 2)\nif t is (a, 1) or a == 10 { print(a) }";
    assert_eq!(output(src), "10");
}

#[test]
fn a_default_sees_names_where_its_declared() {
    let src = "func base() -> Int { 5 }\nfunc f(n: Int = base()) -> Int { n }\nfunc caller() -> Int {\n  let base = func() => 99\n  f()\n}\nprint(caller())";
    assert_eq!(output(src), "5");
}

#[test]
fn deep_programs_reach_the_call_limit_without_overflowing() {
    let src = |levels: i64| {
        format!(
            "func f(n: Int) -> Int {{\n  match n {{\n    0 => 0\n    _ {{\n      var t = 0\n      for i in 0..1 {{ t = [1].map(func(x) => x + f(n - 1))[0] ?? 0 }}\n      t\n    }}\n  }}\n}}\nprint(f({levels}))"
        )
    };
    assert_eq!(output(&src(4_900)), "4900");
    panics_with(&src(5_100), "stack_overflow");
    let list = |len: i64, end: &str| {
        format!(
            "enum L {{ Nil, Cons(Int, L) }}\nvar l: L = Nil\nvar i = 0\nwhile i < {len} {{\n  l = Cons(i, l)\n  i += 1\n}}\n{end}"
        )
    };
    assert_eq!(output(&list(200_000, "let m = l\nprint(l == m)")), "true");
    assert_eq!(
        output(&list(200_000, "let s = \"{l}\"\nprint(\"done\")")),
        "done"
    );
}

#[test]
fn try_cant_be_used_in_init() {
    let src = "class P {\n  let name: String\n  var hp: Int = 0\n  init {\n    this.hp = try Int.parse(this.name)\n  }\n}";
    assert_eq!(first_error(src), "try_outside_failable");
}

#[test]
fn functions_cant_be_compared() {
    assert_eq!(
        first_error("func one() -> Int { 1 }\nprint(one == one)"),
        "invalid_operand_types"
    );
}

#[test]
fn a_lambda_cant_change_what_it_captured() {
    assert_eq!(
        first_error("var count = 0\nlet inc = func() { count += 1 }"),
        "assign_to_captured"
    );
    assert_eq!(
        first_error("func f() {\n  var xs = [1]\n  let add = func(n: Int) { xs.push(n) }\n}"),
        "assign_to_captured"
    );
    assert!(warnings("var n = 0\nlet f = func() { n += 1 }").is_empty());
    let own = "let f = func() -> Int {\n  var n = 0\n  n += 1\n  n\n}\nprint(f())";
    assert_eq!(output(own), "1");
}

#[test]
fn a_narrowed_index_that_is_gone_panics() {
    let src = "var xs = [1, 2, 3]\nfunc clear() { xs = [] }\nfor i in xs.indices() {\n  clear()\n  print(xs[i] * 2)\n}";
    panics_with(src, "index_out_of_range");
}

#[test]
fn powers_of_zero_one_and_minus_one_never_overflow() {
    let src = "func pw(a: Int, b: Int) -> Int { a ** b }\nprint(\"{pw(0, 9999999999)} {pw(1, 9999999999)} {pw(-1, 5000000001)} {pw(0, 0)}\")";
    assert_eq!(output(src), "0 1 -1 1");
}

#[test]
fn the_smallest_int_can_be_written() {
    assert_eq!(
        output("print(-9223372036854775808)"),
        "-9223372036854775808"
    );
    assert_eq!(first_error("print(9223372036854775808)"), "literal_too_big");
    assert_eq!(
        first_error("print(-9223372036854775808 ** 1)"),
        "literal_too_big"
    );
}

/// Loads `item.con` while the schema unit's `stock.conx` holds `stock`, and returns the panic
/// code if loading panicked: loading builds every data value in the unit.
fn data_panic(stock: &str) -> Option<String> {
    let files = MemFiles(HashMap::from([
        (
            "shop/shop.check".to_string(),
            "class Item(name: String, price: Int)".to_string(),
        ),
        ("shop/stock.conx".to_string(), stock.to_string()),
    ]));
    let item = "Item(\"shield\", 3)";
    match check_core::check_data(&files, "shop/", "item", "item.con", item, "Item") {
        Err(Report {
            outcome: Outcome::Panicked(code),
            ..
        }) => Some(code.to_string()),
        Err(r) => panic!(
            "{:?}",
            r.diagnostics
                .iter()
                .map(|d| d.code.to_string())
                .collect::<Vec<_>>()
        ),
        Ok(_) => None,
    }
}

#[test]
fn loading_data_stops_on_a_failed_try_or_an_endless_loop() {
    assert_eq!(
        data_panic("[Item(\"sword\", try Int.parse(\"x\"))]").as_deref(),
        Some("data_try_failed")
    );
    assert_eq!(
        data_panic("[Item(\"sword\", { loop { } })]").as_deref(),
        Some("out_of_steps")
    );
    // Few loop passes, but each `contains` looks at 100,000 items.
    let busy = "[Item(\"sword\", {\n  var xs: List<Int> = []\n  for i in 0..100000 { xs.push(i) }\n  var n = 0\n  for j in 0..200 { if xs.contains(-1) { n += 1 } }\n  n\n})]";
    assert_eq!(data_panic(busy).as_deref(), Some("out_of_steps"));
    // Map inserts are hashed, so this is quick (it took minutes with linear lookups).
    let inserts = "[Item(\"sword\", {\n  var m: Map<Int, Int> = [=]\n  for i in 0..100000 { m[i] = i }\n  m.length()\n})]";
    assert_eq!(data_panic(inserts), None);
    // `length()` doesn't scan, so it costs one step however long the list is.
    let fine = "[Item(\"sword\", {\n  var xs: List<Int> = []\n  for i in 0..5000 { xs.push(i) }\n  var i = 0\n  while i < xs.length() { i += 1 }\n  i\n})]";
    assert_eq!(data_panic(fine), None);
}

#[test]
fn nan_matches_no_float_range() {
    let src = "let x = 0.0 / 0.0\nprint(match x { 0.0..1.0 => \"in\", _ => \"out\" })";
    assert_eq!(output(src), "out");
}

#[test]
fn maps_and_sets_find_every_kind_of_key_and_keep_their_order() {
    let src = "enum Dir { Up, Down }\nlet keys = [true = 1, false = 2]\nlet dirs = [Dir.Up = 1, Dir.Down = 2]\nlet pairs = [(1, \"x\") = 1, (2, \"y\") = 2]\nclass P(x: Int)\nlet points = [P(1) = 1, P(2) = 2]\nprint(\"{keys[false]} {dirs[Dir.Down]} {pairs[(2, \"y\")]} {points[P(1)]}\")\nvar s = Set([3, 1, 2])\ns.remove(1)\ns.add(3)\ns.add(4)\nvar n = [\"a\" = 1, \"b\" = 2, \"c\" = 3]\nn.remove(\"a\")\nn[\"a\"] = 4\nprint(\"{s} {n}\")";
    assert_eq!(
        output(src),
        "Some(2) Some(2) Some(2) Some(1)\nSet([3, 2, 4]) [\"b\" = 2, \"c\" = 3, \"a\" = 4]"
    );
}

#[test]
fn a_lambda_keeps_what_it_captured_when_it_was_made() {
    let src = "var g = 1\nlet f = func() -> Int { g }\ng = 2\nvar fs: List<func() -> Int> = []\nfor i in 0..3 { fs.push(func() => i * 10) }\nfunc outer() -> func() -> func() -> Int {\n  let x = 5\n  func() => func() => x + 1\n}\nprint(\"{f()} {fs.map(func(h) => h())} {outer()()()}\")";
    assert_eq!(output(src), "1 [0, 10, 20] 6");
}

#[test]
fn a_lambda_changes_shared_values_it_captured_and_calls_its_own_captures() {
    let src = "ref class Box(var x: Int)\nfunc g() -> Int { 1 }\nfunc main() {\n  let r = Ref([1])\n  let b = Box(1)\n  let g = func() => 2\n  let f = func() {\n    r.push(2)\n    b.x = 7\n  }\n  f()\n  let h = func() => g()\n  print(\"{r} {b} {h()}\")\n}\nmain()";
    assert_eq!(output(src), "Ref([1, 2]) Box(x = 7) 2");
    let callback = "ref class Player {\n  var score: Int = 0\n  func bump() -> func() { func() { this.score += 1 } }\n}\nlet p = Player()\np.bump()()\nprint(p.score)";
    assert_eq!(output(callback), "1");
}
