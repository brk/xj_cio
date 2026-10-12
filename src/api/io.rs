//! Bulk and char I/O: `fread`, `fwrite`, `fgetc`, `fputc`, `ungetc`,
//! `getc`, `putc`, `getchar`, `putchar`.

use std::mem::MaybeUninit;

use crate::errno;
use crate::state::OpenMode;
use crate::{EOF, FILE_ptr};

use super::ops;

/// Safe sibling of `fread`. Reads up to `dst.len()` bytes into `dst`
/// and returns the number of bytes actually read. Sets `eof`/`err`.
pub fn fread_(dst: &mut [u8], t: FILE_ptr) -> usize {
    fread_dest(dst, t)
}

/// Reads up to `size * count` bytes into `dst` and returns the number of
/// complete items read. Only the bytes actually read are changed.
///
/// Returns zero without accessing the buffer or stream when either `size`
/// or `count` is zero. If their product overflows, sets `errno` to `EINVAL`
/// and returns zero.
///
/// # Panics
///
/// Panics if the nonzero, nonoverflowing product exceeds `dst.len()`.
#[inline]
pub fn fread_items(dst: &mut [u8], size: usize, count: usize, t: FILE_ptr) -> usize {
    let Some(total) = item_bytes(size, count) else {
        return 0;
    };
    fread_(&mut dst[..total], t) / size
}

/// A nonzero byte count shared by the bounded and raw item APIs.
#[inline]
fn item_bytes(size: usize, count: usize) -> Option<usize> {
    if size == 0 || count == 0 {
        return None;
    }
    size.checked_mul(count).or_else(|| {
        errno::set(libc::EINVAL);
        None
    })
}

/// Shared body of `fread_` and `fread`.
fn fread_dest<D: ops::ReadDest + ?Sized>(dst: &mut D, t: FILE_ptr) -> usize {
    if dst.dest_len() == 0 {
        return 0;
    }
    let got: Option<usize> = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return 0usize;
        }
        match ops::read_into_dest(s, dst) {
            Ok(n) => n,
            Err(_) => 0,
        }
    });
    got.unwrap_or(0)
}

/// `fread(buf, size, n, fp)`. Returns the number of *complete items*
/// actually read (i.e. `bytes_read / size`). Sets `eof`/`err`.
///
/// # Safety
///
/// If `size * n` is nonzero and does not overflow, `buf` must be valid
/// for reads and writes of `size * n` bytes (which may be
/// uninitialized), and not accessed through any other pointer or
/// reference during the call.
///
/// As in C, only the bytes read are written; the rest of the buffer is
/// left untouched.
pub unsafe fn fread(buf: *mut u8, size: usize, n: usize, t: FILE_ptr) -> usize {
    let Some(total) = item_bytes(size, n) else {
        return 0;
    };
    // SAFETY: `total` is nonzero, so the caller guarantees `buf` is
    // valid for reads and writes of `total` bytes, exclusively ours for
    // this call. `MaybeUninit<u8>` has no validity requirement, so the
    // bytes need not be initialized.
    let dst = unsafe { std::slice::from_raw_parts_mut(buf.cast::<MaybeUninit<u8>>(), total) };
    fread_dest(&mut ops::UninitDest::new(dst), t) / size
}

/// Safe sibling of `fwrite`. Writes `src` to the stream and returns
/// the number of bytes actually written. Includes stored NULs and hidden
/// bytes; use [`crate::fputs`] to write only the effective C string.
pub fn fwrite_<B: crate::ByteView + ?Sized>(src: &B, t: FILE_ptr) -> usize {
    let src = src.view_bytes();
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

/// Writes up to `size * count` bytes from `src` and returns the number of
/// complete items written. Stored NULs and hidden bytes within this range
/// are included.
///
/// Returns zero without accessing the byte view or stream when either
/// `size` or `count` is zero. If their product overflows, sets `errno` to
/// `EINVAL` and returns zero.
///
/// # Panics
///
/// Panics if the nonzero, nonoverflowing product exceeds the byte view's length.
#[inline]
pub fn fwrite_items<B: crate::ByteView + ?Sized>(
    src: &B,
    size: usize,
    count: usize,
    t: FILE_ptr,
) -> usize {
    let Some(total) = item_bytes(size, count) else {
        return 0;
    };
    fwrite_(&src.view_bytes()[..total], t) / size
}

/// `fwrite(buf, size, n, fp)`. Returns the number of complete items
/// actually written.
///
/// # Safety
///
/// If `size * n` is nonzero and does not overflow, `buf` must be valid
/// for reads of `size * n` initialized bytes, which must not be
/// written through any other pointer during the call.
pub unsafe fn fwrite(buf: *const u8, size: usize, n: usize, t: FILE_ptr) -> usize {
    let Some(total) = item_bytes(size, n) else {
        return 0;
    };
    // SAFETY: `total` is nonzero, so the caller guarantees `total`
    // readable, initialized bytes at `buf`, unmodified for this call.
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
        if !s.mode.readable()
            && s.mode != OpenMode::Wplus
            && s.mode != OpenMode::Aplus
            && s.mode != OpenMode::Rplus
        {
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
