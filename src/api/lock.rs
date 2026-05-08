//! `flockfile`, `funlockfile`, `ftrylockfile`.
//!
//! These functions don't fit `with_token`'s lock-then-borrow_mut
//! protocol because their effect is to *retain* the lock across the
//! call boundary. We solve this by lifting the guard into a
//! thread-local stack: each `flockfile`/`ftrylockfile` pushes a
//! `LockEntry` (which holds both the `Arc<FILE>` keeping the mutex
//! alive and a `'static`-extended guard) and `funlockfile` pops one.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::sync::Arc;

use parking_lot::ReentrantMutexGuard;

use crate::slots;
use crate::state::{FILE, State};
use crate::FILE_ptr;

/// One lock acquisition. Field order matters: `guard` is dropped
/// before `_arc`, so the mutex (which lives inside `*_arc`) is still
/// valid at the moment we release it.
struct LockEntry {
    /// SAFETY: the underlying mutex lives in `_arc` (the `FILE` it
    /// references). Lifetime extension is sound because we keep the
    /// `Arc` alive and drop the guard first.
    guard: ManuallyDrop<ReentrantMutexGuard<'static, RefCell<State>>>,
    _arc: Arc<FILE>,
    token: FILE_ptr,
}

impl Drop for LockEntry {
    fn drop(&mut self) {
        // SAFETY: `_arc` is still alive (it's a sibling field
        // dropped after `guard`).
        unsafe { ManuallyDrop::drop(&mut self.guard) };
    }
}

thread_local! {
    static LOCK_STACK: RefCell<Vec<LockEntry>> = const { RefCell::new(Vec::new()) };
}

fn make_entry(arc: Arc<FILE>, token: FILE_ptr) -> LockEntry {
    let g = arc.raw_lock();
    // SAFETY: we pin the Arc alongside the guard and ensure Drop
    // order. The mutex is `&'arc ReentrantMutex` underneath, but
    // because `_arc` cannot be dropped before `guard` we may safely
    // promote the guard's lifetime to `'static` for storage.
    let g: ReentrantMutexGuard<'static, RefCell<State>> = unsafe { std::mem::transmute(g) };
    LockEntry { guard: ManuallyDrop::new(g), _arc: arc, token }
}

fn make_entry_try(arc: Arc<FILE>, token: FILE_ptr) -> Option<LockEntry> {
    let g = arc.raw_try_lock()?;
    let g: ReentrantMutexGuard<'static, RefCell<State>> = unsafe { std::mem::transmute(g) };
    Some(LockEntry { guard: ManuallyDrop::new(g), _arc: arc, token })
}

pub fn flockfile(t: FILE_ptr) {
    if let Some(arc) = slots::lookup(t) {
        let entry = make_entry(arc, t);
        LOCK_STACK.with(|s| s.borrow_mut().push(entry));
    }
}

pub fn ftrylockfile(t: FILE_ptr) -> core::ffi::c_int {
    let arc = match slots::lookup(t) {
        Some(a) => a,
        None => return -1,
    };
    match make_entry_try(arc, t) {
        Some(e) => {
            LOCK_STACK.with(|s| s.borrow_mut().push(e));
            0
        }
        None => 1,
    }
}

pub fn funlockfile(t: FILE_ptr) {
    LOCK_STACK.with(|s| {
        let mut v = s.borrow_mut();
        // POSIX: matched to the most recent flockfile on this token.
        if let Some(pos) = v.iter().rposition(|e| e.token == t) {
            let entry = v.remove(pos);
            drop(entry);
        }
    });
}
