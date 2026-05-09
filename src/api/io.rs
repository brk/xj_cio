//! Bulk and char I/O: `fread`, `fwrite`, `fgetc`, `fputc`, `ungetc`,
//! `getc`, `putc`, `getchar`, `putchar`.

use crate::errno;
use crate::state::OpenMode;
use crate::{EOF, FILE_ptr};

use super::ops;

/// Safe sibling of `fread`. Reads up to `dst.len()` bytes into `dst`
/// and returns the number of bytes actually read. Sets `eof`/`err`.
pub fn fread_(dst: &mut [u8], t: FILE_ptr) -> usize {
    if dst.is_empty() {
        return 0;
    }
    let got: Option<usize> = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return 0usize;
        }
        match ops::read_into(s, dst) {
            Ok(n) => n,
            Err(_) => 0,
        }
    });
    got.unwrap_or(0)
}

/// `fread(buf, size, n, fp)`. Returns the number of *complete items*
/// actually read (i.e. `bytes_read / size`). Sets `eof`/`err`.
pub fn fread(buf: *mut u8, size: usize, n: usize, t: FILE_ptr) -> usize {
    if size == 0 || n == 0 {
        return 0;
    }
    let total = match size.checked_mul(n) {
        Some(t) => t,
        None => {
            errno::set(libc::EINVAL);
            return 0;
        }
    };
    let dst = unsafe { std::slice::from_raw_parts_mut(buf, total) };
    fread_(dst, t) / size
}

/// Safe sibling of `fwrite`. Writes `src` to the stream and returns
/// the number of bytes actually written.
pub fn fwrite_(src: &[u8], t: FILE_ptr) -> usize {
    if src.is_empty() {
        return 0;
    }
    ops::with_token(t, |s| {
        if !s.mode.writable() {
            errno::set(libc::EBADF);
            s.err = true;
            return 0usize;
        }
        match ops::write_from(s, src) {
            Ok(k) => k,
            Err(_) => 0,
        }
    })
    .unwrap_or(0)
}

/// `fwrite(buf, size, n, fp)`. Returns the number of complete items
/// actually written.
pub fn fwrite(buf: *const u8, size: usize, n: usize, t: FILE_ptr) -> usize {
    if size == 0 || n == 0 {
        return 0;
    }
    let total = match size.checked_mul(n) {
        Some(t) => t,
        None => {
            errno::set(libc::EINVAL);
            return 0;
        }
    };
    let src = unsafe { std::slice::from_raw_parts(buf, total) };
    fwrite_(src, t) / size
}

pub fn fgetc(t: FILE_ptr) -> core::ffi::c_int {
    let r = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return EOF;
        }
        match ops::read_one(s) {
            Ok(Some(b)) => b as libc::c_int,
            Ok(None) => EOF,
            Err(_) => EOF,
        }
    });
    r.unwrap_or(EOF)
}

#[inline]
pub fn getc(t: FILE_ptr) -> core::ffi::c_int {
    fgetc(t)
}

pub fn fputc(c: libc::c_int, t: FILE_ptr) -> core::ffi::c_int {
    let byte = (c as u32 & 0xff) as u8;
    let r = ops::with_token(t, |s| {
        if !s.mode.writable() {
            errno::set(libc::EBADF);
            s.err = true;
            return EOF;
        }
        match ops::write_from(s, &[byte]) {
            Ok(1) => byte as libc::c_int,
            _ => EOF,
        }
    });
    r.unwrap_or(EOF)
}

#[inline]
pub fn putc(c: libc::c_int, t: FILE_ptr) -> core::ffi::c_int {
    fputc(c, t)
}

pub fn getchar() -> core::ffi::c_int {
    fgetc(crate::api::stdio_::stdin())
}

pub fn putchar(c: libc::c_int) -> core::ffi::c_int {
    fputc(c, crate::api::stdio_::stdout())
}

/// `ungetc(c, fp)`. Pushes one byte onto the stream's pushback. Our
/// implementation grows the pushback unboundedly (the C standard
/// guarantees only one byte). Clears `eof`. Returns `c` on success
/// or `EOF` if `c == EOF`.
pub fn ungetc(c: libc::c_int, t: FILE_ptr) -> core::ffi::c_int {
    if c == EOF {
        return EOF;
    }
    let byte = (c as u32 & 0xff) as u8;
    let r = ops::with_token(t, |s| {
        if !s.mode.readable() && s.mode != OpenMode::Wplus && s.mode != OpenMode::Aplus && s.mode != OpenMode::Rplus {
            errno::set(libc::EBADF);
            return EOF;
        }
        // Switch to read direction (so ungetc'd bytes appear before
        // any subsequent read).
        let _ = ops::switch_to_read(s);
        s.pushback.push(byte);
        s.eof = false;
        byte as libc::c_int
    });
    r.unwrap_or(EOF)
}
