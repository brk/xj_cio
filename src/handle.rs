//! Handle dispatch: the moral kernel-side of every stream.
//!
//! A `Handle` is the `enum` over every kind of "thing a `FILE` might
//! be backed by": a real disk file, one of the three standard
//! streams (shared with `std::io`, or bypassed after `setvbuf`), a
//! pipe to a child process, an in-memory buffer, or "closed".

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::Child;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeDir {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemMode {
    /// Read-only ("r")
    R,
    /// Write, truncate, NUL-terminate ("w")
    W,
    /// Append, NUL-terminate ("a")
    A,
    /// Read+write, no truncate ("r+")
    Rplus,
    /// Read+write, truncate ("w+")
    Wplus,
    /// Append+read ("a+")
    Aplus,
}

impl MemMode {
    pub fn writable(self) -> bool {
        !matches!(self, MemMode::R)
    }
    pub fn readable(self) -> bool {
        !matches!(self, MemMode::W | MemMode::A)
    }
    pub fn append(self) -> bool {
        matches!(self, MemMode::A | MemMode::Aplus)
    }
    pub fn truncates(self) -> bool {
        matches!(self, MemMode::W | MemMode::Wplus)
    }
}

/// The backing store for an `fmemopen` stream.
pub struct MemBacking {
    /// Either a user-provided pointer (we never free) or our own
    /// `Vec<u8>` (we own).
    pub storage: MemStorage,
    pub capacity: usize,
    /// Logical length; advanced by writes, never beyond `capacity`.
    /// Reads above this offset return EOF; reads below return the
    /// stored bytes.
    pub size: usize,
    /// Cursor in bytes from the start.
    pub pos: usize,
    pub mode: MemMode,
}

pub enum MemStorage {
    User { ptr: *mut u8, len: usize },
    Owned(Vec<u8>),
}

unsafe impl Send for MemStorage {}
unsafe impl Sync for MemStorage {}

impl MemBacking {
    pub fn slice(&self) -> &[u8] {
        match &self.storage {
            MemStorage::User { ptr, len } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
            MemStorage::Owned(v) => v.as_slice(),
        }
    }
    pub fn slice_mut(&mut self) -> &mut [u8] {
        match &mut self.storage {
            MemStorage::User { ptr, len } => unsafe { std::slice::from_raw_parts_mut(*ptr, *len) },
            MemStorage::Owned(v) => v.as_mut_slice(),
        }
    }
}

pub enum Handle {
    /// Real disk file (or `fdopen`-created file).
    Regular(File),
    /// Default sharing modes for the standard streams.
    StdinShared,
    StdoutShared,
    StderrShared,
    /// Bypass mode after the user called `setvbuf` on a stdio
    /// stream and we elected to take over buffering. The inner
    /// `File` was constructed via `File::from_raw_fd(dup(STD*_FILENO))`
    /// — we own the dup'd fd and close it on Drop.
    StdinBypass(File),
    StdoutBypass(File),
    StderrBypass(File),
    /// Pipe to / from a child started by `popen`.
    Pipe {
        child: Option<Child>,
        file: File,
        dir: PipeDir,
    },
    /// In-memory `fmemopen` stream.
    Mem(MemBacking),
    /// Post-`fclose` placeholder. The slot is removed from the
    /// registry first, so this state is rarely observed; it exists so
    /// `Drop` can run cleanup uniformly.
    Closed,
}

impl Handle {
    /// Read up to `dst.len()` bytes from the underlying source.
    pub fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        match self {
            Handle::Regular(f) => f.read(dst),
            Handle::StdinShared => {
                let stdin = io::stdin();
                let mut g = stdin.lock();
                g.read(dst)
            }
            Handle::StdinBypass(f) => f.read(dst),
            Handle::Pipe { file, dir, .. } => {
                if *dir != PipeDir::Read {
                    return Err(io::Error::from_raw_os_error(libc::EBADF));
                }
                file.read(dst)
            }
            Handle::Mem(m) => {
                if !m.mode.readable() {
                    return Err(io::Error::from_raw_os_error(libc::EBADF));
                }
                if m.pos >= m.size {
                    return Ok(0);
                }
                let avail = m.size - m.pos;
                let n = dst.len().min(avail);
                dst[..n].copy_from_slice(&m.slice()[m.pos..m.pos + n]);
                m.pos += n;
                Ok(n)
            }
            Handle::StdoutShared
            | Handle::StderrShared
            | Handle::StdoutBypass(_)
            | Handle::StderrBypass(_) => Err(io::Error::from_raw_os_error(libc::EBADF)),
            Handle::Closed => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    /// Write `src` to the underlying sink.
    pub fn write(&mut self, src: &[u8]) -> io::Result<usize> {
        match self {
            Handle::Regular(f) => f.write(src),
            Handle::StdoutShared => {
                let so = io::stdout();
                let mut g = so.lock();
                let n = g.write(src)?;
                Ok(n)
            }
            Handle::StderrShared => {
                let se = io::stderr();
                let mut g = se.lock();
                let n = g.write(src)?;
                Ok(n)
            }
            Handle::StdoutBypass(f) | Handle::StderrBypass(f) => f.write(src),
            Handle::Pipe { file, dir, .. } => {
                if *dir != PipeDir::Write {
                    return Err(io::Error::from_raw_os_error(libc::EBADF));
                }
                file.write(src)
            }
            Handle::Mem(m) => {
                if !m.mode.writable() {
                    return Err(io::Error::from_raw_os_error(libc::EBADF));
                }
                if m.mode.append() {
                    m.pos = m.size;
                }
                if m.pos >= m.capacity {
                    return Err(io::Error::from_raw_os_error(libc::ENOSPC));
                }
                let n = src.len().min(m.capacity - m.pos);
                let pos = m.pos;
                let buf = m.slice_mut();
                buf[pos..pos + n].copy_from_slice(&src[..n]);
                m.pos += n;
                if m.pos > m.size {
                    m.size = m.pos;
                }
                Ok(n)
            }
            Handle::StdinShared | Handle::StdinBypass(_) => {
                Err(io::Error::from_raw_os_error(libc::EBADF))
            }
            Handle::Closed => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match self {
            Handle::Regular(f) => f.flush(),
            Handle::StdoutShared => {
                let so = io::stdout();
                let mut g = so.lock();
                g.flush()
            }
            Handle::StderrShared => {
                let se = io::stderr();
                let mut g = se.lock();
                g.flush()
            }
            Handle::StdoutBypass(f) | Handle::StderrBypass(f) => f.flush(),
            Handle::Pipe { file, .. } => file.flush(),
            Handle::Mem(m) => {
                // For "w"/"a" modes, ensure NUL terminator after written
                // region (per POSIX fmemopen semantics).
                if m.mode.writable() {
                    let nul_at = m.size.min(m.capacity.saturating_sub(1));
                    if nul_at < m.capacity {
                        let buf = m.slice_mut();
                        if nul_at < buf.len() {
                            buf[nul_at] = 0;
                        }
                    }
                }
                Ok(())
            }
            Handle::StdinShared | Handle::StdinBypass(_) => Ok(()),
            Handle::Closed => Ok(()),
        }
    }

    pub fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Handle::Regular(f) => f.seek(pos),
            Handle::StdoutBypass(f) | Handle::StderrBypass(f) | Handle::StdinBypass(f) => {
                f.seek(pos)
            }
            Handle::Mem(m) => {
                let cap = m.capacity as i64;
                let cur = m.pos as i64;
                let end = m.size as i64;
                let target: i64 = match pos {
                    SeekFrom::Start(o) => o as i64,
                    SeekFrom::Current(o) => cur + o,
                    SeekFrom::End(o) => end + o,
                };
                if target < 0 || target > cap {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL));
                }
                m.pos = target as usize;
                Ok(target as u64)
            }
            // Pipes and shared stdio are unseekable.
            _ => Err(io::Error::from_raw_os_error(libc::ESPIPE)),
        }
    }

    /// Underlying fd, or -1 if there is none (mem stream / closed).
    pub fn raw_fd(&self) -> core::ffi::c_int {
        match self {
            Handle::Regular(f) => f.as_raw_fd(),
            Handle::StdinShared => libc::STDIN_FILENO,
            Handle::StdoutShared => libc::STDOUT_FILENO,
            Handle::StderrShared => libc::STDERR_FILENO,
            Handle::StdinBypass(f) | Handle::StdoutBypass(f) | Handle::StderrBypass(f) => {
                f.as_raw_fd()
            }
            Handle::Pipe { file, .. } => file.as_raw_fd(),
            Handle::Mem(_) | Handle::Closed => -1,
        }
    }

    pub fn is_seekable(&self) -> bool {
        matches!(
            self,
            Handle::Regular(_)
                | Handle::Mem(_)
                | Handle::StdoutBypass(_)
                | Handle::StderrBypass(_)
                | Handle::StdinBypass(_)
        )
    }

    /// Wrap a raw fd (after `dup`) as a `File`. Caller asserts `fd`
    /// is a valid kernel-owned descriptor we may close on drop.
    pub unsafe fn file_from_dup(fd: libc::c_int) -> File {
        unsafe {
            let dup = libc::dup(fd);
            File::from_raw_fd(dup)
        }
    }
}
