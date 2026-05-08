//! In-memory streams: `fmemopen`, `open_memstream`.

use std::ffi::CStr;
use std::sync::Arc;

use crate::buffer::BufferPolicy;
use crate::errno;
use crate::handle::{Handle, MemBacking, MemMode, MemStorage};
use crate::slots;
use crate::state::{FILE, OpenMode, State};
use crate::FILE_ptr;

fn parse_mem_mode(s: &CStr) -> Option<(MemMode, OpenMode)> {
    let bytes = s.to_bytes();
    let mut base = None;
    let mut plus = false;
    for &c in bytes {
        match c {
            b'r' if base.is_none() => base = Some(b'r'),
            b'w' if base.is_none() => base = Some(b'w'),
            b'a' if base.is_none() => base = Some(b'a'),
            b'+' => plus = true,
            b'b' | b'e' => {}
            _ => return None,
        }
    }
    Some(match (base?, plus) {
        (b'r', false) => (MemMode::R, OpenMode::R),
        (b'w', false) => (MemMode::W, OpenMode::W),
        (b'a', false) => (MemMode::A, OpenMode::A),
        (b'r', true) => (MemMode::Rplus, OpenMode::Rplus),
        (b'w', true) => (MemMode::Wplus, OpenMode::Wplus),
        (b'a', true) => (MemMode::Aplus, OpenMode::Aplus),
        _ => return None,
    })
}

pub fn fmemopen(buf: *mut libc::c_void, size: libc::size_t, mode: &CStr) -> FILE_ptr {
    let (mm, om) = match parse_mem_mode(mode) {
        Some(p) => p,
        None => {
            errno::set(libc::EINVAL);
            return 0;
        }
    };
    if size == 0 {
        errno::set(libc::EINVAL);
        return 0;
    }

    let storage = if buf.is_null() {
        MemStorage::Owned(vec![0u8; size])
    } else {
        MemStorage::User {
            ptr: buf as *mut u8,
            len: size,
        }
    };

    // Initial size: for "r" / "r+" the buffer is considered already
    // populated up to `size` (or up to the first NUL — POSIX is
    // explicit but for simplicity we treat it as `size`). For "w" /
    // "w+" we truncate. For "a" / "a+" we seek to the first NUL.
    let mut backing = MemBacking {
        storage,
        capacity: size,
        size: match mm {
            MemMode::R | MemMode::Rplus => size,
            MemMode::W | MemMode::Wplus => 0,
            MemMode::A | MemMode::Aplus => {
                // Seek past existing content (up to first NUL).
                // For owned (we just zero'd) this is 0; for user-buf
                // it's the strlen.
                let start = match &storage_helper(&mm) {
                    _ => 0usize,
                };
                start
            }
        },
        pos: 0,
        mode: mm,
    };

    if matches!(mm, MemMode::A | MemMode::Aplus) {
        // Compute initial size as strlen-or-cap of existing buf.
        let bytes = backing.slice();
        let mut n = bytes.len();
        for (i, &b) in bytes.iter().enumerate() {
            if b == 0 {
                n = i;
                break;
            }
        }
        backing.size = n;
        backing.pos = n;
    }

    let state = State::new(Handle::Mem(backing), om, BufferPolicy::Full, crate::BUFSIZ);
    slots::insert(Arc::new(FILE::new(state)))
}

fn storage_helper(_m: &MemMode) -> usize {
    0
}

/// `open_memstream(*bufp, *sizep)` is not implemented in v1; returns
/// 0 with `errno = ENOSYS`.
pub fn open_memstream(_bufp: *mut *mut libc::c_char, _sizep: *mut libc::size_t) -> FILE_ptr {
    errno::set(libc::ENOSYS);
    0
}
