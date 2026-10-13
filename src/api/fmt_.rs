//! Pre-parsed C printing calls and the `scanf` family.
//! Printing macros and plan APIs use xj_cfmt's common renderer; scanning
//! uses the shared `xj_scanf` engine, with safe `ScanTarget` destinations or
//! raw `OutArg` destinations.

use crate::ByteView;
use xj_bytes::c_bytes;

use crate::FILE_ptr;
use crate::errno;
use crate::format::{RawArgs, scan_bytes, scan_stream};

pub use crate::format::OutArg;
pub use xj_cfmt::printf::Arg;
/// Safe scanning destinations, shared with `xj_scanf`.
pub use xj_scanf::ScanTarget;

/// `fscanf` with safe destinations: integers, `f32`/`f64`, bounded byte
/// storage (`[u8; N]`, `&mut [u8]`, [`CStrDest`](crate::CStrDest)),
/// `Vec<u8>` and `String`.
///
/// A destination whose type does not fit its directive stops the call
/// before that directive reads input, with `errno` set to `EINVAL`. A
/// bounded string field that does not fit stores the prefix that fits and
/// sets `EOVERFLOW`; the rest of the field stays unread.
pub fn fscanf<B: ByteView + ?Sized>(
    t: FILE_ptr,
    fmt: &B,
    args: &mut [&mut dyn ScanTarget],
) -> core::ffi::c_int {
    scan_stream(
        t,
        c_bytes(fmt.view_bytes()),
        &mut xj_scanf::Targets::new(args),
    )
}

/// `scanf` with safe destinations; see [`fscanf`].
pub fn scanf<B: ByteView + ?Sized>(fmt: &B, args: &mut [&mut dyn ScanTarget]) -> core::ffi::c_int {
    fscanf(crate::api::stdio_::stdin(), fmt, args)
}

/// `fscanf` with raw destinations. The slice is `&mut` because
/// [`OutArg::Buf`] borrows its destination mutably.
///
/// # Safety
///
/// `args` must meet the requirements in [`OutArg`]'s "Pointer
/// validity" section for the conversions in `fmt`, for the whole call.
pub unsafe fn fscanf_raw<B: ByteView + ?Sized>(
    t: FILE_ptr,
    fmt: &B,
    args: &mut [OutArg<'_>],
) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    let mut args = unsafe { RawArgs::new(args) };
    scan_stream(t, c_bytes(fmt.view_bytes()), &mut args)
}

/// `scanf` with raw destinations.
///
/// # Safety
///
/// As for [`fscanf_raw`].
pub unsafe fn scanf_raw<B: ByteView + ?Sized>(
    fmt: &B,
    args: &mut [OutArg<'_>],
) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { fscanf_raw(crate::api::stdio_::stdin(), fmt, args) }
}

/// `sscanf` with raw destinations. The input is a C string: it stops at
/// its first NUL or view end, as does the format. For safe destinations,
/// use `xj_scanf::sscanf`.
///
/// # Safety
///
/// As for [`fscanf_raw`].
pub unsafe fn sscanf_raw<I: ByteView + ?Sized, B: ByteView + ?Sized>(
    input: &I,
    fmt: &B,
    args: &mut [OutArg<'_>],
) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    let mut args = unsafe { RawArgs::new(args) };
    scan_bytes(
        c_bytes(input.view_bytes()),
        c_bytes(fmt.view_bytes()),
        &mut args,
    )
}

/// Destinations pulled from a `va_list`, one pointer of exactly the type
/// each directive requires. Expands to a [`crate::format::VaArgs`].
#[cfg(feature = "c_variadic")]
macro_rules! va_args {
    ($ap:ident) => {
        // SAFETY: the entry point's caller vouches for every pointer in
        // the list.
        unsafe { crate::format::VaArgs::new(|req| crate::format::va_arg(&mut $ap, req)) }
    };
}

/// C `vfscanf`. Requires the `c_variadic` feature (nightly).
///
/// # Safety
///
/// Each pointer in `ap` must have the type its directive requires and
/// meet the requirements in [`OutArg`]'s "Pointer validity" section.
#[cfg(feature = "c_variadic")]
pub unsafe fn vfscanf<B: ByteView + ?Sized>(
    t: FILE_ptr,
    fmt: &B,
    mut ap: core::ffi::VaList,
) -> core::ffi::c_int {
    scan_stream(t, c_bytes(fmt.view_bytes()), &mut va_args!(ap))
}

/// C `vscanf`. Requires the `c_variadic` feature (nightly).
///
/// # Safety
///
/// As for [`vfscanf`].
#[cfg(feature = "c_variadic")]
pub unsafe fn vscanf<B: ByteView + ?Sized>(fmt: &B, ap: core::ffi::VaList) -> core::ffi::c_int {
    // SAFETY: forwarded from the caller.
    unsafe { vfscanf(crate::api::stdio_::stdin(), fmt, ap) }
}

/// C `vsscanf`, over a C string input. Requires the `c_variadic` feature
/// (nightly).
///
/// # Safety
///
/// As for [`vfscanf`].
#[cfg(feature = "c_variadic")]
pub unsafe fn vsscanf<I: ByteView + ?Sized, B: ByteView + ?Sized>(
    input: &I,
    fmt: &B,
    mut ap: core::ffi::VaList,
) -> core::ffi::c_int {
    scan_bytes(
        c_bytes(input.view_bytes()),
        c_bytes(fmt.view_bytes()),
        &mut va_args!(ap),
    )
}

/// Reports a shared parser failure using the printing profile's errno policy.
pub fn format_error(e: xj_cfmt::cformat::Error) -> i32 {
    errno::set(xj_cfmt::printf::Error::Format(e).errno());
    -1
}
/// Execute a pre-parsed call, including a va_list call.
/// # Safety
/// Raw arguments must satisfy the common renderer's contracts.
pub unsafe fn fprintf_plan_raw(
    t: FILE_ptr,
    plan: &xj_cfmt::cformat::Plan<'_>,
    args: &[Arg<'_>],
) -> i32 {
    crate::emit_c(t, unsafe { xj_cfmt::printf::render_plan(plan, args) })
}
/// # Safety
/// As for [`fprintf_plan_raw`].
pub unsafe fn printf_plan_raw(plan: &xj_cfmt::cformat::Plan<'_>, args: &[Arg<'_>]) -> i32 {
    unsafe { fprintf_plan_raw(crate::stdout(), plan, args) }
}
/// # Safety
/// Raw arguments must satisfy the common renderer's contracts. `buf` must
/// be writable for `min(size, rendered_length + 1)` bytes. A zero size writes
/// nothing and permits a null buffer.
pub unsafe fn snprintf_plan_raw(
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
pub unsafe fn sprintf_plan_raw(
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
