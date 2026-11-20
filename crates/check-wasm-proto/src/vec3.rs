//! Builds a list of N `Vec3`s, then moves every element once per frame.

use crate::builder::*;
use crate::rc::{self, Rc};
use wasm_encoder::ConstExpr;

const N: i32 = 100_000;
const FRAMES: i32 = 1000;
const DT: f64 = 0.001;

/// Pushes component `k` of element `i`'s starting position.
fn initial(f: &mut Sink, i: u32, k: u64) {
    f.local_get(i).f64_convert_i32_s();
    match k {
        0 => f,
        1 => f.f64_const(0.5.into()).f64_mul(),
        _ => f.f64_neg(),
    };
}

/// Moves the position in locals x, y, z one frame, using `t` as scratch.
fn step(f: &mut Sink, [x, y, z]: [u32; 3], t: u32) {
    f.local_get(x)
        .local_get(y)
        .f64_const(DT.into())
        .f64_mul()
        .f64_add()
        .local_set(t);
    f.local_get(y)
        .local_get(z)
        .f64_const(DT.into())
        .f64_mul()
        .f64_add()
        .local_set(y);
    f.local_get(z)
        .local_get(x)
        .f64_const(DT.into())
        .f64_mul()
        .f64_sub()
        .local_set(z);
    f.local_get(t).local_set(x);
}

/// Adds x + y + z from locals to the f64 sum in local `acc`.
fn add_to(f: &mut Sink, acc: u32, [x, y, z]: [u32; 3]) {
    f.local_get(acc)
        .local_get(x)
        .local_get(y)
        .f64_add()
        .local_get(z)
        .f64_add()
        .f64_add()
        .local_set(acc);
}

/// The list is its length, padding, then the elements inline, 24 bytes each.
pub fn rc() -> Vec<u8> {
    let mut b = Builder::new();
    let rc = Rc::new(&mut b);
    let list = b.global(I32, ConstExpr::i32_const(0));
    let element = |f: &mut Sink, p: u32, i: u32, e: u32| {
        f.local_get(p)
            .local_get(i)
            .i32_const(24)
            .i32_mul()
            .i32_add()
            .local_set(e);
    };
    let xyz = [m(8, 3), m(16, 3), m(24, 3)];
    let setup = b.func(&[], &[], &[I32, I32, I32], |f| {
        f.i32_const(8 + N * 24)
            .call(rc.alloc)
            .local_tee(1)
            .i32_const(N)
            .i32_store(m(0, 2));
        repeat(f, 0, To::Const(N), |f| {
            element(f, 1, 0, 2);
            for (k, a) in xyz.into_iter().enumerate() {
                f.local_get(2);
                initial(f, 0, k as u64);
                f.f64_store(a);
            }
        });
        f.local_get(1).global_set(list);
    });
    let unit = b.func(&[I32], &[], &[I32, I32, I32, F64, F64, F64, F64], |f| {
        f.global_get(list).local_set(2);
        repeat(f, 1, To::Const(N), |f| {
            rc::unshare(f, 2);
            element(f, 2, 1, 3);
            for (k, a) in xyz.into_iter().enumerate() {
                f.local_get(3).f64_load(a).local_set(4 + k as u32);
            }
            step(f, [4, 5, 6], 7);
            for (k, a) in xyz.into_iter().enumerate() {
                f.local_get(3).local_get(4 + k as u32).f64_store(a);
            }
        });
    });
    let finish = b.func(&[], &[F64], &[I32, I32, I32, F64, F64, F64, F64], |f| {
        f.global_get(list).local_set(1);
        repeat(f, 0, To::Const(N), |f| {
            element(f, 1, 0, 2);
            for (k, a) in xyz.into_iter().enumerate() {
                f.local_get(2).f64_load(a).local_set(4 + k as u32);
            }
            add_to(f, 3, [4, 5, 6]);
        });
        f.local_get(3);
    });
    b.finish(setup, unit, FRAMES, finish)
}

/// Each element is its own `Vec3` struct. The naive version copies the list each frame and replaces
/// each element with a new struct; the best case changes the structs in place.
pub fn gc_boxed(naive: bool) -> Vec<u8> {
    let mut b = Builder::new();
    let vec3 = b.struct_type(&[F64, F64, F64]);
    let ty = b.array_type(r(vec3));
    let list = b.global(r(ty), null(ty));
    let setup = b.func(&[], &[], &[I32, r(ty)], |f| {
        f.i32_const(N).array_new_default(ty).local_set(1);
        repeat(f, 0, To::Const(N), |f| {
            f.local_get(1).local_get(0);
            (0..3).for_each(|k| initial(f, 0, k));
            f.struct_new(vec3).array_set(ty);
        });
        f.local_get(1).global_set(list);
    });
    let unit = b.func(
        &[I32],
        &[],
        &[I32, r(ty), r(vec3), F64, F64, F64, F64],
        |f| {
            if naive {
                f.i32_const(N)
                    .array_new_default(ty)
                    .local_tee(2)
                    .i32_const(0);
                f.global_get(list)
                    .i32_const(0)
                    .i32_const(N)
                    .array_copy(ty, ty);
                f.local_get(2).global_set(list);
            } else {
                f.global_get(list).local_set(2);
            }
            repeat(f, 1, To::Const(N), |f| {
                f.local_get(2).local_get(1).array_get(ty).local_set(3);
                (0..3).for_each(|k| {
                    f.local_get(3).struct_get(vec3, k).local_set(4 + k);
                });
                step(f, [4, 5, 6], 7);
                if naive {
                    f.local_get(2)
                        .local_get(1)
                        .local_get(4)
                        .local_get(5)
                        .local_get(6)
                        .struct_new(vec3)
                        .array_set(ty);
                } else {
                    (0..3).for_each(|k| {
                        f.local_get(3).local_get(4 + k).struct_set(vec3, k);
                    });
                }
            });
        },
    );
    let finish = b.func(
        &[],
        &[F64],
        &[I32, r(ty), r(vec3), F64, F64, F64, F64],
        |f| {
            f.global_get(list).local_set(1);
            repeat(f, 0, To::Const(N), |f| {
                f.local_get(1).local_get(0).array_get(ty).local_set(2);
                (0..3).for_each(|k| {
                    f.local_get(2).struct_get(vec3, k).local_set(4 + k);
                });
                add_to(f, 3, [4, 5, 6]);
            });
            f.local_get(3);
        },
    );
    b.finish(setup, unit, FRAMES, finish)
}

/// The list is split into one f64 array per field. The naive version copies all three each frame.
pub fn gc_split(naive: bool) -> Vec<u8> {
    let mut b = Builder::new();
    let ty = b.array_type(F64);
    let lists = [0, 1, 2].map(|_| b.global(r(ty), null(ty)));
    let setup = b.func(&[], &[], &[I32], |f| {
        for g in lists {
            f.i32_const(N).array_new_default(ty).global_set(g);
        }
        repeat(f, 0, To::Const(N), |f| {
            for (k, g) in lists.into_iter().enumerate() {
                f.global_get(g).local_get(0);
                initial(f, 0, k as u64);
                f.array_set(ty);
            }
        });
    });
    let unit = b.func(
        &[I32],
        &[],
        &[I32, r(ty), r(ty), r(ty), F64, F64, F64, F64],
        |f| {
            for (k, g) in lists.into_iter().enumerate() {
                let a = 2 + k as u32;
                if naive {
                    f.i32_const(N)
                        .array_new_default(ty)
                        .local_tee(a)
                        .i32_const(0);
                    f.global_get(g).i32_const(0).i32_const(N).array_copy(ty, ty);
                    f.local_get(a).global_set(g);
                } else {
                    f.global_get(g).local_set(a);
                }
            }
            repeat(f, 1, To::Const(N), |f| {
                (0..3).for_each(|k| {
                    f.local_get(2 + k)
                        .local_get(1)
                        .array_get(ty)
                        .local_set(5 + k);
                });
                step(f, [5, 6, 7], 8);
                (0..3).for_each(|k| {
                    f.local_get(2 + k)
                        .local_get(1)
                        .local_get(5 + k)
                        .array_set(ty);
                });
            });
        },
    );
    let finish = b.func(&[], &[F64], &[I32, F64, F64, F64, F64], |f| {
        repeat(f, 0, To::Const(N), |f| {
            for (k, g) in lists.into_iter().enumerate() {
                f.global_get(g)
                    .local_get(0)
                    .array_get(ty)
                    .local_set(2 + k as u32);
            }
            add_to(f, 1, [2, 3, 4]);
        });
        f.local_get(1);
    });
    b.finish(setup, unit, FRAMES, finish)
}
