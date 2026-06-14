//! Sprint 34 — `<c-struct>` family for IDE-essential Win32 shapes.
//!
//! Headline: a Dylan `make(<point>)` allocates a struct-shaped byte
//! payload; the marshaler passes `wrapper_ptr + 8` to a Win32 API
//! declared `LPPOINT`; the API populates the bytes; Dylan reads the
//! fields back via `point-x`/`point-y`. Real cursor coordinates flow
//! through the runtime — empirical proof that allocation, marshaling,
//! C-side mutation, and Dylan-side reading all work end-to-end.
//!
//! Sprint 34 covers six seed structs (POINT, RECT, SIZE, FILETIME,
//! SYSTEMTIME, MSG). All field offsets are documented in
//! `nod-runtime/src/structs.rs`.
//!
//! Every test is `#[serial]` because the runtime's class registry and
//! literal pool are process-global. `setup()` re-runs the standard
//! registration triple plus `ensure_structs_registered`.

#![cfg(windows)]
// Test fn names mirror the Win32 API names being exercised.
#![allow(non_snake_case)]

use nod_sema::{eval_expr_to_string, eval_expr_with_items_to_string};
use serial_test::serial;

fn setup() {
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_c_ffi_error_registered();
    nod_runtime::ensure_structs_registered();
    nod_runtime::_reset_handler_stack_for_tests();
}

// ─── Pure-Dylan field roundtrips (no Win32 dependency) ────────────────────

#[test]
#[serial]
fn point_alloc_zeroes_fields() {
    setup();
    let s = eval_expr_to_string("let p = make(<point>); point-x(p) + point-y(p)")
        .unwrap_or_else(|e| panic!("point alloc test failed: {e:?}"));
    assert_eq!(s, "0", "make(<point>) must zero-fill the payload");
}

#[test]
#[serial]
fn point_field_setter_roundtrip() {
    setup();
    let s = eval_expr_to_string(
        "let p = make(<point>); \
         point-x(p) := 42; \
         point-y(p) := 99; \
         point-x(p) + point-y(p)",
    )
    .unwrap_or_else(|e| panic!("point setter roundtrip failed: {e:?}"));
    assert_eq!(s, "141", "point setters must roundtrip through getters");
}

#[test]
#[serial]
fn rect_all_four_fields() {
    setup();
    let s = eval_expr_to_string(
        "let r = make(<rect>); \
         rect-left(r) := 10; \
         rect-top(r) := 20; \
         rect-right(r) := 100; \
         rect-bottom(r) := 200; \
         rect-right(r) - rect-left(r) + rect-bottom(r) - rect-top(r)",
    )
    .unwrap_or_else(|e| panic!("rect field test failed: {e:?}"));
    // width (100-10) + height (200-20) = 90 + 180 = 270
    assert_eq!(s, "270", "rect width + height must equal 270");
}

#[test]
#[serial]
fn systemtime_u16_field_roundtrip() {
    setup();
    let s = eval_expr_to_string(
        "let st = make(<systemtime>); \
         systemtime-year(st) := 2026; \
         systemtime-month(st) := 5; \
         systemtime-day(st) := 22; \
         systemtime-year(st) + systemtime-month(st) + systemtime-day(st)",
    )
    .unwrap_or_else(|e| panic!("systemtime roundtrip failed: {e:?}"));
    // 2026 + 5 + 22 = 2053
    assert_eq!(s, "2053", "systemtime u16 fields must roundtrip");
}

#[test]
#[serial]
fn msg_mixed_width_fields_roundtrip() {
    setup();
    // MSG carries pointer (hwnd), u32 (message), u64 (wParam), i64
    // (lParam), u32 (time), i32 (pt.x, pt.y), u32 (lPrivate). One
    // expression exercises every width.
    let s = eval_expr_to_string(
        "let m = make(<msg>); \
         msg-message(m) := 17; \
         msg-wparam(m) := 1000; \
         msg-lparam(m) := 2000; \
         msg-time(m) := 12345; \
         msg-pt-x(m) := 7; \
         msg-pt-y(m) := 8; \
         msg-message(m) + msg-wparam(m) + msg-lparam(m) + msg-time(m) \
           + msg-pt-x(m) + msg-pt-y(m)",
    )
    .unwrap_or_else(|e| panic!("msg field test failed: {e:?}"));
    // 17 + 1000 + 2000 + 12345 + 7 + 8 = 15377
    assert_eq!(s, "15377", "msg mixed-width fields must roundtrip");
}

#[test]
#[serial]
fn point_is_subclass_of_c_struct() {
    setup();
    let s = eval_expr_to_string("instance?(make(<point>), <c-struct>)")
        .unwrap_or_else(|e| panic!("instance? check failed: {e:?}"));
    assert_eq!(s, "#t", "make(<point>) must be an instance of <c-struct>");
}

// ─── Rust-side metadata checks ────────────────────────────────────────────

#[test]
#[serial]
fn instance_sizes_match_win64_sizeof() {
    setup();
    // instance_size = 8 (wrapper) + sizeof(struct on Win64).
    use nod_runtime::class_metadata_for;
    assert_eq!(class_metadata_for(nod_runtime::point_class_id()).instance_size, 16);
    assert_eq!(class_metadata_for(nod_runtime::rect_class_id()).instance_size, 24);
    assert_eq!(class_metadata_for(nod_runtime::size_class_id()).instance_size, 16);
    assert_eq!(class_metadata_for(nod_runtime::filetime_class_id()).instance_size, 16);
    assert_eq!(class_metadata_for(nod_runtime::systemtime_class_id()).instance_size, 24);
    assert_eq!(class_metadata_for(nod_runtime::msg_class_id()).instance_size, 56);
}

#[test]
#[serial]
fn point_survives_minor_gc() {
    setup();
    // Allocate a <point>, set x=42, force a minor GC, then read x back.
    // Without proper GC layout / scan, evacuation would corrupt the
    // payload or the wrapper.
    use nod_runtime::{Word, class_metadata_for, point_class_id, rust_make,
                      nod_struct_set_i32, nod_struct_get_i32};
    let md = class_metadata_for(point_class_id());
    // SAFETY: registered metadata, no init keywords.
    let w = unsafe { rust_make(md, &[]) };
    // Offsets passed as tagged Words — same shape JIT-emitted code
    // uses (a Dylan integer literal `0` materializes as `0 << 1 = 0`,
    // `4` as `8`, etc.). The primitive decodes the tag internally.
    let off0 = Word::fixnum_unchecked(0).raw();
    // SAFETY: w is a freshly-allocated <point>.
    unsafe {
        nod_struct_set_i32(Word::fixnum_unchecked(42).raw(), w.raw(), off0);
    }
    // Register w as a root so it survives the collection.
    let slot: Word = w;
    // SAFETY: slot is a stack local; we'll unregister before it goes
    // out of scope.
    unsafe { nod_runtime::nod_register_root(&slot as *const Word as *mut Word); }
    nod_runtime::collect_minor();
    let x = unsafe {
        let raw = nod_struct_get_i32(slot.raw(), off0);
        Word::from_raw(raw).as_fixnum().expect("integer")
    };
    // SAFETY: matching unregister.
    unsafe { nod_runtime::nod_unregister_root(&slot as *const Word as *mut Word); }
    assert_eq!(x, 42, "<point>.x must survive a minor GC unchanged");
}

// ─── Win32 headline acceptances ───────────────────────────────────────────

/// **The Sprint 34 headline.** GetCursorPos populates a POINT struct
/// with the current screen cursor coordinates. We allocate the struct
/// in Dylan, marshal a pointer to its payload, the OS writes the
/// coordinates, then Dylan reads them back via `point-x`/`point-y`.
///
/// The assertion is loose because cursor position is a real-world
/// number we don't control: a fresh test run can hit the cursor
/// anywhere on the desktop. We assert "non-negative and not silly-
/// large" — anything outside that suggests marshaling garbage.
#[test]
#[serial]
fn get_cursor_pos_returns_screen_coords() {
    setup();
    let s = eval_expr_with_items_to_string(
        "\
define c-function GetCursorPos (lpPoint :: <c-pointer>) => (success :: <c-bool>);
  library: \"user32.dll\";
end;
",
        "let pt = make(<point>); \
         GetCursorPos(pt); \
         point-x(pt) + point-y(pt)",
    )
    .unwrap_or_else(|e| panic!("GetCursorPos test failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return");
    // Sum of x + y. Both are valid screen coordinates; a typical
    // desktop is at most ~16000 across multi-monitor setups; the
    // worst case sum is well under 100000. Negative values can
    // appear on multi-monitor configurations where a secondary
    // monitor is positioned left/above the primary, but the sum
    // should still be well within ±100000.
    assert!(
        n.abs() < 100_000,
        "cursor coord sum {n} suggests marshaling garbage (expected ±100k range)"
    );
}

/// **Sprint 34 headline #2.** GetSystemTime populates a SYSTEMTIME
/// struct with the current UTC. We allocate, marshal, and read
/// `wYear` back. Anything outside [2020, 3000) means the marshaling
/// path is broken (wrong offset, wrong width, wrong pointer).
#[test]
#[serial]
fn get_system_time_returns_current_year() {
    setup();
    let s = eval_expr_with_items_to_string(
        "\
define c-function GetSystemTime (lpst :: <c-pointer>) => ();
  library: \"kernel32.dll\";
end;
",
        "let st = make(<systemtime>); \
         GetSystemTime(st); \
         systemtime-year(st)",
    )
    .unwrap_or_else(|e| panic!("GetSystemTime test failed: {e:?}"));
    let year: i64 = s.parse().expect("integer return");
    assert!(
        year >= 2020,
        "year must be >= 2020, got {year} — marshaling broken?"
    );
    assert!(
        year < 3000,
        "year must be < 3000, got {year} — marshaling garbage?"
    );
}

/// **Sprint 34 headline #3.** SetRect writes all four fields of a
/// RECT through the user32 API. We then read each field back from
/// Dylan and combine via positional decimal packing
/// (`left + (top*10) + (right*100) + (bottom*1000)`) — the sum
/// `10 + (20*10) + (30*100) + (40*1000) = 43210` is a single integer
/// that breaks if any field went to the wrong offset.
///
/// NOTE: the products MUST be parenthesised. Dylan has flat (DRM)
/// operator precedence, so the unparenthesised
/// `left + top*10 + right*100 + bottom*1000` regroups left-assoc as
/// `((((left+top)*10+right)*100+bottom)*1000)` = 33040000 for
/// (10,20,30,40) — a false failure even though SetRect populated the
/// fields correctly.
#[test]
#[serial]
fn set_rect_populates_all_four_fields() {
    setup();
    let s = eval_expr_with_items_to_string(
        "\
define c-function SetRect
    (lprc :: <c-pointer>, xLeft :: <c-int>, yTop :: <c-int>,
     xRight :: <c-int>, yBottom :: <c-int>)
 => (success :: <c-bool>);
  library: \"user32.dll\";
end;
",
        "let r = make(<rect>); \
         SetRect(r, 10, 20, 30, 40); \
         rect-left(r) + (rect-top(r) * 10) + (rect-right(r) * 100) + (rect-bottom(r) * 1000)",
    )
    .unwrap_or_else(|e| panic!("SetRect test failed: {e:?}"));
    let n: i64 = s.parse().expect("integer return");
    assert_eq!(
        n, 43210,
        "SetRect must populate left=10, top=20, right=30, bottom=40 — got packed sum {n}"
    );
}

/// **Sprint 34 GetLocalTime variant.** Same shape as
/// `get_system_time_returns_current_year` but using the local-time API,
/// which writes `wMonth` and `wDay` as small integers. We pack year +
/// month + day into a single int and check the components are sane.
#[test]
#[serial]
fn get_local_time_returns_sensible_month_and_day() {
    setup();
    let s = eval_expr_with_items_to_string(
        "\
define c-function GetLocalTime (lpst :: <c-pointer>) => ();
  library: \"kernel32.dll\";
end;
",
        "let st = make(<systemtime>); \
         GetLocalTime(st); \
         systemtime-month(st) * 100 + systemtime-day(st)",
    )
    .unwrap_or_else(|e| panic!("GetLocalTime test failed: {e:?}"));
    let packed: i64 = s.parse().expect("integer return");
    let month = packed / 100;
    let day = packed % 100;
    assert!(
        (1..=12).contains(&month),
        "month must be 1..=12, got {month} — marshaling broken?"
    );
    assert!(
        (1..=31).contains(&day),
        "day must be 1..=31, got {day} — marshaling broken?"
    );
}
