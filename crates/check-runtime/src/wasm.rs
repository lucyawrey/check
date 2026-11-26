use std::alloc::{Layout, alloc, dealloc, handle_alloc_error, realloc};
use std::ptr::{copy, copy_nonoverlapping};

use crate::Panic;

const HEADER: usize = 8;

#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    fn fd_write(fd: u32, iovs: *const [u32; 2], count: u32, written: *mut u32) -> u32;
}

#[link(wasm_import_module = "check")]
unsafe extern "C" {
    #[link_name = "panic"]
    fn host_panic(code: u32, at: u32, reason: Str);
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Str(*mut u8);

impl Str {
    fn layout(len: usize) -> Layout {
        Layout::from_size_align(HEADER + len, 4).expect("string size")
    }

    fn count(self) -> *mut u32 {
        self.0 as *mut u32
    }

    fn len(self) -> usize {
        unsafe { *(self.0.add(4) as *const u32) as usize }
    }

    fn bytes<'a>(self) -> &'a [u8] {
        unsafe { std::slice::from_raw_parts(self.0.add(HEADER), self.len()) }
    }

    fn from_parts(parts: &[&[u8]]) -> Str {
        let s = check_new_string(parts.iter().map(|p| p.len()).sum::<usize>() as u32);
        let mut at = HEADER;
        for part in parts {
            unsafe { copy_nonoverlapping(part.as_ptr(), s.0.add(at), part.len()) };
            at += part.len();
        }
        s
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn check_new_string(len: u32) -> Str {
    let layout = Str::layout(len as usize);
    unsafe {
        let s = alloc(layout);
        if s.is_null() {
            handle_alloc_error(layout);
        }
        *(s as *mut u32) = 1;
        *(s.add(4) as *mut u32) = len;
        Str(s)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn check_retain(s: Str) -> Str {
    if !s.0.is_null() {
        unsafe { *s.count() += 1 };
    }
    s
}

const BLOCK_ALIGN: usize = 8;
const LIST_HEADER: usize = 16;

fn block_layout(size: usize) -> Layout {
    Layout::from_size_align(size, BLOCK_ALIGN).expect("block size")
}

#[unsafe(no_mangle)]
pub extern "C" fn check_alloc(size: u32) -> *mut u8 {
    let layout = block_layout(size as usize);
    unsafe {
        let p = alloc(layout);
        if p.is_null() {
            handle_alloc_error(layout);
        }
        *(p as *mut u32) = 1;
        p
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn check_free(p: *mut u8, size: u32) {
    unsafe { dealloc(p, block_layout(size as usize)) };
}

#[unsafe(no_mangle)]
pub extern "C" fn check_copy(p: *mut u8, size: u32) -> *mut u8 {
    let copy = check_alloc(size);
    unsafe {
        copy_nonoverlapping(p.add(4), copy.add(4), size as usize - 4);
        *(p as *mut u32) -= 1;
    }
    copy
}

struct List(*mut u8);

impl List {
    fn field(&self, at: usize) -> *mut u32 {
        unsafe { self.0.add(at) as *mut u32 }
    }

    fn len(&self) -> usize {
        unsafe { *self.field(4) as usize }
    }

    fn cap(&self) -> usize {
        unsafe { *self.field(8) as usize }
    }

    fn set(&self, len: usize, cap: usize) {
        unsafe {
            *self.field(4) = len as u32;
            *self.field(8) = cap as u32;
        }
    }

    fn item(&self, stride: u32, i: usize) -> *mut u8 {
        unsafe { self.0.add(LIST_HEADER + stride as usize * i) }
    }
}

fn list_size(stride: u32, cap: usize) -> usize {
    LIST_HEADER + stride as usize * cap
}

#[unsafe(no_mangle)]
pub extern "C" fn check_list_new(stride: u32, len: u32) -> *mut u8 {
    let list = List(check_alloc(list_size(stride, len as usize) as u32));
    list.set(len as usize, len as usize);
    list.0
}

#[unsafe(no_mangle)]
pub extern "C" fn check_list_free(list: *mut u8, stride: u32) {
    let size = list_size(stride, List(list).cap());
    unsafe { dealloc(list, block_layout(size)) };
}

#[unsafe(no_mangle)]
pub extern "C" fn check_list_copy(list: *mut u8, stride: u32) -> *mut u8 {
    let old = List(list);
    let copy = List(check_list_new(stride, old.len() as u32));
    unsafe {
        copy_nonoverlapping(
            old.item(stride, 0),
            copy.item(stride, 0),
            list_size(stride, old.len()) - LIST_HEADER,
        );
        *(list as *mut u32) -= 1;
    }
    copy.0
}

#[unsafe(no_mangle)]
pub extern "C" fn check_list_open(list: *mut u8, stride: u32, at: u32) -> *mut u8 {
    let mut list = List(list);
    let len = list.len();
    if len == list.cap() {
        let cap = (len * 2).max(4);
        let old = block_layout(list_size(stride, len));
        list = List(unsafe { realloc(list.0, old, list_size(stride, cap)) });
        if list.0.is_null() {
            handle_alloc_error(block_layout(list_size(stride, cap)));
        }
        list.set(len, cap);
    }
    let at = at as usize;
    unsafe {
        copy(
            list.item(stride, at),
            list.item(stride, at + 1),
            (len - at) * stride as usize,
        )
    };
    list.set(len + 1, list.cap());
    list.0
}

#[unsafe(no_mangle)]
pub extern "C" fn check_list_close(list: *mut u8, stride: u32, at: u32) {
    let list = List(list);
    let (len, at) = (list.len(), at as usize);
    unsafe {
        copy(
            list.item(stride, at + 1),
            list.item(stride, at),
            (len - at - 1) * stride as usize,
        )
    };
    list.set(len - 1, list.cap());
}

#[unsafe(no_mangle)]
pub extern "C" fn check_release(s: Str) {
    if s.0.is_null() {
        return;
    }
    unsafe {
        *s.count() -= 1;
        if *s.count() == 0 {
            dealloc(s.0, Str::layout(s.len()));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn check_concat(a: Str, b: Str) -> Str {
    Str::from_parts(&[a.bytes(), b.bytes()])
}

#[unsafe(no_mangle)]
pub extern "C" fn check_str_eq(a: Str, b: Str) -> bool {
    a.bytes() == b.bytes()
}

#[unsafe(no_mangle)]
pub extern "C" fn check_str_cmp(a: Str, b: Str) -> i32 {
    a.bytes().cmp(b.bytes()) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn check_str_quote(s: Str) -> Str {
    let text = std::str::from_utf8(s.bytes()).expect("strings are UTF-8");
    Str::from_parts(&[crate::quote(text).as_bytes()])
}

#[unsafe(no_mangle)]
pub extern "C" fn check_int_text(n: i64) -> Str {
    Str::from_parts(&[n.to_string().as_bytes()])
}

#[unsafe(no_mangle)]
pub extern "C" fn check_float_text(f: f64) -> Str {
    Str::from_parts(&[crate::float_text(f).as_bytes()])
}

#[unsafe(no_mangle)]
pub extern "C" fn check_bool_text(b: bool) -> Str {
    Str::from_parts(&[if b { b"true" } else { b"false" }])
}

#[unsafe(no_mangle)]
pub extern "C" fn check_float_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

#[unsafe(no_mangle)]
pub extern "C" fn check_print(s: Str) {
    let newline = b'\n';
    let iovs = [
        [s.bytes().as_ptr() as u32, s.len() as u32],
        [&newline as *const u8 as u32, 1],
    ];
    for iov in &iovs {
        let mut written = 0;
        unsafe { fd_write(1, iov, 1, &mut written) };
    }
}

fn stop(p: Panic, at: u32) -> ! {
    check_panic(p as u32, at, Str(std::ptr::null_mut()))
}

#[unsafe(no_mangle)]
pub extern "C" fn check_panic(code: u32, at: u32, reason: Str) -> ! {
    unsafe { host_panic(code, at, reason) };
    core::arch::wasm32::unreachable()
}

#[unsafe(no_mangle)]
pub extern "C" fn check_int_neg(a: i64, at: u32) -> i64 {
    crate::neg(a).unwrap_or_else(|p| stop(p, at))
}

#[unsafe(no_mangle)]
pub extern "C" fn check_int_div(a: i64, b: i64, at: u32) -> f64 {
    crate::div(a, b).unwrap_or_else(|p| stop(p, at))
}

macro_rules! int_ops {
    ($($name:ident => $op:path),* $(,)?) => {$(
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(a: i64, b: i64, at: u32) -> i64 {
            $op(a, b).unwrap_or_else(|p| stop(p, at))
        }
    )*};
}

int_ops! {
    check_int_add => crate::add,
    check_int_sub => crate::sub,
    check_int_mul => crate::mul,
    check_int_floor_div => crate::floor_div,
    check_int_mod => crate::modulo,
    check_int_pow => crate::pow,
    check_int_shl => crate::shl,
    check_int_shr => crate::shr,
}
