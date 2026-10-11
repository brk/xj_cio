# `xj_cio`: Specification and Implementation Plan

A faithful, mostly-safe Rust re-implementation of C's `<stdio.h>`, designed as a translation target for tools like C2Rust.

---

## 1. Goals and Non-Goals

### 1.1 Goals

1. **Semantic fidelity.** Implement the 57 stream-oriented functions of POSIX 2024 `<stdio.h>` such that programs ported by mechanical translation behave indistinguishably from their C originals on the cases that matter (buffering visible to other parts of the program, error indicator timing, position tracking with `ungetc`, `freopen` identity preservation, `flockfile` reentrancy, etc.).
2. **Safety.** The public API is safe Rust except where the C semantics fundamentally require otherwise: raw fd ingestion, raw buffer and C-string pointers, and format arguments that carry pointers. Those entry points are `unsafe fn`s with documented preconditions (§7.1). A program that calls only the safe subset cannot trigger UB through the library.
3. **FORCRAT-completeness.** All eleven categories that FORCRAT identifies as "unsupported" become tractable when this library is the translation target. The library handles them; the translator's job collapses to syntactic rewriting.
4. **Coexistence with `std::io`.** For the standard streams (`stdin`/`stdout`/`stderr`), the library shares buffering and locking with `std::io::Std{in,out,err}`, so `print!` and `our_fprintf(our_stdout, ...)` interleave correctly and a flush in either system propagates to the other.
5. **Translator-friendliness.** The public surface mirrors libc's signatures (modulo Rust types for buffers and error returns) so that a rewriter can produce calls one-to-one without complex shape changes.

### 1.2 Non-Goals (v1)

- **Bit-compatible `FILE` layout.** Mixed Rust/C builds where C code calls `fread` *on our streams* are out of scope; FFI in the other direction (Rust calls libc, our library is unrelated) is unaffected.
- **Wide character streams** (`fwprintf`, `fwide`, orientation). Deferred to a v2 milestone; v1 returns `EILSEQ`-equivalent on wide-character entry points if exposed at all.
- **Locale-aware formatting.** `printf` honors the C locale only — no thousands separators, no localized decimal points.
- **Async I/O.** All operations are blocking; no `async` API.
- **Performance parity with native `std::io` idioms.** We aim for "comparable to libc stdio," not "comparable to a hand-tuned `BufReader<File>`."

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│   Public C-style API:  fopen, fread, fprintf, ferror, ...        │
├──────────────────────────────────────────────────────────────────┤
│   FILE_ptr = opaque `*const _File` handle issued by the slotmap  │
├──────────────────────────────────────────────────────────────────┤
│   Slotmap<FileKey, Arc<FILE>>                                    │
│     ├─ Generational keys: stale handles reject deterministically │
│     ├─ Arc clone on lookup: in-flight ops survive concurrent close│
│     └─ Doubles as the registry for fflush(NULL) and atexit       │
├──────────────────────────────────────────────────────────────────┤
│   FILE = Stream wrapper                                          │
│     ├─ flockfile reentrant mutex                                 │
│     └─ State { buffer, pushback, indicators, mode, handle }      │
├──────────────────────────────────────────────────────────────────┤
│   Subsystems                                                     │
│     ├─ Buffer engine (read/write/line/none policies)             │
│     ├─ Format engine (printf/scanf runtime parsers)              │
│     └─ Handle dispatch (File, Stdio, Pipe, Mem, Closed)          │
├──────────────────────────────────────────────────────────────────┤
│   Foundations:  std::io, std::fs, std::process, std::os::fd      │
└──────────────────────────────────────────────────────────────────┘
```

The architecture is strictly layered. Each public function is a thin adapter that acquires the stream's mutex, invokes one or more subsystems, updates indicators, and returns. Subsystems do not call back into the public API.

---

## 3. Public API

### 3.1 Function inventory (POSIX 2024, stream-based subset)

| Category | Functions |
|---|---|
| Open/close | `fopen`, `freopen`, `fclose`, `fdopen`, `fmemopen`, `open_memstream`, `popen`, `pclose`, `tmpfile` |
| Bulk I/O | `fread`, `fwrite` |
| Char I/O | `fgetc`, `getc`, `getchar`, `fputc`, `putc`, `putchar`, `ungetc` |
| Line I/O | `fgets`, `fputs`, `puts`, `getline`, `getdelim` |
| Formatted output | `fprintf!`, `printf!`, their safe forms, and pre-parsed plan APIs |
| Formatted input | `fscanf`, `scanf`, `vfscanf`, `vscanf` |
| Positioning | `fseek`, `fseeko`, `ftell`, `ftello`, `fgetpos`, `fsetpos`, `rewind` |
| Buffering | `setbuf`, `setvbuf`, `fflush` |
| Indicators | `clearerr`, `feof`, `ferror`, `perror` |
| Locking | `flockfile`, `ftrylockfile`, `funlockfile` |
| Globals | `stdin()`, `stdout()`, `stderr()` (functions, not statics) |
| Misc | `fileno`, `remove`, `rename` |

### 3.2 Naming

Functions live in a `xj_cio` crate and are named identically to their POSIX counterparts. The `FILE_ptr` type is an opaque pointer-typed handle:

```rust
pub enum _File {}
pub type FILE_ptr = *const _File;     // null means "no file" (replaces NULL)

pub fn fopen(path: &CStr, mode: &CStr) -> FILE_ptr;
pub unsafe fn fread(buf: *mut u8, size: usize, n: usize, f: FILE_ptr) -> usize;
pub fn fread_(dst: &mut [u8], f: FILE_ptr) -> usize;
pub fn ferror(f: FILE_ptr) -> c_int;
// ...
```

The handle-based signatures are chosen over Rust-native (`&mut Stream`) ones to keep mechanical translation close to the original source: every C `FILE *f` becomes a Rust `f: FILE_ptr`, and every `f == NULL` test becomes `f.is_null()`.

C `char *` and `const char *` parameters and returns become `*mut u8` and `*const u8`, never `libc::c_char`. Examples are the buffers of `fgets`, `snprintf`, `tmpnam` and `setvbuf`, `*lineptr` in `getline`, the strings of `perror` and `freopen`, and `OutArg::Str`. Bytes are what the library reads and writes, and `u8` matches the safe siblings' `&[u8]` and `CStrDest` without casts. It also avoids `c_char`'s platform-dependent signedness. A pointer obtained from `CStr::as_ptr` or `CString::as_ptr` (which return `*const c_char`) is passed as `p.cast()`.

Where a POSIX signature takes a raw buffer pointer, the raw version is an `unsafe fn` whose `# Safety` section states the caller's contract (§7.1), and a safe sibling with a trailing underscore takes Rust types instead: `fread_`, `fwrite_`, `fmemopen_`, `perror_`, `freopen_`, and the string functions of §8 (`fgets_`, `tmpnam_`, `ctermid_`, `getline_`, `getdelim_`). The two always share one implementation. Usually the raw version wraps its pointer and delegates to the sibling. Where the raw buffer may be uninitialized, both instead call a common core that writes only the bytes C writes (§7.1).

The `c_abi` module supplies shared `extern "C"` entry points for fixed-signature stdio functions. Translated direct calls use ordinary APIs when their arguments match, including borrowed C-string views for literals and guided storage. Raw C strings and `void *` buffers can use the shared argument adapters. C function references use `c_abi` so their calling convention matches the translated function-pointer type; Tenjin does not emit a local forwarding function for every stdio declaration. `fgetpos` and `fsetpos` accept a pointer type parameter for translated `fpos_t` records, under the ordinary layout and storage safety requirements. The wrappers use `u8` for C character storage and `FILE_ptr` for stream handles, and do not export libc symbol names. Variadic formatted I/O remains on the macros and plan APIs.

The choice of `*const _File` is still compatible with the safety story (§7): although the handle is pointer-typed, it is not a real pointer into our data structures. The `_File` type is just an opaque placeholder, and the pointer bits are synthesized from slotmap keys rather than borrowed from a live `FILE` allocation. See §4.3.

### 3.3 Error model

- Every function sets a thread-local `errno` (mirrored to the platform `errno` on platforms where that's safe and zero-cost; otherwise our own `thread_local!`).
- Every function also sets the per-stream `error` indicator on failure (or `eof` if EOF was the cause), exactly as libc does.
- Return values match POSIX (e.g., `fopen` returns a null handle on failure, `fread` returns the short count, `fseek` returns -1 on failure).
- Functions that receive an invalid `FILE_ptr` (one that was never issued, or whose slot has been freed and possibly reissued with a different generation) set `errno = EBADF` and return the appropriate failure value. They do not touch any stream state.

---

## 4. Core Types

### 4.1 The stream wrapper

```rust
pub struct FILE {
    inner: ReentrantMutex<RefCell<State>>,
}

struct State {
    handle: Handle,
    buffer: Buffer,
    pushback: SmallVec<[u8; 8]>,    // ungetc bytes, popped LIFO before buffer
    last_op: Direction,             // Read | Write | Either (after seek/flush)
    eof: bool,
    err: bool,
    mode: OpenMode,                 // r, w, a, rplus, wplus, aplus
    orientation: Orientation,       // Byte | Wide | Unset (v2)
    user_buffer: Option<UserBuf>,   // tracks a setvbuf-supplied buffer
}

enum Direction { Read, Write, Either }

enum Handle {
    Regular(std::fs::File),                        // fopen, tmpfile
    StdoutShared(std::io::Stdout),                 // stdout — see §6
    StderrShared(std::io::Stderr),                 // stderr — see §6
    StdinShared(std::io::Stdin),                   // stdin  — see §6
    StdoutBypass(OwnedFd),                         // after setvbuf opt-out
    StderrBypass(OwnedFd),
    StdinBypass(OwnedFd),
    Pipe { child: Option<Child>, fd: OwnedFd, dir: PipeDir },
    Mem(Cursor<MemBacking>),                       // fmemopen, open_memstream
    Closed,                                        // post-fclose
}
```

`ReentrantMutex` is required because `flockfile` allows the same thread to lock the same stream multiple times. We use [`parking_lot::ReentrantMutex`] (or hand-roll one) wrapped around a `RefCell`. The `ReentrantMutex<RefCell<State>>` pattern is a standard one for "logically `&mut` from multiple lock holds in the same thread."

### 4.2 The buffer engine

```rust
struct Buffer {
    data: Vec<u8>,
    capacity: usize,
    fill: usize,        // bytes currently in `data`
    cursor: usize,      // next byte to read (read mode) or 0 (write mode)
    dirty: bool,        // write mode: buffer holds unflushed writes
    policy: BufferPolicy,
}

enum BufferPolicy {
    Full(usize),    // _IOFBF — flush only when full or on fflush
    Line(usize),    // _IOLBF — flush on '\n', when full, or on fflush
    None,           // _IONBF — write-through, no buffering
}
```

Read- and write-buffering share the same `Vec` because at any moment a stream is in exactly one direction (`last_op`); the buffer is fully drained on direction change.

### 4.3 The handle store

The translated C code never receives a pointer into our data structures. Instead, every `FILE_ptr` is an opaque `*const _File` synthesized from a global, generational slotmap key:

```rust
use slotmap::{SlotMap, KeyData};

slotmap::new_key_type! { struct FileKey; }

static SLOTS: Lazy<RwLock<SlotMap<FileKey, Arc<FILE>>>> = ...;

fn encode(k: FileKey) -> FILE_ptr {
    let raw: usize = k.data().as_ffi() as usize;    // packs (slot_idx, generation)
    std::ptr::without_provenance(raw)               // null cannot be produced — see slotmap docs
}

fn decode(t: FILE_ptr) -> Option<FileKey> {
    if t.is_null() { return None; }
    Some(FileKey::from(KeyData::from_ffi(t.addr() as u64)))
}
```

Slotmap keys carry a generation counter alongside the slot index. Removing a slot bumps its generation; reusing the slot for a later `fopen` produces a *different* handle. Stale handles — whether held as aliases of closed streams, fabricated from arbitrary pointer values, or copied from a previous run — fail the generation check on lookup and produce `EBADF`.

`fopen` and friends do:

```rust
pub fn fopen(path: &CStr, mode: &CStr) -> FILE_ptr {
    let f = match FILE::new(path, mode) {
        Ok(f) => f,
        Err(e) => { errno::set(e); return std::ptr::null(); }
    };
    let key = SLOTS.write().insert(Arc::new(f));
    encode(key)
}
```

`fclose` removes the slot:

```rust
pub fn fclose(t: FILE_ptr) -> c_int {
    let Some(key) = decode(t) else {
        errno::set(libc::EBADF);
        return libc::EOF;
    };
    let arc = match SLOTS.write().remove(key) {
        Some(a) => a,
        None => { errno::set(libc::EBADF); return libc::EOF; }
    };
    // Other threads may still hold Arc clones from in-flight ops.
    // The Drop of FILE (which closes the OS resource) runs when the last
    // Arc is released — i.e. when those operations complete.
    let mut state = arc.lock();
    state.flush_internal()
        .map(|_| 0)
        .unwrap_or(libc::EOF)
}
```

Every other operation looks up the handle, clones the `Arc` while holding the slotmap lock briefly, releases the slotmap lock, then operates on the `Arc<FILE>`:

```rust
fn lookup(t: FILE_ptr) -> Option<Arc<FILE>> {
    let key = decode(t)?;
    SLOTS.read().get(key).cloned()
}

pub fn fread(buf: *mut u8, size: usize, n: usize, t: FILE_ptr) -> usize {
    let Some(f) = lookup(t) else {
        errno::set(libc::EBADF);
        return 0;
    };
    // SLOTS lock released. f keeps the FILE alive even if another thread
    // calls fclose(t) right now.
    let mut state = f.lock();
    state.read_into(buf, size, n)
}
```

**Why this design.** It simultaneously solves three problems:

1. **Bounded memory.** `fclose` actually reclaims storage. Memory usage is `O(currently-open streams)`, not `O(total fopens)`. Long-running daemons don't grow without bound.
2. **Concurrent close.** An in-flight `fread` cannot be invalidated by another thread's `fclose`, because the in-flight op holds an `Arc` clone. The OS resource is released exactly when the last in-flight op completes.
3. **Defense against pointer corruption.** The translated code holds only an opaque `*const _File` handle. That handle is not a real pointer to our `State`, our `ReentrantMutex`, our `Arc` refcount, or anything else internal: the bits come from `slotmap::KeyData`, not from a dereferenceable allocation. A malicious or buggy `memset` can overwrite the handle variable itself, but the next call only sees a different synthesized handle and rejects it at slotmap lookup unless it accidentally matches a currently-live key. See §7 for the full safety analysis.

The cost is one `RwLock::read` and one `Arc::clone` per public call, on the order of 20–50ns. Negligible against any operation that touches the kernel; potentially measurable on hot character I/O (`getc` in a tight loop), where it can be amortized via `flockfile`-bracketed sequences that batch many operations under a single lookup.

### 4.4 Registry duties

The slotmap doubles as the registry. Operations that need to enumerate all open streams (`fflush(NULL)`, the `atexit` flush handler, diagnostics) iterate `SLOTS.read()`:

```rust
pub fn fflush_all() -> c_int {
    let snapshot: Vec<Arc<FILE>> = SLOTS.read().values().cloned().collect();
    // Lock released before per-stream work, so a stream's fclose
    // running concurrently with this iteration cannot deadlock.
    let mut rv = 0;
    for f in snapshot {
        if f.lock().flush_internal().is_err() { rv = libc::EOF; }
    }
    rv
}
```

The lock-order discipline is: slotmap lock < per-stream reentrant mutex < `std::io::Std*` lock. The library never acquires these in any other order.

---

## 5. Stream Semantics

### 5.1 Buffer policy and direction switching

The C standard requires a synchronization point (`fseek`, `fflush`, `fsetpos`, `rewind`, or EOF) when switching between reading and writing on a stream opened in update mode. The library enforces this *automatically* rather than requiring caller cooperation:

```
on read after Direction::Write:
    flush write buffer
    last_op = Read

on write after Direction::Read:
    if mode is update:
        seek backwards by (buffer.fill - buffer.cursor + pushback.len())
        discard buffer and pushback
    last_op = Write
```

This is *more lenient* than libc, which leaves it as UB. Code that worked in C continues to work; code that *would have* been UB now produces correct results. This is the safety win paying off at the semantic level.

### 5.2 Position tracking

`ftell` returns:
- **Read direction**: `(handle's underlying offset) - (buffer.fill - buffer.cursor) - pushback.len()`
- **Write direction**: `(handle's underlying offset) + buffer.fill`
- **Either (post-flush/seek)**: handle's underlying offset.

For unseekable handles (`Pipe`, some `Stdio` cases), `ftell` returns -1 and sets `errno = ESPIPE`.

`fseek` flushes any write buffer, discards any read buffer and pushback, seeks the underlying handle, and resets `last_op = Either`. `eof` is cleared; `err` is preserved (matching libc).

### 5.3 ungetc

`ungetc(c, f)` pushes `c` onto `pushback`. The C standard guarantees one byte; we guarantee unbounded (the `Vec` grows). Returns `c` on success, `EOF` if `c == EOF`. Clears the `eof` indicator. After a successful seek, `pushback` is discarded (per POSIX).

### 5.4 freopen

`freopen(path, mode, t)` keeps the same handle `t` valid (so caller-held aliases continue to work) and:

1. Looks up `t` in the slotmap; aborts with `EBADF` if invalid.
2. Acquires the stream lock.
3. Flushes and closes the current handle.
4. Opens the new path.
5. Replaces `state.handle`, resets `buffer`, `pushback`, indicators, `mode`, `last_op`.

Note that the slot is *not* removed and reissued — the same `FILE` (and the same `Arc`) is reused with new internal state. This preserves handle equality (`t1 == t2` if they were the same before, still are afterward), matching libc's pointer-identity guarantee.

Special case: `freopen(NULL, mode, t)` changes the mode of the existing handle (POSIX 2008+). For regular files this is implementable via `fcntl(F_SETFL)`; for stdio shares we degrade gracefully (return a null handle, set errno).

### 5.5 Indicator semantics

- `eof` is set when an attempted read finds the underlying stream at EOF *and* delivered fewer bytes than requested.
- `err` is set when the underlying I/O returns an error (`io::Error` other than `UnexpectedEof`).
- `clearerr` clears both.
- `feof`/`ferror` read them; they do **not** clear them.
- On a successful operation, indicators are *not* cleared (matching libc; `clearerr` is the only way to clear).

### 5.6 Locking

`flockfile(f)` acquires the reentrant mutex. `funlockfile(f)` releases one level. `ftrylockfile(f)` returns 0 on success, nonzero on contention.

Every public function internally takes the lock for its duration. If `flockfile` was called by the same thread, the recursive acquisition is free. This means `flockfile`-bracketed sequences observe the stream atomically, exactly as libc guarantees.

### 5.7 Closing

`fclose(t)`:
1. Decode and validate `t`; on failure return `EOF` with `EBADF`.
2. Remove the slot from `SLOTS`. The bumped generation immediately invalidates any stale aliases.
3. Take the stream lock on the `Arc<FILE>` we just removed.
4. If `dirty`, flush. Errors during this flush set the return value but do not abort closing.
5. Drop the `Arc`. When the last in-flight operation releases its clone, the underlying `Handle` is dropped, which closes the OS resource.
6. Return 0 on success, `EOF` on flush failure.

**Subsequent operations on a closed (slot-removed) stream** return `EOF`/-1 with `errno = EBADF` and do not touch any stream state — there is no stream state to touch, because the slotmap lookup fails before we get that far. This is the principal safety improvement over libc.

`pclose(t)` additionally waits on the child process and returns its exit status.

---

## 6. Stdio Integration with `std::io`

### 6.1 The principle

For the three standard origins, `Handle` defaults to wrapping the corresponding `std::io::Std*` handle rather than a raw fd. The singletons are handles, lazily registered into the slotmap on first access:

```rust
fn stdout_handle() -> FILE_ptr {
    static T: OnceLock<usize> = OnceLock::new();
    std::ptr::without_provenance(*T.get_or_init(|| {
        let f = FILE::new_stdio_shared(
            Handle::StdoutShared(std::io::stdout()),
            BufferPolicy::Line(BUFSIZ),    // libc default for terminals
        );
        encode(SLOTS.write().insert(Arc::new(f))).addr()
    }))
}
```

Because the handle is registered in the slotmap, the standard streams participate in `fflush(NULL)` and atexit flushing the same way regular files do. They are never `fclose`d implicitly, so their slot lives for the program's lifetime.

Writes through `our_fwrite(our_stdout, ...)` proceed as:

```
take stream lock
drain our buffer into a local Vec (or keep as &[u8])
{
    let mut g = io_stdout.lock();   // same mutex print! takes
    g.write_all(&bytes)?;           // into std's LineWriter
}
release stream lock
```

So we share `std`'s output mutex *and* its line-buffer, transitively. Consequences:

- `print!` output and `our_fputs(our_stdout, ...)` interleave at write granularity, never within a write.
- `our_fflush(our_stdout)` calls `io_stdout.lock().flush()`, which forces `std`'s `LineWriter` to push to the OS — so anything `print!` left buffered goes out.
- `cargo test`'s `--nocapture` machinery, which works by replacing the global stdout sink, captures our output too.
- `BrokenPipe` handling matches `print!`'s behavior; the runtime's main-exit translation of `EPIPE` covers both.

### 6.2 The `setvbuf` decision

Three modes, controlled by a runtime flag:

| Mode | Behavior on `setvbuf(stdout, ...)` | Trade-off |
|---|---|---|
| **`StdioMode::Shared`** (default) | Ignore buffer-size argument; honor `_IONBF` by inserting a flush after every write. Otherwise no-op. | Mixed `print!`/`fprintf` code stays correct. Loses ability to set a custom buffer size on stdio. |
| **`StdioMode::Bypass`** | First `setvbuf` call swaps `Handle::StdoutShared` → `Handle::StdoutBypass(OwnedFd)`. Our buffer takes over; `print!` writes now race with our writes between flushes. | Closest to C semantics; required if the program literally depends on setting a 1MB stdout buffer. |
| **`StdioMode::Capture`** | At init, call `io::set_output_capture` (or future stable API) to route `print!` itself through our buffer. | Most invasive; not yet stable in `std`. Reserved for a future milestone. |

Default is `Shared`. The translator emits a warning when it sees `setvbuf(stdout, ...)` so the user can opt into `Bypass` if needed.

### 6.3 fdopen on stdio fds

`fdopen(1, "w")` does *not* return our stdout singleton. It returns a *new* `FILE` whose `Handle` is `Regular(File::from_raw_fd(1))` (with a `dup` so we don't close the inherited fd on drop). That stream owns its own buffer and locks independently. This matches libc, where `fdopen(1, ...)` and `stdout` share an fd but not buffering — and code that cares about ordering between such streams was already explicitly flushing.

---

## 7. Safety Analysis

### 7.1 Unsafe surface

Every `unsafe fn` has a `# Safety` section stating its preconditions, and every `unsafe` block has a `SAFETY` comment saying why they hold.

**Caller-facing: `unsafe fn` entry points.** These are the only ways translated code can reach UB through the library. The translator emits them inside `unsafe` blocks.

1. **Raw fd ingestion.** `fdopen(fd, mode)` requires `fd` to be an open descriptor the caller may use for the duration of the call (I/O safety, as for a `BorrowedFd`). The stream wraps a `dup` of it, so our `Drop` never closes a caller-managed fd. The `dup` result is checked before `File::from_raw_fd`.
2. **Buffer pointers.** `fread`, `fwrite`, `fgets`, `snprintf`, `sprintf`, `fgetpos`, `fsetpos`, and `tmpnam`/`ctermid` with a non-null buffer require the buffer to be valid for the length C implies, and not accessed through any other pointer or reference during the call.
   - **Destinations may be uninitialized.** That covers `fread`, `fgets`, `snprintf`, `sprintf`, `tmpnam` and `ctermid`. Each one writes exactly the bytes C specifies and leaves the rest untouched. C gives no licence to write elsewhere: for example, `fread` may only make a *partial* final element indeterminate (C11 §7.21.8.1). So these functions never zero-fill a buffer just so they can borrow it as `&mut [u8]`:
     - **`fread`** borrows the buffer as `&mut [MaybeUninit<u8>]` and copies in only the bytes read. Large requests that would read directly into a caller's initialized slice instead go through a scratch buffer of at most 64 KiB per read. That scratch request is never larger than the remaining request, so unbuffered streams never consume extra input.
     - **`fgets`** shares its read loop with `fgets_` and writes each byte, and the NUL, through the raw pointer.
     - **`tmpnam` and `ctermid`** generate the name in a local array and copy out only the name and its NUL.
     - **`snprintf!` and `sprintf!`** copy output bytes and their terminator directly through the raw pointer without constructing a destination slice.
   - **Sources must be initialized.** `fwrite` and `fsetpos` read the caller's bytes. `fsetpos` expects an `fpos_t` filled in by `fgetpos`, which stores the offset in its first 8 bytes. A compile-time assertion checks that `fpos_t` is at least that large.
3. **C-string pointers.** `perror` and `freopen` accept a null pointer or a valid NUL-terminated string.
4. **`getline` / `getdelim`.** `lineptr` and `n` must be valid for reads and writes. `*lineptr` must be null or a live `malloc`-family allocation of at least `*n` bytes. As POSIX specifies, `*n` is ignored when `*lineptr` is null.
5. **Static result buffers.** `tmpnam(NULL)` and `ctermid(NULL)` write a shared static buffer, as in C. Concurrent calls, or overlapping use of a previously returned pointer, are a data race. That is the caller's contract.
6. **`fmemopen` over a caller's buffer.** The buffer must stay valid for reads and writes of `size` initialized bytes for as long as the stream may touch it. That means until `fclose`, `pclose` or `freopen` returns, or until program exit and the `atexit` flush if the stream is never closed. One case extends this further. If the closing thread holds a `StreamGuard` on the stream, `fclose` defers its flush. The buffer must then stay valid until that guard drops and any operations other threads had already started have returned. Between operations the caller may access the buffer, as C programs do after `fflush`, but not during one. This contract is exactly the invariant on `MemStorage::User` that lets the safe `MemBacking::slice`/`slice_mut` build slices over the buffer. The owned-buffer path (`fmemopen_`, or `fmemopen` with a null buffer) needs no contract.
7. **Format arguments.** Printing macros adapt arguments by Rust type into `Arg::I`, `U`, `F`, `Bytes`, or unclassified `Raw` carriers. The common renderer interprets each carrier according to its conversion. Raw `%s` reads and `%n` stores require an unsafe caller; the `_safe` macros reject raw pointers and count stores. Pre-parsed printing APIs take `&[Arg]`, including the translator's thin `va_list` adapter. Scanning retains `&mut [OutArg]` and its separate pointer-validity contracts. Moving or sharing raw carriers does not access their pointees; consuming them requires the corresponding unsafe API.

**Internal.** These are not visible to callers.

8. **Lifetime-extended lock guards.** `StreamGuard` and the per-thread `flockfile` stack store a `ReentrantMutexGuard`, and `StreamGuard` also stores a `RefMut`, each transmuted to `'static`. This is sound because:
   - each sits next to the `Arc<FILE>` that owns the mutex, and that `Arc` keeps the heap-allocated `FILE` from moving or being freed;
   - explicit drop order releases the `RefMut`, then the guard, then the `Arc`;
   - the guard type is `!Send`, so it is always released on the thread that acquired it.
9. **Inherited stdio fd materialization.** `setvbuf` in `StdioMode::Bypass` calls `Handle::file_from_dup(STD*_FILENO)`, an `unsafe fn` with the same contract as `fdopen`. The standard descriptors are always borrowable, which std also assumes in `impl AsFd for Stdout`. If `dup` fails, `setvbuf` returns -1 with `errno` set rather than wrapping an invalid fd.
10. **Small libc calls:**
    - `atexit` registration, whose handler catches panics and so never unwinds into libc;
    - the `errno` mirror through `__errno_location`;
    - `getpid`;
    - the `popen` pipe fd hand-off into `File`;
    - `strerror_r` into a local buffer for `perror_`, never `strerror`, whose static buffer another thread may overwrite.
11. **scanf numeric fields.** These use `str::from_utf8_unchecked` over buffers that the scanner fills only with ASCII bytes.

Notably absent from this list: anything to do with `FILE_ptr` itself. Handle encode/decode and slotmap lookup are entirely safe Rust (`ptr::without_provenance` and `.addr()`), and the translated code never has access to a real pointer into our state.

`tests/smoke.rs` checks the C-conformance points above:
- a short `fread` leaves the unread elements untouched;
- `fread` into a truly uninitialized buffer larger than `BUFSIZ`, on buffered and unbuffered streams;
- `fgets` and `tmpnam` leave the bytes after their NUL untouched.

### 7.2 Threat model

Three categories of misbehavior from the translated program:

1. **Fabricated handles.** The C code synthesizes a `FILE_ptr` from an arbitrary integer/pointer bit-pattern, a previous program run, or a stale alias of a closed stream.
2. **In-place mutation of `FILE` state.** The C code attempts to overwrite the bytes of a `FILE` struct directly, e.g. `memset(f, 0, sizeof(FILE))`.
3. **Misuse of the safe API.** Closing twice, reading from a write-only stream, passing a buffer of the wrong length, etc.

### 7.3 What the design rules out

- **(1) is fully caught.** Every entry point begins with a slotmap decode + lookup. Handles that were never issued, or whose generation is stale because the slot has been freed (and possibly reissued), produce `EBADF` deterministically. No state is touched, no real pointer is dereferenced, no UB.
- **(2) is impossible by construction.** The translated code holds opaque `*const _File` handles, but they are not pointers to an exposed `_File` object or to any live internal allocation. The library does not export the layout of `_File` or `FILE`, does not provide `sizeof(FILE)`, and does not return pointers to its internal `Arc<FILE>` / `State`. A C-side `memset` or integer cast on a `FILE_ptr` only scribbles on the handle bits; the next call simply produces `EBADF` unless those bits happen to name a currently-live slot key.
- **(3) produces defined error returns**, never UB. Closing twice: the second `fclose` finds an empty slot. Reading from a write-only stream: the `OpenMode` check fails inside the safe Rust state machine. Wrong buffer length: that's the caller's contract on the raw pointer, same as libc, and is only reachable through the `unsafe fn` entry points of §7.1. The safe siblings take sized destinations instead, and turn what would be an overflow into an error return or a bounds-check panic.

Stronger stated: **a program that calls only the safe entry points cannot trigger UB through this library.** The `unsafe fn` entry points of §7.1 are the inherent FFI boundary: raw fds, raw buffer and string pointers, and pointer-carrying format arguments. The translator marks their calls with `unsafe` blocks, so the translation still localizes the unsafety to those sites instead of dispersing it across every stdio call. Code that can use the safe siblings sheds the unsafety entirely.

### 7.4 What can still go wrong

- **Logic errors** in the translated program produce defined error returns rather than UB, but the program is still buggy. Tools like static analysis or runtime tracing can find these; the library itself just refuses to misbehave.
- **Unsafe entry points** of §7.1 expose pointer-validity requirements to their callers. Translated printing calls leave those operations visible so required unsafety propagates to function signatures and callers. Violating these contracts is UB and not detectable by the library, exactly as in C. Examples include a garbage fd, an undersized buffer, a dangling `%s` argument, or an `fmemopen` buffer freed while the stream is open.
- **Filesystem-level races** (TOCTOU on the path passed to `fopen`) are inherent to the OS and not addressable here.
- **Slow `fclose` followed by `fopen` reusing the slot** can in principle produce a "false-positive valid handle": if a stale alias's encoded `(slot, gen)` value happens to match a *future* `(slot, gen)` after wraparound of the 32-bit generation counter, lookup would succeed and the operation would target the wrong stream. Slotmap uses a 32-bit generation field; for wraparound to occur, the same slot must be `fclose`d and `fopen`d 2³² times. This is not a practical concern, but is documented for completeness and can be hardened by widening the generation field if a concrete adversary model demands it.

---

## 8. String Types: Integration with `xj_zstr`

`xj_cio` depends on `xj_zstr`, the `<string.h>` counterpart that provides the mutable, bounded, NUL-terminated `ZStr` and its owned form `ZString`. The dependency runs one way: `xj_zstr` knows nothing about streams, as `<string.h>` knows nothing about `<stdio.h>`.

There is no parallel ZStr-specific copy of the API. Most functions need nothing ZStr-specific. The ones that do are those that write a terminated string into a caller's buffer, or that grow one.

### 8.1 Parameter kinds

| What C does with the string | Functions | `xj_cio` parameter | Passing a `ZStr` |
|---|---|---|---|
| Reads it | `fopen`/`freopen` path and mode, `popen`, `remove`, `rename`, `fputs`, `puts`, `perror_`, scanf formats | `&CStr` | `z.as_c_str()`, which stops at the first NUL, as C does |
| Reads or writes bytes with an explicit length | `fread_`, `fwrite_` | `&mut [u8]`, `&[u8]` | The view matching C's length argument: `as_bytes()` for `strlen(s)`, `as_buffer()` for `sizeof buf`, `as_buffer_mut()` or a sub-slice for reads |
| Writes a terminated string into a sized buffer | `fgets_`, `tmpnam_`, `ctermid_`, scanf `%s`, `%c` and `%[` | `xj_zstr::CStrDest` | `z.into()` for the whole buffer, `z.limit(n)` for C size `n` |
| Grows a string | `getline_`, `getdelim_` | `&mut ZString` | The `ZString` itself |

Read-only parameters stay `&CStr`, so string literals (`c"r"`) and `CString`s work with no `xj_zstr` types. Parameters are concrete types rather than generics, so translated calls need no annotations.

| C | Rust |
|---|---|
| `fopen(path, "r")`, `path` a `ZStr` | `fopen(path.as_c_str(), c"r")` |
| `fgets(buf, sizeof buf, f)`, `char buf[N]` | `fgets_((&mut buf).into(), f)` |
| `fgets(out, n, f)`, `out` a `ZStr` | `fgets_(out.limit(n), f)` |
| `snprintf(out, n, fmt, …)`, guided storage and safe arguments | `snprintf_safe!(out, n, fmt, …)` |
| `getline(&line, &cap, f)` | `getline_(&mut line, f)`, `line: ZString`; read `cap` as `line.capacity() + 1` |
| `fscanf(f, "%15s %d", word, &n)`, `char word[16]` | `fscanf(f, c"%15s %d", &mut [OutArg::Buf((&mut word).into()), OutArg::Int(&mut n)])` |

### 8.2 `CStrDest`

C passes these functions a buffer and a size that includes the terminator. `CStrDest` carries the same information: a writable region, and whether a NUL is already guaranteed immediately after it. `c_size()` recovers C's size, and `max_len() = c_size() - 1` is the longest string that fits.

- **A byte slice or array of length N** is `char buf[N]`: an N-byte region, not terminated. C size N.
- **A `ZStr` with backing length N** hands out only its N − 1 writable bytes, because its final NUL is reserved and never writable. That NUL terminates the region, so the C size is still N.
- **`ZStr::limit(n)`** gives C size `n` for any `n` up to the backing length, and panics beyond it, as slicing does.

Passing `z.as_buffer_mut()` as a plain slice is safe but wrong. The slice has N − 1 bytes and spends one on the NUL, so `fgets_` would read at most N − 2 bytes where C reads N − 1, and a line of exactly N − 1 bytes would arrive in two pieces.

`xj_cio` always enables `xj_zstr/cfmt`, including the `CSnprintfDest` adapter for `CStrDest`. A value such as `out.limit(n)` can serve the bounded formatting macros and the functions below.

Semantics:

- **`fgets_(dst, f) -> Option<usize>`** behaves as `fgets(buf, dst.c_size(), f)`. It returns the number of bytes stored before the terminator. It returns `None` on EOF before any byte, on error, or when `c_size()` is zero, leaving `dst` unchanged on EOF.
- **`snprintf_safe!(dst, n, fmt, …)`** uses the common C renderer for both literal and runtime formats, applies truncation and termination, and returns the full output length. `sprintf_safe!` uses the storage's capacity. The `_unchecked` forms accept raw arguments under an unsafe caller contract.
- **`tmpnam_(dst)` and `ctermid_(dst)` -> `Option<usize>`** write the whole name and return its length. If it does not fit, they fail with `ERANGE` and leave `dst` untouched, because a truncated path names a different file. `c_size() >= L_tmpnam` (respectively `L_ctermid`) always suffices; generated temporary names are at most `L_tmpnam - 1` bytes.
- **The raw printing macros** (`snprintf!`, `sprintf!`) accept writable pointer destinations and require an unsafe caller. They copy only the output bytes and terminator, without constructing an oversized slice. The raw functions `fgets`, `tmpnam`, and `ctermid` retain their pointer contracts (§7.1): `fgets` shares its read loop with `fgets_`, while `tmpnam` and `ctermid` use a local array and copy out the name and NUL.

### 8.3 scanf: `OutArg::Buf`

`OutArg<'a>` has a checked string variant, `Buf(CStrDest<'a>)`, alongside the unchecked raw `Str(*mut u8)`. Because `Buf` borrows mutably, `OutArg` is not `Copy`, and the scanf family takes `&mut [OutArg<'_>]`.

Where C would overflow the buffer, a `Buf` conversion fails instead:

- **`%s` and `%[`** read at most `max_len() + 1` field bytes, or fewer if the format gives a width. If the field does not fit, the destination receives the first `max_len()` bytes, terminated. The extra byte is pushed back, so the rest of the field stays unread.
- **`%Nc`** stores N bytes and no terminator, as in C. It may therefore fill the whole region, but not a `ZStr`'s reserved NUL. If `region_len() < N`, it fails before reading anything.
- **On failure**, the call stops, returns the assignments made before the failed conversion, and sets `errno` to `EOVERFLOW`.

### 8.4 `getline_` and `getdelim_`

`getdelim_(line: &mut ZString, delim, f) -> ssize_t` reads through the next `delim` byte (inclusive) or EOF, and returns the number of bytes read. It returns -1 on EOF before any byte, or on error. `getline_` is `getdelim_` with `'\n'`.

- **Growth.** `line` grows as C's `getdelim` reallocates `*lineptr`, through `ZString::grow_to`, to backing sizes 128, 256, 512, and so on. It never shrinks, so a reused `ZString` behaves like a reused `malloc` buffer.
- **Embedded NULs.** As in C, the return value is the line's length. A line containing NUL bytes is stored whole, but `line.len()` stops at its first NUL.
- **Leftover bytes.** Bytes after the new terminator are left unchanged.

The raw `getline` and `getdelim` keep their `malloc`/`realloc` buffers. Those are owned by C's allocator and cannot become a `ZString`. They are `unsafe fn`s (§7.1), and as in POSIX they ignore `*n` when `*lineptr` is null.

### 8.5 Formatting

Formatting is always available through the required `xj_cfmt` dependency, with `xj_zstr/cfmt` enabled. Literal and runtime C formats use the same parser and renderer via `printf!`, `fprintf!`, `sprintf!`, and `snprintf!`, their safe forms, and the pre-parsed plan APIs. `ZStr` and `ZString` provide borrowed byte arguments and checked storage adapters. The separate brace-format macros `cprintf!`, `ceprintf!`, and `cfprintf!` also use the byte view.

### 8.6 Tests

`tests/zstr.rs` covers:

- `fgets_` reading the same number of bytes as C into a `ZString`, an array and a `limit(n)` view, for C sizes 0 and 1.
- `getline_` growth across a 300-byte line, and `getdelim_` with an embedded NUL.
- Guided truncation with `snprintf_safe!` and raw destination macros `snprintf!` and `sprintf!`.
- `tmpnam_` and `ctermid_`, including the `ERANGE` case.
- scanf `Buf` conversions:
  - an exact fit;
  - `%s` overflow, with the unread remainder;
  - a width smaller than the destination;
  - `%c` into an array and into a `ZStr` too small for it.
- `&CStr` and `&[u8]` views of a `ZStr` passed to `fopen`, `fputs` and `fwrite_`.

`tests/cfmt.rs` checks that a `ZStr` argument to `cfprintf!` takes the byte path.

---

## 9. Testing Strategy

### 9.3 The print!-interleave matrix

Specifically for the §6 integration concern, a dedicated test suite that runs every combination of:

- `{print!, println!, eprintln!}` ×
- `{fputs, fprintf, fwrite}` ×
- `{interspersed, all-print-then-all-fputs, alternating}` ×
- `{with explicit flush, without}` ×
- `{stdout, stderr}`

…and asserts that captured output (via `set_output_capture`) matches a hand-computed expected string. This is the only way to be confident the §6 design actually works in practice.

---

## 10. Open Questions

1. **Wide character support.** Do any meaningful translation targets require it? Probably yes (gettext-using programs). Plan: phase 10 in a v2 milestone, with `Orientation` already wired in to the state.
2. **`setvbuf StdioMode::Capture`.** Depends on `io::set_output_capture` stabilizing or on us implementing an equivalent via a custom `Stdout` implementation. Track the upstream RFC.
3. **Interaction with `std::process::exit`.** `exit(3)` runs `atexit` handlers (including our flusher); `_exit(2)` does not. Document, test, and ensure our atexit is registered late enough to flush after user atexit handlers that produce output.
4. **Windows.** Most of this assumes POSIX. Windows `fopen` mode strings include `t` and `b`; CRLF translation in text mode is a separate buffering layer. Plan: support Windows in v0.2 via a `TextMode` filter atop the buffer engine.
5. **`fileno` round-trip.** `fileno(t)` returns an fd; calling `fdopen(fileno(t), ...)` is legal in C. We must ensure such code does not double-close. Solution: `fileno` returns the *borrowed* raw fd; `fdopen` always `dup`s. Documented contract.
6. **`flockfile` and `getc_unlocked` family.** POSIX defines unlocked variants for performance. Implement them as functions that assume the caller holds `flockfile` and skip the lock acquisition. Sound only if the caller actually holds the lock; the reentrant lock makes the misuse harmless (just slower) rather than UB.
7. **Slotmap lookup overhead on hot character I/O.** A `getc` loop pays one `RwLock::read` + `Arc::clone` per call. Mitigation: encourage `flockfile`-bracketed sequences (which we can extend to also cache the `Arc` for the duration of the lock hold), or expose `getc_unlocked` that takes a pre-resolved handle. Profile before optimizing.
8. **Performance target.** 2× libc on bulk, 3× on chars is aspirational; the slotmap and locking overhead alone may push us further on the character case. If we miss, the answer is profiling, not redesign — the architecture has no inherent slowness.
