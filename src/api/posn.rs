//! Positioning: `fseek`, `fseeko`, `ftell`, `ftello`, `rewind`,
//! `fgetpos`, `fsetpos`.

use std::io::SeekFrom;

use crate::errno;
use crate::state::Direction;
use crate::FILE_ptr;

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
            let backup = (s.buffer.fill - s.buffer.cursor) + s.pushback.len();
            SeekFrom::Current(o - backup as i64)
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
    let r = ops::with_token(t, |s| seek_internal(s, offset as i64, whence));
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
    let r = ops::with_token(t, |s| seek_internal(s, offset as i64, whence));
    match r {
        Some(Ok(_)) => 0,
        Some(Err(e)) => {
            errno::set(e);
            -1
        }
        None => -1,
    }
}

pub fn ftell(t: FILE_ptr) -> libc::c_long {
    ftello(t) as libc::c_long
}

pub fn ftello(t: FILE_ptr) -> libc::off_t {
    let r = ops::with_token(t, |s| -> Result<i64, libc::c_int> {
        let cur = s
            .handle
            .seek(SeekFrom::Current(0))
            .map_err(|e| errno::from_io(&e))?;
        let mut pos = cur as i64;
        match s.last_op {
            Direction::Read => {
                let buffered = (s.buffer.fill - s.buffer.cursor) as i64;
                pos -= buffered;
                pos -= s.pushback.len() as i64;
            }
            Direction::Write => {
                pos += s.buffer.fill as i64;
            }
            Direction::Either => {}
        }
        Ok(pos)
    });
    match r {
        Some(Ok(p)) => p as libc::off_t,
        Some(Err(e)) => {
            errno::set(e);
            -1
        }
        None => -1,
    }
}

pub fn rewind(t: FILE_ptr) {
    let _ = ops::with_token(t, |s| {
        let _ = seek_internal(s, 0, libc::SEEK_SET);
        s.err = false;
    });
}

/// `fgetpos`/`fsetpos` stash the position inside the user's `fpos_t`.
/// We store an `i64` byte offset (libc's `fpos_t` is opaque so callers
/// can't tell the difference).
pub fn fgetpos(t: FILE_ptr, pos: *mut libc::fpos_t) -> core::ffi::c_int {
    if pos.is_null() {
        errno::set(libc::EINVAL);
        return -1;
    }
    let off = ftello(t);
    if off < 0 {
        return -1;
    }
    unsafe {
        // libc::fpos_t on linux is a struct; we write an i64 into its
        // first 8 bytes. This works because translated programs treat
        // fpos_t opaquely.
        std::ptr::write_unaligned(pos as *mut i64, off as i64);
    }
    0
}

pub fn fsetpos(t: FILE_ptr, pos: *const libc::fpos_t) -> core::ffi::c_int {
    if pos.is_null() {
        errno::set(libc::EINVAL);
        return -1;
    }
    let off: i64 = unsafe { std::ptr::read_unaligned(pos as *const i64) };
    fseeko(t, off as libc::off_t, libc::SEEK_SET)
}
