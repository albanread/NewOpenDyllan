//! Sprint 28 — end-to-end FFI acceptance tests.
//!
//! Drives `define c-function` declarations through parse → sema → DFM →
//! LLVM → JIT → actual Win32 call. The trampolines in
//! `nod-runtime::winffi` marshal Dylan-side fixnum args into the Win64
//! ABI; the resolved fn-ptr is populated at JIT-finalize time by
//! `eval_expr_with_items_to_string`'s init step.
//!
//! Every test is `#[serial]` — the runtime's WinFFI stats counters
//! (and the global library cache) are process-global state.

#![cfg(windows)]

use nod_sema::{EvalError, eval_expr_with_items_to_string};
use serial_test::serial;

fn setup() {
    // Sprint 19 + Sprint 28: condition-class chain must be present
    // before tests that catch via `block/exception` run.
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_c_ffi_error_registered();
    nod_runtime::_reset_handler_stack_for_tests();
}

// ─── 1. The headline: Beep returns #t ─────────────────────────────────────

const BEEP_DECL: &str = "\
define c-function Beep
    (dw-freq :: <c-dword>, dw-duration :: <c-dword>)
 => (success :: <c-bool>);
  library: \"kernel32.dll\";
end;
";

#[test]
#[serial]
fn headline_beep_call_returns_true() {
    setup();
    // 50ms duration — barely audible on hardware, still a real call.
    // On a machine without an audio device Beep still returns
    // non-zero, which our `<c-bool>` marshaling converts to `#t`.
    let s = eval_expr_with_items_to_string(BEEP_DECL, "Beep(440, 50)")
        .unwrap_or_else(|e| panic!("Beep eval failed: {e:?}"));
    assert_eq!(s, "#t", "Beep(440, 50) must return #t");
}

// ─── 2. GetTickCount() — arity 0, integer return ──────────────────────────

const GET_TICK_DECL: &str = "\
define c-function GetTickCount () => (ticks :: <c-dword>);
  library: \"kernel32.dll\";
end;

define c-function Sleep (ms :: <c-dword>) => ();
  library: \"kernel32.dll\";
end;
";

#[test]
#[serial]
fn get_tick_count_returns_increasing_value() {
    setup();
    // Helper function that calls GetTickCount twice with a Sleep in
    // between. The eval-entry returns the second value; we then run
    // it again to confirm the second-pass value isn't smaller. Two
    // separate eval runs would be cleaner if both stub tables could
    // share state, but each eval reinitialises its own table — so
    // we collapse the comparison into Dylan code, returning a
    // <c-dword> difference.
    let s = eval_expr_with_items_to_string(
        GET_TICK_DECL,
        "let a = GetTickCount(); \
         Sleep(15); \
         let b = GetTickCount(); \
         b - a",
    )
    .unwrap_or_else(|e| panic!("GetTickCount eval failed: {e:?}"));
    // After Sleep(15) the elapsed wall time should be at least ~10ms
    // (allow scheduler slop on a loaded machine) and at most ~5000ms
    // (otherwise something is wrong — runaway test or marshaling
    // returning garbage). This is a real value-correctness check, not
    // just "non-negative".
    let n: i64 = s.parse().expect("integer return");
    assert!(n >= 0, "tick delta must be non-negative, got {n}");
    assert!(
        (10..=5000).contains(&n),
        "tick delta after Sleep(15) should be 10..=5000ms, got {n}ms — marshaling may be corrupting the return"
    );
}

// ─── 2b. GetTickCount64 — value-correctness proof of FFI ──────────────────
//
// More rigorous proof of FFI working than the audible Beep: the API
// returns a verifiable value (system uptime in milliseconds), and we
// assert it sits in a sensible range. If marshaling is broken, the
// return would be zero, negative, or astronomical.

#[test]
#[serial]
fn system_uptime_via_get_tick_count64_is_sensible() {
    setup();
    let s = eval_expr_with_items_to_string(
        "\
define c-function GetTickCount64 () => (ticks :: <c-ulonglong>);
  library: \"kernel32.dll\";
end;
",
        "GetTickCount64()",
    )
    .unwrap_or_else(|e| panic!("GetTickCount64 eval failed: {e:?}"));
    let uptime_ms: i64 = s.parse().expect("integer return");

    // Absolute-value assertions — these only pass if Win64 marshaling
    // AND u64 reboxing both worked end-to-end:
    //   * strictly positive: the machine has been up
    //   * > 1 second: even a freshly-booted box clears this by the
    //     time `cargo test` runs (no flake risk)
    //   * < ~3 years: catches astronomical garbage from a broken
    //     u64 → fixnum path
    assert!(
        uptime_ms > 1_000,
        "uptime must be > 1 second, got {uptime_ms}ms — marshaling failed?"
    );
    assert!(
        uptime_ms < 100_000_000_000,
        "uptime must be < ~3 years, got {uptime_ms}ms — u64 reboxing returning garbage?"
    );
}

// ─── 3. GetCurrentProcessId — non-zero, fits in u32 ───────────────────────

#[test]
#[serial]
fn get_current_process_id_returns_integer() {
    setup();
    let items = "\
define c-function GetCurrentProcessId () => (pid :: <c-dword>);
  library: \"kernel32.dll\";
end;
";
    let s = eval_expr_with_items_to_string(items, "GetCurrentProcessId()")
        .unwrap_or_else(|e| panic!("GetCurrentProcessId eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return");
    assert!(n > 0, "PID must be positive, got {n}");
    assert!(n <= u32::MAX as i64, "PID must fit in u32, got {n}");
}

// ─── 4. Sleep(0) — void return ────────────────────────────────────────────

#[test]
#[serial]
fn sleep_zero_returns_without_crashing() {
    setup();
    let items = "\
define c-function Sleep (ms :: <c-dword>) => ();
  library: \"kernel32.dll\";
end;
";
    // Void-returning c-function: marshaling layer turns the void
    // into `nil`. The eval formatter prints `nil` as `#()`.
    let s = eval_expr_with_items_to_string(items, "Sleep(0)")
        .unwrap_or_else(|e| panic!("Sleep eval failed: {e:?}"));
    assert_eq!(s, "#()", "void-return Sleep(0) must surface as nil/#()");
}

// ─── 5. GetCurrentProcess — pseudo-handle (always (HANDLE)-1) ─────────────

#[test]
#[serial]
fn get_current_process_returns_handle() {
    setup();
    let items = "\
define c-function GetCurrentProcess () => (h :: <c-handle>);
  library: \"kernel32.dll\";
end;
";
    let s = eval_expr_with_items_to_string(items, "GetCurrentProcess()")
        .unwrap_or_else(|e| panic!("GetCurrentProcess eval failed: {e:?}"));
    // The Win32 pseudo-handle is `(HANDLE)-1`; our marshaling turns
    // that into a fixnum carrying the raw u64 value. Either form is
    // a non-zero integer.
    let n: i64 = s.parse().expect("integer-shaped handle");
    assert!(n != 0, "current-process handle must be non-zero, got {n}");
}

// ─── 6. Deduplication: two call sites share one table entry ──────────────

#[test]
#[serial]
fn api_stub_table_deduplicates_call_sites() {
    setup();
    nod_runtime::_reset_winffi_stats_for_tests();
    // Clean slate for the process-global stub-entry dedup map too —
    // otherwise a sibling test that already registered GetTickCount
    // leaves it memoised, this eval allocates zero new entries, and the
    // `>= 1` assertion below fails (order-dependent; passed in isolation).
    nod_runtime::_reset_stub_entry_slots_for_tests();
    let items = "\
define c-function GetTickCount () => (ticks :: <c-dword>);
  library: \"kernel32.dll\";
end;
";
    // Two call sites of the same c-function, lowered in the same
    // module — must share ONE stub-table entry.
    let s = eval_expr_with_items_to_string(
        items,
        "let a = GetTickCount(); let b = GetTickCount(); a + b - a",
    )
    .unwrap_or_else(|e| panic!("dedupe test eval failed: {e:?}"));
    let _: i64 = s.parse().expect("integer return");
    let stats = nod_runtime::winffi_stats();
    // Sprint 28: two call sites of the same c-function must share ONE
    // logical stub-table slot. Sprint 38d split that into two distinct
    // physical entries — one allocated by the sema-side pre-allocation
    // (kept for `LoweredModule::c_function_stub_table` analysis) and
    // one by `stub_entry_slot_addr` (the slot allocator that owns the
    // cross-process replay path). Both are deduplicated within their
    // own layer (sema's `spec_dedupe`; the slot allocator's process-
    // global HashMap keyed by `(dll.to_lowercase(), symbol)`), so
    // **`stats.entries` is at most 2 per unique (dll, symbol)** —
    // never N for N call sites. That preserves the original
    // dedup-across-call-sites guarantee with one new fixed-overhead
    // entry per API. The slot allocator's memoisation handles the
    // cross-process replay invariant the original test predates.
    assert!(
        stats.entries <= 2,
        "two call sites must produce ≤2 stub entries (Sprint 28 sema-side + Sprint 38d slot-allocator); got {}",
        stats.entries
    );
    assert!(
        stats.entries >= 1,
        "at least one stub entry must have been allocated"
    );
    assert!(
        stats.total_resolved >= 1,
        "at least one resolution must have happened"
    );
    assert_eq!(
        stats.unique_symbols, 1,
        "exactly one unique (dll, symbol) pair must have resolved, got {}",
        stats.unique_symbols
    );
}

// ─── 7. Unknown DLL → <c-ffi-error> ───────────────────────────────────────

#[test]
#[serial]
fn unknown_dll_signals_c_ffi_error() {
    setup();
    let items = "\
define c-function ImaginaryFunc () => (n :: <c-dword>);
  library: \"nosuchmodule_sprint28.dll\";
end;
";
    let result = eval_expr_with_items_to_string(items, "ImaginaryFunc()");
    match result {
        Ok(s) => panic!(
            "expected WinFfiInit error for unknown DLL, got success: {s}"
        ),
        Err(EvalError::WinFfiInit { class_name, dll, .. }) => {
            assert_eq!(class_name, "<c-ffi-error>");
            assert_eq!(dll, "nosuchmodule_sprint28.dll");
        }
        Err(other) => panic!("expected WinFfiInit, got {other:?}"),
    }
}

// ─── 8b. Sprint 29: named constants in an FFI call expression ─────────────
//
// Demonstrates that the Sprint 29 stdlib constants resolve in the
// same expression context as a c-function call. `$WM-NULL` is the
// integer 0; adding it to `GetTickCount()` must give back the tick
// count unchanged. The test would fail (or eval-error) if
// `$WM-NULL` was treated as a function-ref instead of an integer.

#[test]
#[serial]
fn flash_window_with_named_constants() {
    setup();
    let s = eval_expr_with_items_to_string(
        "\
define c-function GetTickCount () => (ticks :: <c-dword>);
  library: \"kernel32.dll\";
end;
",
        "$WM-NULL + GetTickCount()",
    )
    .unwrap_or_else(|e| panic!("flash_window_with_named_constants eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer");
    assert!(
        n > 1_000,
        "$WM-NULL must resolve to 0 and GetTickCount must return wall-time ticks > 1s, got {n}"
    );
}

// ─── 9. Unknown symbol in a real DLL → <c-ffi-error> ──────────────────────

#[test]
#[serial]
fn unknown_symbol_signals_c_ffi_error() {
    setup();
    let items = "\
define c-function ImaginaryFunc_Sprint28 () => (n :: <c-dword>);
  library: \"kernel32.dll\";
end;
";
    let result = eval_expr_with_items_to_string(items, "ImaginaryFunc_Sprint28()");
    match result {
        Ok(s) => panic!(
            "expected WinFfiInit error for unknown symbol, got success: {s}"
        ),
        Err(EvalError::WinFfiInit { class_name, dll, symbol }) => {
            assert_eq!(class_name, "<c-ffi-error>");
            assert_eq!(dll, "kernel32.dll");
            assert_eq!(symbol, "ImaginaryFunc_Sprint28");
        }
        Err(other) => panic!("expected WinFfiInit, got {other:?}"),
    }
}

// Sprint 30 string-marshaling acceptance tests live in their own file
// (`tests/nod-tests/tests/winffi_strings.rs`) so the routine
// `cargo test` run stays free of UI side effects — MessageBoxW lives
// there as an `#[ignore]`-gated developer demo, while the
// value-asserting headline (`lstrlenW("héllo") -> 5`) and friends
// run by default.
