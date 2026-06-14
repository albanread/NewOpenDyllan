//! Sprint 29 — Win32 named constants acceptance tests.
//!
//! These tests cover the end-to-end resolution path for the curated
//! `$MB-OK`, `$WM-PAINT`, … set: from the source file
//! `data/win32_constants.txt`, through the build-time embedding into
//! `nod-winapi`, through the generator-emitted
//! `src/nod-dylan/dylan-sources/win32-constants.dylan`, through the
//! stdlib loader's process-global `STDLIB_CONSTANTS` table, into
//! user-code lowering where `Expr::Ident("$MB-OK")` becomes
//! `ConstValue::Integer(0)`.
//!
//! Every test is `#[serial]` because they share the process-global
//! stdlib loader state. A single load happens lazily on the first
//! `eval_expr_to_string` call; subsequent calls observe the same
//! table.

use nod_sema::{EvalError, eval_expr_to_string};
use serial_test::serial;

// ─── 1. Basic resolution — small zero-value flag ──────────────────────────

#[test]
#[serial]
fn mb_ok_resolves_to_zero() {
    let s =
        eval_expr_to_string("$MB-OK").unwrap_or_else(|e| panic!("$MB-OK eval failed: {e:?}"));
    assert_eq!(s, "0", "$MB-OK must resolve to the integer 0");
}

// ─── 2. Window message PAINT — small hex value ────────────────────────────

#[test]
#[serial]
fn wm_paint_resolves_to_15() {
    let s = eval_expr_to_string("$WM-PAINT")
        .unwrap_or_else(|e| panic!("$WM-PAINT eval failed: {e:?}"));
    assert_eq!(s, "15", "$WM-PAINT must resolve to 0x000F = 15");
}

// ─── 3. Icon flag with hex value ──────────────────────────────────────────

#[test]
#[serial]
fn mb_iconerror_resolves_to_16() {
    let s = eval_expr_to_string("$MB-ICONERROR")
        .unwrap_or_else(|e| panic!("$MB-ICONERROR eval failed: {e:?}"));
    assert_eq!(s, "16", "$MB-ICONERROR must resolve to 0x10 = 16");
}

// ─── 4. Computed mask — WS_OVERLAPPEDWINDOW ───────────────────────────────
//
// WS_OVERLAPPEDWINDOW = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU
//                       | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX
//                     = 0 | 0xC00000 | 0x80000 | 0x40000 | 0x20000 | 0x10000
//                     = 0x00CF0000 = 13565952
//
// We curated the value as 0x00CF0000 directly; this test pins the
// expected decimal so that the build-time hex→i64 round-trip stays
// honest.

#[test]
#[serial]
fn ws_overlappedwindow_is_complex_mask() {
    let s = eval_expr_to_string("$WS-OVERLAPPEDWINDOW")
        .unwrap_or_else(|e| panic!("$WS-OVERLAPPEDWINDOW eval failed: {e:?}"));
    assert_eq!(
        s, "13565952",
        "$WS-OVERLAPPEDWINDOW must resolve to 0x00CF0000 = 13565952"
    );
}

// ─── 5. Negative constant — GWL_STYLE offset ──────────────────────────────

#[test]
#[serial]
fn gwl_style_resolves_to_minus_16() {
    let s = eval_expr_to_string("$GWL-STYLE")
        .unwrap_or_else(|e| panic!("$GWL-STYLE eval failed: {e:?}"));
    assert_eq!(s, "-16", "$GWL-STYLE must resolve to -16");
}

// ─── 6. Unknown constant — proper UndefinedIdent at lowering ──────────────

#[test]
#[serial]
fn unknown_constant_errors_at_lower() {
    let result = eval_expr_to_string("$NOT-A-REAL-CONSTANT");
    match result {
        Ok(s) => panic!(
            "expected lowering error for $NOT-A-REAL-CONSTANT, got success: {s}"
        ),
        Err(EvalError::Lower(errors)) => {
            // We expect at least one UndefinedIdent diagnostic.
            let msg = errors
                .iter()
                .map(|e| format!("{e}"))
                .collect::<Vec<_>>()
                .join("; ");
            assert!(
                msg.contains("undefined ident") && msg.contains("$NOT-A-REAL-CONSTANT"),
                "expected an `undefined ident `$NOT-A-REAL-CONSTANT`` diagnostic, got: {msg}"
            );
        }
        Err(other) => panic!("expected EvalError::Lower, got {other:?}"),
    }
}

// ─── 7. Constants compose under arithmetic ────────────────────────────────
//
// `$MB-OK + $MB-ICONERROR` proves both names resolve as integers
// (not function-refs) in the same expression. A function-ref would
// fail to lower under `+` or, worse, type-mismatch at run-time.

#[test]
#[serial]
fn constant_usable_in_arithmetic() {
    let s = eval_expr_to_string("$MB-OK + $MB-ICONERROR")
        .unwrap_or_else(|e| panic!("$MB-OK + $MB-ICONERROR eval failed: {e:?}"));
    assert_eq!(s, "16", "0 + 16 = 16");
}

// ─── 8. Lock the lower-bound on coverage at the nod-winapi layer ──────────
//
// This test lives here (rather than in `nod-winapi/tests/lookup.rs`)
// because it asserts an end-to-end property: stdlib registers at
// least 50 constants. If `data/win32_constants.txt` were ever
// emptied or the loader skipped the file, this would flag it.

#[test]
#[serial]
fn stdlib_constants_count_at_least_50() {
    // Force the stdlib to load by evaluating a constant first.
    let _ = eval_expr_to_string("$MB-OK").expect("stdlib loads on first eval");
    let table = nod_sema::stdlib::constants_table()
        .expect("STDLIB_CONSTANTS must be populated after ensure_loaded");
    assert!(
        table.len() >= 50,
        "stdlib constants count is {}; expected at least 50",
        table.len()
    );
    // Spot-check the canonical names — the curated file would need
    // major surgery for any of these to disappear.
    for needle in ["$MB-OK", "$WM-PAINT", "$WS-OVERLAPPEDWINDOW", "$SW-SHOW"] {
        assert!(
            table.contains_key(needle),
            "expected stdlib constants to carry {needle}"
        );
    }
}

// ─── 9. nod-winapi iter_constants lower bound ─────────────────────────────
//
// Separately from the stdlib-table count, the embedded blob's
// `iter_constants()` API must surface at least 50 entries — this
// catches a regression where build.rs skips the curated file but
// the generated `.dylan` is still in the tree.

#[test]
fn winapi_iter_constants_count_at_least_50() {
    let count = nod_winapi::iter_constants().count();
    assert!(
        count >= 50,
        "nod_winapi::iter_constants() returned {count}; expected at least 50"
    );
    // The Sprint 27 stats() reading must agree with iter_constants().
    let stats = nod_winapi::stats();
    assert_eq!(stats.constant_count, count);
}
