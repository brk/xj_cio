//! `printf` / `fprintf` / `vprintf` / `vfprintf` and the `scanf`
//! family. The translator marshals each variadic call into an `Arg`
//! / `OutArg` slice.

use std::ffi::CStr;

use crate::errno;
use crate::format::{run_printf, run_scanf};
use crate::FILE_ptr;

pub use crate::format::{Arg, OutArg};

use super::ops;

pub fn fprintf(t: FILE_ptr, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let r = ops::with_token(t, |s| run_printf(s, bytes, args));
    r.unwrap_or(-1)
}

pub fn vfprintf(t: FILE_ptr, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    fprintf(t, fmt, args)
}

pub fn printf(fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    fprintf(crate::api::stdio_::stdout(), fmt, args)
}

pub fn vprintf(fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    printf(fmt, args)
}

pub fn fscanf(t: FILE_ptr, fmt: &CStr, args: &[OutArg]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let r = ops::with_token(t, |s| run_scanf(s, bytes, args));
    r.unwrap_or(-1)
}

pub fn vfscanf(t: FILE_ptr, fmt: &CStr, args: &[OutArg]) -> core::ffi::c_int {
    fscanf(t, fmt, args)
}

pub fn scanf(fmt: &CStr, args: &[OutArg]) -> core::ffi::c_int {
    fscanf(crate::api::stdio_::stdin(), fmt, args)
}

pub fn vscanf(fmt: &CStr, args: &[OutArg]) -> core::ffi::c_int {
    scanf(fmt, args)
}

/// `sprintf`-style formatter into a caller-provided buffer.
/// Returns the number of bytes that *would have been* written
/// (excluding the NUL terminator).
pub fn snprintf(buf: *mut libc::c_char, n: libc::size_t, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    crate::format::format_to_vec(&mut out, bytes, args);
    if !buf.is_null() && n > 0 {
        let copy = (n as usize - 1).min(out.len());
        unsafe {
            for i in 0..copy {
                *buf.add(i) = out[i] as libc::c_char;
            }
            *buf.add(copy) = 0;
        }
    }
    out.len() as libc::c_int
}

pub fn sprintf(buf: *mut libc::c_char, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    snprintf(buf, libc::size_t::MAX, fmt, args)
}

/// Suppress unused-warning for errno when format engine doesn't need
/// it directly here.
#[allow(dead_code)]
fn _touch_errno() {
    errno::get();
}
