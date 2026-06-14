//! Sprint 32 — Win32 callbacks (closure → C function pointer).
//!
//! Drives Dylan closures through the Sprint 32 trampoline-pool path
//! and confirms the OS can call them through the standard Win64 C
//! ABI. The headline acceptance is **`EnumWindows`** enumerating the
//! desktop's top-level windows, with the callback incrementing a
//! Dylan-side counter via captured-variable mutation — proving every
//! Sprint 32 piece (closure creation, callback registration,
//! trampoline dispatch, arg marshaling, env-bound counter, return
//! marshaling, Sprint 28 stub-table integration) works end-to-end in
//! one expression.
//!
//! Sprint 32 scope: `WNDPROC` and `WNDENUMPROC` only. The fixed pool
//! is 32 slots per signature; pool exhaustion signals
//! `<c-ffi-error>`. There is no callback release path in Sprint 32 —
//! every `as-wndproc-callback` / `as-wndenumproc-callback` consumes a
//! slot for the process lifetime (leak-by-design).
//!
//! ## Discipline
//!
//! Every test is `#[serial]`. The callback registry, the WinFFI stats
//! counters, and the global library cache are all process-global
//! state. `_reset_callbacks_for_tests()` clears the registry between
//! tests so the 32-slot cap doesn't leak between cases.

#![cfg(windows)]
// Test fn names mirror the Win32 API names being exercised.
#![allow(non_snake_case)]

use nod_sema::{eval_expr_to_string, eval_expr_with_items_to_string};
use serial_test::serial;

fn setup() {
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_c_ffi_error_registered();
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::_reset_callbacks_for_tests();
}

// ─── Headline acceptance: EnumWindows ─────────────────────────────────────

const ENUM_WINDOWS_DECL: &str = "\
define c-function EnumWindows
    (callback :: <c-pointer>, lparam :: <c-pointer>)
 => (success :: <c-bool>);
  library: \"user32.dll\";
end;
";

/// **The Sprint 32 headline.** `EnumWindows` invokes our Dylan
/// closure once per top-level desktop window. The closure increments
/// a counter captured by reference (via Dylan's cell-promotion). The
/// counter must end up > 0 (every Windows machine has the taskbar at
/// minimum) and reasonable (< 100_000, otherwise something is
/// looping uncontrollably).
#[test]
#[serial]
fn enum_windows_invokes_callback_for_each_top_level_window() {
    setup();
    let s = eval_expr_with_items_to_string(
        ENUM_WINDOWS_DECL,
        "let count = 0; \
         let cb = method (hwnd, lp) count := count + 1; #t end; \
         let cb-ptr = as-wndenumproc-callback(cb); \
         EnumWindows(cb-ptr, $NULL); \
         count",
    )
    .unwrap_or_else(|e| panic!("EnumWindows test failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return");
    // A normal desktop has at least 5-10 top-level windows (taskbar,
    // explorer, devenv, conhost, etc.). The bound is intentionally
    // loose so the test isn't flaky across machines.
    assert!(
        n > 0,
        "EnumWindows must invoke the callback at least once, got count={n}"
    );
    assert!(
        n < 100_000,
        "callback count is suspiciously large ({n}); suggests runaway loop or marshaling bug"
    );
    eprintln!("[sprint-32 headline] EnumWindows enumerated {n} top-level windows");
    nod_runtime::_reset_callbacks_for_tests();
}

// ─── Supporting tests ─────────────────────────────────────────────────────

/// `as-wndenumproc-callback(method...)` returns a non-zero pointer
/// (the trampoline's address). The fixnum-tagged return rides through
/// the `<c-pointer>` ABI Sprint 28+ adopted.
#[test]
#[serial]
fn register_wndenumproc_returns_non_null_pointer() {
    setup();
    let s = eval_expr_to_string(
        "as-wndenumproc-callback(method (hwnd, lp) #t end)",
    )
    .unwrap_or_else(|e| panic!("register-wndenumproc eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return");
    assert!(n != 0, "trampoline pointer must be non-zero, got {n}");
    nod_runtime::_reset_callbacks_for_tests();
}

/// Same for `WNDPROC`. Builds an arity-4 closure since that's the
/// signature `WNDPROC` expects (HWND, UINT, WPARAM, LPARAM).
#[test]
#[serial]
fn register_wndproc_returns_non_null_pointer() {
    setup();
    let s = eval_expr_to_string(
        "as-wndproc-callback(method (hwnd, msg, wp, lp) 0 end)",
    )
    .unwrap_or_else(|e| panic!("register-wndproc eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return");
    assert!(n != 0, "trampoline pointer must be non-zero, got {n}");
    nod_runtime::_reset_callbacks_for_tests();
}

/// Two distinct closures must land in distinct pool slots and
/// therefore have distinct trampoline addresses. This is what makes
/// per-slot dispatch work — each slot trampoline has a unique
/// address, and Win32 stores the address; the dispatcher recovers
/// the closure from the slot ID baked into the trampoline body.
#[test]
#[serial]
fn two_callbacks_get_distinct_addresses() {
    setup();
    let s = eval_expr_with_items_to_string(
        "",
        "let a = as-wndenumproc-callback(method (h, l) #t end); \
         let b = as-wndenumproc-callback(method (h, l) #f end); \
         if (a = b) 0 else 1 end",
    )
    .unwrap_or_else(|e| panic!("distinct-addresses eval failed: {e:?}"));
    assert_eq!(
        s, "1",
        "two distinct closures must map to distinct trampoline addresses; got equal addresses"
    );
    nod_runtime::_reset_callbacks_for_tests();
}

/// Filling the pool past the Sprint 32 cap (32 slots) returns
/// `Err(PoolFull)` from the Rust-side `register_callback`. The
/// Dylan-side surface translates that to a signalled `<c-ffi-error>`
/// (which would unwind out of the JIT'd entry as a panic in this
/// test harness — not asserted here, to avoid the process abort that
/// crossing `extern "system"` on Windows causes); the Rust-API check
/// suffices to prove the cap fires.
///
/// This test resets the pool at the end so the 33rd-attempt
/// saturation doesn't permanently fill the registry for subsequent
/// tests.
#[test]
#[serial]
fn callback_pool_full_signals_error() {
    setup();
    use nod_runtime::{CallbackSignature, register_callback};
    nod_runtime::ensure_functions_registered();
    nod_runtime::ensure_operator_shims_registered();
    // Use a pre-registered Rust function (`+` shim) so we don't
    // allocate 32 separate Dylan closures. Each `register_callback`
    // consumes a slot regardless of whether the closure Word is the
    // same.
    let f =
        nod_runtime::make_function_ref("+", 2).expect("+ shim available");
    // Fill all 32 slots.
    for i in 0..nod_runtime::CALLBACK_POOL_SIZE {
        register_callback(f, CallbackSignature::Wndenumproc).unwrap_or_else(
            |e| {
                panic!(
                    "slot {i} should be free in a fresh pool, got {e:?}"
                )
            },
        );
    }
    // The 33rd attempt must report PoolFull.
    let err = register_callback(f, CallbackSignature::Wndenumproc);
    assert!(
        matches!(err, Err(nod_runtime::CallbackRegisterError::PoolFull)),
        "33rd registration must report PoolFull, got {err:?}"
    );
    assert_eq!(
        nod_runtime::_callback_occupied_count_for_tests(
            CallbackSignature::Wndenumproc
        ),
        nod_runtime::CALLBACK_POOL_SIZE,
        "pool should be fully occupied at the saturation point"
    );
    nod_runtime::_reset_callbacks_for_tests();
    assert_eq!(
        nod_runtime::_callback_occupied_count_for_tests(
            CallbackSignature::Wndenumproc
        ),
        0,
        "reset should clear the pool for subsequent tests"
    );
}

/// Closures registered as callbacks must survive a GC cycle. We
/// register a closure (whose `<function>` instance lives in the
/// moveable heap), force a minor GC, and then invoke the trampoline
/// directly via Rust. The callback's body must still run.
///
/// The Dylan surface (`as-wndenumproc-callback(...)`) returns the
/// trampoline address as a fixnum. We pull that address out of the
/// eval and call it directly through an `extern "system"` fn-pointer
/// transmute — that's exactly what Win32 would do.
#[test]
#[serial]
fn closure_survives_gc_pressure() {
    setup();
    // Register a callback and pull its trampoline address.
    let s = eval_expr_to_string(
        "as-wndenumproc-callback(method (h, l) 12345 end)",
    )
    .unwrap_or_else(|e| panic!("gc-survival eval failed: {e:?}"));
    let addr: usize = s.parse::<i64>().expect("integer return") as usize;
    assert!(addr != 0, "trampoline address must be non-zero");

    // Force GC. Each `collect_minor` walks roots and evacuates
    // surviving young objects. Our `<function>` is in slot 0; the
    // registry cell is registered as a GC root, so the cell's Word
    // gets rewritten if the `<function>` moves.
    nod_runtime::collect_minor();
    nod_runtime::collect_minor();

    // Invoke the trampoline directly — what Win32 would do.
    let slot_fn: unsafe extern "system" fn(u64, u64) -> i32 =
        // SAFETY: `addr` is a real `wndenumproc_slot_N` function
        // address obtained from `as-wndenumproc-callback`. Its
        // declared ABI is `extern "system" fn(HWND, LPARAM) -> BOOL`,
        // matching `(u64, u64) -> i32` at the Win64 level.
        unsafe { std::mem::transmute(addr) };
    // SAFETY: see above; we just registered the slot via the Dylan
    // surface and the registry is still populated post-GC.
    let r = unsafe { slot_fn(0xCAFE, 0xBEEF) };
    // The closure returns fixnum 12345; `rebox_bool32` maps that to
    // BOOL TRUE (1) since 12345 != 0.
    assert_eq!(
        r, 1,
        "post-GC trampoline must invoke the closure and return BOOL TRUE; got {r}"
    );
    nod_runtime::_reset_callbacks_for_tests();
}
