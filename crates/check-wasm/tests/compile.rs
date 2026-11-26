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

fn stays_bounded(src: &str) {
    let mut interpreted = Vec::new();
    run_source("test.check", src, &mut std::io::empty(), &mut interpreted);
    let checked = check_source("test.check", src);
    let program = lower(&checked.program, &checked.library, &checked.info).unwrap();
    let stdout = MemoryOutputPipe::new(1 << 10);
    let run = execute(&compile(&program, RUNTIME).unwrap(), stdout.clone()).unwrap();
    assert_eq!(output(&stdout), String::from_utf8(interpreted).unwrap());
    assert!(run.pages < 40, "memory grew to {} pages", run.pages);
}

#[test]
fn memory_stays_bounded_while_strings_come_and_go() {
    stays_bounded(
        "var keep = \"\"\nfor i in 0..200000 {\n  let s = \"item {i} \" + \"and more text\"\n  keep = s\n}\nprint(keep)",
    );
}

#[test]
fn memory_stays_bounded_through_parameters_and_discarded_results() {
    stays_bounded(
        "func echo(s: String) -> String { s }\nfunc ignore(s: String) { }\n\
         var keep = \"\"\nfor i in 0..200000 {\n  ignore(\"ignored {i}\")\n  echo(\"dropped {i}\")\n  keep = echo(\"kept {i}\")\n}\nprint(keep)",
    );
}

#[test]
fn memory_stays_bounded_through_early_exits() {
    stays_bounded(
        "func first(n: Int) -> String {\n  for i in 0..3 {\n    let s = \"n{n} i{i}\"\n    if i == 1 { return s }\n  }\n  \"none\"\n}\n\
         var keep = \"\"\nfor i in 0..200000 {\n  let a = \"a{i}\"\n  if i mod 2 == 0 { continue }\n  keep = first(i)\n  while true {\n    let b = \"b{i}\"\n    break\n  }\n}\nprint(keep)",
    );
}

#[test]
fn memory_stays_bounded_while_comparing_strings() {
    stays_bounded(
        "var n = 0\nfor i in 0..200000 {\n  if \"k{i}\" == \"k5\" or \"a{i}\" < \"b\" { n += 1 }\n}\nprint(n)",
    );
}

#[test]
fn dyn_and_optional_chaining_are_unsupported_for_now() {
    for src in [
        "print(Json.parse(\"1\"))",
        "let n: Int? = Some(1)\nprint(n?.toFloat())",
    ] {
        let files = MemFiles(HashMap::from([("test.check".to_string(), src.to_string())]));
        let report = run_program("test.check", &files, false, MemoryOutputPipe::new(1 << 20));
        let codes: Vec<String> = report
            .diagnostics
            .iter()
            .map(|d| d.code.to_string())
            .collect();
        assert_eq!(codes, ["wasm_unsupported"], "{src}");
    }
}

#[test]
fn generic_functions_run_the_same_at_each_type() {
    same(
        "func same<T>(x: T) -> T { x }\n\
         func twice<T>(x: T) -> String { \"{same(x)} {same(x)}\" }\n\
         func pick<A, B>(useFirst: Bool, a: A, b: B) -> String { if useFirst { \"{a}\" } else { \"{b}\" } }\n\
         print(same(3) + 1)\nprint(same(\"hi\"))\nprint(same(2.5))\nprint(twice(true))\nprint(twice(\"ho\"))\n\
         print(pick(true, 1, \"one\"))\nprint(pick(false, 1, \"one\"))",
    );
}

#[test]
fn memory_stays_bounded_through_generic_functions() {
    stays_bounded("func same<T>(x: T) -> T { x }\nfor i in 0..100000 { let s = same(\"x{i}\") }");
}

#[test]
fn maybes_unwrap_default_and_print_the_same() {
    same(
        r#"let some: String? = "hi"
let none: Int? = None
print(some)
print(none)
print("{some} and {none}")
print(none ?? 5)
print(some ?? "fallback")
print(none ?? None ?? 7)
print(some == "hi")
print(none == None)
print(none == 3)
let nested: Int?? = Some(None)
print(nested)
print(nested ?? 1)
print(if none == None { 3 })
print(some.orPanic())
var xs: Int?[] = [None, Some(1)]
print(xs.first())
print(xs.pop())
print(xs.pop())
print(xs.pop())"#,
    );
}

#[test]
fn maybe_panics_match_the_interpreter() {
    for src in [
        "let n: Int? = None\nprint(n.orPanic())",
        "let n: String? = None\nprint(n.orPanic(\"needs a name\"))",
        "let xs = [1, 2]\nfor i in xs.indices() { print(xs[i]) }\nvar ys = xs\nys[2] = 5",
        "var xs = [1]\nxs.insert(3, 2)",
    ] {
        same(src);
    }
}

#[test]
fn records_and_classes_build_change_and_print_the_same() {
    same(
        r#"class Point(var x: Int, var y: Int = 0)
class Task {
  let title: String
  var done = false
  var tags: String[] = []
  private var log = ""
  init { this.log = "made {this.title}" }
  func describe() -> String { "{this.title}: {this.done} {this.log}" }
  func finish() {
    this.done = true
    this.tags.push("finished")
  }
}
class Pair<T>(first: T, second: T)
func swap<T>(p: Pair<T>) -> Pair<T> { Pair(first = p.second, second = p.first) }
var p = Point(x = 1)
p.y += 2
print(p)
let r = (name = "a", size = 2)
print(r)
print(r.name)
let t = (1, "two", 3.5)
print(t)
var task = Task(title = "write")
print(task.describe())
task.finish()
print(task)
print(swap(Pair(first = 1, second = 2)))
print(swap(Pair(first = "a", second = "b")))
print(Point(x = 1, y = 2) == Point(x = 1, y = 2))
print(r == (name = "a", size = 3))"#,
    );
}

#[test]
fn copies_stay_unchanged_when_the_original_changes() {
    same(
        r#"class Task(title: String, var done: Bool = false)
class Board(var tasks: Task[] = [], var name: String = "board")
func finishAll(ref tasks: Task[]) {
  for i in tasks.indices() { tasks[i].done = true }
}
var board = Board()
board.tasks.push(Task(title = "a"))
board.tasks.push(Task(title = "b"))
let before = board
board.tasks[0].done = true
print(board)
print(before)
var copy = board.tasks
finishAll(copy)
print(copy)
print(board.tasks)
finishAll(board.tasks)
print(board)
print(before)"#,
    );
}

#[test]
fn lists_index_change_and_loop_the_same() {
    same(
        r#"var xs = [3, 1, 4]
print(xs.length())
print(xs[0])
print(xs[3])
xs.push(1)
xs.insert(0, 9)
print(xs)
print(xs.removeAt(1))
print(xs.removeAt(10))
print(xs.isEmpty())
print(xs.last())
for x in xs { print(x * 2) }
var words: String[] = []
for i in 0..20 { words.push("w{i}") }
print(words.length())
print(words[19])
while words.length() > 2 { words.pop() }
print(words)
let grid = [[1, 2], [3]]
print(grid)
print(grid == [[1, 2], [3]])
let floats = [1.5, 2.0]
print(floats)
let empty: Int[] = []
print(empty.first())
for x in empty { print(x) }
for w in words {
  if w == "w0" { continue }
  print(w)
  break
}"#,
    );
}

#[test]
fn memory_stays_bounded_while_values_come_and_go() {
    stays_bounded(
        r#"class Task(title: String, var tags: String[] = [])
var keep: Task[] = []
for i in 0..20000 {
  var tasks: Task[] = []
  for j in 0..5 { tasks.push(Task(title = "t{j}", tags = ["x{i}"])) }
  let copy = tasks
  tasks[0].tags.push("more")
  let n: Int? = Some(i)
  let m = n ?? 0
  let pair = (a = "a{i}", b = Some("b"))
  keep = copy
}
print(keep.length())"#,
    );
}

#[test]
fn methods_refs_and_recursive_classes_run_the_same() {
    same(
        r#"class Node(value: Int, next: Node? = None)
class Counter {
  var n = 0
  func bump() { this.n += 1 }
  func twice() -> Int {
    this.bump()
    this.bump()
    this.n
  }
}
class Box<T> {
  var item: T
  func get() -> T { this.item }
  func put(item: T) { this.item = item }
}
func find(xs: String[], want: String) -> Int? {
  var i = 0
  for x in xs {
    if x == want { return Some(i) }
    i += 1
  }
  None
}
func grow(ref xs: Int[], n: Int) -> Int {
  for i in 0..n { xs.push(i) }
  xs.length()
}
var list = Node(1, Some(Node(2, Some(Node(3)))))
print(list)
var cs = [Counter(), Counter()]
for i in cs.indices() { cs[i].bump() }
print(cs)
var c = Counter()
print(c.twice())
print(c)
var b = Box(item = "x")
b.put("y")
print(b.get())
var bi = Box(item = 1)
bi.put(bi.get() + 1)
print(bi)
print(find(["a", "b", "c"], "b"))
print(find(["a"], "z"))
var nums: Int[] = []
print(grow(nums, 3))
print(nums)
let maybe = if nums.length() > 2 { "long" }
print(maybe)
let nested = [Some([1]), None]
print(nested)
print([(1, "a")] == [(1, "a")])"#,
    );
}
