//! Token store: a global slotmap keyed by generational
//! `(slot, gen)` pairs, with `Arc<FILE>` values.
//!
//! Every public stdio function decodes the caller's `FILE_ptr`, looks
//! up the slot, clones the `Arc<FILE>`, releases the slotmap lock,
//! and then operates on the stream. This sequence guarantees that an
//! in-flight operation cannot be invalidated by a concurrent
//! `fclose` from another thread (the `Arc` clone keeps the `FILE`
//! alive until the operation completes).

use std::sync::Arc;

use parking_lot::RwLock;
use slotmap::{KeyData, SlotMap};

use crate::FILE_ptr;
use crate::state::FILE;

slotmap::new_key_type! {
    pub struct FileKey;
}

/// Global registry of every open stream. Doubles as the iteration
/// surface for `fflush(NULL)` and the atexit flush handler.
static SLOTS: RwLock<Option<SlotMap<FileKey, Arc<FILE>>>> = RwLock::new(None);

fn with_write<R>(f: impl FnOnce(&mut SlotMap<FileKey, Arc<FILE>>) -> R) -> R {
    let mut g = SLOTS.write();
    if g.is_none() {
        *g = Some(SlotMap::with_key());
    }
    f(g.as_mut().unwrap())
}

fn with_read<R>(f: impl FnOnce(&SlotMap<FileKey, Arc<FILE>>) -> R) -> R {
    let g = SLOTS.read();
    match g.as_ref() {
        Some(m) => f(m),
        None => {
            drop(g);
            with_write(|m| f(m))
        }
    }
}

/// Encode a slotmap key into a `FILE_ptr`. The encoding never yields
/// `0` because slotmap's `KeyData::as_ffi()` reserves the low bit of
/// the version field (it's always odd), so the resulting `u64` is
/// always non-zero.
pub fn encode(k: FileKey) -> FILE_ptr {
    use slotmap::Key;
    k.data().as_ffi() as FILE_ptr
}

/// Decode a `FILE_ptr` into a slotmap key, or `None` if the token is
/// the null token (`0`).
pub fn decode(t: FILE_ptr) -> Option<FileKey> {
    if t == 0 {
        return None;
    }
    Some(KeyData::from_ffi(t as u64).into())
}

/// Insert a fresh `FILE` and return its token.
pub fn insert(f: Arc<FILE>) -> FILE_ptr {
    with_write(|m| encode(m.insert(f)))
}

/// Look up a token. Returns `Some(Arc<FILE>)` if the token is valid
/// and the slot has not been freed; `None` otherwise. The returned
/// `Arc` is a clone — callers may release the slotmap lock and still
/// rely on the `FILE` outliving any concurrent `fclose`.
pub fn lookup(t: FILE_ptr) -> Option<Arc<FILE>> {
    let key = decode(t)?;
    with_read(|m| m.get(key).cloned())
}

/// Remove a slot, returning the `Arc<FILE>` that lived there.
/// Subsequent lookups of the same token fail (generation mismatch).
pub fn remove(t: FILE_ptr) -> Option<Arc<FILE>> {
    let key = decode(t)?;
    with_write(|m| m.remove(key))
}

/// Snapshot every currently-open stream. Used by `fflush(NULL)` and
/// the atexit flush handler. Releases the slotmap lock before the
/// caller iterates so a concurrent `fclose` cannot deadlock.
pub fn snapshot() -> Vec<Arc<FILE>> {
    with_read(|m| m.values().cloned().collect())
}
