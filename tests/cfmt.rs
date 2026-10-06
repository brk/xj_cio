#![cfg(feature = "cfmt")]

use std::ffi::CString;
use std::fmt;

use xj_cio::{FILE_ptr, cfprintf, fclose, ferror, fopen};

fn path(name: &str) -> (std::path::PathBuf, CString) {
    let path = std::env::temp_dir().join(format!("xj_cio_cfmt_{}_{}", std::process::id(), name));
    let c_path = CString::new(path.to_str().unwrap()).unwrap();
    (path, c_path)
}

struct Nested(FILE_ptr);

impl fmt::Display for Nested {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        assert_eq!(cfprintf!(self.0, "inner|"), 6);
        formatter.pad("outer")
    }
}

#[test]
fn stream_macro_uses_xj_cio_buffering_and_renders_before_locking() {
    let (path, c_path) = path("nested.txt");
    let stream = fopen(&c_path, c"w");
    assert!(!stream.is_null());

    let width = 7usize;
    assert_eq!(cfprintf!(stream, "{:>width$}", Nested(stream)), 7);
    assert_eq!(fclose(stream), 0);
    assert_eq!(std::fs::read(&path).unwrap(), b"inner|  outer");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_dead_stream_returns_minus_one() {
    let (path, c_path) = path("dead.txt");
    let stream = fopen(&c_path, c"w");
    assert_eq!(fclose(stream), 0);
    assert_eq!(cfprintf!(stream, "hello"), -1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_non_writable_stream_sets_its_error_indicator() {
    let (path, c_path) = path("readonly.txt");
    std::fs::write(&path, b"input").unwrap();
    let stream = fopen(&c_path, c"r");
    assert_eq!(cfprintf!(stream, "hello"), -1);
    assert_ne!(ferror(stream), 0);
    assert_eq!(xj_cio::errno::get(), libc::EBADF);
    assert_eq!(fclose(stream), 0);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn zstr_arguments_use_the_byte_path() {
    let (path, c_path) = path("zstr.txt");
    let stream = fopen(&c_path, c"w");
    let name = xj_zstr::ZStr::from_bytes_with_nul(b"\xffab\0hidden\0").unwrap();
    assert_eq!(cfprintf!(stream, "[{:>5}|{:.2}]", name, name), 10);
    assert_eq!(fclose(stream), 0);
    assert_eq!(std::fs::read(&path).unwrap(), b"[  \xffab|\xffa]");
    std::fs::remove_file(path).unwrap();
}
