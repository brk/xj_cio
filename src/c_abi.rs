//! C ABI entry points for fixed-signature stdio functions.
//!
//! Direct Rust calls can use the crate's ordinary APIs. These adapters provide
//! C function pointers and accept raw C strings where the ordinary API takes
//! `&CStr`. Like the rest of this crate, C `char` storage uses `u8` and streams
//! use [`FILE_ptr`]. These are Rust namespaced functions, not exported libc
//! symbols. Formatted I/O remains on the macros and plan APIs.

use crate::FILE_ptr;
use core::ffi::{CStr, c_int, c_long, c_void};

macro_rules! forward {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {$(
        #[doc = concat!("C ABI entry point for [`crate::", stringify!($name), "`].")]
        #[inline]
        pub extern "C" fn $name($($arg: $ty),*) -> $ret {
            crate::$name($($arg),*)
        }
    )*};
}

macro_rules! forward_unsafe {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty => $target:ident;)*) => {$(
        #[doc = concat!("C ABI entry point for [`crate::", stringify!($target), "`].")]
        ///
        /// # Safety
        ///
        #[doc = concat!("The caller must satisfy [`crate::", stringify!($target), "`]'s safety contract.")]
        #[inline]
        pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
            // SAFETY: the caller supplies the ordinary API's requirements.
            unsafe { crate::$target($($arg),*) }
        }
    )*};
}

forward! {
    fn fclose(stream: FILE_ptr) -> c_int;
    fn fileno(stream: FILE_ptr) -> c_int;
    fn tmpfile() -> FILE_ptr;
    fn pclose(stream: FILE_ptr) -> c_int;
    fn open_memstream(buf: *mut *mut u8, size: *mut libc::size_t) -> FILE_ptr;
    fn fgetc(stream: FILE_ptr) -> c_int;
    fn getc(stream: FILE_ptr) -> c_int;
    fn getchar() -> c_int;
    fn fputc(byte: c_int, stream: FILE_ptr) -> c_int;
    fn putc(byte: c_int, stream: FILE_ptr) -> c_int;
    fn putchar(byte: c_int) -> c_int;
    fn ungetc(byte: c_int, stream: FILE_ptr) -> c_int;
    fn fflush(stream: FILE_ptr) -> c_int;
    fn setvbuf(stream: FILE_ptr, buf: *mut u8, mode: c_int, size: libc::size_t) -> c_int;
    fn setbuf(stream: FILE_ptr, buf: *mut u8) -> ();
    fn feof(stream: FILE_ptr) -> c_int;
    fn ferror(stream: FILE_ptr) -> c_int;
    fn clearerr(stream: FILE_ptr) -> ();
    fn flockfile(stream: FILE_ptr) -> ();
    fn ftrylockfile(stream: FILE_ptr) -> c_int;
    fn funlockfile(stream: FILE_ptr) -> ();
    fn fseek(stream: FILE_ptr, offset: c_long, whence: c_int) -> c_int;
    fn fseeko(stream: FILE_ptr, offset: libc::off_t, whence: c_int) -> c_int;
    fn ftell(stream: FILE_ptr) -> c_long;
    fn ftello(stream: FILE_ptr) -> libc::off_t;
    fn rewind(stream: FILE_ptr) -> ();
}

forward_unsafe! {
    fn tmpnam(buf: *mut u8) -> *mut u8 => tmpnam_raw;
    fn ctermid(buf: *mut u8) -> *mut u8 => ctermid_raw;
    fn fgets(buf: *mut u8, size: c_int, stream: FILE_ptr) -> *mut u8 => fgets_raw;
    fn getline(line: *mut *mut u8, size: *mut libc::size_t, stream: FILE_ptr) -> libc::ssize_t => getline_raw;
    fn getdelim(line: *mut *mut u8, size: *mut libc::size_t, delim: c_int, stream: FILE_ptr) -> libc::ssize_t => getdelim_raw;
    fn perror(prefix: *const u8) -> () => perror_raw;
}

macro_rules! strings {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty => $body:expr;)*) => {$(
        #[doc = concat!("Raw C-string entry point for `", stringify!($name), "`.")]
        ///
        /// # Safety
        ///
        /// Each string passed to the ordinary API as `&CStr` must point to a
        /// readable NUL-terminated string that remains unchanged for the call.
        /// Other pointer arguments must satisfy the corresponding raw API's contract.
        #[inline]
        pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
            // SAFETY: C strings and any other unsafe arguments are valid by
            // the caller's contract.
            unsafe { $body }
        }
    )*};
}

strings! {
    fn fopen(path: *const u8, mode: *const u8) -> FILE_ptr =>
        crate::fopen(CStr::from_ptr(path.cast()), CStr::from_ptr(mode.cast()));
    fn fdopen(fd: c_int, mode: *const u8) -> FILE_ptr =>
        crate::fdopen(fd, CStr::from_ptr(mode.cast()));
    fn freopen(path: *const u8, mode: *const u8, stream: FILE_ptr) -> FILE_ptr =>
        crate::freopen_raw(path, CStr::from_ptr(mode.cast()), stream);
    fn popen(command: *const u8, mode: *const u8) -> FILE_ptr =>
        crate::popen(CStr::from_ptr(command.cast()), CStr::from_ptr(mode.cast()));
    fn fmemopen(buf: *mut c_void, size: libc::size_t, mode: *const u8) -> FILE_ptr =>
        crate::fmemopen_raw(buf, size, CStr::from_ptr(mode.cast()));
    fn remove(path: *const u8) -> c_int =>
        crate::remove(CStr::from_ptr(path.cast()));
    fn rename(from: *const u8, to: *const u8) -> c_int =>
        crate::rename(CStr::from_ptr(from.cast()), CStr::from_ptr(to.cast()));
    fn fputs(text: *const u8, stream: FILE_ptr) -> c_int =>
        crate::fputs(CStr::from_ptr(text.cast()), stream);
    fn puts(text: *const u8) -> c_int =>
        crate::puts(CStr::from_ptr(text.cast()));
}

/// C buffer entry point for [`crate::fread_raw`].
///
/// # Safety
///
/// The byte storage at `buf` must satisfy [`crate::fread_raw`]'s safety contract.
#[inline]
pub unsafe extern "C" fn fread(
    buf: *mut c_void,
    size: libc::size_t,
    count: libc::size_t,
    stream: FILE_ptr,
) -> libc::size_t {
    // SAFETY: casting changes only the pointee type; the caller provides the
    // byte storage required by fread.
    unsafe { crate::fread_raw(buf.cast(), size, count, stream) }
}

/// C buffer entry point for [`crate::fwrite_raw`].
///
/// # Safety
///
/// The byte storage at `buf` must satisfy [`crate::fwrite_raw`]'s safety contract.
#[inline]
pub unsafe extern "C" fn fwrite(
    buf: *const c_void,
    size: libc::size_t,
    count: libc::size_t,
    stream: FILE_ptr,
) -> libc::size_t {
    // SAFETY: the caller provides the byte storage required by fwrite.
    unsafe { crate::fwrite_raw(buf.cast(), size, count, stream) }
}

/// C ABI entry point for [`crate::fgetpos_raw`]. The type parameter accommodates
/// translated C definitions of the opaque `fpos_t` record.
///
/// # Safety
///
/// `pos` must point to storage with `libc::fpos_t`'s layout, size, and alignment,
/// and satisfy [`crate::fgetpos_raw`]'s write and aliasing requirements.
#[inline]
pub unsafe extern "C" fn fgetpos<T>(stream: FILE_ptr, pos: *mut T) -> c_int {
    // SAFETY: the caller guarantees the original C fpos_t storage layout.
    unsafe { crate::fgetpos_raw(stream, pos.cast()) }
}

/// C ABI entry point for [`crate::fsetpos_raw`]. The type parameter accommodates
/// translated C definitions of the opaque `fpos_t` record.
///
/// # Safety
///
/// `pos` must point to a value with `libc::fpos_t`'s layout, size, and alignment,
/// and satisfy [`crate::fsetpos_raw`]'s read and aliasing requirements.
#[inline]
pub unsafe extern "C" fn fsetpos<T>(stream: FILE_ptr, pos: *const T) -> c_int {
    // SAFETY: the caller guarantees the original C fpos_t storage layout.
    unsafe { crate::fsetpos_raw(stream, pos.cast()) }
}
