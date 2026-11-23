use check_wasm::{Module, RUNTIME, ValType::I32};
use wasmtime::{Engine, Linker, Store};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

fn run(wasm: &[u8]) -> String {
    let engine = Engine::default();
    let module = wasmtime::Module::new(&engine, wasm).expect("valid module");
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |ctx| ctx).unwrap();
    let stdout = MemoryOutputPipe::new(1 << 16);
    let ctx = WasiCtxBuilder::new().stdout(stdout.clone()).build_p1();
    let mut store = Store::new(&engine, ctx);
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .unwrap();
    start.call(&mut store, ()).unwrap();
    String::from_utf8(stdout.contents().to_vec()).unwrap()
}

#[test]
fn a_program_calls_the_merged_runtime() {
    let mut m = Module::new(RUNTIME);
    let new_string = m.runtime_func("check_new_string");
    let concat = m.runtime_func("check_concat");
    let print = m.runtime_func("check_print");
    let hello = m.passive_data(b"Hello, ");
    let world = m.passive_data(b"World!");
    let start = m.func(&[], &[], &[I32, I32], |f| {
        for (local, segment, len) in [(0, hello, 7), (1, world, 6)] {
            f.i32_const(len).call(new_string).local_tee(local);
            f.i32_const(8).i32_add().i32_const(0).i32_const(len);
            f.memory_init(0, segment);
        }
        f.local_get(0).local_get(1).call(concat).call(print);
    });
    m.export_func("_start", start);
    assert_eq!(run(&m.finish()), "Hello, World!\n");
}
