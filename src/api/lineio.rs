//! Line-oriented I/O: `fgets`, `fputs`, `puts`, `getline`, `getdelim`.

use std::ffi::CStr;

use xj_zstr::{CStrDest, ZString};

use crate::errno;
use crate::{EOF, FILE_ptr};

use super::ops;

/// Safe sibling of `fgets`. `dst.c_size()` plays the role of C's `n`:
/// reads up to `n - 1` bytes, stopping at `\n` (which is included) or
/// EOF, and terminates them. Returns the number of payload bytes
/// (excluding the NUL) on success, `None` on EOF-before-any-bytes,
/// error, or `n == 0`.
///
/// Pass a byte array as `(&mut buf).into()`, a whole `ZStr` or
/// `ZString` as `z.into()`, or a `ZStr` with a separate C size as
/// `z.limit(n)`. Never pass `z.as_buffer_mut()`: as a plain slice it
/// reads one byte less per call than C would.
pub fn fgets_(mut dst: CStrDest<'_>, t: FILE_ptr) -> Option<usize> {
    if dst.c_size() == 0 {
        return None;
    }
    let max = dst.max_len();
    if max == 0 {
        // Per POSIX: write only the NUL.
        dst.terminate(0);
        return Some(0);
    }
    let got = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return None;
        }
        let region = dst.region_mut();
        let mut written = 0usize;
        while written < max {
            match ops::read_one(s) {
                Ok(Some(b)) => {
                    region[written] = b;
                    written += 1;
                    if b == b'\n' {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => return None,
            }
        }
        if written == 0 {
            return None;
        }
        dst.terminate(written);
        Some(written)
    });
    got.flatten()
}

/// `fgets(buf, n, fp)`. Reads up to `n-1` bytes; stops at `\n`
/// (which is included) or EOF. Always NUL-terminates if `n >= 1`.
/// Returns `buf` on success, `NULL` on EOF-before-any-bytes or error.
pub fn fgets(buf: *mut libc::c_char, n: libc::c_int, t: FILE_ptr) -> *mut libc::c_char {
    if n <= 0 || buf.is_null() {
        return std::ptr::null_mut();
    }
    let dst = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, n as usize) };
    match fgets_(dst.into(), t) {
        Some(_) => buf,
        None => std::ptr::null_mut(),
    }
}

/// `fputs(s, fp)`. Writes the entire NUL-terminated string. Returns
/// a non-negative integer on success, `EOF` on error.
pub fn fputs(s: &CStr, t: FILE_ptr) -> core::ffi::c_int {
    let bytes = s.to_bytes();
    let r = ops::with_token(t, |st| {
        if !st.mode.writable() {
            errno::set(libc::EBADF);
            st.err = true;
            return EOF;
        }
        match ops::write_from(st, bytes) {
            Ok(n) if n == bytes.len() => 1,
            _ => EOF,
        }
    });
    r.unwrap_or(EOF)
}

/// `puts(s)`. Writes the string followed by `\n` to stdout.
pub fn puts(s: &CStr) -> core::ffi::c_int {
    let bytes = s.to_bytes();
    let stdout = crate::api::stdio_::stdout();
    let r = ops::with_token(stdout, |st| {
        if ops::write_from(st, bytes).is_err() {
            return EOF;
        }
        match ops::write_from(st, b"\n") {
            Ok(_) => 1,
            Err(_) => EOF,
        }
    });
    r.unwrap_or(EOF)
}

/// Common implementation for `getline` and `getdelim`.
fn getdelim_impl(
    line_ptr: *mut *mut libc::c_char,
    n_ptr: *mut libc::size_t,
    delim: i32,
    t: FILE_ptr,
) -> libc::ssize_t {
    if line_ptr.is_null() || n_ptr.is_null() {
        errno::set(libc::EINVAL);
        return -1;
    }

    let r = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return -1isize as libc::ssize_t;
        }
        // Take ownership of the existing buffer (if any) into a Vec
        // of the right capacity. `realloc` semantics: if the user
        // passed a NULL pointer, allocate fresh; if non-NULL, the
        // current allocation is owned by malloc, so we must use libc
        // realloc rather than Vec::from_raw_parts.
        let mut cur: *mut libc::c_char = unsafe { *line_ptr };
        let mut cap: usize = unsafe { *n_ptr };
        let mut len: usize = 0;

        let ensure = |cur: &mut *mut libc::c_char, cap: &mut usize, want: usize| -> bool {
            if *cap >= want {
                return true;
            }
            let new_cap = (*cap).max(128).max(want).next_power_of_two();
            let p = unsafe { libc::realloc(*cur as *mut libc::c_void, new_cap) };
            if p.is_null() {
                errno::set(libc::ENOMEM);
                return false;
            }
            *cur = p as *mut libc::c_char;
            *cap = new_cap;
            true
        };

        let mut got_anything = false;
        loop {
            match ops::read_one(s) {
                Ok(Some(b)) => {
                    got_anything = true;
                    if !ensure(&mut cur, &mut cap, len + 2) {
                        unsafe {
                            *line_ptr = cur;
                            *n_ptr = cap;
                        }
                        return -1isize as libc::ssize_t;
                    }
                    unsafe {
                        *cur.add(len) = b as libc::c_char;
                    }
                    len += 1;
                    if delim >= 0 && b as i32 == delim {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    unsafe {
                        *line_ptr = cur;
                        *n_ptr = cap;
                    }
                    return -1isize as libc::ssize_t;
                }
            }
        }
        if !got_anything {
            unsafe {
                *line_ptr = cur;
                *n_ptr = cap;
            }
            return -1isize as libc::ssize_t;
        }
        // NUL-terminate.
        if !ensure(&mut cur, &mut cap, len + 1) {
            unsafe {
                *line_ptr = cur;
                *n_ptr = cap;
            }
            return -1isize as libc::ssize_t;
        }
        unsafe {
            *cur.add(len) = 0;
            *line_ptr = cur;
            *n_ptr = cap;
        }
        len as libc::ssize_t
    });
    r.unwrap_or(-1)
}

/// Safe sibling of `getline`, reading into an owned [`ZString`].
/// See [`getdelim_`].
pub fn getline_(line: &mut ZString, t: FILE_ptr) -> libc::ssize_t {
    getdelim_(line, b'\n' as libc::c_int, t)
}

/// Safe sibling of `getdelim`. Reads through the next `delim` byte
/// (which is included) or EOF into `line`, growing it as `getdelim`
/// reallocates `*lineptr`, and terminates the result. Returns the
/// number of bytes read, or -1 on EOF-before-any-bytes or error.
///
/// As in C, the return value is the line's length: a line containing
/// NUL bytes is stored whole, but `line.len()` stops at its first NUL.
/// Bytes after the new terminator are left unchanged. `line`'s
/// contents are unspecified after an error, as in C.
pub fn getdelim_(line: &mut ZString, delim: libc::c_int, t: FILE_ptr) -> libc::ssize_t {
    let r = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return -1;
        }
        let mut len = 0usize;
        loop {
            match ops::read_one(s) {
                Ok(Some(b)) => {
                    if len == line.capacity() {
                        // Backing sizes 128, 256, 512, …, as `getdelim_impl`
                        // grows its malloc'd buffer.
                        line.grow_to((2 * len + 1).max(127));
                    }
                    line.as_buffer_mut()[len] = b;
                    len += 1;
                    if delim >= 0 && b as i32 == delim {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => return -1,
            }
        }
        if len == 0 {
            return -1;
        }
        CStrDest::from(&mut *line).terminate(len);
        len as libc::ssize_t
    });
    r.unwrap_or(-1)
}

pub fn getline(
    line_ptr: *mut *mut libc::c_char,
    n_ptr: *mut libc::size_t,
    t: FILE_ptr,
) -> libc::ssize_t {
    getdelim_impl(line_ptr, n_ptr, b'\n' as i32, t)
}

pub fn getdelim(
    line_ptr: *mut *mut libc::c_char,
    n_ptr: *mut libc::size_t,
    delim: libc::c_int,
    t: FILE_ptr,
) -> libc::ssize_t {
    getdelim_impl(line_ptr, n_ptr, delim, t)
}
