//! Writes the memory-model test programs as `.wasm` files into the given folder, for
//! `scripts/memory-model-prototype.mjs` to run.

mod builder;
mod graph;
mod map;
mod rc;
mod vec3;

use builder::Builder;

fn empty() -> Vec<u8> {
    let mut b = Builder::new();
    let nothing = b.func(&[], &[], &[], |_| {});
    let unit = b.func(&[builder::I32], &[], &[], |_| {});
    let zero = b.func(&[], &[builder::F64], &[], |f| {
        f.f64_const(0.0.into());
    });
    b.finish(nothing, unit, 1, zero)
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: check-wasm-proto <output folder>");
    std::fs::create_dir_all(&dir).unwrap();
    let modules = [
        ("empty", empty()),
        ("vec3-rc", vec3::rc()),
        ("vec3-gc-boxed-naive", vec3::gc_boxed(true)),
        ("vec3-gc-boxed-best", vec3::gc_boxed(false)),
        ("vec3-gc-split-naive", vec3::gc_split(true)),
        ("vec3-gc-split-best", vec3::gc_split(false)),
        ("map-rc", map::rc()),
        ("map-gc-naive", map::gc(true)),
        ("map-gc-best", map::gc(false)),
        ("graph-rc", graph::rc()),
        ("graph-gc", graph::gc()),
    ];
    for (name, bytes) in modules {
        std::fs::write(format!("{dir}/{name}.wasm"), bytes).unwrap();
    }
}
