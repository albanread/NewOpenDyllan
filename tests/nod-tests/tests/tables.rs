//! Sprint 22 — `<table>` + hashing end-to-end tests.
//!
//! These tests drive Dylan source through `eval_expr_to_string` to
//! exercise the runtime `<table>` machinery wired into stdlib generics
//! via `define method size (t :: <table>)`, …
//!
//! Every test is `#[serial]` because the runtime's class registry,
//! literal pool, dispatch table, and function-ref registry are
//! process-global.

use serial_test::serial;

use nod_runtime::{
    _reset_handler_stack_for_tests, ensure_collections_registered, ensure_tables_registered,
};

fn setup() {
    _reset_handler_stack_for_tests();
    ensure_collections_registered();
    ensure_tables_registered();
}

// ─── Headline acceptance — make(<table>); t["foo"] := 42; t["foo"] ────────

#[test]
#[serial]
fn dylan_table_headline_string_key_roundtrip() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        r#"let t = make(<table>); t["foo"] := 42; t["foo"]"#,
    )
    .expect("eval table headline");
    assert_eq!(s, "42");
}

// ─── Basics ─────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn make_empty_table_size_zero() {
    setup();
    let s = nod_sema::eval_expr_to_string("size(make(<table>))")
        .expect("eval size(make(<table>))");
    assert_eq!(s, "0");
}

#[test]
#[serial]
fn table_set_and_get_integer_key() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); t[1] := 42; t[1]",
    )
    .expect("eval integer-key roundtrip");
    assert_eq!(s, "42");
}

#[test]
#[serial]
fn table_set_and_get_string_key() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        r#"let t = make(<table>); t["foo"] := 1; t["foo"]"#,
    )
    .expect("eval string-key roundtrip");
    assert_eq!(s, "1");
}

#[test]
#[serial]
fn table_set_and_get_symbol_key() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        r#"let t = make(<table>); t[#"foo"] := 1; t[#"foo"]"#,
    )
    .expect("eval symbol-key roundtrip");
    assert_eq!(s, "1");
}

#[test]
#[serial]
fn table_default_on_missing_key() {
    // No `default:` keyword path in this sprint — surface via the
    // primitive. The high-level `element(t, k, default: …)` will land
    // when `<keyword-argument>` calls reach methods (see DEFERRED.md).
    // For Sprint 22, `element(t, k)` already returns `#f` on miss
    // (see %table-element). Let's verify that.
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); t[1]",
    )
    .expect("eval missing-key returns #f");
    assert_eq!(s, "#f");
}

#[test]
#[serial]
fn table_overwrite_key() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); t[1] := 100; t[1] := 200; size(t)",
    )
    .expect("eval overwrite size");
    assert_eq!(s, "1");
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); t[1] := 100; t[1] := 200; t[1]",
    )
    .expect("eval overwrite value");
    assert_eq!(s, "200");
}

#[test]
#[serial]
fn table_grows_past_initial_capacity() {
    // Sprint 22: drive 100 inserts through the runtime API directly
    // because we don't have `for` loops over `<range>` reaching the
    // table primitives via Dylan source yet (the eval entry point
    // wraps a single function body). We exercise the JIT path with a
    // smaller scale, then the runtime path with 100 keys.
    setup();
    // Smaller Dylan-source path: insert 4 keys, check all survive.
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); \
         t[0] := 100; t[1] := 101; t[2] := 102; t[3] := 103; \
         t[0] + t[1] + t[2] + t[3]",
    )
    .expect("eval 4-key sum");
    assert_eq!(s, "406");

    // Larger scale via the runtime API.
    let t = nod_runtime::make_table(0);
    for i in 0..100i64 {
        let k = nod_runtime::Word::from_fixnum(i).unwrap();
        let v = nod_runtime::Word::from_fixnum(i * 10).unwrap();
        nod_runtime::table_element_setter(v, t, k);
    }
    assert_eq!(nod_runtime::table_size(t), 100);
    for i in 0..100i64 {
        let k = nod_runtime::Word::from_fixnum(i).unwrap();
        let got = nod_runtime::table_element(
            t,
            k,
            nod_runtime::Word::from_fixnum(-1).unwrap(),
        );
        assert_eq!(got.as_fixnum(), Some(i * 10), "key {i}");
    }
}

#[test]
#[serial]
fn table_remove_key() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); \
         t[1] := 11; t[2] := 22; \
         remove-key!(t, 1); \
         size(t)",
    )
    .expect("eval remove-key! size");
    assert_eq!(s, "1");
}

#[test]
#[serial]
fn table_keys_returns_vector() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); \
         t[1] := 11; t[2] := 22; t[3] := 33; \
         size(keys(t))",
    )
    .expect("eval size(keys(t))");
    assert_eq!(s, "3");
}

#[test]
#[serial]
fn table_collision_handling() {
    // Pick two keys whose hash collide modulo small power-of-two
    // capacity. We can't easily compute hash collisions across the
    // Dylan boundary, but we can verify that lots of inserts all
    // survive — collisions happen in the natural course at 100+ keys.
    setup();
    let t = nod_runtime::make_table(0);
    // Two keys with the same low-3-bits hash (initial capacity is 8):
    // mix_hash(0) = 0, mix_hash(8) = ((8 * golden) & 0x7FF...) — different.
    // Use 0 and the multiplicative inverse step.
    // Easier: just store 16 keys and verify the contents survive the
    // probe chains.
    for i in 0..16i64 {
        let k = nod_runtime::Word::from_fixnum(i).unwrap();
        let v = nod_runtime::Word::from_fixnum(i + 1000).unwrap();
        nod_runtime::table_element_setter(v, t, k);
    }
    for i in 0..16i64 {
        let k = nod_runtime::Word::from_fixnum(i).unwrap();
        let got = nod_runtime::table_element(
            t,
            k,
            nod_runtime::Word::from_fixnum(-1).unwrap(),
        );
        assert_eq!(got.as_fixnum(), Some(i + 1000), "key {i}");
    }
    assert_eq!(nod_runtime::table_size(t), 16);
}

#[test]
#[serial]
fn table_iteration_visits_all_keys() {
    // Sum 1..10 stored in a table by iterating via the FIP
    // primitives (the `for-each (x in c) body end` macro is
    // registered in stdlib but can't drive the surface syntax
    // through the parser yet — see DEFERRED.md). We drive the same
    // expansion that `for-each` would emit, against
    // `keys(t)`, which returns a `<simple-object-vector>` whose FIP
    // is already wired up.
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); \
         t[1] := 1; t[2] := 2; t[3] := 3; t[4] := 4; t[5] := 5; \
         t[6] := 6; t[7] := 7; t[8] := 8; t[9] := 9; t[10] := 10; \
         let total = 0; \
         let s = %fip-init(keys(t)); \
         until (%fip-finished?(s)) \
           let k = %fip-current-element(s); \
           total := total + t[k]; \
           %fip-advance!(s) \
         end; \
         total",
    )
    .expect("eval table iteration sum");
    assert_eq!(s, "55");
}

#[test]
#[serial]
fn non_hashable_key_signals() {
    setup();
    // Try a `<simple-object-vector>` as a key — `#(1, 2, 3)`. The hash
    // path signals `<not-hashable-error>` which propagates as an
    // unhandled condition; we catch the panic.
    let outcome = std::panic::catch_unwind(|| {
        let _ = nod_sema::eval_expr_to_string(
            "let t = make(<table>); t[#(1, 2, 3)] := 1; t[#(1, 2, 3)]",
        );
    });
    _reset_handler_stack_for_tests();
    let err = outcome.expect_err("non-hashable key must signal");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&'static str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("<not-hashable-error>"),
        "expected `<not-hashable-error>` in panic message, got: {msg}"
    );
}

#[test]
#[serial]
fn table_survives_gc_across_insertions() {
    // GC stress: insert 100 keys, force GC, then read them all back.
    // This exercises that the buckets SOV and the table header both
    // survive a minor collection mid-stream.
    //
    // Sprint 23: `t` MUST be registered as a precise root or the
    // collector moves the table out from under us. The Sprint 22
    // baseline (semispace) failed this test in the original form
    // for a different reason (the buckets-SOV pointer dangled after
    // the table header moved); NewGC turned the same hidden bug
    // into a different observable failure. Registering `t` as a
    // root fixes the test under both backends — the fix is in the
    // *test*, not the GC.
    setup();
    let t = nod_runtime::make_table(0);
    nod_runtime::heap_register_root(&t as *const nod_runtime::Word);
    for i in 0..50i64 {
        let k = nod_runtime::Word::from_fixnum(i).unwrap();
        let v = nod_runtime::Word::from_fixnum(i * 7).unwrap();
        nod_runtime::table_element_setter(v, t, k);
    }
    nod_runtime::collect_minor();
    for i in 50..100i64 {
        let k = nod_runtime::Word::from_fixnum(i).unwrap();
        let v = nod_runtime::Word::from_fixnum(i * 7).unwrap();
        nod_runtime::table_element_setter(v, t, k);
    }
    nod_runtime::collect_minor();
    // Read `t` back through the rooted slot — the collector may
    // have rewritten it.
    let t = unsafe { *(&t as *const nod_runtime::Word) };
    assert_eq!(nod_runtime::table_size(t), 100);
    for i in 0..100i64 {
        let k = nod_runtime::Word::from_fixnum(i).unwrap();
        let got = nod_runtime::table_element(
            t,
            k,
            nod_runtime::Word::from_fixnum(-1).unwrap(),
        );
        assert_eq!(got.as_fixnum(), Some(i * 7), "post-GC key {i}");
    }
    nod_runtime::heap_unregister_root(&t as *const nod_runtime::Word);
}
