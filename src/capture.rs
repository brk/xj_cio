//! Bridge to `std::io::set_output_capture`. Active only when the
//! `set_output_capture` Cargo feature is enabled (requires nightly).
//!
//! Wiring summary:
//!
//! - On `set_stdio_mode(StdioMode::Capture)`, [`enable`] installs a
//!   process-shared `Arc<Mutex<Vec<u8>>>` sink as the current
//!   thread's capture target. Any subsequent `print!`/`eprintln!`
//!   writes go into the sink instead of the actual stdout.
//! - Before each `our_fwrite(stdout, ...)` (and on `fflush`), the
//!   stdio write path calls [`drain`] and prepends the captured
//!   bytes to the outgoing chunk. This preserves the natural
//!   ordering in which `print!` and our writes were issued — the
//!   alternative (writing our bytes first) would emit our buffered
//!   output ahead of `print!` text that arrived earlier.
//! - On exit / `atexit`, the final flush also drains the sink.
//!
//! Limitation: the sink is per-thread because
//! `set_output_capture` is per-thread. Threads created *after* the
//! mode switch will not capture unless they also call
//! `set_stdio_mode(StdioMode::Capture)`.

use std::sync::{Arc, Mutex, OnceLock};

pub(crate) type Sink = Arc<Mutex<Vec<u8>>>;

static SINK: OnceLock<Sink> = OnceLock::new();

fn sink_handle() -> Sink {
    SINK.get_or_init(|| Arc::new(Mutex::new(Vec::new()))).clone()
}

/// Install our capture sink for the current thread.
pub fn enable() {
    let s = sink_handle();
    std::io::set_output_capture(Some(s));
}

/// Disable capture for the current thread.
pub fn disable() {
    std::io::set_output_capture(None);
}

/// Drain captured bytes accumulated since the last call. Returns
/// them in the order they were written.
pub fn drain() -> Vec<u8> {
    let Some(s) = SINK.get() else { return Vec::new(); };
    let mut g = match s.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    std::mem::take(&mut *g)
}

/// Has the capture sink ever been initialized? Used by the write
/// path to skip a `lock()` when capture mode was never enabled.
pub(crate) fn is_active() -> bool {
    SINK.get().is_some()
}
