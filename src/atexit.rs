use std::sync::Once;

static REGISTER_FLUSH_AT_EXIT: Once = Once::new();

pub fn ensure_registered() {
    // SAFETY: `atexit` only records the function pointer. The handler
    // is an `extern "C" fn()` with the signature libc expects, it never
    // unwinds into libc (its body runs under `catch_unwind`), and it
    // touches only `'static` state (the slotmap and the `Arc<FILE>`s it
    // holds), so it stays valid whenever `exit` runs it.
    REGISTER_FLUSH_AT_EXIT.call_once(|| unsafe {
        let _ = libc::atexit(flush_open_streams);
    });
}

extern "C" fn flush_open_streams() {
    let _ = std::panic::catch_unwind(|| {
        for arc in crate::slots::snapshot() {
            // `slots::remove` drops closed streams out of the snapshot
            // before setting the closed bit, so every stream here is
            // still logically open. Use the non-blocking/try-borrow
            // path so exit flushing never panics or deadlocks if user
            // code still holds a `StreamGuard`.
            let Some(lock) = arc.raw_try_lock() else {
                continue;
            };
            let Ok(mut state) = lock.try_borrow_mut() else {
                continue;
            };
            let _ = crate::api::ops::flush_internal(&mut state);
        }
    });
}
