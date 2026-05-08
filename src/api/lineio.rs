//! Line-oriented I/O: `fgets`, `fputs`, `puts`, `getline`, `getdelim`.

use std::ffi::CStr;

use crate::errno;
use crate::{EOF, FILE_ptr};

use super::ops;

/// `fgets(buf, n, fp)`. Reads up to `n-1` bytes; stops at `\n`
/// (which is included) or EOF. Always NUL-terminates if `n >= 1`.
/// Returns `buf` on success, `NULL` on EOF-before-any-bytes or error.
pub fn fgets(buf: *mut libc::c_char, n: libc::c_int, t: FILE_ptr) -> *mut libc::c_char {
    if n <= 0 || buf.is_null() {
        return std::ptr::null_mut();
    }
    let cap = n as usize;
    if cap == 1 {
        // Per POSIX: write only the NUL.
        unsafe { *buf = 0 };
        return buf;
    }
    let dst = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, cap) };
    let got = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return None;
        }
        let mut written = 0usize;
        while written + 1 < cap {
            match ops::read_one(s) {
                Ok(Some(b)) => {
                    dst[written] = b;
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
            // EOF before any byte
            return None;
        }
        dst[written] = 0;
        Some(written)
    });
    match got {
        Some(Some(_)) => buf,
        _ => std::ptr::null_mut(),
    }
}

/// `fputs(s, fp)`. Writes the entire NUL-terminated string. Returns
/// a non-negative integer on success, `EOF` on error.
pub fn fputs(s: &CStr, t: FILE_ptr) -> libc::c_int {
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
pub fn puts(s: &CStr) -> libc::c_int {
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
