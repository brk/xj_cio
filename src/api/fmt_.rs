//! `printf` / `fprintf` / `vprintf` / `vfprintf` and the `scanf`
//! family. The translator marshals each variadic call into an `Arg`
//! / `OutArg` slice.

use std::ffi::CStr;

use xj_zstr::CStrDest;

use crate::errno;
use crate::format::{run_printf, run_scanf};
use crate::FILE_ptr;

pub use crate::format::{Arg, OutArg};

use super::ops;

pub fn fprintf(t: FILE_ptr, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    fprintfb(t, fmt.to_bytes(), args)
}

pub fn fprintfb(t: FILE_ptr, fmt_bytes: &[u8], args: &[Arg]) -> core::ffi::c_int {
    let r = ops::with_token(t, |s| run_printf(s, fmt_bytes, args));
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

/// `fscanf`. The slice is `&mut` because [`OutArg::Buf`] borrows its
/// destination mutably.
pub fn fscanf(t: FILE_ptr, fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let r = ops::with_token(t, |s| run_scanf(s, bytes, args));
    r.unwrap_or(-1)
}

pub fn vfscanf(t: FILE_ptr, fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    fscanf(t, fmt, args)
}

pub fn scanf(fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    fscanf(crate::api::stdio_::stdin(), fmt, args)
}

pub fn vscanf(fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    scanf(fmt, args)
}

/// Safe sibling of `snprintf` for runtime (non-literal) formats.
/// `dst.c_size()` plays the role of C's `n`. Writes as much of the
/// output as fits, terminates it, and returns the full output length.
/// Literal formats should use `xj_cfmt::csnprintf!` instead.
pub fn snprintf_(mut dst: CStrDest<'_>, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    crate::format::format_to_vec(&mut out, bytes, args);
    dst.write_truncated(&out);
    out.len() as libc::c_int
}

/// `sprintf`-style formatter into a caller-provided buffer.
/// Returns the number of bytes that *would have been* written
/// (excluding the NUL terminator).
pub fn snprintf(buf: *mut libc::c_char, n: libc::size_t, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    crate::format::format_to_vec(&mut out, bytes, args);
    if !buf.is_null() && n > 0 {
        // Only the bytes actually written are borrowed, so `sprintf`'s
        // unbounded `n` never forms an oversized slice.
        let len = n.min(out.len() + 1);
        let region = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len) };
        CStrDest::from(region).write_truncated(&out);
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
