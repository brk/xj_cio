//! Internal operations shared by every public entry point.
//!
//! The pattern: a public function locks the stream, calls one of
//! these helpers (which mutate `State` directly), and translates the
//! result into the POSIX return-value convention (errno + return
//! code).

use std::io::SeekFrom;

use crate::buffer::BufferPolicy;
use crate::errno;
use crate::handle::Handle;
use crate::state::{Direction, State};

/// Drain the write buffer to the underlying handle.
pub fn flush_internal(state: &mut State) -> std::io::Result<()> {
    drain_capture_into_buffer(state);
    if state.last_op == Direction::Write && state.buffer.dirty {
        let n = state.buffer.fill;
        let mut written = 0;
        while written < n {
            match state.handle.write(&state.buffer.data[written..n]) {
                Ok(0) => {
                    state.err = true;
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
                Ok(k) => written += k,
                Err(e) => {
                    state.err = true;
                    return Err(e);
                }
            }
        }
        state.buffer.fill = 0;
        state.buffer.dirty = false;
    }
    state.handle.flush()?;
    Ok(())
}

/// In `StdioMode::Capture`, std's `print!` writes accumulate in our
/// shared capture sink. To preserve user-program ordering, we drain
/// the sink and prepend its bytes to whatever we're about to flush
/// or buffer. Compiles out entirely without the feature.
#[inline]
fn drain_capture_into_buffer(_state: &mut State) {
    #[cfg(feature = "set_output_capture")]
    {
        use crate::handle::Handle;
        if !matches!(_state.handle, Handle::StdoutShared | Handle::StderrShared) {
            return;
        }
        if !crate::capture::is_active() {
            return;
        }
        if crate::api::stdio_::stdio_mode() != crate::api::stdio_::StdioMode::Capture {
            return;
        }
        let drained = crate::capture::drain();
        if drained.is_empty() {
            return;
        }
        // Push through the buffer policy directly. This can recurse
        // into `flush_internal` if the buffer fills, but the sink is
        // now empty so the recursive `drain_capture_into_buffer`
        // call is a no-op.
        let _ = write_through_buffer(_state, &drained);
    }
}

/// Prepare the stream to read. If we were writing, flush.
pub fn switch_to_read(state: &mut State) -> std::io::Result<()> {
    match state.last_op {
        Direction::Write => {
            flush_internal(state)?;
            state.last_op = Direction::Read;
        }
        _ => {
            state.last_op = Direction::Read;
        }
    }
    Ok(())
}

/// Prepare the stream to write. If we had a read buffer with
/// unconsumed bytes (or pushback), discard them — and seek backwards
/// on a seekable stream so the underlying offset reflects the bytes
/// the caller has actually consumed.
pub fn switch_to_write(state: &mut State) -> std::io::Result<()> {
    if state.last_op == Direction::Read {
        let backup = state.buffer.read_remaining() + state.pushback.len();
        if backup > 0 && state.handle.is_seekable() {
            let _ = state.handle.seek(SeekFrom::Current(-(backup as i64)));
        }
        state.buffer.discard();
        state.pushback.clear();
    }
    state.last_op = Direction::Write;
    Ok(())
}

/// Pull one byte. Honors pushback first, then buffer, then refill.
/// On EOF returns `Ok(None)`; on hard error returns `Err`.
pub fn read_one(state: &mut State) -> std::io::Result<Option<u8>> {
    switch_to_read(state)?;

    if let Some(b) = state.pushback.pop() {
        return Ok(Some(b));
    }
    if state.buffer.cursor < state.buffer.fill {
        let b = state.buffer.data[state.buffer.cursor];
        state.buffer.cursor += 1;
        return Ok(Some(b));
    }
    refill_buffer(state)?;
    if state.buffer.cursor < state.buffer.fill {
        let b = state.buffer.data[state.buffer.cursor];
        state.buffer.cursor += 1;
        Ok(Some(b))
    } else {
        state.eof = true;
        Ok(None)
    }
}

/// Refill the read buffer from the underlying handle. Sets `eof` on
/// short read; sets `err` on I/O failure.
pub fn refill_buffer(state: &mut State) -> std::io::Result<()> {
    if state.buffer.capacity() == 0 {
        // Unbuffered: grow a tiny scratch slot rather than performing
        // a single-byte syscall via a stack buffer.
        state.buffer.data.resize(1, 0);
    }
    state.buffer.cursor = 0;
    state.buffer.fill = 0;
    let cap = state.buffer.capacity();
    match state.handle.read(&mut state.buffer.data[..cap]) {
        Ok(0) => {
            state.eof = true;
            Ok(())
        }
        Ok(n) => {
            state.buffer.fill = n;
            Ok(())
        }
        Err(e) => {
            state.err = true;
            Err(e)
        }
    }
}

/// Read into a caller-supplied slice, honoring pushback and the
/// internal read buffer. Returns the number of bytes delivered (may
/// be less than `dst.len()` on EOF or short read; the caller decides
/// whether to retry).
pub fn read_into(state: &mut State, dst: &mut [u8]) -> std::io::Result<usize> {
    switch_to_read(state)?;
    let mut written = 0;

    while written < dst.len() {
        // Pushback first (LIFO). `ungetc` pushed in order, so we pop
        // from the end.
        if let Some(b) = state.pushback.pop() {
            dst[written] = b;
            written += 1;
            continue;
        }
        // Then buffered data.
        if state.buffer.cursor < state.buffer.fill {
            let avail = state.buffer.fill - state.buffer.cursor;
            let take = avail.min(dst.len() - written);
            dst[written..written + take]
                .copy_from_slice(&state.buffer.data[state.buffer.cursor..state.buffer.cursor + take]);
            state.buffer.cursor += take;
            written += take;
            continue;
        }
        // Nothing buffered and the request is at least as large as
        // our buffer — read directly into the caller's slice (no
        // double-copy).
        let remaining = dst.len() - written;
        if state.buffer.capacity() == 0 || remaining >= state.buffer.capacity() {
            match state.handle.read(&mut dst[written..]) {
                Ok(0) => {
                    state.eof = true;
                    break;
                }
                Ok(n) => {
                    written += n;
                }
                Err(e) => {
                    state.err = true;
                    return Err(e);
                }
            }
        } else {
            // Smaller request: refill buffer then loop.
            refill_buffer(state)?;
            if state.buffer.fill == 0 {
                break;
            }
        }
    }
    Ok(written)
}

/// Write a slice through the buffering policy.
pub fn write_from(state: &mut State, src: &[u8]) -> std::io::Result<usize> {
    switch_to_write(state)?;
    drain_capture_into_buffer(state);
    write_through_buffer(state, src)
}

/// Inner buffer/pass-through logic, with no capture drain. Called by
/// both `write_from` (after draining) and `drain_capture_into_buffer`
/// (which must not re-drain).
fn write_through_buffer(state: &mut State, src: &[u8]) -> std::io::Result<usize> {
    if state.buffer.policy == BufferPolicy::None || matches!(state.handle, Handle::StderrShared) {
        // Pass-through. For `StderrShared`, libc historically defaults
        // to unbuffered; we mirror that by using the handle directly.
        let mut written = 0;
        while written < src.len() {
            match state.handle.write(&src[written..]) {
                Ok(0) => {
                    state.err = true;
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
                Ok(n) => written += n,
                Err(e) => {
                    state.err = true;
                    return Err(e);
                }
            }
        }
        return Ok(written);
    }

    let mut written = 0;
    while written < src.len() {
        // Find newline if line-buffered, so we can flush at the right
        // boundary.
        let cap = state.buffer.capacity();
        let avail = cap - state.buffer.fill;
        if avail == 0 {
            flush_internal(state)?;
            continue;
        }
        let take = avail.min(src.len() - written);
        let chunk = &src[written..written + take];
        let dst_off = state.buffer.fill;
        state.buffer.data[dst_off..dst_off + take].copy_from_slice(chunk);
        state.buffer.fill += take;
        state.buffer.dirty = true;
        written += take;

        let needs_flush_for_newline = state.buffer.policy == BufferPolicy::Line
            && chunk.contains(&b'\n');
        let needs_flush_for_full = state.buffer.fill == cap;
        if needs_flush_for_newline || needs_flush_for_full {
            flush_internal(state)?;
        }
    }
    Ok(written)
}

/// Common entry-point boilerplate: look up a token, lock, run the
/// closure with `&mut State`. Returns `None` if the token is invalid
/// (errno already set to EBADF).
pub fn with_token<R>(t: crate::FILE_ptr, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let arc = match crate::slots::lookup(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return None;
        }
    };
    Some(arc.with_state(f))
}
