//! The buffer engine.
//!
//! A single `Vec<u8>` is reused for both read- and write-buffering:
//! a stream is in exactly one direction at any moment (`State::last_op`),
//! and the buffer is fully drained on direction change.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferPolicy {
    /// `_IOFBF`: flush on full or on `fflush`.
    Full,
    /// `_IOLBF`: flush on `\n`, on full, or on `fflush`.
    Line,
    /// `_IONBF`: write-through, no buffering.
    None,
}

/// The buffer state owned by every stream.
pub struct Buffer {
    /// Backing store. Always owned by us; `setvbuf(buf, ...)` records
    /// the policy and size but the caller's buffer pointer is ignored.
    pub data: Vec<u8>,
    /// In read mode: bytes 0..fill are the next bytes to read,
    /// starting at `cursor`. In write mode: bytes 0..fill are
    /// pending writes (cursor is unused).
    pub fill: usize,
    pub cursor: usize,
    /// True when `data[0..fill]` contains writes not yet sent to
    /// the underlying handle.
    pub dirty: bool,
    pub policy: BufferPolicy,
}

impl Buffer {
    pub fn new(policy: BufferPolicy, capacity: usize) -> Self {
        let cap = if matches!(policy, BufferPolicy::None) {
            0
        } else {
            capacity.max(1)
        };
        Buffer {
            data: vec![0u8; cap],
            fill: 0,
            cursor: 0,
            dirty: false,
            policy,
        }
    }

    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    /// Reset all read/write state. Does not change capacity or policy.
    pub fn discard(&mut self) {
        self.fill = 0;
        self.cursor = 0;
        self.dirty = false;
    }

    /// Bytes available to be returned by a read pull from the buffer.
    pub fn read_remaining(&self) -> usize {
        self.fill.saturating_sub(self.cursor)
    }

    /// Free space (in bytes) left in the write buffer.
    pub fn write_remaining(&self) -> usize {
        self.capacity().saturating_sub(self.fill)
    }
}
