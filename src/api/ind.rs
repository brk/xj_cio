//! Indicators: `feof`, `ferror`, `clearerr`, `perror`.

use std::ffi::CStr;

use crate::errno;
use crate::FILE_ptr;

use super::ops;

pub fn feof(t: FILE_ptr) -> core::ffi::c_int {
    ops::with_token(t, |s| if s.eof { 1 } else { 0 }).unwrap_or(0)
}

pub fn ferror(t: FILE_ptr) -> core::ffi::c_int {
    ops::with_token(t, |s| if s.err { 1 } else { 0 }).unwrap_or(0)
}

pub fn clearerr(t: FILE_ptr) {
    let _ = ops::with_token(t, |s| {
        s.eof = false;
        s.err = false;
    });
}

/// `perror(prefix)` writes "<prefix>: <strerror(errno)>\n" to stderr.
pub fn perror(prefix: *const libc::c_char) {
    let saved = errno::get();
    let stderr = crate::api::stdio_::stderr();
    let _ = ops::with_token(stderr, |s| {
        if !prefix.is_null() {
            let cstr = unsafe { CStr::from_ptr(prefix) };
            let bytes = cstr.to_bytes();
            if !bytes.is_empty() {
                let _ = ops::write_from(s, bytes);
                let _ = ops::write_from(s, b": ");
            }
        }
        // strerror(saved) — call libc directly.
        let msg = unsafe { libc::strerror(saved) };
        if !msg.is_null() {
            let cstr = unsafe { CStr::from_ptr(msg) };
            let _ = ops::write_from(s, cstr.to_bytes());
        }
        let _ = ops::write_from(s, b"\n");
        let _ = ops::flush_internal(s);
    });
}
