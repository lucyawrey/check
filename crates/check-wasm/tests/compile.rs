use check_core::diag::{Code, Severity};
use check_core::interp::panic_code;
use check_core::{Outcome, check_source, lower::lower, run_source};
use check_runtime::Panic;
use check_wasm::{RUNTIME, compile};
use wasmtime::{Engine, Linker, Store, Trap};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

#[derive(Debug)]
struct HostPanic(u32, u32);

impl std::fmt::Display for HostPanic {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "panic {} at {}", self.0, self.1)
    }
}

impl std::error::Error for HostPanic {}

struct Run {
    output: String,
    panic: Option<(Code, Option<usize>)>,
    pages: u64,
}

fn run_wasm(src: &str) -> Run {
    let checked = check_source("test.check", src);
    let errors: Vec<String> = (checked.diagnostics.iter())
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{:?} at {:?}", d.code, checked.sources.text(d.span)))
        .collect();
    assert!(errors.is_empty(), "{src}\nhas errors: {errors:?}");
    let program = lower(&checked.program, &checked.library, &checked.info).expect("lowers");
    let wasm = compile(&program, RUNTIME).expect("compiles");
    let engine = Engine::default();
    let module = wasmtime::Module::new(&engine, &wasm).expect("a valid module");
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |ctx| ctx).unwrap();
    linker
        .func_wrap(
            "check",
            "panic",
            |code: u32, at: u32| -> wasmtime::Result<()> {
                Err(wasmtime::Error::new(HostPanic(code, at)))
            },
        )
        .unwrap();
    let stdout = MemoryOutputPipe::new(1 << 20);
    let ctx = WasiCtxBuilder::new().stdout(stdout.clone()).build_p1();
    let mut store = Store::new(&engine, ctx);
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .unwrap();
    let panic = start.call(&mut store, ()).err().map(|e| {
        if let Some(HostPanic(code, at)) = e.downcast_ref() {
            (panic_code(Panic::ALL[*code as usize]), Some(*at as usize))
        } else if e.downcast_ref::<Trap>() == Some(&Trap::StackOverflow) {
            (Code::stack_overflow, None)
        } else {
            panic!("{src} trapped: {e:?}")
        }
    });
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    Run {
        output: String::from_utf8(stdout.contents().to_vec()).unwrap(),
        panic,
        pages: memory.size(&store),
    }
}

fn same(src: &str) {
    let mut output = Vec::new();
    let report = run_source("test.check", src, &mut std::io::empty(), &mut output);
    let wasm = run_wasm(src);
    assert_eq!(wasm.output, String::from_utf8(output).unwrap(), "{src}");
    match (report.outcome, wasm.panic) {
        (Outcome::Finished, None) => {}
        (Outcome::Panicked(code), Some((wasm_code, at))) => {
            assert_eq!(wasm_code, code, "{src}");
            if let Some(at) = at {
                let (path, line, col, _) = report.sources.locate(at);
                let text = report.panic_text.expect("a panic's text");
                assert!(
                    text.contains(&format!("{path}:{line}:{col}")),
                    "{src}: {text}"
                );
            }
        }
        (_, panic) => panic!("{src}: the interpreter and WASM differ, WASM gave {panic:?}"),
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
    let run = run_wasm(
        "var keep = \"\"\nfor i in 0..200000 {\n  let s = \"item {i} \" + \"and more text\"\n  keep = s\n}\nprint(keep)",
    );
    assert_eq!(run.output, "item 199999 and more text\n");
    assert!(run.pages < 40, "memory grew to {} pages", run.pages);
}
