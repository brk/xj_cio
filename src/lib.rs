//! `xj_cio`: faithful Rust re-implementation of POSIX `<stdio.h>`.
//!
//! See `spec.md` for design rationale.

#![cfg_attr(feature = "set_output_capture", feature(internal_output_capture))]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::missing_safety_doc)]
#![allow(dead_code)]

mod api;
mod atexit;
mod buffer;
pub mod errno;
pub(crate) mod format;
mod handle;
mod print;
mod slots;
mod state;

mod cfmt;

pub use cfmt::{CioSink, emit_c};

#[doc(hidden)]
pub use xj_cfmt as __cfmt;

/// Formats through xj_cio's standard output stream.
#[macro_export]
macro_rules! cprintf {
    ($($tokens:tt)*) => {
        $crate::__cfmt::cemit!($crate::CioSink($crate::stdout()), $($tokens)*)
    };
}

/// Formats through xj_cio's standard error stream.
#[macro_export]
macro_rules! ceprintf {
    ($($tokens:tt)*) => {
        $crate::__cfmt::cemit!($crate::CioSink($crate::stderr()), $($tokens)*)
    };
}

/// Formats through an xj_cio stream.
#[macro_export]
macro_rules! cfprintf {
    ($stream:expr, $($tokens:tt)*) => {
        $crate::__cfmt::cemit!($crate::CioSink($stream), $($tokens)*)
    };
}

#[cfg(feature = "set_output_capture")]
pub mod capture;

pub use api::*;
pub use print::{_eprint, _eprintln, _print, _println};

/// Opaque placeholder for translated `FILE *` handles.
///
/// `FILE_ptr` values are never dereferenced as `_File`; their address
/// bits encode slotmap keys.
pub enum _File {}

/// Opaque handle representing a stream. A null pointer means "no
/// file".
///
/// Internally the pointer address stores a slotmap key (slot index +
/// generation).
pub type FILE_ptr = *const _File;

/// Default buffer size, exposed for callers translating `BUFSIZ`.
pub const BUFSIZ: usize = 8192;

/// `EOF` constant exported in libc terms. Returned by char-I/O calls.
pub const EOF: libc::c_int = -1;

/// `setvbuf` mode constants (libc-compatible).
pub const _IOFBF: libc::c_int = 0;
pub const _IOLBF: libc::c_int = 1;
pub const _IONBF: libc::c_int = 2;

/// Maximum length of `tmpnam`-generated path string.
pub const L_tmpnam: usize = 20;
/// Maximum length of `ctermid` path string.
pub const L_ctermid: usize = 9;

/// Shared printing syntax and engine.
pub use xj_cfmt::cformat;
pub use xj_cfmt::printf::{Adapt, SafeAdapt};
/// C buffer macros, sharing the stream renderer.
pub use xj_cfmt::{
    snprintf, snprintf_safe, snprintf_unchecked, sprintf, sprintf_safe, sprintf_unchecked,
};

/// C printf. Requires an unsafe caller for raw arguments.
#[macro_export]
macro_rules! printf {
    ($fmt:expr $(, $arg:expr)* $(,)?) => {
        $crate::fprintf!($crate::stdout(), $fmt $(, $arg)*)
    };
}
/// C fprintf. Requires an unsafe caller for raw arguments.
#[macro_export]
macro_rules! fprintf {
    ($stream:expr, $fmt:expr $(, $arg:expr)* $(,)?) => {
        $crate::emit_c($stream, $crate::__cfmt::printf::render(
            $crate::__cfmt::CBytes::c_bytes(&$fmt), &[$($crate::Adapt::c_arg(&$arg)),*]))
    };
}
/// Safe C printf, using borrowed string views and scalar arguments.
#[macro_export]
macro_rules! printf_safe {
    ($fmt:expr $(, $arg:expr)* $(,)?) => {
        $crate::fprintf_safe!($crate::stdout(), $fmt $(, $arg)*)
    };
}
/// Safe C fprintf, using borrowed string views and scalar arguments.
#[macro_export]
macro_rules! fprintf_safe {
    ($stream:expr, $fmt:expr $(, $arg:expr)* $(,)?) => {{
        $crate::__cfmt::__private::validate_safe_format!($fmt);
        $crate::emit_c($stream, $crate::__cfmt::printf::render_safe(
            $crate::__cfmt::CBytes::c_bytes(&$fmt), &[$($crate::SafeAdapt::safe_arg(&$arg)),*]))
    }};
}

#[doc(hidden)]
pub use libc as __libc;
