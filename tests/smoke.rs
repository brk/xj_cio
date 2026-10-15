//! Round-trip smoke tests for the public API. Validates the
//! architecture end-to-end on a handful of representative flows.

use std::ffi::CString;
use xj_cio::*;

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

#[test]
fn fopen_fwrite_fclose_fopen_fread_roundtrip() {
    let path = std::env::temp_dir().join("xj_cio_smoke.bin");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("wb"));
    assert!(!fp.is_null());
    let data = b"hello world";
    // SAFETY: the source is a live byte slice of the given length.
    let n = unsafe { fwrite_raw(data.as_ptr(), 1, data.len(), fp) };
    assert_eq!(n, data.len());
    assert_eq!(fclose(fp), 0);

    let fp = fopen(&p, &cstr("rb"));
    assert!(!fp.is_null());
    let mut buf = [0u8; 32];
    // SAFETY: the destination is a live local array of the given length.
    let n = unsafe { fread_raw(buf.as_mut_ptr(), 1, buf.len(), fp) };
    assert_eq!(n, data.len());
    assert_eq!(&buf[..n], data);
    assert_eq!(fclose(fp), 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn fgetc_ungetc_roundtrip() {
    let path = std::env::temp_dir().join("xj_cio_ungetc.bin");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("wb"));
    // SAFETY: the source is a live byte slice of the given length.
    unsafe { fwrite_raw(b"ABC".as_ptr(), 1, 3, fp) };
    fclose(fp);

    let fp = fopen(&p, &cstr("rb"));
    let a = fgetc(fp);
    assert_eq!(a, b'A' as i32);
    let u = ungetc(b'A' as i32, fp);
    assert_eq!(u, b'A' as i32);
    let a2 = fgetc(fp);
    assert_eq!(a2, b'A' as i32);
    let b = fgetc(fp);
    assert_eq!(b, b'B' as i32);
    fclose(fp);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn fprintf_renders_int_and_string() {
    let path = std::env::temp_dir().join("xj_cio_printf.txt");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("w"));
    let s = cstr("world");
    let r = xj_cio::fprintf!(fp, c"%d %s\n", 42, s);
    assert!(r > 0);
    fclose(fp);

    let got = std::fs::read_to_string(&path).unwrap();
    assert_eq!(got, "42 world\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fscanf_two_ints_with_whitespace() {
    use xj_cio::OutArg;
    let path = std::env::temp_dir().join("xj_cio_scanf.txt");
    let p = cstr(path.to_str().unwrap());
    std::fs::write(&path, "  12   34 ").unwrap();

    let fp = fopen(&p, &cstr("r"));
    let mut a: libc::c_int = -1;
    let mut b: libc::c_int = -1;
    // SAFETY: each raw `OutArg` points to a live local large enough for its conversion.
    let r = unsafe {
        fscanf_raw(
            fp,
            &cstr("%d %d"),
            &mut [OutArg::Int(&mut a as *mut _), OutArg::Int(&mut b as *mut _)],
        )
    };
    assert_eq!(r, 2);
    assert_eq!(a, 12);
    assert_eq!(b, 34);
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn invalid_token_returns_ebadf() {
    let bad: FILE_ptr = std::ptr::without_provenance(0x12345678usize);
    assert_eq!(fclose(bad), EOF);
    assert_eq!(errno::get(), libc::EBADF);
}

#[test]
fn stale_token_returns_ebadf_after_reopen() {
    let path = std::env::temp_dir().join("xj_cio_stale_token.txt");
    let p = cstr(path.to_str().unwrap());

    let stale = fopen(&p, &cstr("wb"));
    assert!(!stale.is_null());
    assert_eq!(fclose(stale), 0);

    let fresh = fopen(&p, &cstr("wb"));
    assert!(!fresh.is_null());
    assert_eq!(fclose(stale), EOF);
    assert_eq!(errno::get(), libc::EBADF);
    assert_eq!(fclose(fresh), 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn fseek_ftell_roundtrip() {
    let path = std::env::temp_dir().join("xj_cio_seek.txt");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("wb"));
    // SAFETY: the source is a live byte slice of the given length.
    unsafe { fwrite_raw(b"0123456789".as_ptr(), 1, 10, fp) };
    fclose(fp);

    let fp = fopen(&p, &cstr("rb"));
    assert_eq!(fseek(fp, 0, libc::SEEK_END), 0);
    assert_eq!(ftell(fp), 10);
    assert_eq!(fseek(fp, 5, libc::SEEK_SET), 0);
    assert_eq!(ftell(fp), 5);
    let c = fgetc(fp);
    assert_eq!(c, b'5' as i32);
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fgetpos_fsetpos_roundtrip() {
    let fp = tmpfile();
    assert!(!fp.is_null());
    // Exercise offsets beyond 32 bits when the native offset type permits it.
    // Seeking without writing does not allocate a large file.
    let offset = libc::off_t::try_from(1u64 << 33).unwrap_or(libc::off_t::MAX);
    assert_eq!(fseeko(fp, offset, libc::SEEK_SET), 0);

    let mut pos = std::mem::MaybeUninit::<fpos_t>::uninit();
    // SAFETY: `pos` provides native position storage, exclusively borrowed here.
    assert_eq!(unsafe { fgetpos_raw(fp, pos.as_mut_ptr()) }, 0);
    assert_eq!(fseeko(fp, 0, libc::SEEK_SET), 0);
    // SAFETY: fgetpos_raw initialized the saved offset in `pos`.
    assert_eq!(unsafe { c_abi::fsetpos(fp, pos.as_ptr()) }, 0);
    assert_eq!(ftello(fp), offset);
    assert_eq!(fclose(fp), 0);
}

#[test]
fn fmemopen_write_roundtrip() {
    let mut buf = [b'#'; 16];
    // SAFETY: `buf` outlives the stream (closed below) and is untouched until then.
    let fp = unsafe { fmemopen_raw(buf.as_mut_ptr() as *mut _, buf.len(), &cstr("w")) };
    assert!(!fp.is_null());
    let cs = cstr("hi");
    fputs(&cs, fp);
    fclose(fp);
    assert_eq!(buf[0], b'h');
    assert_eq!(buf[1], b'i');
    assert_eq!(buf[2], b'\0');
    assert_eq!(buf[3], b'#');
}

#[test]
fn stream_guard_bufread_on_regular_file() {
    use std::io::BufRead;

    let path = std::env::temp_dir().join("xj_cio_streamguard_bufread.txt");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("wb"));
    // SAFETY: the source is a live byte slice of the given length.
    unsafe { fwrite_raw(b"line one\nline two\nline three\n".as_ptr(), 1, 29, fp) };
    fclose(fp);

    let fp = fopen(&p, &cstr("rb"));
    {
        let mut g = stream_lock(fp).expect("valid token");
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let n = g.read_line(&mut line).unwrap();
            if n == 0 {
                break;
            }
            lines.push(line);
        }
        assert_eq!(lines, vec!["line one\n", "line two\n", "line three\n"]);
    }
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn stream_guard_write_then_read() {
    use std::io::{Read, Seek, SeekFrom, Write};

    let path = std::env::temp_dir().join("xj_cio_streamguard_rw.bin");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("w+b"));
    {
        let mut g = stream_lock(fp).unwrap();
        g.write_all(b"abcdef").unwrap();
        g.flush().unwrap();
        g.seek(SeekFrom::Start(0)).unwrap();
        let mut out = Vec::new();
        g.read_to_end(&mut out).unwrap();
        assert_eq!(&out, b"abcdef");
    }
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn stream_guard_pushback_appears_in_fill_buf() {
    use std::io::BufRead;

    let path = std::env::temp_dir().join("xj_cio_streamguard_unget.txt");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("wb"));
    // SAFETY: the source is a live byte slice of the given length.
    unsafe { fwrite_raw(b"world".as_ptr(), 1, 5, fp) };
    fclose(fp);

    let fp = fopen(&p, &cstr("rb"));
    {
        let mut g = stream_lock(fp).unwrap();
        // Push "Xy" — pushback is LIFO, so consumption order is "yX".
        g.unget(b'X').unwrap();
        g.unget(b'y').unwrap();
        let buf = g.fill_buf().unwrap();
        // First two bytes should be the LIFO-ordered pushback,
        // followed by the actual file contents.
        assert_eq!(&buf[..2], b"yX");
        assert!(buf.len() >= 2);
    }
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn stream_guard_invalid_token_sets_ebadf() {
    let bad: FILE_ptr = std::ptr::without_provenance(0x12345678usize);
    assert!(stream_lock(bad).is_none());
    assert_eq!(errno::get(), libc::EBADF);
}

#[test]
fn fclose_while_holding_stream_guard_does_not_panic() {
    use std::io::Write;

    let path = std::env::temp_dir().join("xj_cio_close_while_locked.txt");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("wb"));
    let mut g = stream_lock(fp).unwrap();
    g.write_all(b"hello").unwrap();

    // Same-thread fclose while a StreamGuard is held: must not panic.
    // Returns 0 optimistically; the actual flush happens when the
    // last Arc dies (StreamGuard's Drop).
    let rc = fclose(fp);
    assert_eq!(rc, 0);

    // Subsequent public-API calls on the (now-invalid) token return
    // EBADF without touching state.
    assert_eq!(ferror(fp), 0);
    assert_eq!(fputc(b'x' as i32, fp), EOF);
    assert_eq!(errno::get(), libc::EBADF);

    // Drop the guard. FILE::drop runs; best-effort flush writes
    // the buffered "hello" to disk.
    drop(g);

    let fp2 = fopen(&p, &cstr("rb"));
    assert!(!fp2.is_null());
    let mut buf = [0u8; 16];
    // SAFETY: the destination is a live local array of the given length.
    let n = unsafe { fread_raw(buf.as_mut_ptr(), 1, buf.len(), fp2) };
    assert_eq!(&buf[..n], b"hello");
    fclose(fp2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn print_macros_route_through_xj_cio_streams() {
    // Smoke test: the four macros expand and run without panicking.
    // Byte-level routing through `StreamGuard::write_fmt` is covered
    // by `stream_guard_write_then_read`.
    xj_cio::print!("a");
    xj_cio::print!("b{}", 1);
    xj_cio::println!();
    xj_cio::println!("c");
    xj_cio::println!("d{}", 2);
    xj_cio::eprint!("e");
    xj_cio::eprint!("f{}", 3);
    xj_cio::eprintln!();
    xj_cio::eprintln!("g");
    xj_cio::eprintln!("h{}", 4);
    let _ = fflush(stdout());
    let _ = fflush(stderr());
}

#[test]
fn stream_guard_eof_indicator_after_drain() {
    use std::io::Read;

    let path = std::env::temp_dir().join("xj_cio_streamguard_eof.bin");
    let p = cstr(path.to_str().unwrap());
    let fp = fopen(&p, &cstr("wb"));
    // SAFETY: the source is a live byte slice of the given length.
    unsafe { fwrite_raw(b"ab".as_ptr(), 1, 2, fp) };
    fclose(fp);

    let fp = fopen(&p, &cstr("rb"));
    {
        let mut g = stream_lock(fp).unwrap();
        let mut sink = Vec::new();
        g.read_to_end(&mut sink).unwrap();
        assert_eq!(&sink, b"ab");
        assert!(g.eof());
        assert!(!g.error());
        g.clear_err();
        assert!(!g.eof());
    }
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

/// Opens a fresh temp file holding `contents` for reading.
fn read_fixture(name: &str, contents: &[u8]) -> (FILE_ptr, std::path::PathBuf) {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, contents).unwrap();
    let fp = fopen(&cstr(path.to_str().unwrap()), &cstr("rb"));
    assert!(!fp.is_null());
    (fp, path)
}

#[test]
fn fread_short_read_leaves_unread_elements_untouched() {
    let (fp, path) = read_fixture("xj_cio_fread_short.bin", b"abcdef");
    let mut buf = [b'#'; 10];
    // SAFETY: the destination is a live local array of the given length.
    let n = unsafe { fread_raw(buf.as_mut_ptr(), 2, 5, fp) };
    assert_eq!(n, 3);
    assert_eq!(&buf, b"abcdef####");
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fread_into_uninitialized_buffer() {
    use std::mem::MaybeUninit;
    // Larger than BUFSIZ, so the direct-read path runs.
    let data: Vec<u8> = (0..3 * BUFSIZ + 17).map(|i| (i % 251) as u8).collect();
    for unbuffered in [false, true] {
        let (fp, path) = read_fixture("xj_cio_fread_uninit.bin", &data);
        if unbuffered {
            assert_eq!(setvbuf(fp, std::ptr::null_mut(), _IONBF, 0), 0);
        }
        let mut buf: Vec<MaybeUninit<u8>> = Vec::with_capacity(data.len() + 5);
        // SAFETY: the spare capacity is valid for writes of its length;
        // `fread` accepts uninitialized memory.
        let n = unsafe { fread_raw(buf.as_mut_ptr().cast(), 1, data.len() + 5, fp) };
        assert_eq!(n, data.len());
        // SAFETY: `fread` initialized the first `n` bytes.
        unsafe { buf.set_len(n) };
        // SAFETY: as above.
        let got: Vec<u8> = buf.iter().map(|b| unsafe { b.assume_init() }).collect();
        assert_eq!(got, data);
        assert_eq!(feof(fp), 1);
        fclose(fp);
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn fgets_leaves_bytes_after_the_terminator_untouched() {
    let (fp, path) = read_fixture("xj_cio_fgets_tail.txt", b"ab\ncd");
    let mut buf = [b'#'; 8];
    // SAFETY: the destination is a live local array of length `n`.
    let r = unsafe { fgets_raw(buf.as_mut_ptr(), buf.len() as libc::c_int, fp) };
    assert_eq!(r, buf.as_mut_ptr());
    assert_eq!(buf, *b"ab\n\0####");
    fclose(fp);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn tmpnam_leaves_bytes_after_the_name_untouched() {
    let mut buf = [b'#'; L_tmpnam];
    // SAFETY: the destination is a live local array of `L_tmpnam` bytes.
    let r = unsafe { tmpnam_raw(buf.as_mut_ptr()) };
    assert_eq!(r, buf.as_mut_ptr());
    let len = buf.iter().position(|&b| b == 0).unwrap();
    assert!(buf[len + 1..].iter().all(|&b| b == b'#'));
}
