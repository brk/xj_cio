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
