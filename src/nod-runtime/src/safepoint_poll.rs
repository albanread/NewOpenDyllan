//! Safepoint poll flag + `nod_safepoint_poll` — the stop-the-world
//! request surface for JIT/AOT-emitted poll points.
//!
//! # Design
//! Codegen emits a `call void @nod_safepoint_poll()` at every function
//! entry and at every loop back-edge target (loop header block).
//! The expected common-case cost is **one relaxed load + one correctly-
//! predicted-not-taken branch** — no function-call overhead is visible
//! from the caller once inlining / ICF fires, but even without inlining
//! the runtime overhead is negligible against any real workload.
//!
//! The collector sets `SAFEPOINT_PARK_REQUESTED` to 1 before initiating
//! a stop-the-world pause and clears it after all roots have been
//! scanned and all live objects copied.  Any mutator thread that reaches
//! a poll while the flag is set will spin-park until the flag is
//! cleared.  A proper condvar-based parking protocol can replace the
//! spin when full multi-threaded GC is wired up.

use std::sync::atomic::{AtomicU8, Ordering};

/// Process-wide GC park request flag.  Zero = no pause requested;
/// non-zero = all mutators should park at their next poll point.
pub static SAFEPOINT_PARK_REQUESTED: AtomicU8 = AtomicU8::new(0);

/// Called at every function entry and loop back-edge by JIT/AOT code.
///
/// Fast path (flag == 0): one relaxed load + return.
/// Slow path (flag != 0): spin-park until the collector clears the
/// flag.
#[unsafe(no_mangle)]
pub extern "C" fn nod_safepoint_poll() {
    if SAFEPOINT_PARK_REQUESTED.load(Ordering::Relaxed) == 0 {
        return;
    }
    safepoint_park_slow();
}

/// Request that all mutator threads stop at their next safepoint poll.
///
/// The caller MUST call [`safepoint_resume`] after root scanning is
/// complete, or all mutator threads will spin-park indefinitely.
/// Intended for future stop-the-world multi-threaded GC; single-
/// threaded code drives GC directly via `nod_make` and does not use
/// this path.
pub fn safepoint_request_stop() {
    SAFEPOINT_PARK_REQUESTED.store(1, Ordering::SeqCst);
}

/// Release all threads parked at safepoint polls.  Must be called
/// after [`safepoint_request_stop`] once root scanning is complete.
pub fn safepoint_resume() {
    SAFEPOINT_PARK_REQUESTED.store(0, Ordering::SeqCst);
}

/// C-ABI wrapper for `safepoint_request_stop` — callable from
/// AOT-compiled Dylan code or external runtime coordinators.
#[unsafe(no_mangle)]
pub extern "C" fn nod_safepoint_request_stop() {
    safepoint_request_stop();
}

/// C-ABI wrapper for `safepoint_resume`.
#[unsafe(no_mangle)]
pub extern "C" fn nod_safepoint_resume() {
    safepoint_resume();
}

#[cold]
fn safepoint_park_slow() {
    while SAFEPOINT_PARK_REQUESTED.load(Ordering::Acquire) != 0 {
        std::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_is_noop_when_flag_clear() {
        SAFEPOINT_PARK_REQUESTED.store(0, Ordering::Relaxed);
        // Must return immediately — not block.
        nod_safepoint_poll();
    }

    #[test]
    fn poll_parks_and_resumes_after_flag_cleared() {
        use std::sync::{Arc, Barrier};
        use std::thread;
        use std::time::Duration;

        let barrier = Arc::new(Barrier::new(2));
        let b2 = Arc::clone(&barrier);

        SAFEPOINT_PARK_REQUESTED.store(1, Ordering::SeqCst);

        let handle = thread::spawn(move || {
            b2.wait(); // wait for main to be ready
            nod_safepoint_poll(); // should park, then return after flag cleared
        });

        barrier.wait(); // main is ready
        // Give the spawned thread a moment to reach the poll.
        thread::sleep(Duration::from_millis(5));
        SAFEPOINT_PARK_REQUESTED.store(0, Ordering::SeqCst);

        handle
            .join()
            .expect("thread should finish after flag cleared");
    }
}
