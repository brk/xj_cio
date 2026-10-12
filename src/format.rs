//! The `scanf` family over the shared `xj_scanf` engine.
//!
//! This module supplies the stream input adapter, the raw `OutArg` and
//! `va_list` destination adapters, and the mapping from scan outcomes to C
//! return values and `errno`. The engine owns the format grammar,
//! conversions and safe destinations.

use core::ffi::c_int;
use std::io;

use xj_scanf::xj_scanf_core::{Conv, DataModel, Directive, IntKind, Requirement, Slot};
use xj_scanf::{ScanArgs, ScanInput, ScanOutcome, ScanTarget, Stop, StoreError, Value};

use crate::api::ops;
use crate::errno;
use crate::state::State;

/// One raw output destination for the unsafe `scanf` entry points.
///
/// # Pointer validity
///
/// The `_raw` functions write through the raw-pointer variants. Their
/// callers must ensure, for each argument that a non-suppressed conversion
/// stores into:
///
/// - The integer variants, `Float` and `Double`: the pointer is null (the
///   store is skipped) or valid for an aligned write of its pointee type.
/// - `Str(p)`: `p` is null or valid for writes of the bytes stored. For
///   `%s` and `%[`, that is the matched field plus a NUL (at most
///   width + 1 bytes when the conversion has a width). For `%c`, it is
///   exactly the width (default 1). For an `hh` integer or `%hhn`
///   directive, it is one byte.
///
/// In every case the destination must not be accessed through any other
/// pointer or reference, including the format string, during the call.
///
/// # Compatibility
///
/// Each directive accepts a destination of its class and width, of either
/// signedness (see [`Requirement::accepts`]); otherwise the call stops with
/// `EINVAL` before the directive reads input. `Str` additionally accepts
/// `hh` integer and count directives, for translated `char *` arguments
/// whose format is not known until run time.
pub enum OutArg<'a> {
    SChar(*mut libc::c_schar),
    UChar(*mut libc::c_uchar),
    Short(*mut libc::c_short),
    UShort(*mut libc::c_ushort),
    Int(*mut libc::c_int),
    UInt(*mut libc::c_uint),
    Long(*mut libc::c_long),
    ULong(*mut libc::c_ulong),
    LongLong(*mut libc::c_longlong),
    ULongLong(*mut libc::c_ulonglong),
    /// `size_t` and `uintptr_t`.
    Size(*mut usize),
    /// `ssize_t`, `ptrdiff_t` and `intptr_t`.
    ISize(*mut isize),
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

// SAFETY: the raw pointers are written only by the unsafe `scanf` entry
// points, under their callers' guarantees. `Buf` holds a `CStrDest` (a
// `&mut [u8]` and a flag), which is `Send + Sync` anyway.
unsafe impl Send for OutArg<'_> {}
// SAFETY: as for `Send`.
unsafe impl Sync for OutArg<'_> {}

// --------------------------------------------------------------------------
// Input

/// A locked stream's state as engine input. All lookahead stays in the
/// stream's own pushback and buffer, so nothing is restored on exit.
struct StreamInput<'s> {
    state: &'s mut State,
}

impl ScanInput for StreamInput<'_> {
    fn peek(&mut self) -> io::Result<Option<u8>> {
        let s = &mut *self.state;
        if let Some(&byte) = s.pushback.last() {
            return Ok(Some(byte));
        }
        if s.buffer.cursor >= s.buffer.fill {
            // Sticky EOF: once the indicator is set, scanning reads no
            // further data until `clearerr` or a reposition clears it.
            if s.eof {
                return Ok(None);
            }
            ops::refill_buffer(s)?;
        }
        Ok((s.buffer.cursor < s.buffer.fill).then(|| s.buffer.data[s.buffer.cursor]))
    }

    fn bump(&mut self) {
        if self.state.pushback.pop().is_none() {
            self.state.buffer.cursor += 1;
        }
    }
}

// --------------------------------------------------------------------------
// Raw destinations

fn slot(arg: &OutArg<'_>) -> Slot {
    match arg {
        OutArg::SChar(_) | OutArg::UChar(_) => Slot::Int(IntKind::Char),
        OutArg::Short(_) | OutArg::UShort(_) => Slot::Int(IntKind::Short),
        OutArg::Int(_) | OutArg::UInt(_) => Slot::Int(IntKind::Int),
        OutArg::Long(_) | OutArg::ULong(_) => Slot::Int(IntKind::Long),
        OutArg::LongLong(_) | OutArg::ULongLong(_) => Slot::Int(IntKind::LongLong),
        OutArg::Size(_) => Slot::Int(IntKind::Size),
        OutArg::ISize(_) => Slot::Int(IntKind::PtrDiff),
        OutArg::Float(_) => Slot::Float,
        OutArg::Double(_) => Slot::Double,
        OutArg::Str(_) | OutArg::Buf(_) => Slot::Bytes,
    }
}

/// Validates `arg` for `d` and returns its byte limit.
fn raw_prepare(arg: &mut OutArg<'_>, d: &Directive) -> Result<Option<usize>, StoreError> {
    let req = d.requirement().ok_or(StoreError::TypeMismatch)?;
    match arg {
        OutArg::Buf(dest) => dest.prepare(d),
        // The legacy `char *` fallback: one byte for `hh` integers and
        // `%hhn`, under the same conversion policy as a 1-byte integer.
        OutArg::Str(_)
            if matches!(
                req,
                Requirement::Int {
                    kind: IntKind::Char,
                    ..
                } | Requirement::Count(IntKind::Char)
            ) =>
        {
            Ok(None)
        }
        _ if req.accepts(slot(arg), &DataModel::HOST) => Ok(matches!(
            (&*arg, req),
            (OutArg::SChar(_) | OutArg::UChar(_), Requirement::Chars(_))
        )
        .then_some(1)),
        _ => Err(StoreError::TypeMismatch),
    }
}

/// Stores one conversion through a prepared raw destination.
///
/// # Safety
///
/// `arg` must meet [`OutArg`]'s pointer validity requirements for `d`.
unsafe fn raw_store(arg: &mut OutArg<'_>, d: &Directive, v: Value<'_>) -> Result<(), StoreError> {
    macro_rules! int {
        ($p:expr, $signed:expr, $ty:ty) => {{
            let bits =
                xj_scanf::int_bits(d, v, $signed, <$ty>::BITS).ok_or(StoreError::TypeMismatch)?;
            if !$p.is_null() {
                // SAFETY: a non-null pointer the caller vouches for.
                unsafe { $p.write(bits as $ty) };
            }
        }};
    }
    match arg {
        OutArg::SChar(p) => int!(*p, true, libc::c_schar),
        OutArg::UChar(p) => int!(*p, false, libc::c_uchar),
        OutArg::Short(p) => int!(*p, true, libc::c_short),
        OutArg::UShort(p) => int!(*p, false, libc::c_ushort),
        OutArg::Int(p) => int!(*p, true, libc::c_int),
        OutArg::UInt(p) => int!(*p, false, libc::c_uint),
        OutArg::Long(p) => int!(*p, true, libc::c_long),
        OutArg::ULong(p) => int!(*p, false, libc::c_ulong),
        OutArg::LongLong(p) => int!(*p, true, libc::c_longlong),
        OutArg::ULongLong(p) => int!(*p, false, libc::c_ulonglong),
        OutArg::Size(p) => int!(*p, false, usize),
        OutArg::ISize(p) => int!(*p, true, isize),
        OutArg::Float(p) => {
            let Value::Float32(x) = v else {
                return Err(StoreError::TypeMismatch);
            };
            if !p.is_null() {
                // SAFETY: a non-null pointer the caller vouches for.
                unsafe { p.write(x) };
            }
        }
        OutArg::Double(p) => {
            let Value::Float64(x) = v else {
                return Err(StoreError::TypeMismatch);
            };
            if !p.is_null() {
                // SAFETY: a non-null pointer the caller vouches for.
                unsafe { p.write(x) };
            }
        }
        OutArg::Str(p) => match v {
            Value::Bytes(bytes) if !p.is_null() => {
                let terminate = d.conv != Conv::Chars;
                // SAFETY: the caller guarantees room for the field, plus a
                // NUL for `%s` and `%[`, not otherwise accessed.
                unsafe {
                    p.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
                    if terminate {
                        p.add(bytes.len()).write(0);
                    }
                }
            }
            Value::Bytes(_) => {}
            _ => {
                let signed = !matches!(d.conv, Conv::Int { signed: false, .. });
                int!(*p, signed, u8)
            }
        },
        OutArg::Buf(dest) => dest.store(d, v)?,
    }
    Ok(())
}

/// The engine adapter over a raw argument slice. Created only inside the
/// unsafe entry points; it never escapes the call.
pub(crate) struct RawArgs<'a, 'b> {
    args: &'a mut [OutArg<'b>],
    next: usize,
}

impl<'a, 'b> RawArgs<'a, 'b> {
    /// # Safety
    ///
    /// `args` must meet [`OutArg`]'s pointer validity requirements for the
    /// format this adapter is used with.
    pub(crate) unsafe fn new(args: &'a mut [OutArg<'b>]) -> Self {
        RawArgs { args, next: 0 }
    }
}

impl ScanArgs for RawArgs<'_, '_> {
    fn prepare(&mut self, d: &Directive) -> Result<Option<usize>, StoreError> {
        let arg = self.args.get_mut(self.next).ok_or(StoreError::Missing)?;
        let limit = raw_prepare(arg, d)?;
        self.next += 1;
        Ok(limit)
    }

    fn store(&mut self, d: &Directive, v: Value<'_>) -> Result<(), StoreError> {
        let index = self.next.checked_sub(1).ok_or(StoreError::Missing)?;
        // SAFETY: `new`'s caller vouched for these arguments.
        unsafe { raw_store(&mut self.args[index], d, v) }
    }
}

/// The engine adapter over a `va_list`: each claim pulls one pointer of
/// exactly the type the directive requires.
#[cfg(feature = "c_variadic")]
pub(crate) struct VaArgs<F: FnMut(Requirement) -> OutArg<'static>> {
    pull: F,
    current: Option<OutArg<'static>>,
}

#[cfg(feature = "c_variadic")]
impl<F: FnMut(Requirement) -> OutArg<'static>> VaArgs<F> {
    /// # Safety
    ///
    /// Each pointer `pull` returns must meet [`OutArg`]'s pointer validity
    /// requirements for its directive.
    pub(crate) unsafe fn new(pull: F) -> Self {
        VaArgs {
            pull,
            current: None,
        }
    }
}

#[cfg(feature = "c_variadic")]
impl<F: FnMut(Requirement) -> OutArg<'static>> ScanArgs for VaArgs<F> {
    fn prepare(&mut self, d: &Directive) -> Result<Option<usize>, StoreError> {
        let req = d.requirement().ok_or(StoreError::TypeMismatch)?;
        let mut arg = (self.pull)(req);
        let limit = raw_prepare(&mut arg, d)?;
        self.current = Some(arg);
        Ok(limit)
    }

    fn store(&mut self, d: &Directive, v: Value<'_>) -> Result<(), StoreError> {
        let arg = self.current.as_mut().ok_or(StoreError::Missing)?;
        // SAFETY: the unsafe `va_list` entry point's caller vouched for
        // each pointer in the list.
        unsafe { raw_store(arg, d, v) }
    }
}

/// Pulls the next pointer for `req`.
///
/// # Safety
///
/// The next argument in `ap` must be a pointer of the type `req` names.
#[cfg(feature = "c_variadic")]
pub(crate) unsafe fn va_arg(ap: &mut core::ffi::VaList, req: Requirement) -> OutArg<'static> {
    use libc::{
        c_int, c_long, c_longlong, c_schar, c_short, c_uchar, c_uint, c_ulong, c_ulonglong,
        c_ushort,
    };
    let (kind, signed) = match req {
        Requirement::Int { kind, signed } => (kind, signed),
        Requirement::Count(kind) => (kind, true),
        // SAFETY: forwarded from the caller.
        Requirement::Float => return OutArg::Float(unsafe { ap.arg::<*mut f32>() }),
        // SAFETY: forwarded from the caller.
        Requirement::Double => return OutArg::Double(unsafe { ap.arg::<*mut f64>() }),
        // SAFETY: forwarded from the caller.
        Requirement::Str | Requirement::Chars(_) => {
            return OutArg::Str(unsafe { ap.arg::<*mut u8>() });
        }
    };
    // SAFETY: forwarded from the caller.
    unsafe {
        match (kind, signed) {
            (IntKind::Char, true) => OutArg::SChar(ap.arg::<*mut c_schar>()),
            (IntKind::Char, false) => OutArg::UChar(ap.arg::<*mut c_uchar>()),
            (IntKind::Short, true) => OutArg::Short(ap.arg::<*mut c_short>()),
            (IntKind::Short, false) => OutArg::UShort(ap.arg::<*mut c_ushort>()),
            (IntKind::Int, true) => OutArg::Int(ap.arg::<*mut c_int>()),
            (IntKind::Int, false) => OutArg::UInt(ap.arg::<*mut c_uint>()),
            (IntKind::Long, true) => OutArg::Long(ap.arg::<*mut c_long>()),
            (IntKind::Long, false) => OutArg::ULong(ap.arg::<*mut c_ulong>()),
            // `intmax_t` is `long long` width on every supported target.
            (IntKind::LongLong | IntKind::IntMax, true) => {
                OutArg::LongLong(ap.arg::<*mut c_longlong>())
            }
            (IntKind::LongLong | IntKind::IntMax, false) => {
                OutArg::ULongLong(ap.arg::<*mut c_ulonglong>())
            }
            (IntKind::Size, _) => OutArg::Size(ap.arg::<*mut usize>()),
            (IntKind::PtrDiff, _) => OutArg::ISize(ap.arg::<*mut isize>()),
        }
    }
}

// --------------------------------------------------------------------------
// Entry-point plumbing

/// C's return value, with `errno` for library-defined failures. The input
/// adapter has already maintained the stream's EOF and error indicators.
pub(crate) fn finish(outcome: &ScanOutcome) -> c_int {
    match &outcome.stop {
        Stop::InvalidFormat { .. }
        | Stop::Store {
            err: StoreError::TypeMismatch | StoreError::Missing,
            ..
        } => errno::set(libc::EINVAL),
        Stop::Store {
            err: StoreError::Capacity,
            ..
        } => errno::set(libc::EOVERFLOW),
        Stop::Io(e) => {
            if let Some(code) = e.raw_os_error() {
                errno::set(code);
            }
        }
        Stop::FormatEnd | Stop::MatchingFailure | Stop::InputEnd => {}
    }
    outcome.c_return()
}

/// Scans a locked stream.
pub(crate) fn scan_state<A: ScanArgs + ?Sized>(
    state: &mut State,
    fmt: &[u8],
    args: &mut A,
) -> c_int {
    if !state.mode.readable() {
        errno::set(libc::EBADF);
        state.err = true;
        return -1;
    }
    if ops::switch_to_read(state).is_err() {
        return -1;
    }
    finish(&xj_scanf::vscan(&mut StreamInput { state }, fmt, args))
}

pub(crate) fn scan_stream<A: ScanArgs + ?Sized>(
    t: crate::FILE_ptr,
    fmt: &[u8],
    args: &mut A,
) -> c_int {
    ops::with_token(t, |s| scan_state(s, fmt, args)).unwrap_or(-1)
}

pub(crate) fn scan_bytes<A: ScanArgs + ?Sized>(input: &[u8], fmt: &[u8], args: &mut A) -> c_int {
    finish(&xj_scanf::vscan(
        &mut xj_scanf::SliceInput::new(input),
        fmt,
        args,
    ))
}
