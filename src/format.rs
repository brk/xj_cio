//! `printf` / `scanf` runtime parsers.
//!
//! Produces and consumes byte sequences against a slice of `Arg` /
//! `OutArg`. The translator marshals each C variadic call into one
//! of these slices.

use crate::errno;
use crate::state::State;
use crate::api::ops;

/// One input argument to `printf` family. The translator picks the
/// variant matching the matching format spec.
///
/// # Pointer validity
///
/// The `printf` family dereferences two of the pointer variants. Its
/// (unsafe) callers must ensure, for each argument that a conversion
/// in the format consumes:
///
/// - `S(p)` consumed by `%s`: `p` is null (printed as `(null)`) or
///   valid for reads up to and including its first NUL byte, or of
///   `precision` bytes if the conversion has a precision and no NUL
///   comes first. Those bytes are not written during the call.
/// - `OutN(p)` consumed by `%n`: `p` is null (ignored) or valid for an
///   aligned write of a `c_int`, and not accessed through any other
///   pointer or reference during the call.
///
/// `P`, and `S` consumed by any other conversion, are never
/// dereferenced.
#[derive(Clone, Copy)]
pub enum Arg {
    I(i64),
    U(u64),
    F(f64),
    S(*const libc::c_char),
    C(libc::c_int),
    P(*const libc::c_void),
    OutN(*mut libc::c_int),
}

// SAFETY: `Arg` is plain data. Its pointers are dereferenced only by
// the unsafe formatting functions, whose callers vouch for the pointees
// (including freedom from data races) per "Pointer validity" above.
// Moving or sharing an `Arg` by itself accesses nothing.
unsafe impl Send for Arg {}
// SAFETY: as for `Send`.
unsafe impl Sync for Arg {}

/// One output destination for `scanf` family.
///
/// # Pointer validity
///
/// The `scanf` family writes through the raw-pointer variants. Its
/// (unsafe) callers must ensure, for each argument that a
/// non-suppressed conversion stores into:
///
/// - `Int`, `Short`, `Long`, `LongLong`, `UInt`, `ULong`, `ULongLong`,
///   `Float`, `Double`: the pointer is null (the store is skipped) or
///   valid for an aligned write of its pointee type.
/// - `Str(p)`: `p` is non-null and valid for writes of the bytes
///   stored. For `%s` and `%[`, that is the matched field plus a NUL
///   (at most width + 1 bytes when the conversion has a width). For
///   `%c`, it is exactly the width (default 1).
///
/// In every case the destination must not be accessed through any
/// other pointer or reference, including the format string, during
/// the call.
pub enum OutArg<'a> {
    Suppress,
    Int(*mut libc::c_int),
    Short(*mut libc::c_short),
    Long(*mut libc::c_long),
    LongLong(*mut libc::c_longlong),
    UInt(*mut libc::c_uint),
    ULong(*mut libc::c_ulong),
    ULongLong(*mut libc::c_ulonglong),
    Float(*mut f32),
    Double(*mut f64),
    /// `%s`, `%c`, `%[...]`: write bytes into a char buffer.
    /// The translator must size the buffer; this is *not* checked.
    Str(*mut libc::c_char),
    /// `%s`, `%c`, `%[...]`: write bytes into a sized destination, such
    /// as `(&mut buf).into()`, a `ZStr` as `z.into()`, or `z.limit(n)`.
    ///
    /// Where C would overflow the buffer, the conversion fails instead:
    /// `%s` and `%[` store the first `max_len()` bytes, terminated,
    /// leave the rest of the field unread, and stop. `%Nc`, which
    /// stores N bytes and no terminator, needs `region_len() >= N`;
    /// otherwise it stops before reading. Either way the call returns
    /// the assignments made before the failed conversion and sets
    /// `errno` to `EOVERFLOW`.
    Buf(xj_zstr::CStrDest<'a>),
}

// SAFETY: as for `Arg`: the raw pointers are written only by the
// unsafe `scanf` functions, under their callers' guarantees. `Buf` holds
// a `CStrDest` (a `&mut [u8]` and a flag), which is `Send + Sync` anyway.
unsafe impl Send for OutArg<'_> {}
// SAFETY: as for `Send`.
unsafe impl Sync for OutArg<'_> {}

// --------------------------------------------------------------------------
// printf engine
// --------------------------------------------------------------------------

/// Formats `fmt` with `args` into `out`.
///
/// # Safety
///
/// `args` must meet the requirements in [`Arg`]'s "Pointer validity"
/// section for the conversions in `fmt`.
pub unsafe fn format_to_vec(out: &mut Vec<u8>, fmt: &[u8], args: &[Arg]) {
    let mut i = 0;
    let mut a = 0;
    while i < fmt.len() {
        let b = fmt[i];
        if b != b'%' {
            out.push(b);
            i += 1;
            continue;
        }
        i += 1;
        // Flags
        let mut f_minus = false;
        let mut f_plus = false;
        let mut f_space = false;
        let mut f_hash = false;
        let mut f_zero = false;
        loop {
            match fmt.get(i).copied() {
                Some(b'-') => { f_minus = true; i += 1; }
                Some(b'+') => { f_plus = true; i += 1; }
                Some(b' ') => { f_space = true; i += 1; }
                Some(b'#') => { f_hash = true; i += 1; }
                Some(b'0') => { f_zero = true; i += 1; }
                _ => break,
            }
        }
        // Width
        let mut width: Option<usize> = None;
        if fmt.get(i).copied() == Some(b'*') {
            i += 1;
            let v = take_int(args, &mut a);
            if v < 0 {
                f_minus = true;
                width = Some((-v) as usize);
            } else {
                width = Some(v as usize);
            }
        } else {
            let mut have = false;
            let mut w = 0usize;
            while let Some(c) = fmt.get(i).copied() {
                if c.is_ascii_digit() { w = w * 10 + (c - b'0') as usize; i += 1; have = true; }
                else { break; }
            }
            if have { width = Some(w); }
        }
        // Precision
        let mut precision: Option<usize> = None;
        if fmt.get(i).copied() == Some(b'.') {
            i += 1;
            if fmt.get(i).copied() == Some(b'*') {
                i += 1;
                let v = take_int(args, &mut a);
                if v >= 0 { precision = Some(v as usize); }
            } else {
                let mut p = 0usize;
                let mut have = false;
                while let Some(c) = fmt.get(i).copied() {
                    if c.is_ascii_digit() { p = p * 10 + (c - b'0') as usize; i += 1; have = true; }
                    else { break; }
                }
                precision = Some(if have { p } else { 0 });
            }
        }
        // Length modifiers — consumed and ignored (Arg variants
        // already encode the actual value).
        loop {
            match fmt.get(i).copied() {
                Some(b'h') | Some(b'l') | Some(b'L') | Some(b'z') | Some(b'j') | Some(b't') => i += 1,
                _ => break,
            }
        }
        let conv = match fmt.get(i).copied() {
            Some(c) => c,
            None => return,
        };
        i += 1;
        match conv {
            b'%' => out.push(b'%'),
            b'd' | b'i' => {
                let v = take_int(args, &mut a);
                fmt_signed(out, v, width, precision, f_minus, f_plus, f_space, f_zero);
            }
            b'u' => {
                let v = take_uint(args, &mut a);
                fmt_unsigned(out, v, width, precision, f_minus, f_zero, 10, false, false);
            }
            b'x' => {
                let v = take_uint(args, &mut a);
                fmt_unsigned(out, v, width, precision, f_minus, f_zero, 16, false, f_hash);
            }
            b'X' => {
                let v = take_uint(args, &mut a);
                fmt_unsigned(out, v, width, precision, f_minus, f_zero, 16, true, f_hash);
            }
            b'o' => {
                let v = take_uint(args, &mut a);
                fmt_octal(out, v, width, precision, f_minus, f_zero, f_hash);
            }
            b's' => {
                let p = take_str(args, &mut a);
                // SAFETY: `p` is null or an `Arg::S` consumed by `%s`
                // with this `precision`, which the caller vouches for.
                unsafe { fmt_str(out, p, width, precision, f_minus) };
            }
            b'c' => {
                let v = take_char(args, &mut a);
                fmt_char(out, v as u8, width, f_minus);
            }
            b'f' | b'F' => {
                let v = take_float(args, &mut a);
                fmt_float_f(out, v, width, precision, f_minus, f_plus, f_space, f_zero, conv == b'F');
            }
            b'e' | b'E' => {
                let v = take_float(args, &mut a);
                fmt_float_e(out, v, width, precision, f_minus, f_plus, f_space, f_zero, conv == b'E');
            }
            b'g' | b'G' => {
                let v = take_float(args, &mut a);
                fmt_float_g(out, v, width, precision, f_minus, f_plus, f_space, f_zero, f_hash, conv == b'G');
            }
            b'p' => {
                let p = take_ptr(args, &mut a);
                let s = format!("0x{:x}", p as usize);
                pad_and_emit(out, s.as_bytes(), width, f_minus, false);
            }
            b'n' => {
                let p = take_outn(args, &mut a);
                if !p.is_null() {
                    // SAFETY: a non-null `Arg::OutN` consumed by `%n`,
                    // which the caller guarantees is valid for an
                    // aligned, unaliased `c_int` write.
                    unsafe { *p = out.len() as libc::c_int };
                }
            }
            _ => {
                // Unknown spec: emit literally.
                out.push(b'%');
                out.push(conv);
            }
        }
    }
}

fn take_int(args: &[Arg], a: &mut usize) -> i64 {
    let v = match args.get(*a).copied() {
        Some(Arg::I(v)) => v,
        Some(Arg::U(v)) => v as i64,
        Some(Arg::C(c)) => c as i64,
        _ => 0,
    };
    *a += 1;
    v
}
fn take_uint(args: &[Arg], a: &mut usize) -> u64 {
    let v = match args.get(*a).copied() {
        Some(Arg::U(v)) => v,
        Some(Arg::I(v)) => v as u64,
        Some(Arg::C(c)) => c as u64,
        _ => 0,
    };
    *a += 1;
    v
}
fn take_str(args: &[Arg], a: &mut usize) -> *const libc::c_char {
    let v = match args.get(*a).copied() {
        Some(Arg::S(p)) => p,
        _ => std::ptr::null(),
    };
    *a += 1;
    v
}
fn take_char(args: &[Arg], a: &mut usize) -> core::ffi::c_int {
    let v = match args.get(*a).copied() {
        Some(Arg::C(c)) => c,
        Some(Arg::I(v)) => v as libc::c_int,
        Some(Arg::U(v)) => v as libc::c_int,
        _ => 0,
    };
    *a += 1;
    v
}
fn take_float(args: &[Arg], a: &mut usize) -> f64 {
    let v = match args.get(*a).copied() {
        Some(Arg::F(v)) => v,
        Some(Arg::I(v)) => v as f64,
        Some(Arg::U(v)) => v as f64,
        _ => 0.0,
    };
    *a += 1;
    v
}
fn take_ptr(args: &[Arg], a: &mut usize) -> *const libc::c_void {
    let v = match args.get(*a).copied() {
        Some(Arg::P(p)) => p,
        Some(Arg::S(p)) => p as *const libc::c_void,
        _ => std::ptr::null(),
    };
    *a += 1;
    v
}
fn take_outn(args: &[Arg], a: &mut usize) -> *mut libc::c_int {
    let v = match args.get(*a).copied() {
        Some(Arg::OutN(p)) => p,
        _ => std::ptr::null_mut(),
    };
    *a += 1;
    v
}

// ------------- integer formatters -------------

fn fmt_signed(
    out: &mut Vec<u8>,
    v: i64,
    width: Option<usize>,
    precision: Option<usize>,
    f_minus: bool,
    f_plus: bool,
    f_space: bool,
    f_zero: bool,
) {
    let neg = v < 0;
    let abs = if neg { (v as i128).unsigned_abs() as u64 } else { v as u64 };
    let mut digits = u64_to_decimal(abs);
    if let Some(p) = precision {
        while digits.len() < p {
            digits.insert(0, b'0');
        }
        if v == 0 && p == 0 {
            digits.clear();
        }
    }
    let sign: &[u8] = if neg { b"-" } else if f_plus { b"+" } else if f_space { b" " } else { b"" };

    let total = sign.len() + digits.len();
    let pad = width.map(|w| w.saturating_sub(total)).unwrap_or(0);

    if !f_minus && !f_zero && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
    out.extend_from_slice(sign);
    if !f_minus && f_zero && precision.is_none() && pad > 0 {
        for _ in 0..pad { out.push(b'0'); }
    }
    out.extend_from_slice(&digits);
    if f_minus && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
}

fn fmt_unsigned(
    out: &mut Vec<u8>,
    v: u64,
    width: Option<usize>,
    precision: Option<usize>,
    f_minus: bool,
    f_zero: bool,
    base: u32,
    upper: bool,
    hash: bool,
) {
    let mut digits = u64_to_base(v, base, upper);
    if let Some(p) = precision {
        while digits.len() < p {
            digits.insert(0, b'0');
        }
        if v == 0 && p == 0 {
            digits.clear();
        }
    }
    // Hash for hex: prepend "0x" (or "0X") only if value != 0.
    let prefix: &[u8] = if hash && base == 16 && v != 0 {
        if upper { b"0X" } else { b"0x" }
    } else {
        b""
    };
    let total = prefix.len() + digits.len();
    let pad = width.map(|w| w.saturating_sub(total)).unwrap_or(0);

    if !f_minus && !f_zero && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
    out.extend_from_slice(prefix);
    if !f_minus && f_zero && precision.is_none() && pad > 0 {
        for _ in 0..pad { out.push(b'0'); }
    }
    out.extend_from_slice(&digits);
    if f_minus && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
}

fn fmt_octal(
    out: &mut Vec<u8>,
    v: u64,
    width: Option<usize>,
    precision: Option<usize>,
    f_minus: bool,
    f_zero: bool,
    hash: bool,
) {
    let mut digits = u64_to_base(v, 8, false);
    if let Some(p) = precision {
        while digits.len() < p { digits.insert(0, b'0'); }
        if v == 0 && p == 0 { digits.clear(); }
    }
    if hash {
        // ensure leading 0
        if digits.is_empty() || digits[0] != b'0' {
            digits.insert(0, b'0');
        }
    }
    let total = digits.len();
    let pad = width.map(|w| w.saturating_sub(total)).unwrap_or(0);

    if !f_minus && !f_zero && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
    if !f_minus && f_zero && precision.is_none() && pad > 0 {
        for _ in 0..pad { out.push(b'0'); }
    }
    out.extend_from_slice(&digits);
    if f_minus && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
}

fn u64_to_decimal(mut v: u64) -> Vec<u8> {
    if v == 0 {
        return vec![b'0'];
    }
    let mut out = Vec::with_capacity(20);
    while v > 0 {
        out.push(b'0' + (v % 10) as u8);
        v /= 10;
    }
    out.reverse();
    out
}

fn u64_to_base(mut v: u64, base: u32, upper: bool) -> Vec<u8> {
    if v == 0 {
        return vec![b'0'];
    }
    let lookup = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut out = Vec::new();
    while v > 0 {
        out.push(lookup[(v % base as u64) as usize]);
        v /= base as u64;
    }
    out.reverse();
    out
}

// ------------- string formatter -------------

/// # Safety
///
/// `p` must be null, or valid for reads up to and including its first
/// NUL, or of `precision` bytes if `precision` is `Some` and no NUL
/// comes first. Those bytes must not be written during the call.
unsafe fn fmt_str(
    out: &mut Vec<u8>,
    p: *const libc::c_char,
    width: Option<usize>,
    precision: Option<usize>,
    f_minus: bool,
) {
    let bytes: &[u8] = if p.is_null() {
        b"(null)"
    } else {
        let mut len = 0usize;
        // Walk until NUL or precision cap (precision bounds reads
        // even when no NUL is present, per ISO C).
        let cap = precision.unwrap_or(usize::MAX);
        // SAFETY: the loop reads only bytes before the first NUL or the
        // precision cap (plus the NUL itself), all readable and unwritten
        // per the caller's contract. The slice covers the `len` bytes
        // just read.
        unsafe {
            while len < cap && *p.add(len) != 0 {
                len += 1;
            }
            std::slice::from_raw_parts(p as *const u8, len)
        }
    };
    let used = match precision {
        Some(p) => bytes.len().min(p),
        None => bytes.len(),
    };
    pad_and_emit(out, &bytes[..used], width, f_minus, false);
}

// ------------- char formatter -------------

fn fmt_char(out: &mut Vec<u8>, v: u8, width: Option<usize>, f_minus: bool) {
    pad_and_emit(out, &[v], width, f_minus, false);
}

// ------------- float formatters -------------

fn fmt_float_f(
    out: &mut Vec<u8>,
    v: f64,
    width: Option<usize>,
    precision: Option<usize>,
    f_minus: bool,
    f_plus: bool,
    f_space: bool,
    f_zero: bool,
    upper: bool,
) {
    let prec = precision.unwrap_or(6);
    let body = format_fixed(v, prec, upper);
    pad_signed_number(out, body, width, f_minus, f_plus, f_space, f_zero, v.is_sign_negative());
}

fn fmt_float_e(
    out: &mut Vec<u8>,
    v: f64,
    width: Option<usize>,
    precision: Option<usize>,
    f_minus: bool,
    f_plus: bool,
    f_space: bool,
    f_zero: bool,
    upper: bool,
) {
    let prec = precision.unwrap_or(6);
    let body = format_scientific(v, prec, upper);
    pad_signed_number(out, body, width, f_minus, f_plus, f_space, f_zero, v.is_sign_negative());
}

fn fmt_float_g(
    out: &mut Vec<u8>,
    v: f64,
    width: Option<usize>,
    precision: Option<usize>,
    f_minus: bool,
    f_plus: bool,
    f_space: bool,
    f_zero: bool,
    f_hash: bool,
    upper: bool,
) {
    // Implementation per ISO C: P = precision (default 6, treated as 1
    // if 0). Compute X such that the value rounds to P significant
    // digits. If P > X >= -4 use %f with precision P-1-X; else use %e
    // with precision P-1. Strip trailing zeros (and trailing '.')
    // unless `#` flag.
    let p_orig = precision.unwrap_or(6);
    let p = if p_orig == 0 { 1 } else { p_orig };

    if !v.is_finite() {
        let body = format_fixed(v, 0, upper);
        pad_signed_number(out, body, width, f_minus, f_plus, f_space, f_zero, v.is_sign_negative());
        return;
    }

    // Determine X = exponent of the leading digit.
    let abs = v.abs();
    let x = if abs == 0.0 { 0i32 } else { abs.log10().floor() as i32 };

    let use_e = !((p as i32 > x) && (x >= -4));
    let mut body = if use_e {
        format_scientific(v, (p - 1) as usize, upper)
    } else {
        format_fixed(v, (p as i32 - 1 - x).max(0) as usize, upper)
    };
    if !f_hash {
        body = strip_trailing_zeros_g(body, upper);
    }
    pad_signed_number(out, body, width, f_minus, f_plus, f_space, f_zero, v.is_sign_negative());
}

/// Format the absolute value as fixed, returning the digits without a
/// sign.
fn format_fixed(v: f64, prec: usize, _upper: bool) -> Vec<u8> {
    if v.is_nan() {
        return b"nan".to_vec();
    }
    if v.is_infinite() {
        return b"inf".to_vec();
    }
    let s = format!("{:.*}", prec, v.abs());
    s.into_bytes()
}

/// Format the absolute value in scientific form: `d.dddde+NN`.
fn format_scientific(v: f64, prec: usize, upper: bool) -> Vec<u8> {
    if v.is_nan() {
        return b"nan".to_vec();
    }
    if v.is_infinite() {
        return b"inf".to_vec();
    }
    let abs = v.abs();
    if abs == 0.0 {
        let mut s = String::from("0");
        if prec > 0 {
            s.push('.');
            for _ in 0..prec { s.push('0'); }
        }
        s.push(if upper { 'E' } else { 'e' });
        s.push_str("+00");
        return s.into_bytes();
    }
    // Compute exponent.
    let exp = abs.log10().floor() as i32;
    let mantissa = abs / 10f64.powi(exp);
    // Round mantissa to `prec` decimal places. If rounding bumps
    // mantissa to 10.0, divide by 10 and bump exp.
    let factor = 10f64.powi(prec as i32);
    let rounded = (mantissa * factor).round() / factor;
    let (mantissa, exp) = if rounded >= 10.0 {
        (rounded / 10.0, exp + 1)
    } else {
        (rounded, exp)
    };
    let mant_str = format!("{:.*}", prec, mantissa);
    let mut s = String::with_capacity(mant_str.len() + 5);
    s.push_str(&mant_str);
    s.push(if upper { 'E' } else { 'e' });
    if exp < 0 {
        s.push('-');
    } else {
        s.push('+');
    }
    let abs_exp = exp.unsigned_abs() as i32;
    if abs_exp < 10 {
        s.push('0');
        s.push((b'0' + abs_exp as u8) as char);
    } else {
        s.push_str(&abs_exp.to_string());
    }
    s.into_bytes()
}

fn strip_trailing_zeros_g(mut body: Vec<u8>, upper: bool) -> Vec<u8> {
    let e = if upper { b'E' } else { b'e' };
    // Split off exponent suffix if any.
    let exp_idx = body.iter().position(|&c| c == e);
    let (mantissa, exp) = match exp_idx {
        Some(i) => (body[..i].to_vec(), Some(body[i..].to_vec())),
        None => (body.clone(), None),
    };
    // Only strip trailing zeros if there's a decimal point.
    let mut m = mantissa;
    if m.contains(&b'.') {
        while m.last() == Some(&b'0') { m.pop(); }
        if m.last() == Some(&b'.') { m.pop(); }
    }
    body.clear();
    body.extend_from_slice(&m);
    if let Some(e) = exp { body.extend_from_slice(&e); }
    body
}

fn pad_signed_number(
    out: &mut Vec<u8>,
    digits: Vec<u8>,
    width: Option<usize>,
    f_minus: bool,
    f_plus: bool,
    f_space: bool,
    f_zero: bool,
    is_neg: bool,
) {
    let sign: &[u8] = if is_neg { b"-" } else if f_plus { b"+" } else if f_space { b" " } else { b"" };
    let total = sign.len() + digits.len();
    let pad = width.map(|w| w.saturating_sub(total)).unwrap_or(0);

    if !f_minus && !f_zero && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
    out.extend_from_slice(sign);
    if !f_minus && f_zero && pad > 0 {
        for _ in 0..pad { out.push(b'0'); }
    }
    out.extend_from_slice(&digits);
    if f_minus && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
}

fn pad_and_emit(out: &mut Vec<u8>, body: &[u8], width: Option<usize>, f_minus: bool, _f_zero: bool) {
    let pad = width.map(|w| w.saturating_sub(body.len())).unwrap_or(0);
    if !f_minus && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
    out.extend_from_slice(body);
    if f_minus && pad > 0 {
        for _ in 0..pad { out.push(b' '); }
    }
}

/// Top-level fprintf entry: format into a scratch buffer, then push
/// it through the stream's buffering policy.
///
/// # Safety
///
/// As for [`format_to_vec`].
pub unsafe fn run_printf(state: &mut State, fmt: &[u8], args: &[Arg]) -> core::ffi::c_int {
    let mut out = Vec::with_capacity(fmt.len() + 16);
    // SAFETY: forwarded from the caller.
    unsafe { format_to_vec(&mut out, fmt, args) };
    if !state.mode.writable() {
        errno::set(libc::EBADF);
        state.err = true;
        return -1;
    }
    match ops::write_from(state, &out) {
        Ok(_) => out.len() as libc::c_int,
        Err(_) => -1,
    }
}

// --------------------------------------------------------------------------
// scanf engine
// --------------------------------------------------------------------------

/// Runs a `scanf` format against `state`, storing into `args`.
///
/// # Safety
///
/// `args` must meet the requirements in [`OutArg`]'s "Pointer
/// validity" section for the conversions in `fmt`.
pub unsafe fn run_scanf(state: &mut State, fmt: &[u8], args: &mut [OutArg<'_>]) -> core::ffi::c_int {
    if !state.mode.readable() {
        errno::set(libc::EBADF);
        state.err = true;
        return -1;
    }

    let mut consumed_any = false;
    let mut matched: i32 = 0;
    let mut a = 0usize;
    let mut i = 0usize;

    while i < fmt.len() {
        let b = fmt[i];

        if b.is_ascii_whitespace() {
            // Match any amount of whitespace (incl. zero).
            i += 1;
            loop {
                let b = match ops::read_one(state) {
                    Ok(Some(b)) => b,
                    Ok(None) => break,
                    Err(_) => return -1,
                };
                if !b.is_ascii_whitespace() {
                    state.pushback.push(b);
                    state.eof = false;
                    break;
                }
            }
            continue;
        }

        if b != b'%' {
            // Literal match: read one byte and require equality.
            let r = match ops::read_one(state) {
                Ok(Some(b)) => b,
                Ok(None) => {
                    return if consumed_any { matched } else { -1 };
                }
                Err(_) => return -1,
            };
            if r != b {
                state.pushback.push(r);
                state.eof = false;
                return matched;
            }
            consumed_any = true;
            i += 1;
            continue;
        }

        // Start of a directive.
        i += 1;
        let mut suppress = false;
        if fmt.get(i).copied() == Some(b'*') {
            suppress = true;
            i += 1;
        }
        // Width
        let mut width: Option<usize> = None;
        let mut w = 0usize;
        let mut have_width = false;
        while let Some(c) = fmt.get(i).copied() {
            if c.is_ascii_digit() { w = w * 10 + (c - b'0') as usize; i += 1; have_width = true; }
            else { break; }
        }
        if have_width { width = Some(w); }
        // Length modifier — ignored.
        loop {
            match fmt.get(i).copied() {
                Some(b'h') | Some(b'l') | Some(b'L') | Some(b'z') | Some(b'j') | Some(b't') => i += 1,
                _ => break,
            }
        }
        let conv = match fmt.get(i).copied() {
            Some(c) => c,
            None => return matched,
        };
        i += 1;

        match conv {
            b'%' => {
                let r = match ops::read_one(state) {
                    Ok(Some(b)) => b,
                    Ok(None) => return if consumed_any { matched } else { -1 },
                    Err(_) => return -1,
                };
                if r != b'%' {
                    state.pushback.push(r);
                    state.eof = false;
                    return matched;
                }
                consumed_any = true;
            }
            b'd' | b'i' | b'u' | b'x' | b'X' | b'o' => {
                skip_ws(state);
                let mut buf = Vec::with_capacity(24);
                let mut count = 0usize;
                let mut sign_seen = false;
                let cap = width.unwrap_or(usize::MAX);
                // Optional sign
                if let Ok(Some(b)) = ops::read_one(state) {
                    if (b == b'-' || b == b'+') && count < cap {
                        buf.push(b);
                        count += 1;
                        sign_seen = true;
                    } else {
                        state.pushback.push(b);
                        state.eof = false;
                    }
                }
                let _ = sign_seen;
                while count < cap {
                    let b = match ops::read_one(state) {
                        Ok(Some(b)) => b,
                        Ok(None) => break,
                        Err(_) => return -1,
                    };
                    let ok = match conv {
                        b'd' | b'u' => b.is_ascii_digit(),
                        b'i' => b.is_ascii_digit() || b == b'x' || b == b'X' || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b),
                        b'x' | b'X' => b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b),
                        b'o' => (b'0'..=b'7').contains(&b),
                        _ => false,
                    };
                    if ok {
                        buf.push(b);
                        count += 1;
                    } else {
                        state.pushback.push(b);
                        state.eof = false;
                        break;
                    }
                }
                if buf.is_empty() || (buf.len() == 1 && (buf[0] == b'-' || buf[0] == b'+')) {
                    return if buf.is_empty() && state.eof && !consumed_any && matched == 0 {
                        -1
                    } else {
                        matched
                    };
                }
                // SAFETY: `buf` holds only bytes accepted above (sign,
                // digits, `a-f`/`A-F`, `x`/`X`), all ASCII, so it is
                // valid UTF-8.
                let s = unsafe { std::str::from_utf8_unchecked(&buf) };
                let parsed: i64 = match conv {
                    b'd' | b'i' => parse_int_with_radix(s, 0).unwrap_or(0),
                    b'u' => parse_uint_with_radix(s, 10).unwrap_or(0) as i64,
                    b'x' | b'X' => parse_uint_with_radix(s, 16).unwrap_or(0) as i64,
                    b'o' => parse_uint_with_radix(s, 8).unwrap_or(0) as i64,
                    _ => 0,
                };
                if !suppress {
                    if let Some(arg) = args.get(a) {
                        // SAFETY: `arg` is the destination of this
                        // non-suppressed conversion; the caller vouches
                        // for it.
                        unsafe { write_int_to_outarg(arg, parsed) };
                    }
                    a += 1;
                    matched += 1;
                }
                consumed_any = true;
            }
            b's' => {
                skip_ws(state);
                let cap = field_limit(args, a, suppress, width.unwrap_or(usize::MAX));
                let mut buf = Vec::new();
                while buf.len() < cap {
                    let b = match ops::read_one(state) {
                        Ok(Some(b)) => b,
                        Ok(None) => break,
                        Err(_) => return -1,
                    };
                    if b.is_ascii_whitespace() {
                        state.pushback.push(b);
                        state.eof = false;
                        break;
                    }
                    buf.push(b);
                }
                if buf.is_empty() {
                    return if consumed_any || matched > 0 { matched } else { -1 };
                }
                if !suppress {
                    // SAFETY: the destination of this non-suppressed
                    // `%s`; the caller vouches for room for the field
                    // and its NUL.
                    if unsafe { store_string(state, args.get_mut(a), &mut buf) }.is_err() {
                        return matched;
                    }
                    a += 1;
                    matched += 1;
                }
                consumed_any = true;
            }
            b'c' => {
                let cap = width.unwrap_or(1);
                if !suppress {
                    if let Some(OutArg::Buf(dst)) = args.get(a) {
                        if dst.region_len() < cap {
                            errno::set(libc::EOVERFLOW);
                            return matched;
                        }
                    }
                }
                let mut buf = Vec::with_capacity(cap);
                while buf.len() < cap {
                    let b = match ops::read_one(state) {
                        Ok(Some(b)) => b,
                        Ok(None) => break,
                        Err(_) => return -1,
                    };
                    buf.push(b);
                }
                if buf.len() < cap {
                    return if consumed_any || matched > 0 { matched } else { -1 };
                }
                if !suppress {
                    match args.get_mut(a) {
                        // SAFETY: the destination of this non-suppressed
                        // `%c`; the caller guarantees `p` is valid for
                        // `cap == buf.len()` unaliased bytes of writes.
                        Some(OutArg::Str(p)) => unsafe {
                            for (j, &b) in buf.iter().enumerate() {
                                *p.add(j) = b as libc::c_char;
                            }
                        },
                        Some(OutArg::Buf(dst)) => dst.region_mut()[..cap].copy_from_slice(&buf),
                        _ => {}
                    }
                    a += 1;
                    matched += 1;
                }
                consumed_any = true;
            }
            b'[' => {
                // Parse scanset specifier.
                let mut neg = false;
                if fmt.get(i).copied() == Some(b'^') {
                    neg = true;
                    i += 1;
                }
                let mut set = [false; 256];
                let mut first = true;
                let mut previous = None;
                while let Some(c) = fmt.get(i).copied() {
                    if c == b']' && !first {
                        i += 1;
                        break;
                    }
                    if c == b'-' && !first {
                        if let (Some(start), Some(end)) = (previous, fmt.get(i + 1).copied()) {
                            if end != b']' && start <= end {
                                set[start as usize..=end as usize].fill(true);
                                previous = Some(end);
                                i += 2;
                                continue;
                            }
                        }
                    }
                    set[c as usize] = true;
                    previous = Some(c);
                    first = false;
                    i += 1;
                }
                let cap = field_limit(args, a, suppress, width.unwrap_or(usize::MAX));
                let mut buf = Vec::new();
                while buf.len() < cap {
                    let b = match ops::read_one(state) {
                        Ok(Some(b)) => b,
                        Ok(None) => break,
                        Err(_) => return -1,
                    };
                    let inset = set[b as usize];
                    let accept = if neg { !inset } else { inset };
                    if accept {
                        buf.push(b);
                    } else {
                        state.pushback.push(b);
                        state.eof = false;
                        break;
                    }
                }
                if buf.is_empty() {
                    // A nonmatching byte is a matching failure, not EOF. It
                    // remains unread, and previous destinations stay intact.
                    return if state.eof && !consumed_any && matched == 0 {
                        -1
                    } else {
                        matched
                    };
                }
                if !suppress {
                    // SAFETY: the destination of this non-suppressed
                    // `%[`; the caller vouches for room for the field
                    // and its NUL.
                    if unsafe { store_string(state, args.get_mut(a), &mut buf) }.is_err() {
                        return matched;
                    }
                    a += 1;
                    matched += 1;
                }
                consumed_any = true;
            }
            b'f' | b'e' | b'g' | b'F' | b'E' | b'G' => {
                skip_ws(state);
                let cap = width.unwrap_or(usize::MAX);
                let mut buf = Vec::new();
                let mut have_digit = false;
                let mut have_dot = false;
                let mut have_exp = false;
                let mut count = 0usize;
                // optional sign
                if let Ok(Some(b)) = ops::read_one(state) {
                    if b == b'+' || b == b'-' {
                        buf.push(b); count += 1;
                    } else {
                        state.pushback.push(b); state.eof = false;
                    }
                }
                while count < cap {
                    let b = match ops::read_one(state) {
                        Ok(Some(b)) => b,
                        Ok(None) => break,
                        Err(_) => return -1,
                    };
                    if b.is_ascii_digit() {
                        buf.push(b); count += 1; have_digit = true;
                    } else if b == b'.' && !have_dot && !have_exp {
                        buf.push(b); count += 1; have_dot = true;
                    } else if (b == b'e' || b == b'E') && have_digit && !have_exp {
                        buf.push(b); count += 1; have_exp = true;
                        // optional sign in exponent
                        if let Ok(Some(b)) = ops::read_one(state) {
                            if b == b'+' || b == b'-' {
                                buf.push(b); count += 1;
                            } else {
                                state.pushback.push(b); state.eof = false;
                            }
                        }
                    } else {
                        state.pushback.push(b); state.eof = false;
                        break;
                    }
                }
                if !have_digit {
                    return if consumed_any || matched > 0 { matched } else { -1 };
                }
                // SAFETY: `buf` holds only bytes accepted above (sign,
                // digits, `.`, `e`/`E`), all ASCII, so it is valid UTF-8.
                let s = unsafe { std::str::from_utf8_unchecked(&buf) };
                let v: f64 = s.parse().unwrap_or(0.0);
                if !suppress {
                    if let Some(arg) = args.get(a) {
                        match *arg {
                            // SAFETY: a non-null destination of this
                            // non-suppressed conversion; the caller
                            // guarantees an aligned, unaliased write.
                            OutArg::Float(p) if !p.is_null() => unsafe { *p = v as f32 },
                            // SAFETY: as for `Float`.
                            OutArg::Double(p) if !p.is_null() => unsafe { *p = v },
                            _ => {}
                        }
                    }
                    a += 1;
                    matched += 1;
                }
                consumed_any = true;
            }
            _ => {
                // Unknown spec; bail.
                return matched;
            }
        }
    }
    if matched == 0 && !consumed_any && state.eof {
        return -1;
    }
    matched
}

fn skip_ws(state: &mut State) {
    loop {
        let b = match ops::read_one(state) {
            Ok(Some(b)) => b,
            _ => return,
        };
        if !b.is_ascii_whitespace() {
            state.pushback.push(b);
            state.eof = false;
            return;
        }
    }
}

/// The most field bytes a `%s` or `%[` conversion reads. A checked
/// destination reads one byte past what it can hold, so an overlong
/// field is detected rather than silently split.
fn field_limit(args: &[OutArg<'_>], a: usize, suppress: bool, width: usize) -> usize {
    match args.get(a) {
        Some(OutArg::Buf(dst)) if !suppress => width.min(dst.max_len().saturating_add(1)),
        _ => width,
    }
}

/// Stores a `%s` or `%[` field and its terminator. On overflow of a
/// checked destination, stores the prefix that fits, pushes the one
/// extra byte back, sets `EOVERFLOW`, and returns `Err`.
///
/// # Safety
///
/// If `arg` is `Some(OutArg::Str(p))`, `p` must be valid for writes of
/// `field.len() + 1` bytes, not accessed through any other pointer or
/// reference during the call.
unsafe fn store_string(state: &mut State, arg: Option<&mut OutArg<'_>>, field: &mut Vec<u8>) -> Result<(), ()> {
    match arg {
        // SAFETY: the caller guarantees `field.len() + 1` writable,
        // unaliased bytes at `p`; we write exactly that many.
        Some(OutArg::Str(p)) => unsafe {
            let p = *p;
            for (j, &b) in field.iter().enumerate() {
                *p.add(j) = b as libc::c_char;
            }
            *p.add(field.len()) = 0;
        },
        Some(OutArg::Buf(dst)) => {
            if field.len() > dst.max_len() {
                // `field_limit` read exactly one byte too many.
                let extra = field.pop().expect("overlong field is nonempty");
                state.pushback.push(extra);
                state.eof = false;
                dst.write_truncated(field);
                errno::set(libc::EOVERFLOW);
                return Err(());
            }
            dst.write_truncated(field);
        }
        _ => {}
    }
    Ok(())
}

/// # Safety
///
/// If `arg` is a non-null integer variant, its pointer must be valid
/// for an aligned write of its pointee type, not accessed through any
/// other pointer or reference during the call.
unsafe fn write_int_to_outarg(arg: &OutArg<'_>, v: i64) {
    // SAFETY: only non-null pointers are written, which the caller
    // guarantees are valid, aligned and unaliased for their types.
    unsafe {
        match *arg {
            OutArg::Int(p) if !p.is_null() => *p = v as libc::c_int,
            OutArg::Short(p) if !p.is_null() => *p = v as libc::c_short,
            OutArg::Long(p) if !p.is_null() => *p = v as libc::c_long,
            OutArg::LongLong(p) if !p.is_null() => *p = v as libc::c_longlong,
            OutArg::UInt(p) if !p.is_null() => *p = v as libc::c_uint,
            OutArg::ULong(p) if !p.is_null() => *p = v as libc::c_ulong,
            OutArg::ULongLong(p) if !p.is_null() => *p = v as libc::c_ulonglong,
            _ => {}
        }
    }
}

fn parse_int_with_radix(s: &str, radix_hint: u32) -> Option<i64> {
    let s = s.trim();
    let neg = s.starts_with('-');
    let body = s.trim_start_matches(|c| c == '+' || c == '-');
    let (radix, body) = if radix_hint != 0 {
        (radix_hint, body)
    } else if body.starts_with("0x") || body.starts_with("0X") {
        (16u32, &body[2..])
    } else if body.starts_with('0') && body.len() > 1 {
        (8u32, &body[1..])
    } else {
        (10u32, body)
    };
    let v = i64::from_str_radix(body, radix).ok()?;
    Some(if neg { -v } else { v })
}

fn parse_uint_with_radix(s: &str, radix: u32) -> Option<u64> {
    let (negative, body) = if let Some(body) = s.strip_prefix('-') {
        (true, body)
    } else {
        (false, s.strip_prefix('+').unwrap_or(s))
    };
    let value = u64::from_str_radix(body, radix).ok()?;
    Some(if negative { value.wrapping_neg() } else { value })
}
