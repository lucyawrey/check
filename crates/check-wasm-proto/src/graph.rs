//! Each round creates a ring of N `ref class` nodes, each also pointing at another node, walks the
//! ring, and drops it.

use crate::builder::*;
use crate::rc::{self, Rc};
use wasm_encoder::ConstExpr;

const N: i32 = 10_000;
const ROUNDS: i32 = 100;

/// Pushes the index of the node that node `i` (a local) points to.
fn neighbor(f: &mut Sink, i: u32, step: i32, add: i32) {
    f.local_get(i)
        .i32_const(step)
        .i32_mul()
        .i32_const(add)
        .i32_add()
        .i32_const(N)
        .i32_rem_u();
}

fn value(f: &mut Sink, i: u32) {
    f.local_get(i)
        .f64_convert_i32_s()
        .f64_const(0.25.into())
        .f64_mul();
}

/// A node is [next][other][value]. No cycle detector: the drop breaks the cycles by hand.
pub fn rc() -> Vec<u8> {
    let mut b = Builder::new();
    let rc = Rc::new(&mut b);
    let acc = b.global(F64, ConstExpr::f64_const(0.0.into()));
    let release = b.declare(&[I32], &[]);
    b.define(release, &[I32], |f| {
        f.local_get(0).if_(Empty);
        rc::release_count(f, 0, 1, |f| {
            f.local_get(0).i32_load(m(0, 2)).call(release);
            f.local_get(0).i32_load(m(4, 2)).call(release);
            f.local_get(0).call(rc.free);
        });
        f.end();
    });
    let node = |f: &mut Sink, nodes: u32| {
        f.i32_const(2)
            .i32_shl()
            .local_get(nodes)
            .i32_add()
            .i32_load(m(0, 2));
    };
    // Param round; locals i, nodes, p, q, sum.
    let unit = b.func(&[I32], &[], &[I32, I32, I32, I32, F64], |f| {
        f.i32_const(N * 4).call(rc.alloc).local_set(2);
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(2).local_get(1).i32_const(2).i32_shl().i32_add();
            f.i32_const(16)
                .call(rc.alloc)
                .local_tee(3)
                .i32_store(m(0, 2));
            f.local_get(3).i32_const(0).i32_store(m(0, 2));
            f.local_get(3).i32_const(0).i32_store(m(4, 2));
            f.local_get(3);
            value(f, 1);
            f.f64_store(m(8, 3));
        });
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(1);
            node(f, 2);
            f.local_set(3);
            for (step, add, offset) in [(1, 1, 0), (7919, 13, 4)] {
                neighbor(f, 1, step, add);
                node(f, 2);
                f.local_set(4);
                rc::retain(f, 4);
                f.local_get(3).local_get(4).i32_store(m(offset, 2));
            }
        });
        f.i32_const(0);
        node(f, 2);
        f.local_set(3);
        rc::retain(f, 3);
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(1);
            node(f, 2);
            f.call(release);
        });
        f.local_get(2).call(rc.release);
        // Walk: p starts as the root, which local 3 still owns, so retain it for the walk.
        f.local_get(3).local_set(4);
        rc::retain(f, 4);
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(5).local_get(4).f64_load(m(8, 3)).f64_add();
            f.local_get(4)
                .i32_load(m(4, 2))
                .f64_load(m(8, 3))
                .f64_add()
                .local_set(5);
            f.local_get(4).local_get(4).i32_load(m(0, 2)).local_set(4);
            rc::retain(f, 4);
            f.call(release);
        });
        f.local_get(4).call(release);
        f.global_get(acc).local_get(5).f64_add().global_set(acc);
        // Drop: clear each node's links while holding the next one, then release the root.
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(3).i32_load(m(0, 2)).local_set(4);
            rc::retain(f, 4);
            for offset in [4, 0] {
                f.local_get(3).i32_load(m(offset, 2)).call(release);
                f.local_get(3).i32_const(0).i32_store(m(offset, 2));
            }
            f.local_get(3).call(release).local_get(4).local_set(3);
        });
        f.local_get(3).call(release);
    });
    let setup = b.func(&[], &[], &[], |_| {});
    let finish = b.func(&[], &[F64], &[], |f| {
        f.global_get(acc);
    });
    b.finish(setup, unit, ROUNDS, finish)
}

pub fn gc() -> Vec<u8> {
    let mut b = Builder::new();
    let node = b.next_type();
    b.struct_type(&[r(node), r(node), F64]);
    let nodes = b.array_type(r(node));
    let acc = b.global(F64, ConstExpr::f64_const(0.0.into()));
    // Param round; locals i, nodes, p, sum.
    let unit = b.func(&[I32], &[], &[I32, r(nodes), r(node), F64], |f| {
        f.i32_const(N).array_new_default(nodes).local_set(2);
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(2)
                .local_get(1)
                .ref_null(wasm_encoder::HeapType::Concrete(node));
            f.ref_null(wasm_encoder::HeapType::Concrete(node));
            value(f, 1);
            f.struct_new(node).array_set(nodes);
        });
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(2).local_get(1).array_get(nodes).local_set(3);
            for (step, add, field) in [(1, 1, 0), (7919, 13, 1)] {
                f.local_get(3).local_get(2);
                neighbor(f, 1, step, add);
                f.array_get(nodes).struct_set(node, field);
            }
        });
        f.local_get(2).i32_const(0).array_get(nodes).local_set(3);
        repeat(f, 1, To::Const(N), |f| {
            f.local_get(4).local_get(3).struct_get(node, 2).f64_add();
            f.local_get(3)
                .struct_get(node, 1)
                .struct_get(node, 2)
                .f64_add()
                .local_set(4);
            f.local_get(3).struct_get(node, 0).local_set(3);
        });
        f.global_get(acc).local_get(4).f64_add().global_set(acc);
    });
    let setup = b.func(&[], &[], &[], |_| {});
    let finish = b.func(&[], &[F64], &[], |f| {
        f.global_get(acc);
    });
    b.finish(setup, unit, ROUNDS, finish)
}
