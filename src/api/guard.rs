//! `StreamGuard`: an RAII guard over an open `FILE_ptr` that
//! implements `Read`, `Write`, `BufRead`, and `Seek`.
//!
//! Acquired via [`stream_lock`] (or [`stream_try_lock`]); released
//! when dropped. While held, the guard owns:
//!
//! 1. an `Arc<FILE>` clone (keeps the stream alive through any
//!    concurrent `fclose` from another thread, just like in-flight
//!    public-API calls);
//! 2. the stream's reentrant mutex guard (cross-thread exclusion);
//! 3. a `RefMut` into the stream's `State` (within-thread exclusion).
//!
//! # Constraint
//!
//! While a `StreamGuard` is held, the same thread must not call the
//! public C-style API (`fread_raw`, `fputs`, `fseek`, ...) on the *same*
//! stream. The reentrant mutex would admit such a call cheaply, but
//! its `borrow_mut` on the inner `RefCell` would panic — this guard
//! already holds an outstanding `RefMut`. Use the inherent methods
//! and trait impls on `StreamGuard` instead while it's in scope.
//!
//! Public API calls on *other* streams are unaffected.

use std::cell::{RefCell, RefMut};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::mem::ManuallyDrop;
use std::sync::Arc;

use parking_lot::ReentrantMutexGuard;

use crate::FILE_ptr;
use crate::errno;
use crate::slots;
use crate::state::{Direction, FILE, State};

use super::ops;
use super::stdio_;

/// RAII guard over a locked, buffered stream.
///
/// Field declaration order is the drop order Rust actually performs:
/// `state` is dropped first (releases the `RefCell` borrow), then
/// `_guard` (releases the reentrant mutex), then `_arc` (decrements
/// the FILE's refcount, possibly running `FILE::drop`). All three
/// are required because the `'static` lifetimes on `state` and
/// `_guard` are extended via transmute and rely on `_arc` outliving
/// both borrowers.
pub struct StreamGuard {
    state: ManuallyDrop<RefMut<'static, State>>,
    _guard: ManuallyDrop<ReentrantMutexGuard<'static, RefCell<State>>>,
    _arc: Arc<FILE>,
    token: FILE_ptr,
}

impl StreamGuard {
    fn new(arc: Arc<FILE>, token: FILE_ptr) -> Option<Self> {
        // SAFETY: only the lifetime changes. The guard borrows the
        // mutex inside the `FILE` heap allocation, which does not move
        // when `arc` moves into the struct, and which stays alive while
        // `arc` (and later `_arc`) holds a strong count. Every exit
        // path releases the guard before the `Arc`: on the early
        // return below, the local `guard` drops before the parameter
        // `arc`; once constructed, `Drop for StreamGuard` drops it
        // before `_arc`. The guard never leaves this thread, because
        // `ReentrantMutexGuard` (and so `StreamGuard`) is `!Send`.
        let guard: ReentrantMutexGuard<'static, RefCell<State>> =
            unsafe { std::mem::transmute(arc.raw_lock()) };
        // Race-window check: we may have cloned the Arc just before
        // `fclose` ran. The closed bit (set under `Release` by
        // `fclose`) tells us so. Drop the guard and bail; the caller
        // sets `errno = EBADF`.
        if arc.closed.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        // SAFETY: only the lifetime changes. The `RefMut` points at the
        // `RefCell` inside the `FILE` allocation, not into `guard`, so
        // moving `guard` into the struct does not invalidate it. That
        // allocation outlives it (see above), and `Drop for
        // StreamGuard` releases it while the mutex is still held, so
        // the `RefCell` borrow flag is only touched under the lock.
        // `borrow_mut` itself panics (soundly) if this thread already
        // borrows the state.
        let state: RefMut<'static, State> = unsafe { std::mem::transmute(guard.borrow_mut()) };
        Some(StreamGuard {
            state: ManuallyDrop::new(state),
            _guard: ManuallyDrop::new(guard),
            _arc: arc,
            token,
        })
    }

    fn try_new(arc: Arc<FILE>, token: FILE_ptr) -> Option<Self> {
        // SAFETY: as for the guard in `new`; the `?` early return drops
        // nothing but `arc`.
        let guard: ReentrantMutexGuard<'static, RefCell<State>> =
            unsafe { std::mem::transmute(arc.raw_try_lock()?) };
        if arc.closed.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        // SAFETY: as for the `RefMut` in `new`.
        let state: RefMut<'static, State> = unsafe { std::mem::transmute(guard.borrow_mut()) };
        Some(StreamGuard {
            state: ManuallyDrop::new(state),
            _guard: ManuallyDrop::new(guard),
            _arc: arc,
            token,
        })
    }

    #[inline]
    fn s_mut(&mut self) -> &mut State {
        &mut self.state
    }

    /// `feof`-equivalent without taking the global slotmap lock.
    pub fn eof(&self) -> bool {
        self.state.eof
    }

    /// `ferror`-equivalent.
    pub fn error(&self) -> bool {
        self.state.err
    }

    /// `clearerr`-equivalent.
    pub fn clear_err(&mut self) {
        let s = self.s_mut();
        s.eof = false;
        s.err = false;
    }

    /// Underlying fd, or `-1` for memory streams / closed streams.
    /// Equivalent to `fileno`, but skips the slotmap roundtrip.
    pub fn fileno(&self) -> core::ffi::c_int {
        self.state.handle.raw_fd()
    }

    /// Push one byte back onto the stream. Equivalent to `ungetc`,
    /// but unbounded (the pushback `Vec` grows).
    pub fn unget(&mut self, b: u8) -> io::Result<()> {
        let s = self.s_mut();
        if !s.mode.readable() {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        ops::switch_to_read(s)?;
        s.pushback.push(b);
        s.eof = false;
        Ok(())
    }

    /// Drain the write buffer (if any) to the underlying handle.
    /// Equivalent to `fflush` on the held stream. Provided in addition
    /// to `Write::flush` so it's reachable when the guard is being
    /// used purely as a `BufRead`.
    pub fn flush_buf(&mut self) -> io::Result<()> {
        ops::flush_internal(self.s_mut())
    }

    /// The token this guard was acquired from.
    ///
    /// Holding this guard does not stop another thread from
    /// `fclose`-ing the same token (the slot would be removed from
    /// the registry, but our `Arc<FILE>` keeps the `FILE` alive). So
    /// `token()` may, after such a race, refer to an already-freed
    /// slot — using it on a public-API call after dropping this guard
    /// would yield `EBADF`. Treat it as informational unless you can
    /// rule out concurrent close.
    pub fn token(&self) -> FILE_ptr {
        self.token
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        // SAFETY: each `ManuallyDrop` field is dropped exactly once,
        // here, and never used again. The order (`state`, then
        // `_guard`) releases the `RefCell` borrow while the mutex is
        // still held, and both before `_arc`, which Rust drops after
        // this body and which keeps their referent alive until then.
        unsafe {
            ManuallyDrop::drop(&mut self.state);
            ManuallyDrop::drop(&mut self._guard);
        }
    }
}

/// Lock the stream identified by `t`. Returns `None` if `t` is
/// invalid (and sets `errno = EBADF`).
///
/// Blocks if another thread holds the same stream's lock.
pub fn stream_lock(t: FILE_ptr) -> Option<StreamGuard> {
    let arc = match slots::lookup(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return None;
        }
    };
    let g = StreamGuard::new(arc, t);
    if g.is_none() {
        errno::set(libc::EBADF);
    }
    g
}

/// Try-lock variant. Returns `None` on contention or invalid token.
/// On invalid token, sets `errno = EBADF`; on contention, leaves
/// errno unchanged.
pub fn stream_try_lock(t: FILE_ptr) -> Option<StreamGuard> {
    let arc = match slots::lookup(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return None;
        }
    };
    let g = StreamGuard::try_new(arc, t);
    // Note: we don't set errno on simple contention (None from try_new
    // due to mutex held by another thread); only on the closed-race
    // case do we want EBADF. Distinguishing is awkward without
    // returning a richer error, so we leave errno alone — the caller
    // can call `errno::get()` and test against EBADF if they care.
    g
}

/// Lock stdin. The stdin slot is registered for the program's
/// lifetime, so this never returns `None`.
pub fn stdin_lock() -> StreamGuard {
    stream_lock(stdio_::stdin()).expect("stdin slot present")
}

/// Lock stdout.
pub fn stdout_lock() -> StreamGuard {
    stream_lock(stdio_::stdout()).expect("stdout slot present")
}

/// Lock stderr.
pub fn stderr_lock() -> StreamGuard {
    stream_lock(stdio_::stderr()).expect("stderr slot present")
}

impl Read for StreamGuard {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let s = self.s_mut();
        if !s.mode.readable() {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        ops::read_into(s, buf)
    }
}

impl BufRead for StreamGuard {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let s = self.s_mut();
        if !s.mode.readable() {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        ops::switch_to_read(s)?;

        // Flatten any pushback into the front of the read buffer so
        // we can hand back a single contiguous slice. Pushback is
        // popped LIFO by the rest of the crate; reverse it as we copy
        // so a forward scan over the returned slice yields ungetc'd
        // bytes in last-pushed-first order.
        if !s.pushback.is_empty() {
            let n = s.pushback.len();
            let unread = s.buffer.fill - s.buffer.cursor;

            // Make sure the backing Vec is large enough; for
            // `_IONBF` streams `refill_buffer` may have shrunk it.
            if s.buffer.data.len() < n + unread {
                s.buffer.data.resize(n + unread, 0);
            }
            // Shift any unread buffered bytes right by n.
            s.buffer
                .data
                .copy_within(s.buffer.cursor..s.buffer.cursor + unread, n);
            // Drain pushback (in reverse) into [0..n].
            for (i, b) in s.pushback.drain(..).rev().enumerate() {
                s.buffer.data[i] = b;
            }
            s.buffer.cursor = 0;
            s.buffer.fill = n + unread;
        }

        if s.buffer.cursor == s.buffer.fill {
            ops::refill_buffer(s)?;
        }
        Ok(&s.buffer.data[s.buffer.cursor..s.buffer.fill])
    }

    fn consume(&mut self, amt: usize) {
        let s = self.s_mut();
        // BufRead contract: `amt` must be <= what `fill_buf` returned.
        // We saturate defensively rather than panic if a caller
        // violates that.
        s.buffer.cursor = (s.buffer.cursor + amt).min(s.buffer.fill);
    }
}

impl Write for StreamGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = self.s_mut();
        if !s.mode.writable() {
            errno::set(libc::EBADF);
            s.err = true;
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        ops::write_from(s, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        ops::flush_internal(self.s_mut())
    }
}

impl Seek for StreamGuard {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let s = self.s_mut();
        if !s.handle.is_seekable() {
            return Err(io::Error::from_raw_os_error(libc::ESPIPE));
        }
        // Mirror `fseek` semantics: flush write buffer, discard read
        // buffer + pushback, clear EOF, transition to neutral
        // direction.
        if s.last_op == Direction::Write {
            ops::flush_internal(s)?;
        }
        let adj = match pos {
            SeekFrom::Current(o) if s.last_op == Direction::Read => {
                let backup = (s.buffer.fill - s.buffer.cursor) + s.pushback.len();
                SeekFrom::Current(o - backup as i64)
            }
            other => other,
        };
        let new = s.handle.seek(adj)?;
        s.buffer.discard();
        s.pushback.clear();
        s.eof = false;
        s.last_op = Direction::Either;
        Ok(new)
    }
}
