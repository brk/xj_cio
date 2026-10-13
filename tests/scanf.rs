//! The scanf family on xj_cio streams: the shared conformance table against
//! libc through the stream adapter, plus stream-specific behavior.

use std::ffi::{CString, c_void};

use xj_cio::*;
use xj_scanf::xj_scanf_core::{Format, IntKind, Requirement};

#[path = "../../../xj_scanf/tests/cases/mod.rs"]
mod cases;

/// A stream holding `contents`, positioned at its start.
fn stream_with(contents: &[u8]) -> FILE_ptr {
    let stream = tmpfile();
    assert!(!stream.is_null());
    // SAFETY: the source is a live byte slice of the given length.
    assert_eq!(
        unsafe { fwrite_raw(contents.as_ptr(), 1, contents.len(), stream) },
        contents.len()
    );
    rewind(stream);
    stream
}

/// Raw storage for one destination, prefilled with a sentinel.
#[repr(C, align(8))]
struct Slot {
    req: Requirement,
    bytes: [u8; 32],
}

impl Slot {
    fn new(req: Requirement) -> Self {
        Slot {
            req,
            bytes: [0xa5; 32],
        }
    }

    fn arg(&mut self) -> OutArg<'static> {
        let p = self.bytes.as_mut_ptr();
        match self.req {
            Requirement::Int { kind, signed } => int_arg(p, kind, signed),
            Requirement::Count(kind) => int_arg(p, kind, true),
            Requirement::Float => OutArg::Float(p.cast()),
            Requirement::Double => OutArg::Double(p.cast()),
            Requirement::Str | Requirement::Chars(_) => OutArg::Str(p),
        }
    }
}

fn int_arg(p: *mut u8, kind: IntKind, signed: bool) -> OutArg<'static> {
    match (kind, signed) {
        (IntKind::Char, true) => OutArg::SChar(p.cast()),
        (IntKind::Char, false) => OutArg::UChar(p),
        (IntKind::Short, true) => OutArg::Short(p.cast()),
        (IntKind::Short, false) => OutArg::UShort(p.cast()),
        (IntKind::Int, true) => OutArg::Int(p.cast()),
        (IntKind::Int, false) => OutArg::UInt(p.cast()),
        (IntKind::Long, true) => OutArg::Long(p.cast()),
        (IntKind::Long, false) => OutArg::ULong(p.cast()),
        (IntKind::LongLong | IntKind::IntMax, true) => OutArg::LongLong(p.cast()),
        (IntKind::LongLong | IntKind::IntMax, false) => OutArg::ULongLong(p.cast()),
        (IntKind::Size, _) => OutArg::Size(p.cast()),
        (IntKind::PtrDiff, _) => OutArg::ISize(p.cast()),
    }
}

/// Return value, destination bytes, next byte, EOF and error indicators.
type Observed = (i32, Vec<[u8; 32]>, i32, i32, i32);

fn slots(format: &str) -> Vec<Slot> {
    Format::requirements(format.as_bytes())
        .map(|r| Slot::new(r.unwrap()))
        .collect()
}

fn ours(input: &[u8], format: &str) -> Observed {
    let mut slots = slots(format);
    let mut args: Vec<OutArg<'_>> = slots.iter_mut().map(Slot::arg).collect();
    let stream = stream_with(input);
    // SAFETY: each pointer addresses a live, aligned 32-byte slot, enough
    // for any destination these short cases store.
    let r = unsafe { fscanf_raw(stream, format, &mut args) };
    let observed = (r, fgetc(stream), feof(stream), ferror(stream));
    assert_eq!(fclose(stream), 0);
    drop(args);
    (
        observed.0,
        slots.iter().map(|s| s.bytes).collect(),
        observed.1,
        observed.2,
        observed.3,
    )
}

fn libc_(input: &[u8], format: &str) -> Observed {
    let mut slots = slots(format);
    let fmt = CString::new(format).unwrap();
    // SAFETY: the stream is checked and closed here; each pointer addresses
    // a live, aligned 32-byte slot of the type its directive requires.
    unsafe {
        let f = libc::tmpfile();
        assert!(!f.is_null());
        assert_eq!(
            libc::fwrite(input.as_ptr().cast(), 1, input.len(), f),
            input.len()
        );
        libc::rewind(f);
        let p: Vec<*mut c_void> = slots
            .iter_mut()
            .map(|s| s.bytes.as_mut_ptr().cast())
            .collect();
        let fmt = fmt.as_ptr();
        let r = match p[..] {
            [] => libc::fscanf(f, fmt),
            [a] => libc::fscanf(f, fmt, a),
            [a, b] => libc::fscanf(f, fmt, a, b),
            [a, b, c] => libc::fscanf(f, fmt, a, b, c),
            [a, b, c, d] => libc::fscanf(f, fmt, a, b, c, d),
            _ => panic!("too many destinations"),
        };
        let observed = (r, libc::fgetc(f), libc::feof(f), libc::ferror(f));
        libc::fclose(f);
        (
            observed.0,
            slots.iter().map(|s| s.bytes).collect(),
            observed.1,
            observed.2,
            observed.3,
        )
    }
}

#[test]
fn shared_cases_match_libc_through_the_stream_adapter() {
    let failures: Vec<String> = cases::CASES
        .iter()
        .filter_map(|&(input, format)| {
            let (a, b) = (ours(input, format), libc_(input, format));
            (a != b).then(|| {
                format!(
                    "{:?} {format:?}:\n  ours {a:?}\n  libc {b:?}",
                    String::from_utf8_lossy(input)
                )
            })
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn recorded_libc_differences_hold_for_streams() {
    for &(input, format) in cases::LIBC_DIFFERS {
        assert_ne!(
            ours(input, format),
            libc_(input, format),
            "{input:?} {format:?}"
        );
    }
    // The standard's results.
    assert_eq!(ours(b"1", "%*d%d").0, 0);
    let (r, _, next, ..) = ours(b"100er", "%f");
    assert_eq!((r, next), (0, b'r' as i32));
    let (r, _, next, ..) = ours(b"0xg", "%x");
    assert_eq!((r, next), (0, b'g' as i32));
}

#[test]
fn safe_destinations() {
    let stream = stream_with(b"12 -3 4.5 word bytes rest");
    let (mut a, mut b, mut x) = (0i32, 0u64, 0f64);
    let mut word = [0u8; 8];
    let mut bytes = Vec::new();
    let r = fscanf(
        stream,
        "%d %lld %lf %7s %s",
        &mut [&mut a, &mut b, &mut x, &mut word, &mut bytes],
    );
    assert_eq!(r, 5);
    assert_eq!((a, b, x), (12, u64::MAX - 2, 4.5));
    assert_eq!(&word, b"word\0\0\0\0");
    assert_eq!(bytes, b"bytes");
    assert_eq!(fgetc(stream), b' ' as i32);
    assert_eq!(fclose(stream), 0);
}

#[test]
fn safe_type_mismatch_stops_before_reading() {
    let stream = stream_with(b"  12");
    let mut short = 0i16;
    errno::set(0);
    assert_eq!(fscanf(stream, "%d", &mut [&mut short]), 0);
    assert_eq!(errno::get(), libc::EINVAL);
    assert_eq!(fgetc(stream), b' ' as i32);
    assert_eq!(fclose(stream), 0);
}

#[test]
fn raw_type_mismatch_stops_before_reading() {
    let stream = stream_with(b"12 34");
    let mut short: libc::c_short = 7;
    let mut int: libc::c_int = 0;
    errno::set(0);
    // SAFETY: both pointers address live locals of their variants' types.
    let r = unsafe {
        fscanf_raw(
            stream,
            "%d %d",
            &mut [OutArg::Int(&mut int), OutArg::Short(&mut short)],
        )
    };
    assert_eq!((r, int, short, errno::get()), (1, 12, 7, libc::EINVAL));
    assert_eq!(fgetc(stream), b'3' as i32);
    assert_eq!(fclose(stream), 0);
}

#[test]
fn raw_variants_for_hh_unsigned_short_and_pointer_sizes() {
    let stream = stream_with(b"-7 200 65535 9 -9 3");
    let (mut sc, mut uc): (libc::c_schar, libc::c_uchar) = (0, 0);
    let mut us: libc::c_ushort = 0;
    let (mut z, mut t) = (0usize, 0isize);
    let mut count = 0usize;
    // SAFETY: each pointer addresses a live local of its variant's type.
    let r = unsafe {
        fscanf_raw(
            stream,
            "%hhd %hhu %hu %zu %td%zn",
            &mut [
                OutArg::SChar(&mut sc),
                OutArg::UChar(&mut uc),
                OutArg::UShort(&mut us),
                OutArg::Size(&mut z),
                OutArg::ISize(&mut t),
                OutArg::Size(&mut count),
            ],
        )
    };
    assert_eq!((r, sc, uc, us, z, t, count), (5, -7, 200, 65535, 9, -9, 17));
    assert_eq!(fclose(stream), 0);
}

#[test]
fn raw_str_receives_hh_integers_as_one_byte() {
    let mut buf = [0xa5u8; 4];
    // SAFETY: each pointer addresses a live local with room for one byte.
    let r = unsafe {
        sscanf_raw(
            "-1 77",
            "%hhd %hhu%hhn",
            &mut [
                OutArg::Str(buf.as_mut_ptr()),
                OutArg::Str(buf.as_mut_ptr().add(1)),
                OutArg::Str(buf.as_mut_ptr().add(2)),
            ],
        )
    };
    assert_eq!((r, buf), (2, [0xff, 77, 5, 0xa5]));
}

#[test]
fn sscanf_raw_stops_at_the_first_nul() {
    let (mut a, mut b): (libc::c_int, libc::c_int) = (0, 0);
    // SAFETY: both pointers address live `c_int` locals.
    let r = unsafe {
        sscanf_raw(
            b"1\x002",
            "%d%d",
            &mut [OutArg::Int(&mut a), OutArg::Int(&mut b)],
        )
    };
    assert_eq!((r, a, b), (1, 1, 0));
    let mut word = [0u8; 4];
    errno::set(0);
    // SAFETY: `Buf` is checked.
    let r = unsafe { sscanf_raw("abcdef", "%s", &mut [OutArg::Buf((&mut word).into())]) };
    assert_eq!((r, &word, errno::get()), (0, b"abc\0", libc::EOVERFLOW));
}

#[test]
fn invalid_formats_set_einval_before_reading() {
    let stream = stream_with(b"12");
    let mut x = 0i32;
    for format in ["%p", "%Lf", "%ls", "%y", "%d %"] {
        errno::set(0);
        assert_eq!(fscanf(stream, format, &mut [&mut x]), 0, "{format}");
        assert_eq!(errno::get(), libc::EINVAL, "{format}");
    }
    assert_eq!(fgetc(stream), b'1' as i32);
    assert_eq!(fclose(stream), 0);
}

#[test]
fn eof_is_sticky_for_scanning() {
    let path = std::env::temp_dir().join(format!("xj_cio_scanf_sticky_{}", std::process::id()));
    std::fs::write(&path, b"1").unwrap();
    let c_path = CString::new(path.to_str().unwrap()).unwrap();
    let stream = fopen(&c_path, "r");
    assert!(!stream.is_null());
    let mut x = 0i32;
    // Ending the number peeks at EOF, which sets the indicator.
    assert_eq!(fscanf(stream, "%d", &mut [&mut x]), 1);
    assert_eq!(feof(stream), 1);
    // More data arrives, but the set indicator ends input without reading.
    std::fs::write(&path, b"1 2 3").unwrap();
    assert_eq!(fscanf(stream, " %d", &mut [&mut x]), -1);
    assert_eq!(fscanf(stream, " x", &mut []), -1);
    assert_eq!(feof(stream), 1, "whitespace and literals do not clear EOF");
    clearerr(stream);
    assert_eq!(fscanf(stream, "%d", &mut [&mut x]), 1);
    assert_eq!(x, 2);
    assert_eq!(fclose(stream), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn pushback_is_scanned_first_and_kept_on_mismatch() {
    let stream = stream_with(b"3 rest");
    assert_eq!(ungetc(b'2' as i32, stream), b'2' as i32);
    assert_eq!(ungetc(b'1' as i32, stream), b'1' as i32);
    let mut x = 0i32;
    assert_eq!(fscanf(stream, "%d", &mut [&mut x]), 1);
    assert_eq!(x, 123);
    assert_eq!(ungetc(b'z' as i32, stream), b'z' as i32);
    assert_eq!(fscanf(stream, "%d", &mut [&mut x]), 0);
    assert_eq!(fgetc(stream), b'z' as i32);
    assert_eq!(fclose(stream), 0);
}

#[test]
fn read_errors_set_the_error_indicator() {
    let path = std::env::temp_dir().join(format!("xj_cio_scanf_err_{}", std::process::id()));
    std::fs::write(&path, b"12").unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    // A write-only descriptor opened as a read stream fails every read.
    use std::os::fd::IntoRawFd;
    // SAFETY: the descriptor is open and owned by the new stream.
    let stream = unsafe { fdopen(file.into_raw_fd(), "r") };
    assert!(!stream.is_null());
    let mut x = 0i32;
    errno::set(0);
    assert_eq!(fscanf(stream, "%d", &mut [&mut x]), -1);
    assert_eq!(
        (ferror(stream), feof(stream), errno::get()),
        (1, 0, libc::EBADF)
    );
    assert_eq!(fclose(stream), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn repeated_calls_see_consistent_data() {
    let stream = stream_with(b"1 2 3");
    let mut v = [0i32; 3];
    for slot in &mut v {
        assert_eq!(fscanf(stream, "%d", &mut [slot]), 1);
    }
    assert_eq!(v, [1, 2, 3]);
    assert_eq!(fscanf(stream, "%d", &mut [&mut v[0]]), -1);
    assert_eq!(fclose(stream), 0);
}
