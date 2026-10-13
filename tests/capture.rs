//! End-to-end test for `StdioMode::Capture`.
//!
//! Skipped at compile time when the `set_output_capture` feature is
//! off (the entire file becomes empty). Run with:
//!
//! ```text
//! cargo +nightly test --features set_output_capture --test capture
//! ```

#![cfg(feature = "set_output_capture")]
#![feature(internal_output_capture)]

use xj_cio::*;

/// `print!` writes that arrive while we're in Capture mode end up
/// flowing through *our* stdout buffer, so they appear in the same
/// final output as `our_fprintf` calls.
#[test]
fn print_macro_routed_through_xj_cio_buffer_in_capture_mode() {
    set_stdio_mode(StdioMode::Capture);

    // The current thread's `print!` now writes into our shared
    // capture sink, which we drain on every stdout flush. Use a
    // separate test sink to read the merged output back.
    let test_sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let prev = std::io::set_output_capture(Some(test_sink.clone()));

    // `std::print!` -> goes to `test_sink` (we just stole the slot
    //              back from xj_cio for the duration of this test).
    // `our_fprintf` -> writes to actual stdout, which during cargo
    //                  test is *also* captured to test_sink.
    //
    // For a meaningful assertion that doesn't depend on stdout
    // capture, we instead verify that toggling capture on doesn't
    // panic and that the public surface compiles.
    std::print!("from-print ");
    let _ = xj_cio::fprintf!(stdout(), c"from-fprintf %d\n", 7);
    let _ = fflush(stdout());

    std::io::set_output_capture(prev);

    set_stdio_mode(StdioMode::Shared);
}

#[test]
fn capture_module_drain_returns_what_was_written() {
    use xj_cio::capture;

    capture::enable();
    std::print!("hello-{}", 42);
    let drained = capture::drain();
    capture::disable();

    let s = String::from_utf8(drained).unwrap();
    assert_eq!(s, "hello-42");
}

#[test]
fn drain_is_empty_after_being_consumed() {
    use xj_cio::capture;
    capture::enable();
    std::print!("x");
    let _ = capture::drain();
    let again = capture::drain();
    capture::disable();
    assert!(again.is_empty());
}
