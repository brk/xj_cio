//! Buffering: `setbuf`, `setvbuf`, `fflush`, plus `fflush(NULL)` via
//! the `fflush_all` helper.

use crate::buffer::{Buffer, BufferPolicy};
use crate::errno;
use crate::handle::Handle;
use crate::slots;
use crate::{BUFSIZ, EOF, FILE_ptr, _IOFBF, _IOLBF, _IONBF};

use super::ops;
use super::stdio_::{stdio_mode, StdioMode};

/// `fflush(fp)` — drains the write buffer to the kernel. `fp == 0`
/// flushes every open writable stream.
pub fn fflush(t: FILE_ptr) -> core::ffi::c_int {
    if t == 0 {
        return fflush_all();
    }
    let r = ops::with_token(t, |s| match ops::flush_internal(s) {
        Ok(()) => 0,
        Err(e) => {
            errno::set_from_io(&e);
            EOF
        }
    });
    r.unwrap_or(EOF)
}

pub fn fflush_all() -> core::ffi::c_int {
    let mut rv = 0;
    for arc in slots::snapshot() {
        let r = arc.with_state(|s| ops::flush_internal(s));
        if r.is_err() {
            rv = EOF;
        }
    }
    rv
}

/// `setvbuf(fp, buf, mode, size)`. Switches the policy and (re)allocates
/// the internal buffer to `size` (or `BUFSIZ` if `size == 0`). The
/// `buf` argument is accepted for C compatibility but ignored — xj_cio
/// always owns the backing buffer.
pub fn setvbuf(
    t: FILE_ptr,
    _buf: *mut libc::c_char,
    mode: libc::c_int,
    size: libc::size_t,
) -> core::ffi::c_int {
    let policy = match mode {
        x if x == _IOFBF => BufferPolicy::Full,
        x if x == _IOLBF => BufferPolicy::Line,
        x if x == _IONBF => BufferPolicy::None,
        _ => {
            errno::set(libc::EINVAL);
            return -1;
        }
    };
    let r = ops::with_token(t, |s| {
        // Discard any in-flight buffered bytes (POSIX requires
        // setvbuf to be called before any I/O on the stream — in
        // practice we play it safe).
        let _ = ops::flush_internal(s);
        s.buffer.discard();

        // §6.2: when the stream is one of the standard "Shared"
        // stdio handles, the global StdioMode decides what setvbuf
        // is allowed to do.
        let is_stdio_shared = matches!(
            s.handle,
            Handle::StdoutShared | Handle::StderrShared | Handle::StdinShared
        );
        if is_stdio_shared {
            match stdio_mode() {
                StdioMode::Shared => {
                    // Honor _IONBF only; ignore size/buf otherwise —
                    // we cannot point std's writer at the user buffer.
                    if matches!(policy, BufferPolicy::None) {
                        s.buffer = Buffer::new(BufferPolicy::None, 0);
                    }
                    return 0;
                }
                StdioMode::Bypass => {
                    // Swap to a Bypass handle so the user's buffer
                    // really takes effect. dup() the underlying fd
                    // so std's `Stdout` (which we drop) doesn't
                    // close it on us.
                    let raw = s.handle.raw_fd();
                    let f = unsafe { Handle::file_from_dup(raw) };
                    s.handle = match s.handle {
                        Handle::StdoutShared => Handle::StdoutBypass(f),
                        Handle::StderrShared => Handle::StderrBypass(f),
                        Handle::StdinShared => Handle::StdinBypass(f),
                        _ => unreachable!(),
                    };
                    // Fall through to the regular setvbuf path.
                }
                #[cfg(feature = "set_output_capture")]
                StdioMode::Capture => {
                    // Capture mode: print! is already routed into
                    // our sink, so changing our buffer is safe and
                    // takes effect for everything.
                }
            }
        }

        let cap = match policy {
            BufferPolicy::None => 0,
            _ if size == 0 => BUFSIZ,
            _ => size,
        };
        s.buffer = Buffer::new(policy, cap);
        0
    });
    r.unwrap_or(-1)
}

/// `setbuf(fp, buf)`. Equivalent to `setvbuf(fp, buf, _IOFBF, BUFSIZ)`
/// when `buf != NULL`, else `setvbuf(fp, NULL, _IONBF, 0)`.
pub fn setbuf(t: FILE_ptr, buf: *mut libc::c_char) {
    if buf.is_null() {
        let _ = setvbuf(t, std::ptr::null_mut(), _IONBF, 0);
    } else {
        let _ = setvbuf(t, buf, _IOFBF, BUFSIZ);
    }
}
