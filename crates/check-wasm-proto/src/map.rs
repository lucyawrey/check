//! Inserts and updates small records in an insertion-ordered map with `Int` keys, laid out as
//! indexmap is: entries in insertion order, plus an open-addressing table of entry indexes plus one
//! (0 is an empty slot) that is twice the entries' capacity.

use crate::builder::*;
use crate::rc::{self, Rc};
use wasm_encoder::ConstExpr;

const KEYS: i64 = 100_000;
const OPS: i32 = 200_000;
const BATCH: i32 = 1000;
const START_MASK: i32 = 15;

/// Op `i`'s key from local `i`, cycling through every key so the first KEYS ops insert and the rest update.
fn key(f: &mut Sink, i: u32) {
    f.local_get(i)
        .i64_extend_i32_u()
        .i64_const(7919)
        .i64_mul()
        .i64_const(KEYS)
        .i64_rem_u();
}

fn amount(f: &mut Sink, i: u32) {
    f.local_get(i)
        .f64_convert_i32_s()
        .f64_const(0.5.into())
        .f64_mul();
}

/// Turns the i64 key on the stack into an i32 hash.
fn hash(f: &mut Sink) {
    f.i64_const(0x9E37_79B9_7F4A_7C15_u64 as i64)
        .i64_mul()
        .i64_const(32)
        .i64_shr_u()
        .i32_wrap_i64();
}

/// Sets `op` to the op index for step `j` of batch `batch`, and `amt` to its amount.
fn op(f: &mut Sink, batch: u32, j: u32, op: u32, amt: u32) {
    f.local_get(batch)
        .i32_const(BATCH)
        .i32_mul()
        .local_get(j)
        .i32_add()
        .local_set(op);
    amount(f, op);
    f.local_set(amt);
}

/// Adds the entry in locals key, count, total to the f64 sum in local `acc`.
fn add_to(f: &mut Sink, acc: u32, key: u32, count: u32, total: u32) {
    f.local_get(acc)
        .local_get(key)
        .f64_convert_i64_s()
        .local_get(count)
        .f64_convert_i64_s();
    f.f64_const(3.0.into())
        .f64_mul()
        .f64_add()
        .local_get(total)
        .f64_add()
        .f64_add()
        .local_set(acc);
}

/// The map is [len][mask][entries][table]; each entry is a key, then the record's count and total inline.
pub fn rc() -> Vec<u8> {
    let mut b = Builder::new();
    let rc = Rc::new(&mut b);
    let map = b.global(I32, ConstExpr::i32_const(0));
    let entry = |f: &mut Sink, entries: u32, index: u32| {
        f.local_get(entries)
            .local_get(index)
            .i32_const(24)
            .i32_mul()
            .i32_add();
    };
    // (table, mask, entries, key) -> the key's slot, or the empty slot where it goes.
    let probe = b.func(&[I32, I32, I32, I64], &[I32], &[I32, I32], |f| {
        f.local_get(3);
        hash(f);
        f.local_get(1).i32_and().local_set(4).loop_(Empty);
        f.local_get(0)
            .local_get(4)
            .i32_const(2)
            .i32_shl()
            .i32_add()
            .i32_load(m(0, 2))
            .local_tee(5);
        f.i32_eqz().if_(Empty).local_get(4).return_().end();
        f.local_get(5).i32_const(1).i32_sub().local_set(5);
        entry(f, 2, 5);
        f.i64_load(m(0, 3))
            .local_get(3)
            .i64_eq()
            .if_(Empty)
            .local_get(4)
            .return_()
            .end();
        f.local_get(4)
            .i32_const(1)
            .i32_add()
            .local_get(1)
            .i32_and()
            .local_set(4)
            .br(0);
        f.end().unreachable();
    });
    let new_table = b.func(&[I32], &[I32], &[I32], |f| {
        f.local_get(0)
            .i32_const(1)
            .i32_add()
            .i32_const(4)
            .i32_mul()
            .local_tee(0)
            .call(rc.alloc)
            .local_tee(1);
        f.i32_const(0).local_get(0).memory_fill(0).local_get(1);
    });
    let setup = b.func(&[], &[], &[I32], |f| {
        f.i32_const(16)
            .call(rc.alloc)
            .local_tee(0)
            .i32_const(0)
            .i32_store(m(0, 2));
        f.local_get(0).i32_const(START_MASK).i32_store(m(4, 2));
        f.local_get(0)
            .i32_const((START_MASK + 1) / 2 * 24)
            .call(rc.alloc)
            .i32_store(m(8, 2));
        f.local_get(0)
            .i32_const(START_MASK)
            .call(new_table)
            .i32_store(m(12, 2));
        f.local_get(0).global_set(map);
    });
    // Doubles the map's capacity: param map; locals mask, len, entries, table, i, old.
    let grow = b.func(&[I32], &[], &[I32, I32, I32, I32, I32, I32], |f| {
        f.local_get(0)
            .i32_load(m(4, 2))
            .i32_const(1)
            .i32_shl()
            .i32_const(1)
            .i32_add()
            .local_set(1);
        f.local_get(0).i32_load(m(0, 2)).local_set(2);
        f.local_get(1)
            .i32_const(1)
            .i32_add()
            .i32_const(1)
            .i32_shr_u()
            .i32_const(24)
            .i32_mul();
        f.call(rc.alloc).local_tee(3);
        f.local_get(0)
            .i32_load(m(8, 2))
            .local_tee(6)
            .local_get(2)
            .i32_const(24)
            .i32_mul()
            .memory_copy(0, 0);
        f.local_get(6).call(rc.release);
        f.local_get(0).i32_load(m(12, 2)).call(rc.release);
        f.local_get(1).call(new_table).local_set(4);
        repeat(f, 5, To::Local(2), |f| {
            f.local_get(4).local_get(4).local_get(1).local_get(3);
            entry(f, 3, 5);
            f.i64_load(m(0, 3))
                .call(probe)
                .i32_const(2)
                .i32_shl()
                .i32_add();
            f.local_get(5).i32_const(1).i32_add().i32_store(m(0, 2));
        });
        f.local_get(0).local_get(1).i32_store(m(4, 2));
        f.local_get(0).local_get(3).i32_store(m(8, 2));
        f.local_get(0).local_get(4).i32_store(m(12, 2));
    });
    // Param batch; locals j, op, map, key, slot, entry, amount, len.
    let unit = b.func(
        &[I32],
        &[],
        &[I32, I32, I32, I64, I32, I32, F64, I32],
        |f| {
            f.global_get(map).local_set(3);
            repeat(f, 1, To::Const(BATCH), |f| {
                op(f, 0, 1, 2, 7);
                key(f, 2);
                f.local_set(4);
                rc::unshare(f, 3);
                let find = |f: &mut Sink| {
                    f.local_get(3)
                        .i32_load(m(12, 2))
                        .local_get(3)
                        .i32_load(m(4, 2))
                        .local_get(3)
                        .i32_load(m(8, 2));
                    f.local_get(4).call(probe).local_set(5);
                };
                find(f);
                f.local_get(3)
                    .i32_load(m(12, 2))
                    .local_get(5)
                    .i32_const(2)
                    .i32_shl()
                    .i32_add()
                    .i32_load(m(0, 2));
                f.local_tee(6).if_(Empty);
                f.local_get(3)
                    .i32_load(m(8, 2))
                    .local_get(6)
                    .i32_const(1)
                    .i32_sub()
                    .i32_const(24)
                    .i32_mul()
                    .i32_add();
                f.local_tee(6)
                    .local_get(6)
                    .i64_load(m(8, 3))
                    .i64_const(1)
                    .i64_add()
                    .i64_store(m(8, 3));
                f.local_get(6)
                    .local_get(6)
                    .f64_load(m(16, 3))
                    .local_get(7)
                    .f64_add()
                    .f64_store(m(16, 3));
                f.else_();
                f.local_get(3).i32_load(m(0, 2)).local_tee(8);
                f.local_get(3)
                    .i32_load(m(4, 2))
                    .i32_const(1)
                    .i32_add()
                    .i32_const(1)
                    .i32_shr_u()
                    .i32_eq();
                f.if_(Empty).local_get(3).call(grow);
                find(f);
                f.end();
                f.local_get(3)
                    .i32_load(m(8, 2))
                    .local_get(8)
                    .i32_const(24)
                    .i32_mul()
                    .i32_add()
                    .local_tee(6);
                f.local_get(4).i64_store(m(0, 3));
                f.local_get(6).i64_const(1).i64_store(m(8, 3));
                f.local_get(6).local_get(7).f64_store(m(16, 3));
                f.local_get(3)
                    .i32_load(m(12, 2))
                    .local_get(5)
                    .i32_const(2)
                    .i32_shl()
                    .i32_add();
                f.local_get(8).i32_const(1).i32_add().i32_store(m(0, 2));
                f.local_get(3)
                    .local_get(8)
                    .i32_const(1)
                    .i32_add()
                    .i32_store(m(0, 2));
                f.end();
            });
        },
    );
    // Locals i, entry, key, count, total, acc.
    let finish = b.func(&[], &[F64], &[I32, I32, I64, I64, F64, F64, I32], |f| {
        f.global_get(map).i32_load(m(8, 2)).local_set(6);
        repeat(f, 0, To::Const(KEYS as i32), |f| {
            entry(f, 6, 0);
            f.local_tee(1).i64_load(m(0, 3)).local_set(2);
            f.local_get(1).i64_load(m(8, 3)).local_set(3);
            f.local_get(1).f64_load(m(16, 3)).local_set(4);
            add_to(f, 5, 2, 3, 4);
        });
        f.local_get(5);
    });
    b.finish(setup, unit, OPS / BATCH, finish)
}

/// The map is a struct of three arrays (keys, records, table), and each record is its own struct.
/// The naive version copies the map at the start of each batch and replaces a record to update it;
/// the best case updates records in place.
pub fn gc(naive: bool) -> Vec<u8> {
    let mut b = Builder::new();
    let rec = b.struct_type(&[I64, F64]);
    let keys = b.array_type(I64);
    let recs = b.array_type(r(rec));
    let table = b.array_type(I32);
    let ty = b.struct_type(&[I32, I32, r(keys), r(recs), r(table)]);
    let (len, mask, keys_f, recs_f, table_f) = (0, 1, 2, 3, 4);
    let map = b.global(r(ty), null(ty));
    let probe = b.func(&[r(table), I32, r(keys), I64], &[I32], &[I32, I32], |f| {
        f.local_get(3);
        hash(f);
        f.local_get(1).i32_and().local_set(4).loop_(Empty);
        f.local_get(0).local_get(4).array_get(table).local_tee(5);
        f.i32_eqz().if_(Empty).local_get(4).return_().end();
        f.local_get(2)
            .local_get(5)
            .i32_const(1)
            .i32_sub()
            .array_get(keys);
        f.local_get(3)
            .i64_eq()
            .if_(Empty)
            .local_get(4)
            .return_()
            .end();
        f.local_get(4)
            .i32_const(1)
            .i32_add()
            .local_get(1)
            .i32_and()
            .local_set(4)
            .br(0);
        f.end().unreachable();
    });
    let setup = b.func(&[], &[], &[], |f| {
        f.i32_const(0).i32_const(START_MASK);
        f.i32_const((START_MASK + 1) / 2).array_new_default(keys);
        f.i32_const((START_MASK + 1) / 2).array_new_default(recs);
        f.i32_const(START_MASK + 1).array_new_default(table);
        f.struct_new(ty).global_set(map);
    });
    // Copies array field `field` of the map in local `from` onto the stack.
    let copy = |f: &mut Sink, from: u32, field: u32, array: u32, tmp: u32| {
        f.local_get(from)
            .struct_get(ty, field)
            .array_len()
            .array_new_default(array)
            .local_tee(tmp);
        f.i32_const(0)
            .local_get(from)
            .struct_get(ty, field)
            .i32_const(0);
        f.local_get(from)
            .struct_get(ty, field)
            .array_len()
            .array_copy(array, array)
            .local_get(tmp);
    };
    // Doubles the map's capacity: param map; locals mask, len, keys, records, table, i.
    let grow = b.func(
        &[r(ty)],
        &[],
        &[I32, I32, r(keys), r(recs), r(table), I32],
        |f| {
            f.local_get(0)
                .struct_get(ty, mask)
                .i32_const(1)
                .i32_shl()
                .i32_const(1)
                .i32_add()
                .local_set(1);
            f.local_get(0).struct_get(ty, len).local_set(2);
            for (field, array, local) in [(keys_f, keys, 3), (recs_f, recs, 4)] {
                f.local_get(1)
                    .i32_const(1)
                    .i32_add()
                    .i32_const(1)
                    .i32_shr_u()
                    .array_new_default(array)
                    .local_tee(local);
                f.i32_const(0)
                    .local_get(0)
                    .struct_get(ty, field)
                    .i32_const(0)
                    .local_get(2)
                    .array_copy(array, array);
            }
            f.local_get(1)
                .i32_const(1)
                .i32_add()
                .array_new_default(table)
                .local_set(5);
            repeat(f, 6, To::Local(2), |f| {
                f.local_get(5)
                    .local_get(5)
                    .local_get(1)
                    .local_get(3)
                    .local_get(3)
                    .local_get(6)
                    .array_get(keys);
                f.call(probe)
                    .local_get(6)
                    .i32_const(1)
                    .i32_add()
                    .array_set(table);
            });
            f.local_get(0).local_get(1).struct_set(ty, mask);
            f.local_get(0).local_get(3).struct_set(ty, keys_f);
            f.local_get(0).local_get(4).struct_set(ty, recs_f);
            f.local_get(0).local_get(5).struct_set(ty, table_f);
        },
    );
    // Param batch; locals j, op, map, key, slot, entry, amount, len, record, three copy temps.
    let locals = [
        I32,
        I32,
        r(ty),
        I64,
        I32,
        I32,
        F64,
        I32,
        r(rec),
        r(keys),
        r(recs),
        r(table),
    ];
    let unit = b.func(&[I32], &[], &locals, |f| {
        if naive {
            f.global_get(map).local_set(3);
            f.local_get(3)
                .struct_get(ty, len)
                .local_get(3)
                .struct_get(ty, mask);
            copy(f, 3, keys_f, keys, 10);
            copy(f, 3, recs_f, recs, 11);
            copy(f, 3, table_f, table, 12);
            f.struct_new(ty).local_tee(3).global_set(map);
        } else {
            f.global_get(map).local_set(3);
        }
        repeat(f, 1, To::Const(BATCH), |f| {
            op(f, 0, 1, 2, 7);
            key(f, 2);
            f.local_set(4);
            let find = |f: &mut Sink| {
                f.local_get(3)
                    .struct_get(ty, table_f)
                    .local_get(3)
                    .struct_get(ty, mask);
                f.local_get(3)
                    .struct_get(ty, keys_f)
                    .local_get(4)
                    .call(probe)
                    .local_set(5);
            };
            find(f);
            f.local_get(3)
                .struct_get(ty, table_f)
                .local_get(5)
                .array_get(table)
                .local_tee(6)
                .if_(Empty);
            f.local_get(3)
                .struct_get(ty, recs_f)
                .local_get(6)
                .i32_const(1)
                .i32_sub()
                .local_tee(6);
            f.array_get(recs).local_set(9);
            if naive {
                f.local_get(3).struct_get(ty, recs_f).local_get(6);
                f.local_get(9).struct_get(rec, 0).i64_const(1).i64_add();
                f.local_get(9).struct_get(rec, 1).local_get(7).f64_add();
                f.struct_new(rec).array_set(recs);
            } else {
                f.local_get(9)
                    .local_get(9)
                    .struct_get(rec, 0)
                    .i64_const(1)
                    .i64_add()
                    .struct_set(rec, 0);
                f.local_get(9)
                    .local_get(9)
                    .struct_get(rec, 1)
                    .local_get(7)
                    .f64_add()
                    .struct_set(rec, 1);
            }
            f.else_();
            f.local_get(3).struct_get(ty, len).local_tee(8);
            f.local_get(3)
                .struct_get(ty, mask)
                .i32_const(1)
                .i32_add()
                .i32_const(1)
                .i32_shr_u()
                .i32_eq();
            f.if_(Empty).local_get(3).call(grow);
            find(f);
            f.end();
            f.local_get(3)
                .struct_get(ty, keys_f)
                .local_get(8)
                .local_get(4)
                .array_set(keys);
            f.local_get(3)
                .struct_get(ty, recs_f)
                .local_get(8)
                .i64_const(1)
                .local_get(7)
                .struct_new(rec);
            f.array_set(recs);
            f.local_get(3)
                .struct_get(ty, table_f)
                .local_get(5)
                .local_get(8)
                .i32_const(1)
                .i32_add();
            f.array_set(table);
            f.local_get(3)
                .local_get(8)
                .i32_const(1)
                .i32_add()
                .struct_set(ty, len);
            f.end();
        });
    });
    // Locals i, record, key, count, total, acc.
    let finish = b.func(&[], &[F64], &[I32, r(rec), I64, I64, F64, F64], |f| {
        repeat(f, 0, To::Const(KEYS as i32), |f| {
            f.global_get(map)
                .struct_get(ty, keys_f)
                .local_get(0)
                .array_get(keys)
                .local_set(2);
            f.global_get(map)
                .struct_get(ty, recs_f)
                .local_get(0)
                .array_get(recs)
                .local_tee(1);
            f.struct_get(rec, 0)
                .local_set(3)
                .local_get(1)
                .struct_get(rec, 1)
                .local_set(4);
            add_to(f, 5, 2, 3, 4);
        });
        f.local_get(5);
    });
    b.finish(setup, unit, OPS / BATCH, finish)
}
