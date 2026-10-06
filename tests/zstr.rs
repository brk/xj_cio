//! `xj_zstr` integration: `CStrDest` destinations for functions that
//! write terminated strings, and `ZString` for `getline`/`getdelim`.

use std::ffi::CString;

use xj_cio::*;
use xj_zstr::{ZStr, ZString};

/// A file holding `contents`, opened for reading. The guard removes it.
struct Input {
    path: std::path::PathBuf,
    stream: FILE_ptr,
}

impl Input {
    fn new(name: &str, contents: &[u8]) -> Self {
        let path = std::env::temp_dir()
            .join(format!("xj_cio_zstr_{}_{}", std::process::id(), name));
        std::fs::write(&path, contents).unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        let stream = fopen(&c_path, c"r");
        assert!(!stream.is_null());
        Self { path, stream }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        fclose(self.stream);
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn fgets_into_a_zstr_reads_as_many_bytes_as_c() {
    // C: fgets(buf, 6, f) with char buf[6] reads "abcde" whole.
    let input = Input::new("fgets_zstr", b"abcde\nfg");
    let mut line = ZString::with_capacity(5);
    assert_eq!(fgets_((&mut line).into(), input.stream), Some(5));
    assert_eq!(line.as_bytes(), b"abcde");
    assert_eq!(fgets_((&mut line).into(), input.stream), Some(1));
    assert_eq!(line.as_buffer(), b"\n\0cde\0");
    assert_eq!(fgets_((&mut line).into(), input.stream), Some(2));
    assert_eq!(line.as_bytes(), b"fg");
    assert_eq!(fgets_((&mut line).into(), input.stream), None);
    assert_eq!(line.as_bytes(), b"fg", "EOF leaves the destination unchanged");
}

#[test]
fn fgets_on_a_slice_and_a_limit_keep_c_sizes() {
    let input = Input::new("fgets_sizes", b"abcdef\n");
    let mut array = [b'x'; 4];
    assert_eq!(fgets_((&mut array).into(), input.stream), Some(3));
    assert_eq!(&array, b"abc\0");

    let mut line = ZString::new(b"xxxxx");
    assert_eq!(fgets_(line.limit(3), input.stream), Some(2));
    assert_eq!(line.as_buffer(), b"de\0xx\0");

    // fgets(buf, 1, f) stores only the terminator; fgets(buf, 0, f) fails.
    assert_eq!(fgets_(line.limit(1), input.stream), Some(0));
    assert_eq!(line.as_bytes(), b"");
    assert_eq!(fgets_(line.limit(0), input.stream), None);
    let mut empty = ZString::with_capacity(0);
    assert_eq!(fgets_((&mut empty).into(), input.stream), Some(0));
}

#[test]
fn getline_grows_a_zstring_and_keeps_the_delimiter() {
    let long = vec![b'z'; 300];
    let mut contents = b"short\n".to_vec();
    contents.extend_from_slice(&long);
    contents.extend_from_slice(b"\nlast");
    let input = Input::new("getline", &contents);

    let mut line = ZString::default();
    assert_eq!(getline_(&mut line, input.stream), 6);
    assert_eq!(line.as_bytes(), b"short\n");
    assert_eq!(line.capacity(), 127);

    assert_eq!(getline_(&mut line, input.stream), 301);
    assert_eq!(&line.as_bytes()[..300], &long[..]);
    assert_eq!(line.capacity(), 511);

    assert_eq!(getline_(&mut line, input.stream), 4);
    assert_eq!(line.as_bytes(), b"last");
    assert_eq!(getline_(&mut line, input.stream), -1);
}

#[test]
fn getdelim_counts_bytes_past_an_embedded_nul() {
    let input = Input::new("getdelim", b"a\0b|rest");
    let mut field = ZString::with_capacity(2);
    assert_eq!(getdelim_(&mut field, b'|' as libc::c_int, input.stream), 4);
    assert_eq!(&field.as_buffer()[..5], b"a\0b|\0");
    assert_eq!(field.as_bytes(), b"a");
    assert_eq!(getdelim_(&mut field, b'|' as libc::c_int, input.stream), 4);
    assert_eq!(field.as_bytes(), b"rest");
}

#[test]
fn snprintf_into_a_zstr_uses_every_writable_byte() {
    let mut owned = ZString::with_capacity(4);
    assert_eq!(snprintf_((&mut owned).into(), c"%s-%d", &[Arg::S(c"ab".as_ptr()), Arg::I(7)]), 4);
    assert_eq!(owned.as_bytes(), b"ab-7");

    assert_eq!(snprintf_(owned.limit(3), c"%d", &[Arg::I(12345)]), 5);
    assert_eq!(owned.as_buffer(), b"12\07\0");

    let mut raw = [b'x'; 4];
    let n = snprintf(raw.as_mut_ptr() as *mut libc::c_char, raw.len(), c"%d", &[Arg::I(12345)]);
    assert_eq!((n, &raw), (5, b"123\0"));
    let n = sprintf(raw.as_mut_ptr() as *mut libc::c_char, c"%d", &[Arg::I(9)]);
    assert_eq!(n, 1);
    assert_eq!(&raw, b"9\03\0");
}

#[test]
fn tmpnam_and_ctermid_fill_or_refuse_a_destination() {
    let mut name = ZString::with_capacity(L_tmpnam - 1);
    let len = tmpnam_((&mut name).into()).unwrap();
    assert_eq!(len, name.len());
    assert!(name.as_bytes().starts_with(b"/tmp/xj"));

    let mut terminal = ZString::with_capacity(L_ctermid - 1);
    assert_eq!(ctermid_((&mut terminal).into()), Some(8));
    assert_eq!(terminal.as_bytes(), b"/dev/tty");

    let mut small = *b"xxxx";
    errno::set(0);
    assert_eq!(ctermid_((&mut small).into()), None);
    assert_eq!(errno::get(), libc::ERANGE);
    assert_eq!(&small, b"xxxx");

    assert!(!tmpnam(std::ptr::null_mut()).is_null());
}

#[test]
fn scanf_string_fields_fit_a_zstr_exactly() {
    let input = Input::new("scanf_fit", b"abcd 42 xy]z");
    let mut word = ZString::with_capacity(4);
    let mut set = [b'-'; 3];
    let mut number: libc::c_int = 0;
    let r = fscanf(
        input.stream,
        c"%s %d %[xy]",
        &mut [
            OutArg::Buf((&mut word).into()),
            OutArg::Int(&mut number),
            OutArg::Buf((&mut set).into()),
        ],
    );
    assert_eq!(r, 3);
    assert_eq!(word.as_bytes(), b"abcd");
    assert_eq!(number, 42);
    assert_eq!(&set, b"xy\0");
    assert_eq!(fgetc(input.stream), b']' as libc::c_int);
}

#[test]
fn scanf_overflow_stores_the_prefix_and_leaves_the_rest_unread() {
    let input = Input::new("scanf_overflow", b"abcdefg 5");
    let mut word = ZString::with_capacity(4);
    let mut number: libc::c_int = 0;
    errno::set(0);
    let r = fscanf(
        input.stream,
        c"%s %d",
        &mut [OutArg::Buf((&mut word).into()), OutArg::Int(&mut number)],
    );
    assert_eq!(r, 0);
    assert_eq!(errno::get(), libc::EOVERFLOW);
    assert_eq!(word.as_bytes(), b"abcd");
    assert_eq!(fgetc(input.stream), b'e' as libc::c_int);
    assert_eq!(number, 0);
}

#[test]
fn scanf_width_bounds_the_field_before_the_destination_does() {
    let input = Input::new("scanf_width", b"abcdefg");
    let mut word = ZString::with_capacity(4);
    let r = fscanf(input.stream, c"%3s", &mut [OutArg::Buf(word.limit(4))]);
    assert_eq!(r, 1);
    assert_eq!(word.as_bytes(), b"abc");
}

#[test]
fn scanf_chars_fill_the_region_without_a_terminator() {
    let input = Input::new("scanf_chars", b"abcdefgh");
    let mut exact = *b"xxx";
    let r = fscanf(input.stream, c"%3c", &mut [OutArg::Buf((&mut exact).into())]);
    assert_eq!(r, 1);
    assert_eq!(&exact, b"abc");

    // A ZStr's reserved NUL is not writable, so %3c needs capacity 3.
    let mut small = ZString::new(b"xx");
    errno::set(0);
    let r = fscanf(input.stream, c"%3c", &mut [OutArg::Buf((&mut small).into())]);
    assert_eq!(r, 0);
    assert_eq!(errno::get(), libc::EOVERFLOW);
    assert_eq!(small.as_buffer(), b"xx\0");
    assert_eq!(fgetc(input.stream), b'd' as libc::c_int, "nothing was consumed");
}

#[test]
fn read_only_strings_pass_as_c_str_views() {
    let path = std::env::temp_dir().join(format!("xj_cio_zstr_{}_path", std::process::id()));
    let name = ZString::new(path.to_str().unwrap());
    let stream = fopen(name.as_c_str(), c"w");
    assert!(!stream.is_null());
    let text = ZStr::from_bytes_with_nul(b"hi\0hidden\0").unwrap();
    assert!(fputs(text.as_c_str(), stream) >= 0);
    assert_eq!(fwrite_(text.as_bytes(), stream), 2);
    assert_eq!(fclose(stream), 0);
    assert_eq!(std::fs::read(&path).unwrap(), b"hihi");
    assert_eq!(remove(name.as_c_str()), 0);
}
