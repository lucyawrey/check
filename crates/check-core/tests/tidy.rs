use check_core::check_program;
use check_core::diag::Severity;
use check_core::modules::MemFiles;
use check_core::tidy::{Tidied, tidy};

const BOTH: &[Severity] = &[Severity::Warning, Severity::Error];

fn mem(files: &[(&str, &str)]) -> MemFiles {
    MemFiles(
        files
            .iter()
            .map(|(p, s)| (p.to_string(), s.to_string()))
            .collect(),
    )
}

/// Tidies `files[0]` as the entry, the others beside it, and returns each file's text after.
/// Fails if a fixed rule still fires on the result.
fn tidy_files(files: &[(&str, &str)], levels: &[Severity]) -> (Vec<String>, Tidied) {
    let tidied = tidy(files[0].0, &mem(files), levels);
    let after: Vec<String> = files
        .iter()
        .map(|(path, text)| {
            tidied
                .files
                .iter()
                .find(|(p, _)| p == path)
                .map_or(text.to_string(), |(_, t)| t.clone())
        })
        .collect();
    let pairs: Vec<(&str, &str)> = files
        .iter()
        .zip(&after)
        .map(|((p, _), t)| (*p, t.as_str()))
        .collect();
    let rechecked = check_program(files[0].0, &mem(&pairs), false);
    for change in &tidied.changes {
        for rule in change.rule.split(", ") {
            assert!(
                !rechecked
                    .diagnostics
                    .iter()
                    .any(|d| d.code.to_string() == rule && !d.fix.is_empty()),
                "{rule} still fires after tidying"
            );
        }
    }
    (after, tidied)
}

fn tidied(src: &str) -> String {
    tidy_files(&[("main.check", src)], BOTH).0.remove(0)
}

#[test]
fn fixes_each_rule_in_one_file() {
    let cases = [
        (
            "let a = [1, 2,]\nprint(\"{a}\")",
            "let a = [1, 2]\nprint(\"{a}\")",
        ),
        (
            "let c = 7%2\nprint(\"{c}\")",
            "let c = 7 mod 2\nprint(\"{c}\")",
        ),
        (
            "let t = true\nlet x = t&&!t || !(t)\nprint(\"{x}\")",
            "let t = true\nlet x = t and not t or not (t)\nprint(\"{x}\")",
        ),
        (
            "let n = 1\nif n = 1 { print(\"a\") }",
            "let n = 1\nif n == 1 { print(\"a\") }",
        ),
        (
            "func f() -> Int => { 1 }\nprint(\"{f()}\")",
            "func f() -> Int { 1 }\nprint(\"{f()}\")",
        ),
        ("var n = 1\nprint(\"{n}\")", "let n = 1\nprint(\"{n}\")"),
        (
            "func f(x: Int?) -> Int {\n  let Some(y) = x else { return 0 }\n  y\n}\nprint(\"{f(1)}\")",
            "func f(x: Int?) -> Int {\n  let y = x ?? return 0\n  y\n}\nprint(\"{f(1)}\")",
        ),
        (
            "func f(x: Int?, w: Int?) -> Int {\n  let Some(y) = x ?? w else { return 0 }\n  y\n}\nprint(\"{f(1, 2)}\")",
            "func f(x: Int?, w: Int?) -> Int {\n  let y = (x ?? w) ?? return 0\n  y\n}\nprint(\"{f(1, 2)}\")",
        ),
        (
            "class P(x: Int, y: Int)\nlet q: P = (1, 2)\nprint(\"{q.x}\")",
            "class P(x: Int, y: Int)\nlet q = P(1, 2)\nprint(\"{q.x}\")",
        ),
        (
            "class P(let x: Int)\nprint(\"{P(1).x}\")",
            "class P(x: Int)\nprint(\"{P(1).x}\")",
        ),
        (
            "class Q {\n  readonly let z: Int = 1\n}",
            "class Q {\n  let z: Int = 1\n}",
        ),
        (
            "func a() -> Int => 1\nprint(\"{a()}\")",
            "func a() -> Int { 1 }\nprint(\"{a()}\")",
        ),
        (
            "func b() print(\"b\")\nb()",
            "func b() => print(\"b\")\nb()",
        ),
        (
            "func c()\n  => print(\"c\")\nc()",
            "func c() => print(\"c\")\nc()",
        ),
        (
            "func d() => print(\"d\"); print(\"e\")\nd()",
            "func d() { print(\"d\"); print(\"e\") }\nd()",
        ),
        (
            "let h = 0XFF\nprint(\"{h}\")",
            "let h = 0xFF\nprint(\"{h}\")",
        ),
        ("let e = 1E3\nprint(\"{e}\")", "let e = 1e3\nprint(\"{e}\")"),
        (
            "let n = 1\nif n == 1 {\n  print(\"a\")\n}\nelse {\n  print(\"b\")\n}",
            "let n = 1\nif n == 1 {\n  print(\"a\")\n} else {\n  print(\"b\")\n}",
        ),
    ];
    for (before, after) in cases {
        assert_eq!(tidied(before), after, "{before}");
    }
}

#[test]
fn fixes_imports_and_namespaces() {
    let lib = ("lib/a.check", "func a() -> Int { 1 }");
    let (after, _) = tidy_files(&[("main.check", "import \"lib\""), lib], BOTH);
    assert_eq!(after[0], "import \"lib/\"");

    let main = ("main.check", "import \"game/\"\nprint(game.f())");
    let manifest = ("game/package.con", "namespace game");
    let namespaced = |ns: &str| {
        let file = format!("{ns}func f() -> Int {{ 1 }}");
        tidy_files(&[main, manifest, ("game/a.check", &file)], BOTH).0[2].clone()
    };
    assert_eq!(
        namespaced("namespace other\n\n"),
        "namespace game.other\n\nfunc f() -> Int { 1 }"
    );
    assert_eq!(namespaced("namespace game\n\n"), "func f() -> Int { 1 }");
    assert_eq!(
        namespaced("namespace other.x\n\n"),
        "namespace other.x\n\nfunc f() -> Int { 1 }"
    );
}

#[test]
fn a_fix_behind_an_error_comes_in_a_later_pass() {
    assert_eq!(
        tidied("var n = 1\nif n = 1 { print(\"a\") }"),
        "let n = 1\nif n == 1 { print(\"a\") }"
    );
}

#[test]
fn only_applies_the_chosen_levels() {
    let src = "var n = 1\nlet a = [1,]\nprint(\"{n}{a}\")";
    let (after, _) = tidy_files(&[("main.check", src)], &[Severity::Error]);
    assert_eq!(after[0], "var n = 1\nlet a = [1]\nprint(\"{n}{a}\")");

    let (after, tidied) = tidy_files(&[("main.check", src)], &[Severity::Warning]);
    assert_eq!(after[0], src);
    assert!(tidied.style_blocked);
}

#[test]
fn style_fixes_wait_while_unfixable_errors_remain() {
    let src = "import \"nope/\"\nvar n = 1\nlet a = [1,]\nprint(\"{n}{a}\")";
    let (after, tidied) = tidy_files(&[("main.check", src)], BOTH);
    assert_eq!(
        after[0],
        "import \"nope/\"\nvar n = 1\nlet a = [1]\nprint(\"{n}{a}\")"
    );
    assert!(tidied.style_blocked);
}

#[test]
fn shows_each_change_with_its_rule() {
    let tidied = tidy_files(&[("main.check", "let a = [1,]\nprint(\"{a}\")")], BOTH).1;
    let change = &tidied.changes[0];
    assert_eq!(
        (
            change.line,
            change.rule.as_str(),
            change.before.as_str(),
            change.after.as_str()
        ),
        (1, "trailing_separator", "let a = [1,]\n", "let a = [1]\n")
    );
}

#[test]
fn leaves_alone_what_has_no_safe_fix() {
    for src in [
        "func f() -> Int\n{\n  1\n}",
        "func f()",
        "let n = 1\nif n == 1 {\n  print(\"a\")\n} // done\nelse {\n  print(\"b\")\n}",
        "func f() -> Int // note\n  => 1",
    ] {
        let (after, tidied) = tidy_files(&[("main.check", src)], BOTH);
        assert_eq!((after[0].as_str(), tidied.capped), (src, false), "{src}");
    }
}

#[test]
fn keeps_a_continuation_lines_indentation() {
    assert_eq!(
        tidied("let t = true\nlet x = t\n    && t"),
        "let t = true\nlet x = t\n    and t"
    );
}

#[test]
fn numbers_lines_as_in_the_original_file() {
    let src = "let n = 1\nif n == 1 {\n  print(\"a\")\n}\nelse {\n  print(\"b\")\n}\nvar m = 2\nprint(\"{m}\")";
    let tidied = tidy_files(&[("main.check", src)], BOTH).1;
    let changes: Vec<(usize, &str, &str, &str)> = tidied
        .changes
        .iter()
        .map(|c| (c.line, c.rule.as_str(), c.before.as_str(), c.after.as_str()))
        .collect();
    assert_eq!(
        changes,
        [
            (4, "else_on_new_line", "}\nelse {\n", "} else {\n"),
            (8, "unused_var", "var m = 2\n", "let m = 2\n"),
        ]
    );
}

#[test]
fn the_loaders_style_fixes_wait_too() {
    let main = (
        "main.check",
        "import \"nope/\"\nimport \"game/\"\nprint(game.f())",
    );
    let manifest = ("game/package.con", "namespace game");
    let file = ("game/a.check", "namespace game\n\nfunc f() -> Int { 1 }");
    let (after, tidied) = tidy_files(&[main, manifest, file], BOTH);
    assert_eq!(after[2], file.1);
    assert!(tidied.style_blocked);
}
