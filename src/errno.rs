//! Thread-local errno mirror.
//!
//! On glibc/musl/macOS the platform `errno` is a thread-local already;
//! we still keep our own copy so that error returns from purely
//! in-Rust code paths (slotmap miss, mem-stream out-of-bounds) have
//! somewhere to land without requiring a syscall to set the libc
//! `errno`. We *also* push the value into the platform `errno` so
//! that translated C code reading `errno` directly observes the same
//! value.

use std::cell::Cell;
use std::io;

thread_local! {
    static ERRNO: Cell<libc::c_int> = const { Cell::new(0) };
}

/// Read the most recent errno value set on this thread.
pub fn get() -> core::ffi::c_int {
    ERRNO.with(|c| c.get())
}

/// Set the errno value for this thread.
///
/// Mirrors to the platform `errno` so translated C code that reads
/// `errno` directly sees the same value.
pub fn set(e: libc::c_int) {
    ERRNO.with(|c| c.set(e));
    unsafe {
        *libc::__errno_location() = e;
    }
}

/// Translate a `std::io::Error` into a POSIX errno integer.
pub fn from_io(err: &io::Error) -> core::ffi::c_int {
    err.raw_os_error().unwrap_or(libc::EIO)
}

/// Convenience: set errno from an io::Error and return.
pub fn set_from_io(err: &io::Error) {
    set(from_io(err));
}
