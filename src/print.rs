//! `print!` / `println!` / `eprint!` / `eprintln!` — analogues of the
//! standard-library macros that route through xj_cio's `stdout()` /
//! `stderr()` streams instead of `std::io::stdout()` / `stderr()`.
//!
//! Output is written under the stream's reentrant mutex, so a single
//! `println!("…")` (args + trailing newline) is atomic against other
//! threads' xj_cio writes. The macros silently swallow `io::Error`,
//! matching the behavior of `std::print!`.

use std::io::Write;

use crate::api::stderr_lock;
use crate::api::stdout_lock;

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments<'_>) {
    let mut g = stdout_lock();
    let _ = g.write_fmt(args);
}

#[doc(hidden)]
pub fn _println(args: core::fmt::Arguments<'_>) {
    let mut g = stdout_lock();
    let _ = g.write_fmt(args);
    let _ = g.write_all(b"\n");
}

#[doc(hidden)]
pub fn _eprint(args: core::fmt::Arguments<'_>) {
    let mut g = stderr_lock();
    let _ = g.write_fmt(args);
}

#[doc(hidden)]
pub fn _eprintln(args: core::fmt::Arguments<'_>) {
    let mut g = stderr_lock();
    let _ = g.write_fmt(args);
    let _ = g.write_all(b"\n");
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::_print(::core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::_print(::core::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::_println(::core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {
        $crate::_eprint(::core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! eprintln {
    () => {
        $crate::_eprint(::core::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::_eprintln(::core::format_args!($($arg)*))
    };
}
