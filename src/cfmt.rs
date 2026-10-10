use std::io::{self, Write};

use crate::{FILE_ptr, stream_lock};

/// An [`xj_cfmt::CSink`] that writes through an xj_cio stream's buffering
/// policy and error state.
#[derive(Clone, Copy, Debug)]
pub struct CioSink(pub FILE_ptr);

impl xj_cfmt::CSink for CioSink {
    fn emit(self, bytes: &[u8]) -> io::Result<()> {
        let mut stream =
            stream_lock(self.0).ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        stream.write_all(bytes)
    }
}

/// Transfer one complete rendered C call through the stream's error handling.
pub fn emit_c(stream: FILE_ptr, result: Result<Vec<u8>, xj_cfmt::printf::Error>) -> i32 {
    use xj_cfmt::CSink;
    match result {
        Ok(bytes) => match CioSink(stream).emit(&bytes) {
            Ok(()) => bytes.len() as i32,
            Err(e) => {
                crate::errno::set(e.raw_os_error().unwrap_or(libc::EIO));
                -1
            }
        },
        Err(e) => {
            crate::errno::set(e.errno());
            -1
        }
    }
}
