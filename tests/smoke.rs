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
    let n = unsafe { fwrite(data.as_ptr(), 1, data.len(), fp) };
    assert_eq!(n, data.len());
    assert_eq!(fclose(fp), 0);

    let fp = fopen(&p, &cstr("rb"));
    assert!(!fp.is_null());
    let mut buf = [0u8; 32];
    // SAFETY: the destination is a live, initialized local array of the given length.
    let n = unsafe { fread(buf.as_mut_ptr(), 1, buf.len(), fp) };
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
    unsafe { fwrite(b"ABC".as_ptr(), 1, 3, fp) };
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
    use xj_cio::Arg;
    let path = std::env::temp_dir().join("xj_cio_printf.txt");
    let p = cstr(path.to_str().unwrap());

    let fp = fopen(&p, &cstr("w"));
    let s = cstr("world");
    // SAFETY: each `Arg::S` points to a live NUL-terminated string.
    let r = unsafe { fprintf(
        fp,
        &cstr("%d %s\n"),
        &[Arg::I(42), Arg::S(s.as_ptr())],
    ) };
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
    let r = unsafe { fscanf(
        fp,
        &cstr("%d %d"),
        &mut [OutArg::Int(&mut a as *mut _), OutArg::Int(&mut b as *mut _)],
    ) };
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
fn fscanf_unsigned_signs_match_libc() {
    let input = cstr("-1 -128 +42 -ff -10 -18446744073709551615");
    let format = cstr("%u %u %u %x %o %llu");
    let mut expected = [0 as libc::c_uint; 5];
    let mut expected_wide: libc::c_ulonglong = 0;
    // SAFETY: both strings are live `CString`s, and each conversion's
    // destination is a live local of the type its specifier expects.
    let matched = unsafe {
        libc::sscanf(
            input.as_ptr(),
            format.as_ptr(),
            &mut expected[0] as *mut _,
            &mut expected[1] as *mut _,
            &mut expected[2] as *mut _,
            &mut expected[3] as *mut _,
            &mut expected[4] as *mut _,
            &mut expected_wide as *mut _,
        )
    };
    assert_eq!(matched, 6);
    let stream = tmpfile();
    assert!(!stream.is_null());
    assert_eq!(
        // SAFETY: the source is a live byte slice of the given length.
        unsafe { fwrite(input.as_ptr().cast(), 1, input.as_bytes().len(), stream) },
        input.as_bytes().len()
    );
    rewind(stream);
    let mut actual = [0 as libc::c_uint; 5];
    let mut actual_wide: libc::c_ulonglong = 0;
    let [a, b, c, d, e] = &mut actual;
    assert_eq!(
        // SAFETY: each raw `OutArg` points to a live local large enough for its conversion.
        unsafe { fscanf(
            stream,
            &format,
            &mut [
                OutArg::UInt(a),
                OutArg::UInt(b),
                OutArg::UInt(c),
                OutArg::UInt(d),
                OutArg::UInt(e),
                OutArg::ULongLong(&mut actual_wide),
            ]
        ) },
        matched
    );
    assert_eq!(actual, expected);
    assert_eq!(actual_wide, expected_wide);
    assert_eq!(fclose(stream), 0);
}

#[test]
fn fscanf_scanset_ranges_match_libc() {
    for (input, format) in [
        ("UA129089 TP579!", "%8[A-Z0-9]"),
        ("abc123!", "%[a-z0-9]"),
        ("ABC-!", "%[A-Z-]"),
        ("-+AZ!", "%[-+A-Z]"),
        ("]ABC!", "%[]A-C]"),
        ("AZ-!", "%[Z-A]"),
        ("ABCDE!", "%3[A-Z]"),
        ("123Abc!", "%*[0-9]%[A-Za-z]"),
        ("comments 42!", "%[^A-Z0-9]"),
        ("\nremaining", "%80[^\n]"),
        ("lowercase", "%[A-Z]"),
        ("", "%[A-Z]"),
    ] {
        let input = cstr(input);
        let native_format = cstr(&format!("{format}%n"));
        let format = cstr(format);
        let mut expected = [0u8; 128];
        let mut consumed: libc::c_int = 0;
        // SAFETY: both strings are live `CString`s, `expected` holds any
        // of the short test fields plus its NUL, and `%n` gets a `c_int`.
        let matched = unsafe {
            libc::sscanf(
                input.as_ptr(),
                native_format.as_ptr(),
                expected.as_mut_ptr().cast::<libc::c_char>(),
                &mut consumed as *mut _,
            )
        };
        let stream = tmpfile();
        assert!(!stream.is_null());
        assert_eq!(
            // SAFETY: the source is a live byte slice of the given length.
            unsafe { fwrite(input.as_ptr().cast(), 1, input.as_bytes().len(), stream) },
            input.as_bytes().len()
        );
        rewind(stream);
        let mut actual = [0u8; 128];
        assert_eq!(
            // SAFETY: each raw `OutArg` points to a live local large enough for its conversion.
            unsafe { fscanf(
                stream,
                &format,
                &mut [OutArg::Str(actual.as_mut_ptr().cast())]
            ) },
            matched
        );
        assert_eq!(actual, expected, "input {input:?}, format {format:?}");
        assert_eq!(
            fgetc(stream),
            input
                .as_bytes()
                .get(consumed as usize)
                .map_or(EOF, |b| *b as libc::c_int)
        );
        assert_eq!(fclose(stream), 0);
    }
}

#[test]
fn fscanf_numeric_matching_failure_matches_libc() {
    for (input, format) in [
        ("flight, but to go to Porto.", "%d"),
        ("", "%d"),
        ("-", "%d"),
        ("+rest", "%d"),
        ("12 flight,", "%d %d"),
        ("12", "%d %d"),
    ] {
        let input = cstr(input);
        let format = cstr(format);
        let mut expected = [37 as libc::c_int, 43];
        // SAFETY: the stream is checked non-null and closed before the
        // block ends; the buffers are live `CString`s; each `%d` gets a
        // live `c_int`.
        let (matched, next) = unsafe {
            let stream = libc::tmpfile();
            assert!(!stream.is_null());
            assert_eq!(
                libc::fwrite(input.as_ptr().cast(), 1, input.as_bytes().len(), stream),
                input.as_bytes().len()
            );
            libc::rewind(stream);
            let matched = libc::fscanf(
                stream,
                format.as_ptr(),
                &mut expected[0] as *mut _,
                &mut expected[1] as *mut _,
            );
            let next = libc::fgetc(stream);
            assert_eq!(libc::fclose(stream), 0);
            (matched, next)
        };
        let stream = tmpfile();
        assert!(!stream.is_null());
        assert_eq!(
            // SAFETY: the source is a live byte slice of the given length.
            unsafe { fwrite(input.as_ptr().cast(), 1, input.as_bytes().len(), stream) },
            input.as_bytes().len()
        );
        rewind(stream);
        let mut actual = [37 as libc::c_int, 43];
        let [a, b] = &mut actual;
        assert_eq!(
            // SAFETY: each raw `OutArg` points to a live local large enough for its conversion.
            unsafe { fscanf(stream, &format, &mut [OutArg::Int(a), OutArg::Int(b)]) },
            matched,
            "input {input:?}"
        );
        assert_eq!(actual, expected, "input {input:?}");
        assert_eq!(fgetc(stream), next, "input {input:?}");
        assert_eq!(fclose(stream), 0);
    }
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
    unsafe { fwrite(b"0123456789".as_ptr(), 1, 10, fp) };
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
fn fmemopen_write_roundtrip() {
    let mut buf = [b'#'; 16];
    // SAFETY: `buf` outlives the stream (closed below) and is untouched until then.
    let fp = unsafe { fmemopen(buf.as_mut_ptr() as *mut _, buf.len(), &cstr("w")) };
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
    unsafe { fwrite(b"line one\nline two\nline three\n".as_ptr(), 1, 29, fp) };
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
    unsafe { fwrite(b"world".as_ptr(), 1, 5, fp) };
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
    // SAFETY: the destination is a live, initialized local array of the given length.
    let n = unsafe { fread(buf.as_mut_ptr(), 1, buf.len(), fp2) };
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
    unsafe { fwrite(b"ab".as_ptr(), 1, 2, fp) };
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
