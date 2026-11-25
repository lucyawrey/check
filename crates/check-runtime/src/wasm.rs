use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::ptr::copy_nonoverlapping;

use crate::Panic;

const HEADER: usize = 8;

#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    fn fd_write(fd: u32, iovs: *const [u32; 2], count: u32, written: *mut u32) -> u32;
}

#[link(wasm_import_module = "check")]
unsafe extern "C" {
    #[link_name = "panic"]
    fn host_panic(code: u32, at: u32);
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
    unsafe { *s.count() += 1 };
    s
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
    unsafe { host_panic(p as u32, at) };
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
