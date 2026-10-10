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

/// # Safety
///
/// `args` must meet the requirements in [`Arg`]'s "Pointer validity"
/// section for the conversions in `fmt`.
pub unsafe fn fprintf(t: FILE_ptr, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { fprintfb(t, fmt.to_bytes(), args) }
}

/// `fprintf` with the format as a byte slice (no NUL required).
///
/// # Safety
///
/// `args` must meet the requirements in [`Arg`]'s "Pointer validity"
/// section for the conversions in `fmt_bytes`.
pub unsafe fn fprintfb(t: FILE_ptr, fmt_bytes: &[u8], args: &[Arg]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    let r = ops::with_token(t, |s| unsafe { run_printf(s, fmt_bytes, args) });
    r.unwrap_or(-1)
}

/// # Safety
///
/// `args` must meet the requirements in [`Arg`]'s "Pointer validity"
/// section for the conversions in `fmt`.
pub unsafe fn vfprintf(t: FILE_ptr, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { fprintf(t, fmt, args) }
}

/// # Safety
///
/// `args` must meet the requirements in [`Arg`]'s "Pointer validity"
/// section for the conversions in `fmt`.
pub unsafe fn printf(fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { fprintf(crate::api::stdio_::stdout(), fmt, args) }
}

/// # Safety
///
/// `args` must meet the requirements in [`Arg`]'s "Pointer validity"
/// section for the conversions in `fmt`.
pub unsafe fn vprintf(fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { printf(fmt, args) }
}

/// `fscanf`. The slice is `&mut` because [`OutArg::Buf`] borrows its
/// destination mutably.
///
/// # Safety
///
/// `args` must meet the requirements in [`OutArg`]'s "Pointer
/// validity" section for the conversions in `fmt`.
pub unsafe fn fscanf(t: FILE_ptr, fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    // SAFETY: forwarded from the caller.
    let r = ops::with_token(t, |s| unsafe { run_scanf(s, bytes, args) });
    r.unwrap_or(-1)
}

/// # Safety
///
/// `args` must meet the requirements in [`OutArg`]'s "Pointer
/// validity" section for the conversions in `fmt`.
pub unsafe fn vfscanf(t: FILE_ptr, fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { fscanf(t, fmt, args) }
}

/// # Safety
///
/// `args` must meet the requirements in [`OutArg`]'s "Pointer
/// validity" section for the conversions in `fmt`.
pub unsafe fn scanf(fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { fscanf(crate::api::stdio_::stdin(), fmt, args) }
}

/// # Safety
///
/// `args` must meet the requirements in [`OutArg`]'s "Pointer
/// validity" section for the conversions in `fmt`.
pub unsafe fn vscanf(fmt: &CStr, args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { scanf(fmt, args) }
}

/// Safe sibling of `snprintf` for runtime (non-literal) formats.
/// `dst.c_size()` plays the role of C's `n`. Writes as much of the
/// output as fits, terminates it, and returns the full output length.
/// Literal formats should use `xj_cfmt::csnprintf!` instead.
///
/// # Safety
///
/// `args` must meet the requirements in [`Arg`]'s "Pointer validity"
/// section for the conversions in `fmt`. (The destination itself is
/// checked; only pointer-carrying `args` make this unsafe.)
pub unsafe fn snprintf_(mut dst: CStrDest<'_>, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    // SAFETY: forwarded from the caller.
    unsafe { crate::format::format_to_vec(&mut out, bytes, args) };
    dst.write_truncated(&out);
    out.len() as libc::c_int
}

/// `sprintf`-style formatter into a caller-provided buffer.
/// Returns the number of bytes that *would have been* written
/// (excluding the NUL terminator).
///
/// # Safety
///
/// - `args` must meet the requirements in [`Arg`]'s "Pointer validity"
///   section for the conversions in `fmt`.
/// - If `buf` is non-null and `n > 0`, `buf` must be valid for reads
///   and writes of `min(n, len + 1)` initialized bytes, where `len` is
///   the formatted length (the return value). A buffer of `n` such
///   bytes always suffices. The region must not be accessed through any
///   other pointer or reference, including `fmt`, during the call.
pub unsafe fn snprintf(buf: *mut libc::c_char, n: libc::size_t, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    let bytes = fmt.to_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    // SAFETY: forwarded from the caller.
    unsafe { crate::format::format_to_vec(&mut out, bytes, args) };
    if !buf.is_null() && n > 0 {
        // Only the bytes actually written are borrowed, so `sprintf`'s
        // unbounded `n` never forms an oversized slice.
        let len = n.min(out.len() + 1);
        // SAFETY: `buf` is non-null with `n > 0`, so the caller
        // guarantees `len == min(n, out.len() + 1)` initialized,
        // unaliased bytes. All `args` accesses (including `%n` writes)
        // finished above, before this borrow begins.
        let region = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len) };
        CStrDest::from(region).write_truncated(&out);
    }
    out.len() as libc::c_int
}

/// `sprintf(buf, fmt, ...)`: `snprintf` with an unbounded size.
///
/// # Safety
///
/// As for [`snprintf`] with `n = SIZE_MAX`. In particular, a non-null
/// `buf` must be valid for `len + 1` bytes, where `len` is the
/// formatted length.
pub unsafe fn sprintf(buf: *mut libc::c_char, fmt: &CStr, args: &[Arg]) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { snprintf(buf, libc::size_t::MAX, fmt, args) }
}

/// Suppress unused-warning for errno when format engine doesn't need
/// it directly here.
#[allow(dead_code)]
fn _touch_errno() {
    errno::get();
}
