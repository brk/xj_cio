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

fn open_mem(storage: MemStorage, mode: &CStr) -> FILE_ptr {
    let capacity = match &storage {
        MemStorage::Owned(v) => v.len(),
        MemStorage::User { len, .. } => *len,
    };
    if capacity == 0 {
        errno::set(libc::EINVAL);
        return std::ptr::null();
    }
    let (mm, om) = match parse_mem_mode(mode) {
        Some(p) => p,
        None => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };

    // Initial size: for "r" / "r+" the buffer is considered already
    // populated up to `capacity`. For "w" / "w+" we truncate to 0.
    // For "a" / "a+" we seek to the first NUL (strlen-or-cap).
    let mut backing = MemBacking {
        storage,
        capacity,
        size: match mm {
            MemMode::R | MemMode::Rplus => capacity,
            MemMode::W | MemMode::Wplus | MemMode::A | MemMode::Aplus => 0,
        },
        pos: 0,
        mode: mm,
    };

    if matches!(mm, MemMode::A | MemMode::Aplus) {
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

/// Safe equivalent of `fmemopen` over an owned buffer. The stream
/// takes ownership of `buf`; its capacity is `buf.len()`. Returns a
/// null handle (with `errno`) on invalid mode or empty buffer.
pub fn fmemopen_(buf: Vec<u8>, mode: &CStr) -> FILE_ptr {
    open_mem(MemStorage::Owned(buf), mode)
}

pub fn fmemopen(buf: *mut libc::c_void, size: libc::size_t, mode: &CStr) -> FILE_ptr {
    if buf.is_null() {
        if size == 0 {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
        return fmemopen_(vec![0u8; size], mode);
    }
    open_mem(
        MemStorage::User {
            ptr: buf as *mut u8,
            len: size,
        },
        mode,
    )
}

/// `open_memstream(*bufp, *sizep)` is not implemented in v1; returns a
/// null handle with `errno = ENOSYS`.
pub fn open_memstream(_bufp: *mut *mut libc::c_char, _sizep: *mut libc::size_t) -> FILE_ptr {
    errno::set(libc::ENOSYS);
    std::ptr::null()
}
