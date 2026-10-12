//! Open / close / freopen / fdopen / tmpfile / remove / rename.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::sync::Arc;

use crate::ByteView;
use xj_cfmt_core::c_bytes;
use xj_zstr::CStrDest;

use crate::buffer::BufferPolicy;
use crate::errno;
use crate::handle::Handle;
use crate::slots;
use crate::state::{FILE, OpenMode, State};
use crate::{BUFSIZ, EOF, FILE_ptr};

use super::ops;

fn view_to_path<B: ByteView + ?Sized>(s: &B) -> &Path {
    use std::os::unix::ffi::OsStrExt;
    Path::new(std::ffi::OsStr::from_bytes(c_bytes(s.view_bytes())))
}

fn open_file_for(mode: OpenMode, path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    match mode {
        OpenMode::R => {
            o.read(true);
        }
        OpenMode::W => {
            o.write(true).create(true).truncate(true);
        }
        OpenMode::A => {
            o.write(true).create(true).append(true);
        }
        OpenMode::Rplus => {
            o.read(true).write(true);
        }
        OpenMode::Wplus => {
            o.read(true).write(true).create(true).truncate(true);
        }
        OpenMode::Aplus => {
            o.read(true).write(true).create(true).append(true);
        }
    }
    o.open(path)
}

/// Default buffering policy for a newly-opened stream. POSIX says
/// terminals are line-buffered and everything else is fully buffered,
/// but for v1 we just always pick full buffering for non-stdio
/// streams.
fn default_policy_for(_h: &Handle) -> BufferPolicy {
    BufferPolicy::Full
}

pub fn fopen<P: ByteView + ?Sized, M: ByteView + ?Sized>(path: &P, mode: &M) -> FILE_ptr {
    let m = match OpenMode::parse(c_bytes(mode.view_bytes())) {
        Some(m) => m,
        None => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };
    let p = view_to_path(path);
    let f = match open_file_for(m, p) {
        Ok(f) => f,
        Err(e) => {
            errno::set_from_io(&e);
            return std::ptr::null();
        }
    };
    let state = State::new(Handle::Regular(f), m, BufferPolicy::Full, BUFSIZ);
    slots::insert(Arc::new(FILE::new(state)))
}

pub fn fclose(t: FILE_ptr) -> core::ffi::c_int {
    let arc = match slots::remove(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return EOF;
        }
    };
    // Mark the FILE closed *before* trying to flush. In-flight ops
    // and StreamGuards that already cloned the Arc bail with EBADF
    // when they observe the bit; new lookups already fail because
    // `slots::remove` ran first.
    arc.closed.store(true, std::sync::atomic::Ordering::Release);

    // `inner.lock()` is reentrant on the same thread, so this never
    // self-deadlocks. The panic-prone step is the *RefCell* borrow:
    // a `StreamGuard` held by this thread already owns a `RefMut`,
    // which would make `borrow_mut()` panic. `try_borrow_mut()`
    // returns `Err` in that case and we fall through to the deferred
    // path: the StreamGuard's eventual drop releases the borrow,
    // and `FILE::drop` (running when the last Arc dies) does a
    // best-effort flush. See `notes/_closed_bit.md` for the trade-
    // off (synchronous flush errors are lost in the deferred path).
    let guard = arc.raw_lock();
    let res = match guard.try_borrow_mut() {
        Ok(mut s) => {
            let r = ops::flush_internal(&mut s);
            s.handle = Handle::Closed;
            r
        }
        Err(_) => Ok(()),
    };
    drop(guard);
    drop(arc);
    match res {
        Ok(_) => 0,
        Err(e) => {
            errno::set_from_io(&e);
            EOF
        }
    }
}

/// Safe sibling of `freopen`. `path = None` corresponds to
/// `freopen(NULL, mode, fp)` (mode-change on the existing handle,
/// which we don't support — returns a null handle with
/// `errno = EINVAL`). When passing no path, write `None::<&str>` to
/// specify the input type.
pub fn freopen_<P: ByteView + ?Sized, M: ByteView + ?Sized>(
    path: Option<&P>,
    mode: &M,
    t: FILE_ptr,
) -> FILE_ptr {
    let arc = match slots::lookup(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return std::ptr::null();
        }
    };
    let m = match OpenMode::parse(c_bytes(mode.view_bytes())) {
        Some(m) => m,
        None => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };
    let path = path.map(|p| c_bytes(p.view_bytes()));
    let res: Result<(), libc::c_int> = arc.with_state(|s| {
        // Flush + close current handle.
        let _ = ops::flush_internal(s);
        s.handle = Handle::Closed;
        s.buffer.discard();
        s.pushback.clear();
        s.eof = false;
        s.err = false;
        s.last_op = crate::state::Direction::Either;

        let cstr = match path {
            // POSIX freopen(NULL, mode, fp): change mode of the
            // existing handle. We don't preserve the underlying fd
            // here, so degrade gracefully.
            None => return Err(libc::EINVAL),
            Some(c) => c,
        };
        let p = view_to_path(cstr);
        let f = match open_file_for(m, p) {
            Ok(f) => f,
            Err(e) => return Err(errno::from_io(&e)),
        };
        s.handle = Handle::Regular(f);
        s.mode = m;
        Ok(())
    });
    match res {
        Ok(()) => t,
        Err(e) => {
            errno::set(e);
            std::ptr::null()
        }
    }
}

/// `freopen(path, mode, fp)`. See [`freopen_`].
///
/// # Safety
///
/// `path` must be null or point to a NUL-terminated string that is
/// valid for reads up to and including its NUL and is not modified
/// during the call.
pub unsafe fn freopen<M: ByteView + ?Sized>(path: *const u8, mode: &M, t: FILE_ptr) -> FILE_ptr {
    let p = if path.is_null() {
        None
    } else {
        // SAFETY: non-null, so the caller guarantees a valid,
        // unmodified NUL-terminated string.
        Some(unsafe { CStr::from_ptr(path.cast()) })
    };
    freopen_(p, mode, t)
}

/// `fdopen(fd, mode)`. Unlike C, the stream wraps a `dup` of `fd`, so
/// `fd` stays open (and owned by the caller) after `fclose`.
///
/// # Safety
///
/// `fd` must be negative (which fails with `EBADF`) or an open file
/// descriptor that the caller may use, in the I/O-safety sense of a
/// `BorrowedFd`, for the duration of the call, i.e. not one that
/// another component owns and may close or reuse concurrently.
pub unsafe fn fdopen<M: ByteView + ?Sized>(fd: libc::c_int, mode: &M) -> FILE_ptr {
    let m = match OpenMode::parse(c_bytes(mode.view_bytes())) {
        Some(m) => m,
        None => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };
    if fd < 0 {
        errno::set(libc::EBADF);
        return std::ptr::null();
    }
    // Dup so that our Drop doesn't close the caller's fd.
    // SAFETY: the caller guarantees `fd` is open and borrowable.
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        errno::set_from_io(&std::io::Error::last_os_error());
        return std::ptr::null();
    }
    // SAFETY: `dup` succeeded, so `dup` is a fresh open descriptor that
    // nothing else refers to; the `File` takes sole ownership.
    let f = unsafe { File::from_raw_fd(dup) };
    let state = State::new(Handle::Regular(f), m, BufferPolicy::Full, BUFSIZ);
    slots::insert(Arc::new(FILE::new(state)))
}

pub fn fileno(t: FILE_ptr) -> core::ffi::c_int {
    let arc = match slots::lookup(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return -1;
        }
    };
    arc.with_state(|s| {
        let fd = s.handle.raw_fd();
        if fd < 0 {
            errno::set(libc::EBADF);
        }
        fd
    })
}

pub fn tmpfile() -> FILE_ptr {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // SAFETY: `getpid` has no preconditions.
    let pid = unsafe { libc::getpid() };
    for i in 0..32 {
        let path = format!("/tmp/xj_cio_tmp_{}_{}_{}", pid, nanos, i);
        let mut o = OpenOptions::new();
        o.read(true).write(true).create_new(true);
        match o.open(&path) {
            Ok(f) => {
                let _ = std::fs::remove_file(&path); // unlinked-but-open
                let state = State::new(
                    Handle::Regular(f),
                    OpenMode::Wplus,
                    BufferPolicy::Full,
                    BUFSIZ,
                );
                return slots::insert(Arc::new(FILE::new(state)));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                errno::set_from_io(&e);
                return std::ptr::null();
            }
        }
    }
    errno::set(libc::EEXIST);
    std::ptr::null()
}

/// Copies a whole generated name into `dst`, or fails with `ERANGE`
/// without writing if it does not fit. A truncated path would name a
/// different file.
fn write_name(mut dst: CStrDest<'_>, name: &[u8]) -> Option<usize> {
    if name.len() > dst.max_len() {
        errno::set(libc::ERANGE);
        return None;
    }
    Some(dst.write_truncated(name))
}

/// Safe sibling of `tmpnam` for a caller-supplied buffer. Writes a
/// fresh temporary path into `dst` and returns its length. A
/// destination with `c_size() >= L_tmpnam` always suffices; a smaller
/// one fails with `ERANGE` and is left untouched, where C would
/// overflow it.
pub fn tmpnam_(dst: CStrDest<'_>) -> Option<usize> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // SAFETY: `getpid` has no preconditions.
    let pid = unsafe { libc::getpid() } as u64;
    // Twelve hex digits keep the name within L_tmpnam - 1 == 19 bytes.
    let s = format!("/tmp/xj{:012x}", ((pid << 32) ^ nanos) & 0xffff_ffff_ffff);
    write_name(dst, s.as_bytes())
}

/// `tmpnam(buf)`. Writes a fresh temporary path into `buf`, or into a
/// static buffer if `buf` is null, and returns the buffer used.
///
/// # Safety
///
/// - If `buf` is non-null, it must be valid for writes of `L_tmpnam`
///   bytes (which may be uninitialized), not accessed through any other
///   pointer or reference during the call.
/// - If `buf` is null, the call writes the shared static buffer, as in
///   C. No other thread may call `tmpnam(NULL)` concurrently, and no
///   reference to that buffer from an earlier call may be live (and
///   no access through a pointer to it may overlap the call).
///
/// As in C, only the name and its terminator are written.
pub unsafe fn tmpnam(buf: *mut u8) -> *mut u8 {
    // Static buffer when the user passes NULL.
    static mut STATIC_BUF: [u8; crate::L_tmpnam] = [0; crate::L_tmpnam];

    let dst = if buf.is_null() {
        (&raw mut STATIC_BUF).cast::<u8>()
    } else {
        buf
    };
    let mut name = [0u8; crate::L_tmpnam];
    match tmpnam_((&mut name).into()) {
        // SAFETY: `len < L_tmpnam`, so the copy stays within the
        // `L_tmpnam` bytes at `dst`: a caller's `buf`, valid for
        // unaliased writes per the contract, or `STATIC_BUF`, which is
        // that size and which the caller guarantees nothing else
        // touches during the call. `name` is a distinct local.
        Some(len) => unsafe {
            std::ptr::copy_nonoverlapping(name.as_ptr(), dst, len + 1);
            dst
        },
        None => std::ptr::null_mut(),
    }
}

pub fn remove<P: ByteView + ?Sized>(path: &P) -> core::ffi::c_int {
    let p = view_to_path(path);
    // Try unlink first; fall back to rmdir for directories. Match libc.
    match std::fs::remove_file(p) {
        Ok(()) => 0,
        Err(e) if e.kind() == std::io::ErrorKind::IsADirectory => match std::fs::remove_dir(p) {
            Ok(()) => 0,
            Err(e) => {
                errno::set_from_io(&e);
                -1
            }
        },
        Err(e) => {
            errno::set_from_io(&e);
            -1
        }
    }
}

pub fn rename<F: ByteView + ?Sized, T: ByteView + ?Sized>(from: &F, to: &T) -> core::ffi::c_int {
    let f = view_to_path(from);
    let t = view_to_path(to);
    match std::fs::rename(f, t) {
        Ok(()) => 0,
        Err(e) => {
            errno::set_from_io(&e);
            -1
        }
    }
}

/// Safe sibling of `ctermid` for a caller-supplied buffer. Writes the
/// controlling terminal's path into `dst` and returns its length. A
/// destination with `c_size() >= L_ctermid` always suffices; a smaller
/// one fails with `ERANGE` and is left untouched.
pub fn ctermid_(dst: CStrDest<'_>) -> Option<usize> {
    write_name(dst, b"/dev/tty")
}

/// `ctermid(buf)`. Writes the controlling terminal's path into `buf`,
/// or into a static buffer if `buf` is null, and returns the buffer.
///
/// # Safety
///
/// - If `buf` is non-null, it must be valid for writes of `L_ctermid`
///   bytes (which may be uninitialized), not accessed through any other
///   pointer or reference during the call.
/// - If `buf` is null, the call writes the shared static buffer, as in
///   C. No other thread may call `ctermid(NULL)` concurrently, and no
///   reference to that buffer from an earlier call may be live (and
///   no access through a pointer to it may overlap the call).
///
/// As in C, only the path and its terminator are written.
pub unsafe fn ctermid(buf: *mut u8) -> *mut u8 {
    static mut CTERMID: [u8; crate::L_ctermid] = [0; crate::L_ctermid];
    let dst = if buf.is_null() {
        (&raw mut CTERMID).cast::<u8>()
    } else {
        buf
    };
    let mut path = [0u8; crate::L_ctermid];
    match ctermid_((&mut path).into()) {
        // SAFETY: as in `tmpnam`, with `L_ctermid` and `CTERMID`.
        Some(len) => unsafe {
            std::ptr::copy_nonoverlapping(path.as_ptr(), dst, len + 1);
            dst
        },
        None => std::ptr::null_mut(),
    }
}

/// Internal helper: take ownership of a raw fd and wrap it as a stream.
pub(crate) fn from_raw_handle(handle: Handle, mode: OpenMode, policy: BufferPolicy) -> FILE_ptr {
    let cap = if matches!(policy, BufferPolicy::None) {
        0
    } else {
        BUFSIZ
    };
    let state = State::new(handle, mode, policy, cap);
    slots::insert(Arc::new(FILE::new(state)))
}

// `as_raw_fd` re-export to avoid unused-import lint.
#[allow(unused_imports)]
use AsRawFd as _;
