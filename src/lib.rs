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
mod buffer;
mod handle;
mod state;
mod slots;
pub(crate) mod format;
mod api;
mod print;

#[cfg(feature = "set_output_capture")]
pub mod capture;

pub use api::*;
pub use print::{_eprint, _eprintln, _print, _println};

/// Opaque token representing a stream. `0` is the null token.
///
/// Internally a `usize`-encoded slotmap key (slot index + generation).
/// Translated C code that compares `f == NULL` becomes `f == 0`.
pub type FILE_ptr = usize;

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
