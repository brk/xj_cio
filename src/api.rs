//! Public API.
//!
//! The functions in this module are the translation target for
//! mechanically-rewritten C code. Naming and argument order match
//! POSIX 2024; only `FILE *` becomes `FILE_ptr` and variadic-style
//! input becomes a slice of `Arg` / `OutArg`.

pub(crate) mod ops;

pub mod open;
pub mod io;
pub mod lineio;
pub mod posn;
pub mod bufapi;
pub mod ind;
pub mod lock;
pub mod mem;
pub mod proc_;
pub mod stdio_;
pub mod fmt_;
pub mod guard;

pub use open::*;
pub use io::*;
pub use lineio::*;
pub use posn::*;
pub use bufapi::*;
pub use ind::*;
pub use lock::*;
pub use mem::*;
pub use proc_::*;
pub use stdio_::*;
pub use fmt_::*;
pub use guard::*;
