use crate::builder::*;
use wasm_encoder::ConstExpr;

// Free-list heads, one per power-of-two size class, sit at HEAP_BASE; blocks follow them.
// A block is [class i32][count i32] then the payload; a free block keeps its next link in the count's place.
const HEADS: i32 = HEAP_BASE;

pub struct Rc {
    pub alloc: u32,
    pub free: u32,
    /// Releases an object that holds no counted references.
    pub release: u32,
}

impl Rc {
    pub fn new(b: &mut Builder) -> Rc {
        let top = b.global(I32, ConstExpr::i32_const(HEAP_BASE + 32 * 4));
        // Takes a payload size and returns a payload pointer with a count of 1. The payload isn't zeroed.
        let alloc = b.func(&[I32], &[I32], &[I32, I32, I32], |f| {
            f.i32_const(32)
                .local_get(0)
                .i32_const(7)
                .i32_add()
                .i32_clz()
                .i32_sub()
                .local_set(1);
            f.local_get(1)
                .i32_const(2)
                .i32_shl()
                .i32_const(HEADS)
                .i32_add()
                .local_tee(2);
            f.i32_load(m(0, 2)).local_tee(3).if_(Empty);
            f.local_get(2)
                .local_get(3)
                .i32_load(m(4, 2))
                .i32_store(m(0, 2));
            f.else_();
            f.global_get(top)
                .local_tee(3)
                .i32_const(1)
                .local_get(1)
                .i32_shl()
                .i32_add()
                .global_set(top);
            f.global_get(top)
                .i32_const(65535)
                .i32_add()
                .i32_const(16)
                .i32_shr_u()
                .memory_size(0)
                .i32_sub();
            f.local_tee(2)
                .i32_const(0)
                .i32_gt_s()
                .if_(Empty)
                .local_get(2)
                .memory_grow(0)
                .drop()
                .end();
            f.end();
            f.local_get(3).local_get(1).i32_store(m(0, 2));
            f.local_get(3).i32_const(1).i32_store(m(4, 2));
            f.local_get(3).i32_const(8).i32_add();
        });
        let free = b.func(&[I32], &[], &[I32, I32], |f| {
            f.local_get(0).i32_const(8).i32_sub().local_tee(1);
            f.i32_load(m(0, 2))
                .i32_const(2)
                .i32_shl()
                .i32_const(HEADS)
                .i32_add()
                .local_set(2);
            f.local_get(1)
                .local_get(2)
                .i32_load(m(0, 2))
                .i32_store(m(4, 2));
            f.local_get(2).local_get(1).i32_store(m(0, 2));
        });
        let release = b.func(&[I32], &[], &[I32], |f| {
            release_count(f, 0, 1, |f| {
                f.local_get(0).call(free);
            })
        });
        Rc {
            alloc,
            free,
            release,
        }
    }
}

/// Decrements the count of the non-null object in local `p` and runs `on_zero` when it reaches zero.
pub fn release_count(f: &mut Sink, p: u32, scratch: u32, on_zero: impl FnOnce(&mut Sink)) {
    f.local_get(p)
        .i32_const(4)
        .i32_sub()
        .i32_load(m(0, 2))
        .i32_const(1)
        .i32_sub()
        .local_tee(scratch);
    f.if_(Empty)
        .local_get(p)
        .i32_const(4)
        .i32_sub()
        .local_get(scratch)
        .i32_store(m(0, 2))
        .else_();
    on_zero(f);
    f.end();
}

pub fn retain(f: &mut Sink, p: u32) {
    f.local_get(p).i32_const(4).i32_sub();
    f.local_get(p)
        .i32_const(4)
        .i32_sub()
        .i32_load(m(0, 2))
        .i32_const(1)
        .i32_add()
        .i32_store(m(0, 2));
}

/// The copy-on-write check before writing to the object in local `p`. The copy path never runs in these
/// tests, so it traps instead of copying; the check's cost is what's measured.
pub fn unshare(f: &mut Sink, p: u32) {
    f.local_get(p)
        .i32_const(4)
        .i32_sub()
        .i32_load(m(0, 2))
        .i32_const(1)
        .i32_gt_u();
    f.if_(Empty).unreachable().end();
}
