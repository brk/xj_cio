//! Open / close / freopen / fdopen / tmpfile / remove / rename.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::sync::Arc;

use crate::buffer::BufferPolicy;
use crate::errno;
use crate::handle::Handle;
use crate::slots;
use crate::state::{FILE, OpenMode, State};
use crate::{BUFSIZ, EOF, FILE_ptr};

use super::ops;

fn cstr_to_path(s: &CStr) -> Option<&Path> {
    use std::os::unix::ffi::OsStrExt;
    Some(Path::new(std::ffi::OsStr::from_bytes(s.to_bytes())))
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

pub fn fopen(path: &CStr, mode: &CStr) -> FILE_ptr {
    let m = match OpenMode::parse(mode.to_bytes()) {
        Some(m) => m,
        None => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };
    let p = match cstr_to_path(path) {
        Some(p) => p,
        None => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };
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
/// `errno = EINVAL`).
pub fn freopen_(path: Option<&CStr>, mode: &CStr, t: FILE_ptr) -> FILE_ptr {
    let arc = match slots::lookup(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return std::ptr::null();
        }
    };
    let m = match OpenMode::parse(mode.to_bytes()) {
        Some(m) => m,
        None => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };
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
        let p = match cstr_to_path(cstr) {
            Some(p) => p,
            None => return Err(libc::EINVAL),
        };
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

pub fn freopen(path: *const libc::c_char, mode: &CStr, t: FILE_ptr) -> FILE_ptr {
    let p = if path.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(path) })
    };
    freopen_(p, mode, t)
}

pub fn fdopen(fd: libc::c_int, mode: &CStr) -> FILE_ptr {
    let m = match OpenMode::parse(mode.to_bytes()) {
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
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        errno::set_from_io(&std::io::Error::last_os_error());
        return std::ptr::null();
    }
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
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let pid = unsafe { libc::getpid() };
    for i in 0..32 {
        let path = format!("/tmp/xj_cio_tmp_{}_{}_{}", pid, nanos, i);
        let mut o = OpenOptions::new();
        o.read(true).write(true).create_new(true);
        match o.open(&path) {
            Ok(f) => {
                let _ = std::fs::remove_file(&path); // unlinked-but-open
                let state = State::new(Handle::Regular(f), OpenMode::Wplus, BufferPolicy::Full, BUFSIZ);
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

pub fn tmpnam(buf: *mut libc::c_char) -> *mut libc::c_char {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = unsafe { libc::getpid() } as u64;
    let s = format!("/tmp/xj{:x}", (pid << 32) ^ nanos);
    // L_tmpnam == 20; ensure it fits.
    let bytes = s.as_bytes();

    // Static buffer when the user passes NULL.
    static mut STATIC_BUF: [libc::c_char; crate::L_tmpnam] = [0; crate::L_tmpnam];

    let dst = if buf.is_null() {
        (&raw mut STATIC_BUF) as *mut libc::c_char
    } else {
        buf
    };
    let cap = crate::L_tmpnam;
    let n = bytes.len().min(cap - 1);
    unsafe {
        for i in 0..n {
            *dst.add(i) = bytes[i] as libc::c_char;
        }
        *dst.add(n) = 0;
    }
    dst
}

pub fn remove(path: &CStr) -> core::ffi::c_int {
    let p = match cstr_to_path(path) {
        Some(p) => p,
        None => {
            errno::set(libc::EINVAL);
            return -1;
        }
    };
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

pub fn rename(from: &CStr, to: &CStr) -> core::ffi::c_int {
    let f = match cstr_to_path(from) {
        Some(p) => p,
        None => {
            errno::set(libc::EINVAL);
            return -1;
        }
    };
    let t = match cstr_to_path(to) {
        Some(p) => p,
        None => {
            errno::set(libc::EINVAL);
            return -1;
        }
    };
    match std::fs::rename(f, t) {
        Ok(()) => 0,
        Err(e) => {
            errno::set_from_io(&e);
            -1
        }
    }
}

pub fn ctermid(buf: *mut libc::c_char) -> *mut libc::c_char {
    static mut CTERMID: [libc::c_char; crate::L_ctermid] = [0; crate::L_ctermid];
    let s = b"/dev/tty\0";
    let dst = if buf.is_null() {
        (&raw mut CTERMID) as *mut libc::c_char
    } else {
        buf
    };
    unsafe {
        for (i, &b) in s.iter().enumerate() {
            *dst.add(i) = b as libc::c_char;
        }
    }
    dst
}

/// Internal helper: take ownership of a raw fd and wrap it as a stream.
pub(crate) fn from_raw_handle(handle: Handle, mode: OpenMode, policy: BufferPolicy) -> FILE_ptr {
    let cap = if matches!(policy, BufferPolicy::None) { 0 } else { BUFSIZ };
    let state = State::new(handle, mode, policy, cap);
    slots::insert(Arc::new(FILE::new(state)))
}

// `as_raw_fd` re-export to avoid unused-import lint.
#[allow(unused_imports)]
use AsRawFd as _;
