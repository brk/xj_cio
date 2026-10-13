//! The `va_list` scanning entry points. Run with
//! `cargo +nightly test --features c_variadic`.
#![cfg(feature = "c_variadic")]
#![feature(c_variadic)]

use std::ffi::{CStr, c_char, c_int};

use xj_cio::*;

unsafe extern "C" fn file_scan(stream: FILE_ptr, fmt: *const c_char, ap: ...) -> c_int {
    // SAFETY: the callers pass a C string format and matching pointers.
    unsafe { vfscanf(stream, CStr::from_ptr(fmt), ap) }
}

unsafe extern "C" fn string_scan(input: *const c_char, fmt: *const c_char, ap: ...) -> c_int {
    // SAFETY: as for `file_scan`.
    unsafe { vsscanf(CStr::from_ptr(input), CStr::from_ptr(fmt), ap) }
}

fn stream_with(contents: &[u8]) -> FILE_ptr {
    let stream = tmpfile();
    assert!(!stream.is_null());
    // SAFETY: the source is a live byte slice of the given length.
    assert_eq!(
        unsafe { fwrite_raw(contents.as_ptr(), 1, contents.len(), stream) },
        contents.len()
    );
    rewind(stream);
    stream
}

#[test]
fn unsigned_short_is_stored_in_two_bytes() {
    #[repr(C)]
    struct Guarded {
        before: [u16; 2],
        value: u16,
        after: [u16; 4],
    }
    let mut g = Guarded {
        before: [0xa5a5; 2],
        value: 0,
        after: [0xa5a5; 4],
    };
    let (mut x, mut o) = (0u16, 0u16);
    // SAFETY: each pointer is a live `unsigned short`.
    let r = unsafe {
        string_scan(
            c"7 ff 17".as_ptr(),
            c"%hu %hx %ho".as_ptr(),
            &mut g.value,
            &mut x,
            &mut o,
        )
    };
    assert_eq!((r, g.value, x, o), (3, 7, 0xff, 0o17));
    assert_eq!((g.before, g.after), ([0xa5a5; 2], [0xa5a5; 4]));
}

#[test]
fn hh_does_not_stop_argument_collection() {
    let (mut c, mut d, mut u) = (0i8, 0i32, 0u8);
    let mut n = 0i8;
    // SAFETY: each pointer is a live local of its directive's type.
    let r = unsafe {
        string_scan(
            c"-7 8 200".as_ptr(),
            c"%hhd %d %hhu%hhn".as_ptr(),
            &mut c,
            &mut d,
            &mut u,
            &mut n,
        )
    };
    assert_eq!((r, c, d, u, n), (3, -7, 8, 200, 8));
}

#[test]
fn every_length_and_count_width() {
    let stream = stream_with(b"1 2 3 4 5 6 7 1.5 2.5 word xy");
    let (mut l, mut ll, mut j, mut z, mut t) = (0i64, 0i64, 0i64, 0usize, 0isize);
    let (mut i, mut ul) = (0i32, 0u64);
    let (mut f, mut d) = (0f32, 0f64);
    let mut word = [0u8; 8];
    let mut pair = [0u8; 2];
    let (mut n1, mut n2, mut n3) = (0i32, 0usize, 0i64);
    // SAFETY: each pointer is a live local of its directive's type, and the
    // byte buffers have room for their fields.
    let r = unsafe {
        file_scan(
            stream,
            c"%ld %lld %jd %zu %td %i %lu%n %f %lf %7s %2c%zn%jn".as_ptr(),
            &mut l,
            &mut ll,
            &mut j,
            &mut z,
            &mut t,
            &mut i,
            &mut ul,
            &mut n1,
            &mut f,
            &mut d,
            word.as_mut_ptr(),
            pair.as_mut_ptr(),
            &mut n2,
            &mut n3,
        )
    };
    assert_eq!(r, 11);
    assert_eq!((l, ll, j, z, t, i, ul), (1, 2, 3, 4, 5, 6, 7));
    assert_eq!((f, d), (1.5, 2.5));
    assert_eq!((&word[..5], &pair), (&b"word\0"[..], b"xy"));
    assert_eq!((n1, n2, n3), (13, 29, 29));
    assert_eq!(fclose(stream), 0);
}

#[test]
fn invalid_formats_pull_no_arguments() {
    let mut x = 5i32;
    errno::set(0);
    // SAFETY: the format is rejected before any argument is pulled.
    let r = unsafe {
        string_scan(
            c"1 2".as_ptr(),
            c"%d %p".as_ptr(),
            &mut x,
            std::ptr::null_mut::<u8>(),
        )
    };
    assert_eq!((r, x, errno::get()), (0, 5, libc::EINVAL));
}
