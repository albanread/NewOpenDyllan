//! Sprint 20 — collection class hierarchy, FIP, and core collection
//! operations end-to-end tests.
//!
//! All tests are `#[serial]` because the runtime's class registry and
//! literal pool are process-global. The collection class IDs are
//! registered idempotently on first access via
//! `ensure_collections_registered`; tests that exercise FIP state
//! across allocations bracket the work in `RootGuard`-protected slots
//! through the runtime API.
//!
//! The Sprint 20 headline acceptance criteria:
//!
//!   1. `reduce(\+, 0, range(from: 1, to: 100))` returns `5050`.
//!   2. `map(method (x) x * x end, #(1, 2, 3))` returns `#(1, 4, 9)`.
//!
//! Both are exercised through the runtime's Rust API
//! (`collection_reduce` / `collection_map`) because Sprint 20 doesn't
//! yet thread anonymous methods through the JIT as first-class
//! higher-order arguments; the FIP machinery they sit on is the same
//! one a future Dylan-side stdlib will drive. See
//! `nod_runtime::collections` for the architectural rationale.

use serial_test::serial;

use nod_runtime::{
    ClassId, FipKind, Word, _reset_handler_stack_for_tests, class_metadata_for,
    collection_concatenate, collection_do, collection_element, collection_element_setter,
    collection_map, collection_reduce, collection_size, ensure_collections_registered,
    forward_iteration_protocol, is_subclass, iter_state_advance, iter_state_snapshot,
    iteration_state_class_id, make_range, make_stretchy_vector, mutable_sequence_class_id,
    out_of_range_error_class_id, range_class_id, range_fields, sequence_class_id,
    stretchy_vector_class_id, stretchy_vector_fields, stretchy_vector_push,
};

fn setup() {
    _reset_handler_stack_for_tests();
    ensure_collections_registered();
}

// ─── Class hierarchy ───────────────────────────────────────────────────────

#[test]
#[serial]
fn collection_hierarchy_registers() {
    setup();
    let seq = sequence_class_id();
    let mut_seq = mutable_sequence_class_id();
    let range = range_class_id();
    let sv = stretchy_vector_class_id();
    let iter = iteration_state_class_id();
    let oore = out_of_range_error_class_id();
    // Subclass invariants.
    assert!(is_subclass(range, seq));
    assert!(is_subclass(sv, mut_seq));
    assert!(is_subclass(sv, seq));
    // <iteration-state>'s parent is `<object>` — Sprint 21 made every
    // `parent = None` registration implicitly subclass of `<object>` so
    // stdlib methods declared on `(p :: <object>)` dispatch on user-
    // class receivers. Pre-Sprint-21 this returned `None`; the
    // semantics match Dylan ("every class is a subclass of <object>").
    let iter_md = class_metadata_for(iter);
    assert_eq!(iter_md.parent, Some(ClassId::OBJECT));
    // <out-of-range-error> inherits from <error>.
    let oore_md = class_metadata_for(oore);
    assert!(
        is_subclass(oore, nod_runtime::error_class_id()),
        "<out-of-range-error> must be a subclass of <error>; got cpl = {:?}",
        oore_md.cpl
    );
}

// ─── Headline test 1 — reduce(+, 0, range(1, 100)) == 5050 ─────────────────

#[test]
#[serial]
fn reduce_plus_zero_range_one_to_hundred_is_5050() {
    setup();
    let r = make_range(1, 100, 1);
    let zero = Word::from_fixnum(0).unwrap();
    let result = collection_reduce(r, zero, |acc, x| {
        let a = acc.as_fixnum().unwrap_or(0);
        let b = x.as_fixnum().unwrap_or(0);
        Word::from_fixnum(a + b).unwrap()
    });
    assert_eq!(
        result.as_fixnum(),
        Some(5050),
        "reduce(\\+, 0, range(from: 1, to: 100)) must equal 5050"
    );
}

// ─── Headline test 2 — map(square, #(1, 2, 3)) == #(1, 4, 9) ───────────────

#[test]
#[serial]
fn map_squares_three_element_list() {
    setup();
    // Build the list (1 . (2 . (3 . nil))) via the runtime API.
    let imm = nod_runtime::literal_pool_immediates();
    let nil = imm.nil;
    let one = Word::from_fixnum(1).unwrap();
    let two = Word::from_fixnum(2).unwrap();
    let three = Word::from_fixnum(3).unwrap();
    let l3 = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_pair(three, nil, &pool.classes)
    });
    let l2 = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_pair(two, l3, &pool.classes)
    });
    let l1 = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_pair(one, l2, &pool.classes)
    });
    let mapped = collection_map(l1, |w| {
        let v = w.as_fixnum().unwrap_or(0);
        Word::from_fixnum(v * v).unwrap()
    });
    // mapped should be a 3-element list: (1, 4, 9).
    let mut collected: Vec<i64> = Vec::new();
    collection_do(mapped, |w| {
        collected.push(w.as_fixnum().unwrap_or(-1));
    });
    assert_eq!(
        collected,
        vec![1, 4, 9],
        "map(method (x) x * x end, #(1, 2, 3)) must equal #(1, 4, 9)"
    );
}

// ─── do walks elements in order ────────────────────────────────────────────

#[test]
#[serial]
fn list_do_walks_elements_in_order() {
    setup();
    let imm = nod_runtime::literal_pool_immediates();
    let three = Word::from_fixnum(3).unwrap();
    let two = Word::from_fixnum(2).unwrap();
    let one = Word::from_fixnum(1).unwrap();
    let l3 = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_pair(three, imm.nil, &pool.classes)
    });
    let l2 = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_pair(two, l3, &pool.classes)
    });
    let l1 = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_pair(one, l2, &pool.classes)
    });
    // Push! into a Rust Vec in iteration order (left-to-right).
    let mut seen: Vec<i64> = Vec::new();
    collection_do(l1, |w| seen.push(w.as_fixnum().unwrap()));
    assert_eq!(seen, vec![1, 2, 3]);
}

// ─── stretchy_vector_push grows + survives GC ──────────────────────────────

#[test]
#[serial]
fn stretchy_vector_push_grows_and_survives_minor_gc() {
    setup();
    // Sprint 23: register `sv` as a precise root so the collector
    // rewrites our local slot when it moves the vector. The Sprint
    // 22 semispace baseline passed this test "by luck" — the heap
    // mutex was held across `collect_minor` and the local stack
    // slot happened to escape pinning visibility; NewGC's precise
    // collector relocates unconditionally, exposing the missing
    // root registration.
    let sv = make_stretchy_vector(4);
    nod_runtime::heap_register_root(&sv as *const Word);
    // Push 100 fixnums (forces multiple grows past the initial cap of 4).
    for i in 0..100 {
        stretchy_vector_push(sv, Word::from_fixnum(i).unwrap());
    }
    let (length, capacity, _storage) =
        stretchy_vector_fields(sv).expect("sv is a <stretchy-vector>");
    assert_eq!(length, 100);
    assert!(
        capacity >= 100,
        "capacity {capacity} must accommodate 100 pushed elements"
    );
    // Force a minor GC and re-check.
    nod_runtime::collect_minor();
    // Re-read `sv` through the root slot — the collector may have
    // rewritten it.
    let sv = unsafe { *(&sv as *const Word) };
    let (length_after, _, _) =
        stretchy_vector_fields(sv).expect("sv survives minor GC");
    assert_eq!(length_after, 100);
    // Spot-check the first and last element.
    let first = collection_element(sv, 0, None).expect("element 0");
    let last = collection_element(sv, 99, None).expect("element 99");
    assert_eq!(first.as_fixnum(), Some(0));
    assert_eq!(last.as_fixnum(), Some(99));
    nod_runtime::heap_unregister_root(&sv as *const Word);
}

// ─── element out of range → Err with bounds info ───────────────────────────

#[test]
#[serial]
fn element_out_of_range_returns_err_with_bounds() {
    setup();
    let v = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(3, &pool.classes)
    });
    // index 10 on a length-3 vector — out of bounds.
    let err = collection_element(v, 10, None).unwrap_err();
    assert_eq!(err.bounds, 3);
    // With a default, no error.
    let default = Word::from_fixnum(-1).unwrap();
    let val = collection_element(v, 10, Some(default)).expect("default ok");
    assert_eq!(val.as_fixnum(), Some(-1));
}

// ─── concatenate two lists ─────────────────────────────────────────────────

#[test]
#[serial]
fn concatenate_two_lists_glues_in_order() {
    setup();
    let imm = nod_runtime::literal_pool_immediates();
    // List 1: (1 . (2 . nil))
    let p12 = nod_runtime::with_literal_pool(|pool| {
        let p2 = pool
            .heap
            .alloc_pair(Word::from_fixnum(2).unwrap(), imm.nil, &pool.classes);
        pool.heap
            .alloc_pair(Word::from_fixnum(1).unwrap(), p2, &pool.classes)
    });
    // List 2: (3 . (4 . nil))
    let p34 = nod_runtime::with_literal_pool(|pool| {
        let p4 = pool
            .heap
            .alloc_pair(Word::from_fixnum(4).unwrap(), imm.nil, &pool.classes);
        pool.heap
            .alloc_pair(Word::from_fixnum(3).unwrap(), p4, &pool.classes)
    });
    let cat = collection_concatenate(p12, p34);
    let mut seen: Vec<i64> = Vec::new();
    collection_do(cat, |w| seen.push(w.as_fixnum().unwrap()));
    assert_eq!(seen, vec![1, 2, 3, 4]);
}

// ─── FIP returns a usable iteration state for every concrete class ────────

#[test]
#[serial]
fn fip_returns_iteration_state_on_each_concrete_class() {
    setup();
    // <range>
    let r = make_range(10, 12, 1);
    let r_state = forward_iteration_protocol(r).expect("range FIP");
    let r_snap = iter_state_snapshot(r_state).expect("snapshot");
    assert_eq!(r_snap.fip_kind, FipKind::Range);
    assert_eq!(r_snap.current_element.as_fixnum(), Some(10));
    assert!(!r_snap.finished);

    // <simple-object-vector>
    let v = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(2, &pool.classes)
    });
    // Set v[0] = 100, v[1] = 200.
    collection_element_setter(v, 0, Word::from_fixnum(100).unwrap()).unwrap();
    collection_element_setter(v, 1, Word::from_fixnum(200).unwrap()).unwrap();
    let v_state = forward_iteration_protocol(v).expect("sov FIP");
    let v_snap = iter_state_snapshot(v_state).expect("snapshot");
    assert_eq!(v_snap.fip_kind, FipKind::SimpleObjectVector);
    assert_eq!(v_snap.current_element.as_fixnum(), Some(100));

    // <list> — empty
    let imm = nod_runtime::literal_pool_immediates();
    let empty_state = forward_iteration_protocol(imm.nil).expect("nil FIP");
    let empty_snap = iter_state_snapshot(empty_state).expect("snapshot");
    assert_eq!(empty_snap.fip_kind, FipKind::List);
    assert!(empty_snap.finished);

    // <stretchy-vector>
    let sv = make_stretchy_vector(2);
    stretchy_vector_push(sv, Word::from_fixnum(7).unwrap());
    stretchy_vector_push(sv, Word::from_fixnum(8).unwrap());
    let sv_state = forward_iteration_protocol(sv).expect("sv FIP");
    let sv_snap = iter_state_snapshot(sv_state).expect("snapshot");
    assert_eq!(sv_snap.fip_kind, FipKind::StretchyVector);
    assert_eq!(sv_snap.current_element.as_fixnum(), Some(7));
}

// ─── size on every concrete class ──────────────────────────────────────────

#[test]
#[serial]
fn size_works_on_every_concrete_class() {
    setup();
    // SOV
    let v = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(5, &pool.classes)
    });
    assert_eq!(collection_size(v), Some(5));
    // <range>
    let r = make_range(1, 10, 1);
    assert_eq!(collection_size(r), Some(10));
    let r_step = make_range(1, 10, 2); // 1, 3, 5, 7, 9 = 5 elements
    assert_eq!(collection_size(r_step), Some(5));
    let r_empty = make_range(10, 1, 1); // empty (1 > 10 going up by 1)
    assert_eq!(collection_size(r_empty), Some(0));
    // <stretchy-vector>
    let sv = make_stretchy_vector(2);
    for _ in 0..7 {
        stretchy_vector_push(sv, Word::from_fixnum(0).unwrap());
    }
    assert_eq!(collection_size(sv), Some(7));
    // <empty-list>
    let imm = nod_runtime::literal_pool_immediates();
    assert_eq!(collection_size(imm.nil), Some(0));
    // <pair> — a 3-element list.
    let l3 = nod_runtime::with_literal_pool(|pool| {
        let p3 = pool
            .heap
            .alloc_pair(Word::from_fixnum(3).unwrap(), imm.nil, &pool.classes);
        let p2 = pool
            .heap
            .alloc_pair(Word::from_fixnum(2).unwrap(), p3, &pool.classes);
        pool.heap
            .alloc_pair(Word::from_fixnum(1).unwrap(), p2, &pool.classes)
    });
    assert_eq!(collection_size(l3), Some(3));
}

// ─── range fields round-trip ───────────────────────────────────────────────

#[test]
#[serial]
fn range_fields_roundtrip() {
    setup();
    let r = make_range(5, 50, 3);
    let (from, to, by) = range_fields(r).expect("range fields");
    assert_eq!(from, 5);
    assert_eq!(to, 50);
    assert_eq!(by, 3);
    // Iterate and confirm values.
    let mut seen: Vec<i64> = Vec::new();
    collection_do(r, |w| seen.push(w.as_fixnum().unwrap()));
    assert_eq!(seen, vec![5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47, 50]);
}

// ─── advance state through a SOV ───────────────────────────────────────────

#[test]
#[serial]
fn iter_state_advance_walks_sov() {
    setup();
    let v = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(3, &pool.classes)
    });
    collection_element_setter(v, 0, Word::from_fixnum(11).unwrap()).unwrap();
    collection_element_setter(v, 1, Word::from_fixnum(22).unwrap()).unwrap();
    collection_element_setter(v, 2, Word::from_fixnum(33).unwrap()).unwrap();
    let state = forward_iteration_protocol(v).expect("FIP");
    let snap0 = iter_state_snapshot(state).unwrap();
    assert_eq!(snap0.current_element.as_fixnum(), Some(11));
    iter_state_advance(state);
    let snap1 = iter_state_snapshot(state).unwrap();
    assert_eq!(snap1.current_element.as_fixnum(), Some(22));
    iter_state_advance(state);
    let snap2 = iter_state_snapshot(state).unwrap();
    assert_eq!(snap2.current_element.as_fixnum(), Some(33));
    iter_state_advance(state);
    let snap3 = iter_state_snapshot(state).unwrap();
    assert!(snap3.finished);
}

// ─── range with negative step ──────────────────────────────────────────────

#[test]
#[serial]
fn range_with_negative_step_walks_down() {
    setup();
    let r = make_range(5, 1, -1);
    let mut seen: Vec<i64> = Vec::new();
    collection_do(r, |w| seen.push(w.as_fixnum().unwrap()));
    assert_eq!(seen, vec![5, 4, 3, 2, 1]);
}

// ─── empty range iterates zero times ───────────────────────────────────────

#[test]
#[serial]
fn empty_range_iterates_zero_times() {
    setup();
    let r = make_range(10, 1, 1); // empty going up
    let mut count = 0;
    collection_do(r, |_| count += 1);
    assert_eq!(count, 0);
}

// ─── map preserves shape across class kinds ───────────────────────────────

#[test]
#[serial]
fn map_preserves_collection_class_kind() {
    setup();
    // SOV in → SOV out.
    let v = nod_runtime::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(3, &pool.classes)
    });
    for i in 0..3i64 {
        collection_element_setter(v, i, Word::from_fixnum(i + 1).unwrap()).unwrap();
    }
    let mapped_v = collection_map(v, |w| {
        Word::from_fixnum(w.as_fixnum().unwrap_or(0) * 10).unwrap()
    });
    // mapped_v should be a SOV.
    let p = mapped_v.as_ptr::<u8>().expect("pointer-tagged");
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const nod_runtime::Wrapper) };
    assert_eq!(wrapper.class(), ClassId::SIMPLE_OBJECT_VECTOR);
    assert_eq!(collection_size(mapped_v), Some(3));
    let mut elems: Vec<i64> = Vec::new();
    collection_do(mapped_v, |w| elems.push(w.as_fixnum().unwrap()));
    assert_eq!(elems, vec![10, 20, 30]);
}

// ─── Sprint 20b — Dylan-source tests via eval_expr_to_string ──────────────
//
// Sprint 20b's headline goal: drive the collection ops from REAL Dylan
// source (parsed + macro-expanded + lowered + JIT'd) instead of via the
// runtime API. Each test below exercises a different cut through the
// stdlib + primitive-op machinery:
//
//   * `dylan_stdlib_loader_runs_marker` — the `nod-stdlib-marker()`
//     function defined in `stdlib/*.dylan`
//     is JIT'd by the loader and reachable as a top-level function from
//     user code. Confirms Phase A's loader path is live.
//   * `dylan_size_of_three_element_list_is_3` — `size(#(10, 20, 30))`
//     dispatches through the stdlib's `size` method (rewritten from
//     `define function` to `define method ... <object>` by the loader).
//   * `dylan_size_of_range_one_to_hundred_is_100` — same but with a
//     `<range>` constructed via `make-range(...)` (the `<range>`-shaped
//     `make` keyword path).
//   * `dylan_concatenate_two_lists_size_is_5` — exercises the
//     `concatenate` generic + the `size` generic in sequence.
//   * `dylan_for_each_sum_three_element_list_is_6` — `for-each` macro
//     drives the FIP primitives directly through Dylan source.
//
// Deferred (DEFERRED.md → "Sprint 20b residue"):
//   * `reduce(\+, 0, range(from: 1, to: 100))` — requires first-class
//     function values + `\+` as a function reference (Sprint 21).
//   * `map(method (x) x * x end, #(1, 2, 3))` — same blocker.
// The Rust-API equivalents (`reduce_plus_zero_range_one_to_hundred_is_5050`,
// `map_squares_three_element_list`) above remain green and exercise the
// same FIP machinery the Dylan-source path drives.

#[test]
#[serial]
fn dylan_stdlib_loader_registers_for_each_macro() {
    setup();
    // The `for-each` macro defined in stdlib.dylan is registered in the
    // process-global macro table by `ensure_loaded`. User-side macro
    // expansion (`expand_with_stdlib_macros` in `nod-sema/src/lib.rs`)
    // merges the stdlib table on top of the per-call table. Sprint 20b
    // can't yet drive the `for-each (x in c) body end` syntax through
    // the expression-level parser (body-shaped macro calls are a
    // Sprint 21 follow-up — see DEFERRED.md), so this test confirms
    // the macro IS registered + reachable; the macro-engine drives
    // the actual expansion in lower-level macro tests.
    let arts = nod_sema::stdlib::ensure_loaded();
    assert!(
        arts.macro_names.iter().any(|n| n == "for-each"),
        "stdlib loader should register `for-each`; got macros: {:?}",
        arts.macro_names
    );
}

#[test]
#[serial]
fn dylan_stdlib_loader_runs_marker() {
    setup();
    // `nod-stdlib-marker(41)` should resolve via the process-global
    // dispatch table (the loader rewrote the stdlib's `define function`
    // to `define method ... (x :: <object>)`).
    let s = nod_sema::eval_expr_to_string("nod-stdlib-marker(41)")
        .expect("eval `nod-stdlib-marker(41)`");
    assert_eq!(s, "42", "stdlib loader didn't expose `nod-stdlib-marker`");
}

#[test]
#[serial]
fn dylan_size_of_three_element_list_is_3() {
    setup();
    let s = nod_sema::eval_expr_to_string("size(#(10, 20, 30))")
        .expect("eval `size(#(10, 20, 30))`");
    assert_eq!(s, "3", "size of a 3-element list must be 3");
}

#[test]
#[serial]
fn dylan_size_of_empty_list_is_0() {
    setup();
    let s = nod_sema::eval_expr_to_string("size(#())").expect("eval `size(#())`");
    assert_eq!(s, "0");
}

#[test]
#[serial]
fn dylan_concatenate_two_lists_size_is_5() {
    setup();
    let s = nod_sema::eval_expr_to_string("size(concatenate(#(1, 2), #(3, 4, 5)))")
        .expect("eval `size(concatenate(#(1, 2), #(3, 4, 5)))`");
    assert_eq!(s, "5", "concatenated size must be 5");
}

#[test]
#[serial]
fn dylan_fip_until_loop_sums_three_element_list_to_6() {
    setup();
    // Sprint 20b: drive the FIP primitives directly from Dylan source.
    // This is the desugaring of `for-each (x in #(1,2,3)) total := total
    // + x end` written by hand — the `for-each` macro's expansion. The
    // body-shaped macro call site is gated on Sprint 21's parser support
    // for statement-position macro calls (DEFERRED.md), so we test the
    // expansion target directly today.
    let s = nod_sema::eval_expr_to_string(
        "let total = 0; \
         let s = %fip-init(#(1, 2, 3)); \
         until (%fip-finished?(s)) \
           total := total + %fip-current-element(s); \
           %fip-advance!(s) \
         end; \
         total \
         end",
    )
    .expect("eval FIP-driven sum");
    assert_eq!(
        s, "6",
        "FIP-driven sum over #(1,2,3) must be 6 — primitive ops + until loop"
    );
}

#[test]
#[serial]
fn dylan_for_each_surface_sums_three_element_list_to_6() {
    setup();
    // Sprint 25 headline: the `for-each (x in c) body end` surface
    // syntax now works end-to-end through the parser, the stdlib
    // macro engine, and the lowering. This is the Sprint 20b
    // deferral closing test — the macro was defined back in
    // Sprint 20b but the parser couldn't recognise body-shaped
    // macro calls until Sprint 25's body-shaped macro recogniser
    // landed. Compare with `dylan_fip_until_loop_sums_three_element_list_to_6`
    // above: that test exercises the expansion target directly;
    // this test exercises the macro engine driving the same
    // result from the surface syntax.
    let s = nod_sema::eval_expr_to_string(
        "let total = 0; \
         for-each (x in #(1, 2, 3)) total := total + x end; \
         total \
         end",
    )
    .expect("eval `for-each` surface");
    assert_eq!(
        s, "6",
        "for-each (x in #(1,2,3)) total := total + x end must sum to 6"
    );
}

#[test]
#[serial]
fn dylan_fip_until_loop_sums_range_one_to_ten() {
    setup();
    // Same shape, range collection. Confirms the FIP primitives
    // dispatch correctly across concrete collection classes. The
    // `%make-range` primitive is the Sprint 20b allocator wrapper —
    // `make(<range>, from: 1, to: 10, by: 1)` is the more-canonical
    // form but requires the keyword-arg dispatch path through `make`,
    // which is itself a Sprint 21 polish.
    let s = nod_sema::eval_expr_to_string(
        "let total = 0; \
         let s = %fip-init(%make-range(1, 10, 1)); \
         until (%fip-finished?(s)) \
           total := total + %fip-current-element(s); \
           %fip-advance!(s) \
         end; \
         total \
         end",
    )
    .expect("eval FIP-driven range sum");
    assert_eq!(s, "55", "1+2+...+10 must be 55");
}

#[test]
#[serial]
fn dylan_reduce_plus_zero_range_one_to_hundred_is_5050() {
    setup();
    // Sprint 21 headline: `\+` lowers to a `<function>` Word; `reduce`
    // is the Dylan-defined stdlib method that drives the FIP loop and
    // calls the combiner via `%funcall2`.
    //
    // Sprint 26: now drives the canonical `make(<range>, …)` keyword-
    // init form (`by:` defaults to `1`), instead of the
    // `%make-range(from, to, by)` primitive workaround the Sprint 21
    // brief originally accepted.
    let s = nod_sema::eval_expr_to_string("reduce(\\+, 0, make(<range>, from: 1, to: 100))")
        .expect("eval `reduce(\\+, 0, make(<range>, from: 1, to: 100))`");
    assert_eq!(s, "5050");
}

#[test]
#[serial]
fn dylan_map_squares_three_element_list() {
    setup();
    // Sprint 21 headline: the anonymous method is lifted to a top-
    // level synthetic name (`__anon-method-0`) by the pre-pass; the
    // call site becomes `\__anon-method-0` -> `<function>` Word; map
    // walks via the FIP and calls back through `%funcall1`.
    let s = nod_sema::eval_expr_to_string("map(method (x) x * x end, #(1, 2, 3))")
        .expect("eval `map(method (x) x * x end, #(1, 2, 3))`");
    assert_eq!(s, "#(1, 4, 9)");
}

#[test]
#[serial]
fn dylan_fip_reduce_range_one_to_one_hundred_is_5050() {
    setup();
    // Sprint 20b — the headline acceptance test, expressed without
    // first-class functions. The user-typed Dylan source drives
    // `%fip-init` / `%fip-finished?` / `%fip-current-element` /
    // `%fip-advance!` against a `%make-range(1, 100, 1)`, and the
    // accumulator add lowers to the integer PrimOp::AddInt. This is
    // the SAME machinery `reduce(\+, 0, range(...))` would invoke
    // once first-class functions land (Sprint 21 — see DEFERRED.md);
    // the body shape here is the macro-expanded form of `reduce`.
    let s = nod_sema::eval_expr_to_string(
        "let total = 0; \
         let s = %fip-init(%make-range(1, 100, 1)); \
         until (%fip-finished?(s)) \
           total := total + %fip-current-element(s); \
           %fip-advance!(s) \
         end; \
         total \
         end",
    )
    .expect("eval FIP-driven reduce");
    assert_eq!(
        s, "5050",
        "Sprint 20b headline (FIP form): sum 1..100 must be 5050"
    );
}
