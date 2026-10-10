//! Pre-parsed C printing calls and the `scanf` family.
//! Printing macros and plan APIs use xj_cfmt's common renderer; scanning
//! retains its separate grammar and `OutArg` destinations.

use std::ffi::CStr;

use crate::FILE_ptr;
use crate::errno;
use crate::format::run_scanf;

pub use crate::format::OutArg;
pub use xj_cfmt::printf::Arg;

use super::ops;

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

/// Reports a shared parser failure using the printing profile's errno policy.
pub fn format_error(e: xj_cfmt::cformat::Error) -> i32 {
    errno::set(xj_cfmt::printf::Error::Format(e).errno());
    -1
}
/// Execute a pre-parsed call, including a va_list call.
/// # Safety
/// Raw arguments must satisfy the common renderer's contracts.
pub unsafe fn fprintf_plan(
    t: FILE_ptr,
    plan: &xj_cfmt::cformat::Plan<'_>,
    args: &[Arg<'_>],
) -> i32 {
    crate::emit_c(t, unsafe { xj_cfmt::printf::render_plan(plan, args) })
}
/// # Safety
/// As for [`fprintf_plan`].
pub unsafe fn printf_plan(plan: &xj_cfmt::cformat::Plan<'_>, args: &[Arg<'_>]) -> i32 {
    unsafe { fprintf_plan(crate::stdout(), plan, args) }
}
/// # Safety
/// Raw arguments must satisfy the common renderer's contracts. `buf` must
/// be writable for `min(size, rendered_length + 1)` bytes. A zero size writes
/// nothing and permits a null buffer.
pub unsafe fn snprintf_plan(
    buf: *mut u8,
    size: usize,
    plan: &xj_cfmt::cformat::Plan<'_>,
    args: &[Arg<'_>],
) -> i32 {
    unsafe {
        xj_cfmt::printf::finish_raw(buf, Some(size), xj_cfmt::printf::render_plan(plan, args))
    }
}
/// # Safety
/// Raw arguments must satisfy the common renderer's contracts. `buf` must
/// be writable for the complete rendered output and its NUL terminator.
pub unsafe fn sprintf_plan(
    buf: *mut u8,
    plan: &xj_cfmt::cformat::Plan<'_>,
    args: &[Arg<'_>],
) -> i32 {
    unsafe { xj_cfmt::printf::finish_raw(buf, None, xj_cfmt::printf::render_plan(plan, args)) }
}

/// A pre-parsed call into guided storage.
/// # Safety
/// Raw arguments must satisfy the common renderer's contracts.
pub unsafe fn snprintf_plan_<D: xj_cfmt::printf::Destination + ?Sized>(
    dest: &mut D,
    size: usize,
    plan: &xj_cfmt::cformat::Plan<'_>,
    args: &[Arg<'_>],
) -> i32 {
    xj_cfmt::printf::finish(dest, Some(size), unsafe {
        xj_cfmt::printf::render_plan(plan, args)
    })
}
/// # Safety
/// As for [`snprintf_plan_`].
pub unsafe fn sprintf_plan_<D: xj_cfmt::printf::Destination + ?Sized>(
    dest: &mut D,
    plan: &xj_cfmt::cformat::Plan<'_>,
    args: &[Arg<'_>],
) -> i32 {
    xj_cfmt::printf::finish(dest, None, unsafe {
        xj_cfmt::printf::render_plan(plan, args)
    })
}
