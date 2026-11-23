use std::alloc::{Layout, alloc};

const HEADER: usize = 8;

#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    fn fd_write(fd: u32, iovs: *const [u32; 2], count: u32, written: *mut u32) -> u32;
}

unsafe fn len(s: *const u8) -> usize {
    unsafe { *(s.add(4) as *const u32) as usize }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn check_new_string(len: u32) -> *mut u8 {
    let layout = Layout::from_size_align(HEADER + len as usize, 4).expect("string size");
    unsafe {
        let s = alloc(layout);
        *(s as *mut u32) = 1;
        *(s.add(4) as *mut u32) = len;
        s
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn check_concat(a: *const u8, b: *const u8) -> *mut u8 {
    unsafe {
        let (la, lb) = (len(a), len(b));
        let s = check_new_string((la + lb) as u32);
        std::ptr::copy_nonoverlapping(a.add(HEADER), s.add(HEADER), la);
        std::ptr::copy_nonoverlapping(b.add(HEADER), s.add(HEADER + la), lb);
        s
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn check_print(s: *const u8) {
    let newline = b'\n';
    unsafe {
        let iovs = [
            [s.add(HEADER) as u32, len(s) as u32],
            [&newline as *const u8 as u32, 1],
        ];
        for iov in &iovs {
            let mut written = 0;
            fd_write(1, iov, 1, &mut written);
        }
    }
}
