//! `popen` / `pclose`.

use std::ffi::CStr;
use std::fs::File;
use std::os::fd::{FromRawFd, IntoRawFd};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use crate::buffer::BufferPolicy;
use crate::errno;
use crate::handle::{Handle, PipeDir};
use crate::slots;
use crate::state::{FILE, OpenMode, State};
use crate::{BUFSIZ, EOF, FILE_ptr};

pub fn popen(command: &CStr, mode: &CStr) -> FILE_ptr {
    let bytes = mode.to_bytes();
    let dir = match bytes.first().copied() {
        Some(b'r') => PipeDir::Read,
        Some(b'w') => PipeDir::Write,
        _ => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };

    let cmd_str = match command.to_str() {
        Ok(s) => s,
        Err(_) => {
            errno::set(libc::EINVAL);
            return std::ptr::null();
        }
    };

    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(cmd_str);
    let child: Child = match dir {
        PipeDir::Read => match c.stdout(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                errno::set_from_io(&e);
                return std::ptr::null();
            }
        },
        PipeDir::Write => match c.stdin(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                errno::set_from_io(&e);
                return std::ptr::null();
            }
        },
    };

    let mut child = child;
    let file: File = match dir {
        PipeDir::Read => {
            let p = child.stdout.take().expect("popen: stdout pipe missing");
            unsafe { File::from_raw_fd(p.into_raw_fd()) }
        }
        PipeDir::Write => {
            let p = child.stdin.take().expect("popen: stdin pipe missing");
            unsafe { File::from_raw_fd(p.into_raw_fd()) }
        }
    };

    let handle = Handle::Pipe {
        child: Some(child),
        file,
        dir,
    };
    let om = match dir {
        PipeDir::Read => OpenMode::R,
        PipeDir::Write => OpenMode::W,
    };
    let state = State::new(handle, om, BufferPolicy::Full, BUFSIZ);
    slots::insert(Arc::new(FILE::new(state)))
}

pub fn pclose(t: FILE_ptr) -> core::ffi::c_int {
    let arc = match slots::remove(t) {
        Some(a) => a,
        None => {
            errno::set(libc::EBADF);
            return -1;
        }
    };
    let child_opt: Option<Child> = arc.with_state(|s| {
        let _ = super::ops::flush_internal(s);
        let taken = match &mut s.handle {
            Handle::Pipe { child, .. } => child.take(),
            _ => None,
        };
        s.handle = Handle::Closed;
        taken
    });
    match child_opt {
        Some(mut child) => match child.wait() {
            Ok(status) => {
                use std::os::unix::process::ExitStatusExt;
                // Re-encode the wait status the way POSIX expects:
                // pclose returns the status int suitable for
                // WIFEXITED/WEXITSTATUS/WIFSIGNALED macros.
                if let Some(code) = status.code() {
                    (code & 0xff) << 8
                } else if let Some(sig) = status.signal() {
                    sig & 0x7f
                } else {
                    -1
                }
            }
            Err(e) => {
                errno::set_from_io(&e);
                -1
            }
        },
        None => {
            errno::set(libc::EINVAL);
            EOF
        }
    }
}
