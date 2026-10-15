//! Positioning: `fseek`, `fseeko`, `ftell`, `ftello`, `rewind`,
//! `fgetpos_raw`, `fsetpos_raw`.

use std::io::SeekFrom;

use crate::errno;
use crate::state::Direction;
use crate::{FILE_ptr, fpos_t};

use super::ops;

fn whence_to_seekfrom(whence: libc::c_int, offset: i64) -> Option<SeekFrom> {
    match whence {
        libc::SEEK_SET => {
            if offset < 0 {
                None
            } else {
                Some(SeekFrom::Start(offset as u64))
            }
        }
        libc::SEEK_CUR => Some(SeekFrom::Current(offset)),
        libc::SEEK_END => Some(SeekFrom::End(offset)),
        _ => None,
    }
}

fn seek_internal(
    s: &mut crate::state::State,
    offset: i64,
    whence: libc::c_int,
) -> Result<u64, libc::c_int> {
    // Flush write buffer / discard read buffer.
    if s.last_op == Direction::Write {
        if let Err(e) = ops::flush_internal(s) {
            return Err(errno::from_io(&e));
        }
    }
    let sf = whence_to_seekfrom(whence, offset).ok_or(libc::EINVAL)?;
    // Adjust SeekFrom::Current by the unread buffered+pushback bytes
    // so that the requested offset is relative to the *logical*
    // position (not the kernel offset, which is past the end of the
    // read-ahead buffer).
    let sf = match sf {
        SeekFrom::Current(o) if s.last_op == Direction::Read => {
            let backup = s
                .buffer
                .read_remaining()
                .checked_add(s.pushback.len())
                .and_then(|n| i64::try_from(n).ok())
                .ok_or(libc::EOVERFLOW)?;
            SeekFrom::Current(o.checked_sub(backup).ok_or(libc::EOVERFLOW)?)
        }
        other => other,
    };
    let new = s.handle.seek(sf).map_err(|e| errno::from_io(&e))?;
    s.buffer.discard();
    s.pushback.clear();
    s.eof = false;
    s.last_op = Direction::Either;
    Ok(new)
}

pub fn fseek(t: FILE_ptr, offset: libc::c_long, whence: libc::c_int) -> core::ffi::c_int {
    let r = ops::with_token(t, |s| seek_internal(s, i64::from(offset), whence));
    match r {
        Some(Ok(_)) => 0,
        Some(Err(e)) => {
            errno::set(e);
            -1
        }
        None => -1,
    }
}

pub fn fseeko(t: FILE_ptr, offset: libc::off_t, whence: libc::c_int) -> core::ffi::c_int {
    let r = ops::with_token(t, |s| seek_internal(s, i64::from(offset), whence));
    match r {
        Some(Ok(_)) => 0,
        Some(Err(e)) => {
            errno::set(e);
            -1
        }
        None => -1,
    }
}

fn tell<T: TryFrom<i64>>(t: FILE_ptr) -> Result<T, libc::c_int> {
    ops::with_token(t, |s| {
        let cur = s
            .handle
            .seek(SeekFrom::Current(0))
            .map_err(|e| errno::from_io(&e))?;
        let mut pos = i64::try_from(cur).map_err(|_| libc::EOVERFLOW)?;
        match s.last_op {
            Direction::Read => {
                let buffered =
                    i64::try_from(s.buffer.read_remaining()).map_err(|_| libc::EOVERFLOW)?;
                let pushback = i64::try_from(s.pushback.len()).map_err(|_| libc::EOVERFLOW)?;
                pos = pos
                    .checked_sub(buffered)
                    .and_then(|p| p.checked_sub(pushback))
                    .ok_or(libc::EOVERFLOW)?;
            }
            Direction::Write => {
                let buffered = i64::try_from(s.buffer.fill).map_err(|_| libc::EOVERFLOW)?;
                pos = pos.checked_add(buffered).ok_or(libc::EOVERFLOW)?;
            }
            Direction::Either => {}
        }
        T::try_from(pos).map_err(|_| libc::EOVERFLOW)
    })
    .unwrap_or(Err(libc::EBADF))
}

/// Returns the logical position, or fails with `EOVERFLOW` if it does
/// not fit in the native `long` type.
pub fn ftell(t: FILE_ptr) -> libc::c_long {
    tell(t).unwrap_or_else(|e| {
        errno::set(e);
        -1
    })
}

/// Returns the logical position, or fails with `EOVERFLOW` if it does
/// not fit in the native `off_t` type.
pub fn ftello(t: FILE_ptr) -> libc::off_t {
    tell(t).unwrap_or_else(|e| {
        errno::set(e);
        -1
    })
}

pub fn rewind(t: FILE_ptr) {
    let _ = ops::with_token(t, |s| {
        let _ = seek_internal(s, 0, libc::SEEK_SET);
        s.err = false;
    });
}

/// `fgetpos_raw`/`fsetpos_raw` stash the position inside the user's `fpos_t`.
/// Positions saved by this crate contain an `i64` byte offset in
/// their first eight bytes and must be restored through this crate.
const _: () = assert!(size_of::<fpos_t>() >= size_of::<i64>());

/// # Safety
///
/// `pos` must be null (which fails with `EINVAL`) or valid for writes
/// of a [`crate::fpos_t`], and not accessed through any other pointer or
/// reference during the call. No alignment is required.
pub unsafe fn fgetpos_raw(t: FILE_ptr, pos: *mut fpos_t) -> core::ffi::c_int {
    if pos.is_null() {
        errno::set(libc::EINVAL);
        return -1;
    }
    let off = ftello(t);
    if off < 0 {
        return -1;
    }
    // SAFETY: `pos` is non-null, so the caller guarantees it is
    // writable for `size_of::<fpos_t>()` bytes, which the assertion
    // above shows covers an `i64`. `write_unaligned` needs no
    // alignment.
    unsafe {
        // Store our byte offset in the first eight bytes of the
        // native storage, whether it is a scalar or a struct.
        std::ptr::write_unaligned(pos as *mut i64, i64::from(off));
    }
    0
}

/// # Safety
///
/// `pos` must be null (which fails with `EINVAL`) or valid for reads of
/// a [`crate::fpos_t`] whose first `size_of::<i64>()` bytes are
/// initialized (e.g. one filled in by [`fgetpos_raw`]), and not written
/// through any other pointer during the call. No alignment is required.
pub unsafe fn fsetpos_raw(t: FILE_ptr, pos: *const fpos_t) -> core::ffi::c_int {
    if pos.is_null() {
        errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: `pos` is non-null, so the caller guarantees its first
    // `i64` is readable and initialized; `read_unaligned` needs no
    // alignment.
    let off: i64 = unsafe { std::ptr::read_unaligned(pos as *const i64) };
    let off = match libc::off_t::try_from(off) {
        Ok(off) => off,
        Err(_) => {
            errno::set(libc::EOVERFLOW);
            return -1;
        }
    };
    fseeko(t, off, libc::SEEK_SET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

    #[test]
    fn tell_rejects_offsets_that_do_not_fit() {
        let stream = crate::tmpfile();
        assert!(!stream.is_null());
        let boundary = i64::from(i32::MAX);
        {
            let mut guard = crate::stream_lock(stream).unwrap();
            guard.seek(SeekFrom::Start(boundary as u64)).unwrap();
        }
        assert_eq!(tell::<i32>(stream), Ok(i32::MAX));
        {
            let mut guard = crate::stream_lock(stream).unwrap();
            guard.seek(SeekFrom::Current(1)).unwrap();
        }
        assert_eq!(tell::<i32>(stream), Err(libc::EOVERFLOW));
        assert_eq!(tell::<i64>(stream), Ok(boundary + 1));
        assert_eq!(crate::fclose(stream), 0);
    }
}
