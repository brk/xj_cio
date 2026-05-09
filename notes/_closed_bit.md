# `FILE::closed`: an atomic bit for lock-free `fclose`

## Why

The original `fclose` (per spec.md §5.7) acquired the stream's
reentrant mutex and called `borrow_mut()` on the inner `RefCell` to
flush and replace the handle with `Handle::Closed`. This works in
isolation, but combines badly with `StreamGuard` (the
`Read`/`BufRead`/`Write`/`Seek` lock-guard wrapper introduced after
spec.md was written): a thread that holds a `StreamGuard` and then
calls `fclose` on the same token would re-acquire the reentrant mutex
fine — but the `RefCell::borrow_mut()` would *panic*, because the
`StreamGuard` already holds an outstanding `RefMut`.

`fclose` is uniquely amenable to a fix here, because deferring the
flush to `FILE::drop` is a correct fallback: every operation that
holds an `Arc<FILE>` will eventually drop it, and the last drop runs
`FILE::drop`, which flushes best-effort. So we can detect the
contended case and skip the explicit flush instead of panicking.

The other public-API functions (`freopen`, `fread`, `fputs`, …) are
not amenable: they have no equivalent fallback. They keep the
documented "don't call public API while holding a `StreamGuard` on
the same stream" constraint and the `RefCell` panic that enforces it.

## What

Three changes:

### 1. `FILE` gains an atomic `closed: AtomicBool`

```rust
pub struct FILE {
    pub closed: AtomicBool,                       // NEW
    inner: ReentrantMutex<RefCell<State>>,
}
```

Initialized to `false` in `FILE::new`. Set to `true` exactly once,
by `fclose`, with `Release` ordering, *after* the slotmap removal
but *before* the (best-effort) flush.

### 2. `fclose` uses `try_borrow_mut`, not `borrow_mut`

```rust
pub fn fclose(t: FILE_ptr) -> c_int {
    let arc = match slots::remove(t) { Some(a) => a, _ => return EOF_with_EBADF };
    arc.closed.store(true, Release);                  // (A)
    let guard = arc.raw_lock();                       // (B) reentrant; never deadlocks self
    let res = match guard.try_borrow_mut() {          // (C) Err iff this thread holds a StreamGuard
        Ok(mut s)  => { let r = flush_internal(&mut s); s.handle = Handle::Closed; r }
        Err(_)     => Ok(()),                          // deferred path
    };
    drop(guard); drop(arc);
    /* ... */
}
```

(A) is a **Release** store. (B) blocks only on cross-thread
contention; on the same thread it's a free reentrant re-acquire.
(C) is the panic-prevention: in the deferred path we don't touch
`State` at all. The actual flush happens when the StreamGuard's
`Drop` releases the last `Arc<FILE>`, at which point `FILE::drop`
(`state.rs:165`) runs `flush_internal` best-effort.

### 3. `with_token` and `StreamGuard::{new, try_new}` check the bit

Every public-API call goes through `ops::with_token`, which now
**Acquire**-loads `closed` after `slots::lookup` and bails with
`EBADF` if set. This catches the narrow race where:

> Thread A: `slots::lookup(t)` → `Some(arc)` (slot still present)
> Thread B: `slots::remove(t)` → `Some(arc')` (different `Arc` clone, same `FILE`)
> Thread B: `arc'.closed.store(true, Release)`
> Thread A: now goes into `arc.with_state(...)` — without the closed
>           check, would operate on a logically-closed stream.

`StreamGuard::new` does the same check after acquiring the reentrant
mutex (one atomic load; cheap). `stream_lock` reports `EBADF` when
the construction fails because of a closed-race; `stream_try_lock`
does not (it can't easily distinguish "closed" from "contended"
without a richer return type).

The Release/Acquire pairing establishes the happens-before that
ensures any thread observing `closed == true` also observes the
preceding `slots::remove`.

## Trade-offs

### Lost: synchronous flush errors in the deferred path

POSIX `fclose` returns `EOF` when "any unwritten buffered data … cannot
be written." In the new design:

- **Common case** (no `StreamGuard`/in-flight op held by the calling
  thread): `try_borrow_mut` succeeds, we flush synchronously, errors
  propagate to the caller exactly as before. **No semantic change.**
- **Deferred case** (calling thread holds a `StreamGuard` on the same
  stream): we return `0` optimistically. The flush happens when the
  StreamGuard drops, via `FILE::drop`'s best-effort path, which
  swallows errors (Drop has no error channel). The caller of `fclose`
  doesn't learn about flush failures.

Mitigation: callers who care can call `g.flush_buf()` (or `g.flush()`,
the `Write::flush` impl) on the `StreamGuard` before `fclose`. This
is documented on the `fclose` entry point.

The cross-thread case is **unchanged** from before: `fclose` blocks at
`raw_lock()` until the other thread releases the lock, then flushes
synchronously. POSIX semantics preserved.

### Lost: `Handle::Closed` set in deferred path

In the deferred path, `s.handle` is *not* replaced with
`Handle::Closed` — we never get the borrow needed to write it. This is
purely cosmetic: the `closed` bit (always set) already short-circuits
every public-API entry point with `EBADF`, so no operation ever
reaches `Handle::read`/`write`/`flush` on a closed stream through the
public API. The `Handle::Closed` variant remains useful for the
synchronous path and is kept in the enum.

### Cost: one atomic load per public-API call

`with_token` adds one `Acquire` load. On x86-64 this is a plain
`mov`; on ARM it's a `LDAR`. Negligible against any operation that
touches the kernel; not measurable against the existing slotmap
`RwLock::read` and `Arc::clone` already on the path.

## Diff from spec.md

The spec text needs the following adjustments (not yet applied — this
file is the rationale; the actual spec edits are a follow-up):

### §4.1 (The stream wrapper)

The `FILE` struct definition adds `closed: AtomicBool`:

```rust
pub struct FILE {
    closed: AtomicBool,                          // ADDED
    inner: ReentrantMutex<RefCell<State>>,
}
```

The accompanying paragraph should note: *"`closed` is set exactly
once, by `fclose`, with `Release` ordering. Every public-API entry
point Acquire-loads it after slotmap lookup and bails with `EBADF`
if set — this covers a narrow race window in which an `Arc<FILE>`
clone outlived the slot removal it was issued under."*

### §5.7 (Closing)

The current step list is:

1. Decode/validate `t`; `EOF`+`EBADF` on failure.
2. Remove the slot from `SLOTS`.
3. Take the stream lock on the `Arc<FILE>`.
4. If `dirty`, flush.
5. Drop the `Arc`.
6. Return 0 / `EOF`.

Replace with:

1. Decode/validate `t`; `EOF`+`EBADF` on failure.
2. Remove the slot from `SLOTS` (invalidates the token for new lookups).
3. **`closed.store(true, Release)`** — fences every observer of the
   bit against the preceding `slots::remove` and the upcoming flush.
4. **Reentrant-acquire** the stream's mutex. (Never self-deadlocks.)
5. **`try_borrow_mut()`** the inner `RefCell`:
   - **Ok**: flush, set `handle = Handle::Closed`, return flush result.
   - **Err**: this thread holds a `StreamGuard` on the same stream;
     defer the flush to the eventual `FILE::drop`. Return `0`.
6. Drop the local `Arc`. When the last `Arc<FILE>` is released
   (StreamGuard drop / in-flight ops complete), `FILE::drop` runs
   best-effort flush + handle drop.

Add a new paragraph:

> **Synchronous flush errors.** When the `try_borrow_mut` succeeds
> (the common case — no calling-thread `StreamGuard` held), `fclose`
> returns `EOF` on flush failure exactly as POSIX requires. When the
> deferred path runs, `fclose` returns `0` and any flush error is
> lost; callers that care should invoke `StreamGuard::flush_buf` /
> `Write::flush` on the guard before `fclose`.

### §7.3 (Threat model — what the design rules out)

Add a fourth bullet under category (3) "Misuse of the safe API":

> **Closing while locked from the same thread.** A thread that holds
> a `StreamGuard` and calls `fclose` on the same token does not panic
> (the previous design did, via `RefCell::borrow_mut`). It returns
> `0`, defers the flush to `StreamGuard` drop, and the buffered
> bytes still reach disk via `FILE::drop`. Other public-API calls
> from the same thread on the same stream while the guard is held
> still panic — the `RefCell::borrow_mut` enforcement is unchanged
> for them; only `fclose` opts into the soft-fail path because it
> uniquely has a correct deferred-cleanup fallback.
