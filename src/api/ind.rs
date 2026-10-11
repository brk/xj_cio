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

/// Safe sibling of `perror`. Writes `<prefix>: <strerror(errno)>\n`
/// (or just `<strerror(errno)>\n` if `prefix` is `None`) to stderr.
/// Use `None::<&str>` when omitting the prefix.
pub fn perror_<B: crate::ByteView + ?Sized>(prefix: Option<&B>) {
    let saved = errno::get();
    let prefix = prefix.map(|p| xj_cfmt_core::c_bytes(p.view_bytes()));
    let stderr = crate::api::stdio_::stderr();
    let _ = ops::with_token(stderr, |s| {
        if let Some(bytes) = prefix {
            if !bytes.is_empty() {
                let _ = ops::write_from(s, bytes);
                let _ = ops::write_from(s, b": ");
            }
        }
        // `strerror_r` rather than `strerror`: the latter may return a
        // static buffer that another thread can overwrite while we
        // read it.
        // Even when it reports an unknown `errno` (glibc writes
        // "Unknown error N" and returns `EINVAL`), the buffer is
        // NUL-terminated, so print whatever it holds.
        let mut msg = [0u8; 128];
        // SAFETY: `msg` is valid for writes of `msg.len()` bytes, and
        // `strerror_r` writes no more than that.
        let _ = unsafe { libc::strerror_r(saved, msg.as_mut_ptr().cast(), msg.len()) };
        if let Ok(cstr) = CStr::from_bytes_until_nul(&msg) {
            let _ = ops::write_from(s, cstr.to_bytes());
        }
        let _ = ops::write_from(s, b"\n");
        let _ = ops::flush_internal(s);
    });
}

/// `perror(prefix)` writes `<prefix>: <strerror(errno)>\n` to stderr.
///
/// # Safety
///
/// `prefix` must be null or point to a NUL-terminated string that is
/// valid for reads up to and including its NUL and is not modified
/// during the call.
pub unsafe fn perror(prefix: *const u8) {
    let p = if prefix.is_null() {
        None
    } else {
        // SAFETY: non-null, so the caller guarantees a valid,
        // unmodified NUL-terminated string.
        Some(unsafe { CStr::from_ptr(prefix.cast()) })
    };
    perror_(p);
}
