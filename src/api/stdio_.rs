//! Standard streams: `stdin`/`stdout`/`stderr` lazily registered into
//! the slotmap on first access.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use crate::buffer::BufferPolicy;
use crate::handle::Handle;
use crate::slots;
use crate::state::{FILE, OpenMode, State};
use crate::{BUFSIZ, FILE_ptr};

/// Global policy for how `setvbuf` interacts with the standard
/// streams when our `Handle` is one of the `Shared` variants.
///
/// Defaults to `Shared` (libc-compatible behavior + cooperation with
/// `print!`). The translator may emit a one-time
/// `set_stdio_mode(...)` if the source program demands a different
/// interaction model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StdioMode {
    /// `setvbuf` on stdout/stderr is mostly a no-op: only the
    /// `_IONBF` flag is honored (we then flush after every write).
    /// This is the default — keeps `print!` and `our_fprintf`
    /// interleaving correctly because both go through std's locks.
    Shared = 0,
    /// `setvbuf` on stdout/stderr swaps the handle to a `Bypass`
    /// variant (a duped raw fd we own) and the user-supplied buffer
    /// takes over. `print!` from std is now uncoordinated with our
    /// writes.
    Bypass = 1,
    /// At init, route std's `print!` writes through our buffer via
    /// `io::set_output_capture`. Available only when the
    /// `set_output_capture` Cargo feature is enabled (which requires
    /// a nightly toolchain).
    #[cfg(feature = "set_output_capture")]
    Capture = 2,
}

static STDIO_MODE: AtomicU8 = AtomicU8::new(StdioMode::Shared as u8);

/// Returns the current global stdio mode.
pub fn stdio_mode() -> StdioMode {
    match STDIO_MODE.load(Ordering::Acquire) {
        0 => StdioMode::Shared,
        1 => StdioMode::Bypass,
        #[cfg(feature = "set_output_capture")]
        2 => StdioMode::Capture,
        _ => StdioMode::Shared,
    }
}

/// Sets the global stdio mode. Should be called early — before any
/// `setvbuf` on stdout/stderr — for predictable behavior.
///
/// When called with `StdioMode::Capture`, the function additionally
/// enables `io::set_output_capture` so std's `print!` writes are
/// redirected into our shared sink.
pub fn set_stdio_mode(mode: StdioMode) {
    STDIO_MODE.store(mode as u8, Ordering::Release);
    #[cfg(feature = "set_output_capture")]
    if matches!(mode, StdioMode::Capture) {
        crate::capture::enable();
    }
}

static STDIN_TOK: OnceLock<FILE_ptr> = OnceLock::new();
static STDOUT_TOK: OnceLock<FILE_ptr> = OnceLock::new();
static STDERR_TOK: OnceLock<FILE_ptr> = OnceLock::new();

fn make_stdio(handle: Handle, mode: OpenMode, policy: BufferPolicy) -> FILE_ptr {
    let cap = match policy {
        BufferPolicy::None => 0,
        _ => BUFSIZ,
    };
    let state = State::new(handle, mode, policy, cap);
    slots::insert(Arc::new(FILE::new(state)))
}

pub fn stdin() -> FILE_ptr {
    *STDIN_TOK.get_or_init(|| make_stdio(Handle::StdinShared, OpenMode::R, BufferPolicy::Full))
}

pub fn stdout() -> FILE_ptr {
    *STDOUT_TOK.get_or_init(|| make_stdio(Handle::StdoutShared, OpenMode::W, BufferPolicy::Line))
}

pub fn stderr() -> FILE_ptr {
    *STDERR_TOK.get_or_init(|| make_stdio(Handle::StderrShared, OpenMode::W, BufferPolicy::None))
}
