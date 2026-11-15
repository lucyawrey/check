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

#[test]
fn a_top_level_try_on_none_stops_without_a_panic() {
    let mut out = Vec::new();
    let src = "print(\"before\")\nlet line = try input()\nprint(\"after\")";
    let report = run_source("test.check", src, &mut Cursor::new(Vec::new()), &mut out);
    assert!(matches!(report.outcome, Outcome::Stopped));
    assert_eq!(String::from_utf8(out).expect("utf-8"), "before\n");
}

fn first_error(src: &str) -> String {
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    report
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .unwrap_or_default()
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
