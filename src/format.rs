//! Scanning grammar and execution (separate from C printing).
//!
//! The translator marshals scanning destinations into `OutArg` slices.

use crate::api::ops;
use crate::errno;
use crate::state::State;

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
    Str(*mut u8),
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
    Buf(crate::CStrDest<'a>),
}

// SAFETY: the raw pointers are written only by the
// unsafe `scanf` functions, under their callers' guarantees. `Buf` holds
// a `CStrDest` (a `&mut [u8]` and a flag), which is `Send + Sync` anyway.
unsafe impl Send for OutArg<'_> {}
// SAFETY: as for `Send`.
unsafe impl Sync for OutArg<'_> {}

// --------------------------------------------------------------------------
// scanf engine
// --------------------------------------------------------------------------

/// Runs a `scanf` format against `state`, storing into `args`.
///
/// # Safety
///
/// `args` must meet the requirements in [`OutArg`]'s "Pointer
/// validity" section for the conversions in `fmt`.
pub unsafe fn run_scanf(
    state: &mut State,
    fmt: &[u8],
    args: &mut [OutArg<'_>],
) -> core::ffi::c_int {
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
            if c.is_ascii_digit() {
                w = w * 10 + (c - b'0') as usize;
                i += 1;
                have_width = true;
            } else {
                break;
            }
        }
        if have_width {
            width = Some(w);
        }
        // Length modifier — ignored.
        loop {
            match fmt.get(i).copied() {
                Some(b'h') | Some(b'l') | Some(b'L') | Some(b'z') | Some(b'j') | Some(b't') => {
                    i += 1
                }
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
                        b'i' => {
                            b.is_ascii_digit()
                                || b == b'x'
                                || b == b'X'
                                || (b'a'..=b'f').contains(&b)
                                || (b'A'..=b'F').contains(&b)
                        }
                        b'x' | b'X' => {
                            b.is_ascii_digit()
                                || (b'a'..=b'f').contains(&b)
                                || (b'A'..=b'F').contains(&b)
                        }
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
                    return if consumed_any || matched > 0 {
                        matched
                    } else {
                        -1
                    };
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
                    return if consumed_any || matched > 0 {
                        matched
                    } else {
                        -1
                    };
                }
                if !suppress {
                    match args.get_mut(a) {
                        // SAFETY: the destination of this non-suppressed
                        // `%c`; the caller guarantees `p` is valid for
                        // `cap == buf.len()` unaliased bytes of writes.
                        Some(OutArg::Str(p)) => unsafe {
                            for (j, &b) in buf.iter().enumerate() {
                                *p.add(j) = b;
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
                        buf.push(b);
                        count += 1;
                    } else {
                        state.pushback.push(b);
                        state.eof = false;
                    }
                }
                while count < cap {
                    let b = match ops::read_one(state) {
                        Ok(Some(b)) => b,
                        Ok(None) => break,
                        Err(_) => return -1,
                    };
                    if b.is_ascii_digit() {
                        buf.push(b);
                        count += 1;
                        have_digit = true;
                    } else if b == b'.' && !have_dot && !have_exp {
                        buf.push(b);
                        count += 1;
                        have_dot = true;
                    } else if (b == b'e' || b == b'E') && have_digit && !have_exp {
                        buf.push(b);
                        count += 1;
                        have_exp = true;
                        // optional sign in exponent
                        if let Ok(Some(b)) = ops::read_one(state) {
                            if b == b'+' || b == b'-' {
                                buf.push(b);
                                count += 1;
                            } else {
                                state.pushback.push(b);
                                state.eof = false;
                            }
                        }
                    } else {
                        state.pushback.push(b);
                        state.eof = false;
                        break;
                    }
                }
                if !have_digit {
                    return if consumed_any || matched > 0 {
                        matched
                    } else {
                        -1
                    };
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
unsafe fn store_string(
    state: &mut State,
    arg: Option<&mut OutArg<'_>>,
    field: &mut Vec<u8>,
) -> Result<(), ()> {
    match arg {
        // SAFETY: the caller guarantees `field.len() + 1` writable,
        // unaliased bytes at `p`; we write exactly that many.
        Some(OutArg::Str(p)) => unsafe {
            let p = *p;
            for (j, &b) in field.iter().enumerate() {
                *p.add(j) = b;
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
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}
