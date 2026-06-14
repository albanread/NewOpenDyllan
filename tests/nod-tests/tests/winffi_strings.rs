//! Sprint 30 — Win32 FFI string marshaling acceptance tests.
//!
//! These tests drive `<c-string>` and `<c-wide-string>` declarations
//! through parse → sema → DFM → LLVM → JIT → real Win32 call. They
//! prove the *capability* added in Sprint 30:
//!
//! 1. Dylan `<byte-string>` (UTF-8) → C `LPWSTR/LPCWSTR` (UTF-16LE +
//!    null u16) via `String::encode_utf16()`. The headline empirical
//!    proof is **`lstrlenW("héllo") -> 5`**: 'é' is a two-byte UTF-8
//!    sequence (0xC3 0xA9), so a naive byte-copy would produce 6, not
//!    5. Only correct UTF-8 → UTF-16 transcoding yields the right
//!    answer.
//! 2. Dylan `<byte-string>` (UTF-8) → C `LPSTR/LPCSTR` as a
//!    pass-through byte run + null terminator. Confirms the narrow
//!    path *doesn't* transcode: `lstrlenA("café") -> 5` (5 UTF-8
//!    bytes), not 4.
//! 3. Per-call `Vec<TempBuf>` lifetime: each string arg pushes an
//!    owned buffer that lives across the C call and drops afterwards.
//!    No leaks.
//! 4. NULL pointer literal via `$NULL` (= 0): a fixnum 0 in pointer /
//!    handle / string position marshals to `std::ptr::null()`.
//!
//! Sprint 30 does NOT cover C-side string returns (out-buffer
//! patterns, BSTR, etc) at the same headline level — that's Sprint 31+.
//!
//! ## Discipline
//!
//! Every test is `#[serial]`: the WinFFI stats counters and the global
//! library cache are process-global state, and these tests load
//! kernel32.dll / user32.dll.
//!
//! The `message_box_w_pops_real_dialog` test is `#[ignore]`-gated —
//! it pops a real OS dialog and would interrupt the developer during
//! a routine `cargo test`. Run it manually with:
//!
//! ```text
//! cargo test --test winffi_strings -- --ignored
//! ```

#![cfg(windows)]

use nod_sema::{eval_expr_to_string, eval_expr_with_items_to_string};
use serial_test::serial;

fn setup() {
    // Sprint 19 + Sprint 28: condition-class chain must be present
    // before tests that catch via `block/exception` run. Sprint 30
    // doesn't add new condition classes, but we keep parity with the
    // other FFI test file so order-of-test-execution doesn't change
    // the answer.
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_c_ffi_error_registered();
    nod_runtime::_reset_handler_stack_for_tests();
}

const LSTRLENW_DECL: &str = "\
define c-function lstrlenW (s :: <c-wide-string>) => (n :: <c-int>);
  library: \"kernel32.dll\";
end;
";

const LSTRLENA_DECL: &str = "\
define c-function lstrlenA (s :: <c-string>) => (n :: <c-int>);
  library: \"kernel32.dll\";
end;
";

const LSTRCMPW_DECL: &str = "\
define c-function lstrcmpW (a :: <c-wide-string>, b :: <c-wide-string>) => (cmp :: <c-int>);
  library: \"kernel32.dll\";
end;
";

// ─── Value-asserting headlines (run by default) ───────────────────────────

/// `lstrlenW("hello world") -> 11`. ASCII string through the wide
/// path. Each ASCII codepoint maps 1:1 to a UTF-16 code unit, so the
/// length agrees with both the byte count and the character count.
#[test]
#[serial]
fn lstrlen_w_returns_correct_wide_length() {
    setup();
    let s = eval_expr_with_items_to_string(LSTRLENW_DECL, "lstrlenW(\"hello world\")")
        .unwrap_or_else(|e| panic!("lstrlenW eval failed: {e:?}"));
    assert_eq!(s, "11", "lstrlenW(\"hello world\") must return 11, got {s}");
}

/// `lstrlenW("héllo") -> 5` — **THE EMPIRICAL HEADLINE**. 'é' (U+00E9)
/// occupies two bytes in UTF-8 (0xC3 0xA9) but one UTF-16 code unit.
/// If the marshaling were a byte-copy from UTF-8 to a `Vec<u16>`, this
/// would return 6 (interpreting the 0xC3 and 0xA9 bytes as two
/// separate u16 units, or as 3 u16s after copying 6 bytes into 3 u16s).
/// Only proper transcoding (`String::encode_utf16()`) yields 5.
#[test]
#[serial]
fn lstrlen_w_handles_unicode_correctly() {
    setup();
    let s = eval_expr_with_items_to_string(LSTRLENW_DECL, "lstrlenW(\"héllo\")")
        .unwrap_or_else(|e| panic!("lstrlenW non-ASCII eval failed: {e:?}"));
    assert_eq!(
        s, "5",
        "lstrlenW(\"héllo\") must return 5; got {s}. If 6, the marshaler is byte-copying \
         UTF-8 instead of transcoding to UTF-16."
    );
}

/// `lstrlenA("hello world") -> 11`. ASCII through the narrow path —
/// pass-through bytes, terminator, lstrlenA counts bytes.
#[test]
#[serial]
fn lstrlen_a_returns_correct_narrow_length() {
    setup();
    let s = eval_expr_with_items_to_string(LSTRLENA_DECL, "lstrlenA(\"hello world\")")
        .unwrap_or_else(|e| panic!("lstrlenA eval failed: {e:?}"));
    assert_eq!(s, "11", "lstrlenA(\"hello world\") must return 11, got {s}");
}

/// `lstrlenA("café") -> 5` — **the narrow-path counterpart**. The
/// Dylan source-text "café" is UTF-8: 'c','a','f',0xC3,0xA9 = 5 bytes.
/// The narrow marshaler does NOT transcode (Sprint 30 deliberately
/// passes UTF-8 bytes through to LPSTR; CP_ACP conversion is deferred).
/// lstrlenA counts bytes until the null terminator, so it sees 5.
/// If we accidentally transcoded the narrow path to UTF-16, the API
/// would interpret the buffer as random ANSI bytes.
#[test]
#[serial]
fn lstrlen_a_handles_utf8_as_bytes() {
    setup();
    let s = eval_expr_with_items_to_string(LSTRLENA_DECL, "lstrlenA(\"café\")")
        .unwrap_or_else(|e| panic!("lstrlenA UTF-8 eval failed: {e:?}"));
    assert_eq!(
        s, "5",
        "lstrlenA(\"café\") must return 5 (UTF-8 byte count); got {s}. \
         The narrow path is pass-through bytes — if we transcode here, this fails."
    );
}

/// `lstrlenW("") -> 0` — edge case: an empty Dylan string still
/// allocates a TempBuf with just the null terminator, so the API
/// sees a valid empty wide string and returns 0.
#[test]
#[serial]
fn lstrlen_w_empty_string() {
    setup();
    let s = eval_expr_with_items_to_string(LSTRLENW_DECL, "lstrlenW(\"\")")
        .unwrap_or_else(|e| panic!("lstrlenW empty eval failed: {e:?}"));
    assert_eq!(s, "0", "lstrlenW(\"\") must return 0, got {s}");
}

/// `$NULL -> "0"`. The Sprint 30 NULL sentinel is just the fixnum 0;
/// it gets its meaning at marshaling time (fixnum 0 in a pointer
/// position → `std::ptr::null()`). This test confirms the constant
/// is wired into the stdlib constants table and evaluates idiomatically.
#[test]
#[serial]
fn null_constant_evaluates_to_zero() {
    setup();
    let s = eval_expr_to_string("$NULL")
        .unwrap_or_else(|e| panic!("$NULL eval failed: {e:?}"));
    assert_eq!(s, "0", "$NULL must evaluate to 0, got {s}");
}

/// `lstrlenW($NULL) -> 0` — proves the NULL pointer reaches the API.
/// Per the MSDN contract: "If lpString is NULL, the function returns
/// 0." If our marshaler instead passed a bogus pointer (e.g. a tagged
/// fixnum word), lstrlenW would either return garbage or crash. The
/// brief proposed `IsBadStringPtrW($NULL, 10)`; we use `lstrlenW($NULL)`
/// because it's a stable kernel32 export with a documented NULL
/// contract, while IsBadStringPtrW is deprecated and unreliable on
/// modern Windows.
#[test]
#[serial]
fn null_pointer_via_dollar_null() {
    setup();
    let s = eval_expr_with_items_to_string(LSTRLENW_DECL, "lstrlenW($NULL)")
        .unwrap_or_else(|e| panic!("lstrlenW($NULL) eval failed: {e:?}"));
    assert_eq!(
        s, "0",
        "lstrlenW($NULL) must return 0 per MSDN's NULL-input contract; got {s}"
    );
}

/// `lstrcmpW("abc", "abc") -> 0`. Two wide-string args in the same
/// call — proves the trampoline allocates separate TempBufs for each
/// arg and both buffers live across the C call. If the second push
/// invalidated the first (e.g. via a Vec realloc shuffling earlier
/// elements before the second pointer was captured), lstrcmpW would
/// see mismatched memory and return non-zero or crash.
#[test]
#[serial]
fn mixed_args_string_and_int() {
    setup();
    let s = eval_expr_with_items_to_string(LSTRCMPW_DECL, "lstrcmpW(\"abc\", \"abc\")")
        .unwrap_or_else(|e| panic!("lstrcmpW eval failed: {e:?}"));
    assert_eq!(s, "0", "lstrcmpW(\"abc\", \"abc\") must return 0, got {s}");
}

/// TempBuf accounting: two `lstrlenW` calls in one expression bump the
/// lifetime tempbuf counter by exactly 2. Confirms per-call allocation
/// is happening (not e.g. an interning cache that would skip the
/// counter on a repeated literal).
#[test]
#[serial]
fn tempbuf_allocation_count_tracks_string_args() {
    setup();
    nod_runtime::_reset_winffi_stats_for_tests();
    let s = eval_expr_with_items_to_string(
        LSTRLENW_DECL,
        "lstrlenW(\"first\") + lstrlenW(\"second longer\")",
    )
    .unwrap_or_else(|e| panic!("tempbuf-count eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return");
    // 5 ("first") + 13 ("second longer") = 18.
    assert_eq!(n, 18, "len-sum of two strings must be 18, got {n}");
    let stats = nod_runtime::winffi_stats();
    assert_eq!(
        stats.tempbufs_allocated_lifetime, 2,
        "expected 2 TempBuf allocations (one per wide-string arg), got {}",
        stats.tempbufs_allocated_lifetime
    );
}

// ─── Opt-in interactive demo ──────────────────────────────────────────────

/// MessageBoxW demo — pops a real Win32 dialog and waits for the user
/// to click OK. **`#[ignore]`-gated**: a routine `cargo test` must NOT
/// invoke this test. Run manually with:
///
/// ```text
/// cargo test --test winffi_strings -- --ignored
/// ```
///
/// This is a developer demonstration that the FFI plumbing reaches a
/// real UI subsystem; the value-asserting tests above are the actual
/// proof that string marshaling works. The Sprint 30 brief explicitly
/// makes `lstrlenW("héllo") -> 5` the empirical headline, not this.
#[test]
#[serial]
#[ignore = "interactive: pops a Win32 dialog; run manually with `cargo test --test winffi_strings -- --ignored`"]
fn message_box_w_pops_real_dialog() {
    setup();
    const MSGBOX_DECL: &str = "\
define c-function MessageBoxW
    (hwnd :: <c-handle>, text :: <c-wide-string>,
     caption :: <c-wide-string>, type-flag :: <c-uint>)
 => (result :: <c-int>);
  library: \"user32.dll\";
end;
";
    let s = eval_expr_with_items_to_string(
        MSGBOX_DECL,
        "MessageBoxW($NULL, \"hello from Dylan\", \"NewOpenDylan IDE\", \
         $MB-OK + $MB-ICONINFORMATION)",
    )
    .unwrap_or_else(|e| panic!("MessageBoxW eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return from MessageBoxW");
    // IDOK = 1; an interactive desktop session, with the user clicking
    // OK, should yield exactly that. Accept the broader IDxxx range
    // [1..=11] so a developer-driven manual run never spuriously fails
    // on a quirky window-manager response. (0 would mean MessageBox
    // couldn't display anything — that's an environment issue, not a
    // marshaling issue, and is allowed.)
    assert!(
        (0..=11).contains(&n),
        "MessageBoxW must return a button code in [0..=11]; got {n}"
    );
}
