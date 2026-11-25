use std::collections::HashMap;

use check_core::modules::MemFiles;
use check_core::{Outcome, Report, check_source, lower::lower, run_source};
use check_wasm::{MemoryOutputPipe, RUNTIME, compile, execute, run_program};

fn output(pipe: &MemoryOutputPipe) -> String {
    String::from_utf8(pipe.contents().to_vec()).unwrap()
}

fn run_wasm(src: &str) -> (Report, String) {
    let files = MemFiles(HashMap::from([("test.check".to_string(), src.to_string())]));
    let stdout = MemoryOutputPipe::new(1 << 20);
    let report = run_program("test.check", &files, false, stdout.clone());
    if let Outcome::CheckFailed = report.outcome {
        let errors: String = (report.diagnostics.iter())
            .map(|d| d.render(&report.sources, false))
            .collect();
        panic!("{src}\ndoesn't compile:\n{errors}");
    }
    (report, output(&stdout))
}

fn same(src: &str) {
    let mut interpreted = Vec::new();
    let report = run_source("test.check", src, &mut std::io::empty(), &mut interpreted);
    let (wasm, wasm_output) = run_wasm(src);
    assert_eq!(
        wasm_output,
        String::from_utf8(interpreted).unwrap(),
        "{src}"
    );
    match (report.panic_text, wasm.panic_text) {
        (None, None) => {}
        (Some(expected), Some(text)) => assert!(
            expected.starts_with(&text),
            "{src}\nthe interpreter gave:\n{expected}WASM gave:\n{text}"
        ),
        (expected, text) => panic!("{src}\nthe interpreter gave {expected:?}, WASM {text:?}"),
    }
}

#[test]
fn hello_world_prints_the_same() {
    same(include_str!("../../../silver_tests/hello_world/main.check"));
}

#[test]
fn numbers_compute_and_print_the_same() {
    same(
        "let a = 7\nlet b = -2\n\
         print(a + b)\nprint(a - b)\nprint(a * b)\nprint(a / b)\nprint(a div b)\nprint(a mod b)\n\
         print(-a div 2)\nprint(-a mod 3)\nprint(a mod -1)\nprint(2 ** 10)\nprint((-1) ** 3)\n\
         print(a & 3)\nprint(a | 8)\nprint(a ^ 5)\nprint(~a)\nprint(1 << 62)\nprint(-8 >> 1)\n\
         print(a + 0.5)\nprint(2.0 ** 0.5)\nprint(-(1.5))\nprint(a < b)\nprint(a >= 7)\nprint(a == 7.0)\n\
         print(1.0)\nprint(0.1 + 0.2)\nprint(1e21)\nprint(1e-7)\nprint(123456.0)\nprint(1.0 / 0.0)\n\
         print(-1.0 / 0.0)\nlet nan = 0.0 / 0.0\nprint(nan)\nprint(nan == nan)\nprint(nan != nan)\n\
         print(nan < 1.0)\nprint(true == false)\nprint(not true)\nprint(true and a > 1)\nprint(false or b < 0)",
    );
}

#[test]
fn integer_panics_match_the_interpreter() {
    for src in [
        "let big = 9223372036854775807\nprint(\"before\")\nprint(big + 1)",
        "let low = -9223372036854775807 - 1\nprint(-low)",
        "let z = 0\nprint(1 div z)",
        "let z = 0\nprint(1 mod z)",
        "let z = 0\nprint(1 / z)",
        "let e = -1\nprint(2 ** e)",
        "let s = 64\nprint(1 << s)",
        "var n = 9223372036854775807\nn += 1",
        "print(3 ** 40)",
    ] {
        same(src);
    }
}

#[test]
fn strings_build_compare_and_print_the_same() {
    same(
        "let name = \"Check\"\nvar s = \"\"\nfor i in 0..3 { s = s + \"{i}\" }\nprint(s)\n\
         print(\"{name} has {name == \"Check\"} {1.5} {2 + 3}\")\n\
         print(\"apple\" < \"banana\")\nprint(\"b\" <= \"a\")\nprint(name != \"check\")\n\
         var t = name\nt += \"!\"\nprint(t)\nprint(name)",
    );
}

#[test]
fn functions_and_recursion_run_the_same() {
    same(
        "func fib(n: Int) -> Int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\n\
         func greet(who: String) -> String { let hi = \"Hi, \"\n  hi + who }\n\
         func firstOver(limit: Int) -> Int {\n  for i in 0..100 {\n    let label = \"{i}\"\n    if i * i > limit { return i }\n  }\n  -1\n}\n\
         func shout(s: String) { print(s + \"!\") }\n\
         print(fib(15))\nprint(greet(\"Lucy\"))\nprint(firstOver(50))\nshout(\"hey\")",
    );
}

#[test]
fn deep_recursion_overflows_the_stack() {
    same("func down(n: Int) -> Int { down(n + 1) + 1 }\nprint(down(0))");
}

#[test]
fn control_flow_with_early_exits_runs_the_same() {
    same(
        "var i = 0\nwhile true {\n  i += 1\n  let s = \"at {i}\"\n  if i mod 2 == 0 { continue }\n  if i > 7 { break }\n  print(s)\n}\n\
         for j in 1..=3 {\n  for k in 0..3 {\n    if k == j { break }\n    print(\"{j} {k}\")\n  }\n}\n\
         for j in 9223372036854775806..=9223372036854775807 { print(j) }\n\
         let high = 5
for j in high..2 { print(j) }\n\
         let size = if i > 5 { \"big\" } else { \"small\" }\nprint(size)",
    );
}

#[test]
fn memory_stays_bounded_while_strings_come_and_go() {
    let src = "var keep = \"\"\nfor i in 0..200000 {\n  let s = \"item {i} \" + \"and more text\"\n  keep = s\n}\nprint(keep)";
    let checked = check_source("test.check", src);
    let program = lower(&checked.program, &checked.library, &checked.info).unwrap();
    let stdout = MemoryOutputPipe::new(1 << 10);
    let run = execute(&compile(&program, RUNTIME).unwrap(), stdout.clone()).unwrap();
    assert_eq!(output(&stdout), "item 199999 and more text\n");
    assert!(run.pages < 40, "memory grew to {} pages", run.pages);
}
