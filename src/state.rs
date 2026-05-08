//! The stream wrapper.
//!
//! The `FILE` type is a `parking_lot::ReentrantMutex<RefCell<State>>`.
//! `flockfile` requires reentrancy on the same thread; the
//! `RefCell` materializes the logically-`&mut` access from the
//! `&State` that `ReentrantMutex` exposes.
//!
//! The two layers are not redundant: the mutex enforces *cross*-thread
//! exclusion (with reentrancy on the same thread); the `RefCell`
//! enforces single-borrow within a single lock-holding stack frame.
//! All public stdio functions take a *single* `borrow_mut` for the
//! duration of the call and never call back into another public
//! function while holding it, so the inner borrow never panics.

use std::cell::RefCell;

use parking_lot::ReentrantMutex;
use smallvec::SmallVec;

use crate::buffer::{Buffer, BufferPolicy, UserBuf};
use crate::handle::Handle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Last operation was a read; the buffer (if non-empty) holds
    /// pre-fetched bytes.
    Read,
    /// Last operation was a write; the buffer (if dirty) holds
    /// pending writes.
    Write,
    /// The stream is in a clean state (just opened, just flushed,
    /// just seeked). The next read or write decides direction.
    Either,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// "r": read-only; stream must already exist.
    R,
    /// "w": write-only, truncate.
    W,
    /// "a": write-only, append.
    A,
    /// "r+": read+write, no truncate.
    Rplus,
    /// "w+": read+write, truncate.
    Wplus,
    /// "a+": read+write+append.
    Aplus,
}

impl OpenMode {
    pub fn readable(self) -> bool {
        matches!(self, OpenMode::R | OpenMode::Rplus | OpenMode::Wplus | OpenMode::Aplus)
    }
    pub fn writable(self) -> bool {
        !matches!(self, OpenMode::R)
    }
    pub fn append(self) -> bool {
        matches!(self, OpenMode::A | OpenMode::Aplus)
    }

    pub fn parse(s: &[u8]) -> Option<OpenMode> {
        // Strip 'b' (binary) and 'e' (close-on-exec) anywhere; ignore
        // 'x' for simplicity (POSIX 2024 introduces it via O_EXCL but
        // for our v1 we only need to recognize the base mode).
        let mut base = None;
        let mut plus = false;
        for &c in s {
            match c {
                b'r' if base.is_none() => base = Some(b'r'),
                b'w' if base.is_none() => base = Some(b'w'),
                b'a' if base.is_none() => base = Some(b'a'),
                b'+' => plus = true,
                b'b' | b'e' | b'x' | b't' => {}
                _ => return None,
            }
        }
        match (base?, plus) {
            (b'r', false) => Some(OpenMode::R),
            (b'w', false) => Some(OpenMode::W),
            (b'a', false) => Some(OpenMode::A),
            (b'r', true) => Some(OpenMode::Rplus),
            (b'w', true) => Some(OpenMode::Wplus),
            (b'a', true) => Some(OpenMode::Aplus),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Unset,
    Byte,
    Wide,
}

pub struct State {
    pub handle: Handle,
    pub buffer: Buffer,
    pub pushback: SmallVec<[u8; 8]>,
    pub last_op: Direction,
    pub eof: bool,
    pub err: bool,
    pub mode: OpenMode,
    pub orientation: Orientation,
    pub user_buffer: Option<UserBuf>,
}

impl State {
    pub fn new(handle: Handle, mode: OpenMode, policy: BufferPolicy, cap: usize) -> Self {
        State {
            handle,
            buffer: Buffer::new(policy, cap),
            pushback: SmallVec::new(),
            last_op: Direction::Either,
            eof: false,
            err: false,
            mode,
            orientation: Orientation::Unset,
            user_buffer: None,
        }
    }
}

pub struct FILE {
    inner: ReentrantMutex<RefCell<State>>,
}

impl FILE {
    pub fn new(state: State) -> Self {
        FILE {
            inner: ReentrantMutex::new(RefCell::new(state)),
        }
    }

    /// Lock the stream and run `f` with mutable access to its state.
    /// `f` must not call back into the public API while holding this
    /// borrow (would re-borrow the `RefCell`).
    pub fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let g = self.inner.lock();
        let mut state = g.borrow_mut();
        f(&mut state)
    }

    /// Take the reentrant mutex without borrowing. Used by
    /// `flockfile` so the caller owns one lock count.
    pub fn raw_lock(&self) -> parking_lot::ReentrantMutexGuard<'_, RefCell<State>> {
        self.inner.lock()
    }

    pub fn raw_try_lock(&self) -> Option<parking_lot::ReentrantMutexGuard<'_, RefCell<State>>> {
        self.inner.try_lock()
    }

    /// `funlockfile` semantics: drop one count from the reentrant
    /// mutex. Implemented by leaking a guard at lock time and forcing
    /// it on unlock — see `api::lock`.
    pub fn raw_mutex(&self) -> &ReentrantMutex<RefCell<State>> {
        &self.inner
    }
}

impl Drop for FILE {
    fn drop(&mut self) {
        // Best-effort flush. If the slot was removed via fclose, the
        // explicit flush has already happened; this catches the case
        // of programs that exit while still holding Arc clones.
        let _ = self.with_state(|s| {
            let _ = crate::api::ops::flush_internal(s);
        });
    }
}
