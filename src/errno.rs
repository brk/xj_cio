//! Native thread-local errno, shared with the formatting engine and
//! translated C code.

use std::io;

/// Read the most recent errno value set on this thread.
pub fn get() -> core::ffi::c_int {
    ::errno::errno().0
}

/// Set the errno value for this thread.
///
/// Translated C code and Rust adapters observe the same value.
pub fn set(e: libc::c_int) {
    ::errno::set_errno(::errno::Errno(e));
}

/// Translate a `std::io::Error` into a POSIX errno integer.
pub fn from_io(err: &io::Error) -> core::ffi::c_int {
    err.raw_os_error().unwrap_or(libc::EIO)
}

/// Convenience: set errno from an io::Error and return.
pub fn set_from_io(err: &io::Error) {
    set(from_io(err));
}
