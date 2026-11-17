use std::io::Cursor;

use check_core::ast::Stmt;
use check_core::diag::Severity;
use check_core::modules::MemFiles;
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
    let report = run_source(
        "test.check",
        src,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    );
    report
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.code.to_string())
        .collect()
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
        "class P {\n  readonly let x: Int = 1\n}",
        "class P(x: Int)\nprivate implement P {\n  func f() {}\n}",
    ] {
        assert_eq!(first_error(src), "unexpected_token", "{src}");
    }
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
