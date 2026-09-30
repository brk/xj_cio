//! `xj_cio`: faithful Rust re-implementation of POSIX `<stdio.h>`.
//!
//! See `spec.md` for design rationale.

#![cfg_attr(feature = "set_output_capture", feature(internal_output_capture))]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::missing_safety_doc)]
#![allow(dead_code)]

pub mod errno;
mod atexit;
mod buffer;
mod handle;
mod state;
mod slots;
pub(crate) mod format;
mod api;
mod print;

#[cfg(feature = "cfmt")]
mod cfmt;

#[cfg(feature = "cfmt")]
pub use cfmt::CioSink;

#[cfg(feature = "cfmt")]
#[doc(hidden)]
pub use xj_cfmt as __cfmt;

/// Formats through xj_cio's standard output stream.
#[cfg(feature = "cfmt")]
#[macro_export]
macro_rules! cprintf {
    ($($tokens:tt)*) => {
        $crate::__cfmt::cemit!($crate::CioSink($crate::stdout()), $($tokens)*)
    };
}

/// Formats through xj_cio's standard error stream.
#[cfg(feature = "cfmt")]
#[macro_export]
macro_rules! ceprintf {
    ($($tokens:tt)*) => {
        $crate::__cfmt::cemit!($crate::CioSink($crate::stderr()), $($tokens)*)
    };
}

/// Formats through an xj_cio stream.
#[cfg(feature = "cfmt")]
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
