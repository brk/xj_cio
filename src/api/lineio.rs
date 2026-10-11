//! Line-oriented I/O: `fgets`, `fputs`, `puts`, `getline`, `getdelim`.

use crate::ByteView;
use xj_cfmt_core::c_bytes;

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
    let region = dst.region_mut();
    let written = fgets_core(max, t, |i, b| region[i] = b)?;
    dst.terminate(written);
    Some(written)
}

/// Shared body of `fgets_` and `fgets`: reads up to `max` (> 0) bytes,
/// stopping after `\n` or at EOF, and hands each to `put` with its
/// index (`0..max`). Returns the count, or `None` on
/// EOF-before-any-bytes or error.
fn fgets_core(max: usize, t: FILE_ptr, mut put: impl FnMut(usize, u8)) -> Option<usize> {
    let got = ops::with_token(t, |s| {
        if !s.mode.readable() {
            errno::set(libc::EBADF);
            s.err = true;
            return None;
        }
        let mut written = 0usize;
        while written < max {
            match ops::read_one(s) {
                Ok(Some(b)) => {
                    put(written, b);
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
        Some(written)
    });
    got.flatten()
}

/// `fgets(buf, n, fp)`. Reads up to `n-1` bytes; stops at `\n`
/// (which is included) or EOF. Always NUL-terminates if `n >= 1`.
/// Returns `buf` on success, `NULL` on EOF-before-any-bytes or error.
///
/// # Safety
///
/// If `buf` is non-null and `n > 0`, `buf` must be valid for writes of
/// `n` bytes (which may be uninitialized), and the region must not be
/// accessed through any other pointer or reference for the duration of
/// the call.
///
/// As in C, only the bytes read and the terminator are written.
pub unsafe fn fgets(buf: *mut u8, n: libc::c_int, t: FILE_ptr) -> *mut u8 {
    if n <= 0 || buf.is_null() {
        return std::ptr::null_mut();
    }
    let max = n as usize - 1;
    let written = if max == 0 {
        // Per POSIX: write only the NUL.
        0
    } else {
        // SAFETY: `fgets_core` passes indices `i < max < n`, and the
        // caller guarantees `buf` is valid for unaliased writes of `n`
        // bytes. A raw write needs no prior initialization.
        match fgets_core(max, t, |i, b| unsafe { buf.add(i).write(b) }) {
            Some(w) => w,
            None => return std::ptr::null_mut(),
        }
    };
    // SAFETY: `written <= max < n`; valid for writes as above.
    unsafe { buf.add(written).write(0) };
    buf
}

/// `fputs(s, fp)`. Writes the entire NUL-terminated string. Returns
/// a non-negative integer on success, `EOF` on error.
pub fn fputs<B: ByteView + ?Sized>(s: &B, t: FILE_ptr) -> core::ffi::c_int {
    let bytes = c_bytes(s.view_bytes());
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
pub fn puts<B: ByteView + ?Sized>(s: &B) -> core::ffi::c_int {
    let bytes = c_bytes(s.view_bytes());
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
///
/// # Safety
///
/// See [`getdelim`].
unsafe fn getdelim_impl(
    line_ptr: *mut *mut u8,
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
        // SAFETY: both pointers are non-null (checked above), and the
        // caller guarantees they are valid for aligned reads and writes.
        let mut cur: *mut u8 = unsafe { *line_ptr };
        // As in POSIX, `*n_ptr` is ignored when `*line_ptr` is null, so
        // a stale size cannot make us write through a null pointer.
        // SAFETY: as for `*line_ptr`.
        let mut cap: usize = if cur.is_null() { 0 } else { unsafe { *n_ptr } };
        let mut len: usize = 0;

        // Invariant: `cur` is null with `cap == 0`, or a live malloc
        // allocation of at least `cap` bytes.
        let ensure = |cur: &mut *mut u8, cap: &mut usize, want: usize| -> bool {
            if *cap >= want {
                return true;
            }
            let new_cap = (*cap).max(128).max(want).next_power_of_two();
            // SAFETY: by the invariant (from the caller's contract on
            // `*line_ptr`), `*cur` is null or a live malloc allocation.
            // On success the old pointer is dead and we replace it; on
            // failure it is untouched.
            let p = unsafe { libc::realloc(*cur as *mut libc::c_void, new_cap) };
            if p.is_null() {
                errno::set(libc::ENOMEM);
                return false;
            }
            *cur = p as *mut u8;
            *cap = new_cap;
            true
        };

        let mut got_anything = false;
        loop {
            match ops::read_one(s) {
                Ok(Some(b)) => {
                    got_anything = true;
                    if !ensure(&mut cur, &mut cap, len + 2) {
                        // SAFETY: valid for writes per the caller; this
                        // hands back the (still live) allocation.
                        unsafe {
                            *line_ptr = cur;
                            *n_ptr = cap;
                        }
                        return -1isize as libc::ssize_t;
                    }
                    // SAFETY: `ensure` made `cur` an allocation of at
                    // least `len + 2 > len` bytes.
                    unsafe {
                        *cur.add(len) = b;
                    }
                    len += 1;
                    if delim >= 0 && b as i32 == delim {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    // SAFETY: valid for writes per the caller.
                    unsafe {
                        *line_ptr = cur;
                        *n_ptr = cap;
                    }
                    return -1isize as libc::ssize_t;
                }
            }
        }
        if !got_anything {
            // SAFETY: valid for writes per the caller.
            unsafe {
                *line_ptr = cur;
                *n_ptr = cap;
            }
            return -1isize as libc::ssize_t;
        }
        // NUL-terminate.
        if !ensure(&mut cur, &mut cap, len + 1) {
            // SAFETY: valid for writes per the caller.
            unsafe {
                *line_ptr = cur;
                *n_ptr = cap;
            }
            return -1isize as libc::ssize_t;
        }
        // SAFETY: `ensure` made `cur` at least `len + 1` bytes; the
        // out-pointers are valid for writes per the caller.
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

/// `getline(lineptr, n, fp)`: `getdelim` with `delim = '\n'`.
///
/// # Safety
///
/// See [`getdelim`].
pub unsafe fn getline(
    line_ptr: *mut *mut u8,
    n_ptr: *mut libc::size_t,
    t: FILE_ptr,
) -> libc::ssize_t {
    // SAFETY: forwarded from the caller.
    unsafe { getdelim_impl(line_ptr, n_ptr, b'\n' as i32, t) }
}

/// `getdelim(lineptr, n, delim, fp)`. Reads through the next `delim`
/// byte (or EOF) into `*lineptr`, growing it with `realloc` and
/// updating `*n`. Returns the number of bytes read, or -1.
///
/// # Safety
///
/// - `line_ptr` and `n_ptr` must each be null (which fails with
///   `EINVAL`) or valid for aligned reads and writes.
/// - `*line_ptr` must be null or a live pointer returned by `malloc`,
///   `calloc` or `realloc`. If it is non-null, the allocation must be
///   at least `*n_ptr` bytes. If it is null, `*n_ptr` is ignored.
/// - Neither `*line_ptr`'s allocation nor `*line_ptr`/`*n_ptr` may be
///   accessed through any other pointer or reference during the call.
///
/// On return, `*line_ptr` may have been reallocated (the old pointer
/// is then dangling) and the caller owns it.
pub unsafe fn getdelim(
    line_ptr: *mut *mut u8,
    n_ptr: *mut libc::size_t,
    delim: libc::c_int,
    t: FILE_ptr,
) -> libc::ssize_t {
    // SAFETY: forwarded from the caller.
    unsafe { getdelim_impl(line_ptr, n_ptr, delim, t) }
}
