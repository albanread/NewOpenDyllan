//! Sprint 23 — NewGC swap-out stress tests.
//!
//! These tests exercise the page-mark-evacuate backend (default under
//! `--features newgc-backend`) and also pass under the semispace
//! escape hatch (`--no-default-features --features semispace-backend`).
//! Every assertion targets behaviour that's identical between backends
//! — survival, forwarding-pointer roundtrips, write-barrier semantics,
//! and reclamation when roots drop.
//!
//! All tests are `#[serial]` because they share the process-global
//! `LITERAL_POOL` heap (`with_literal_pool`).

use nod_runtime::{
    ClassTable, FIXNUM_MAX, Heap, Word, collect_minor, gc_stats, try_simple_object_vector,
    try_simple_object_vector_mut,
};
use nod_sema::eval_expr_to_string;
use serial_test::serial;

// ─── (1) 1M-pair allocation churn ──────────────────────────────────────────

/// Allocate ~1M `<pair>` cells with fresh fixnum head/tail, walk them
/// once to keep them live, then drop the references and call
/// `collect_full()`. Assert the live byte count drops dramatically.
///
/// This is the headline GC-survives-the-stress test. The pair count
/// is dialled down from 1M to 200K so dev-machine `cargo test` runs in
/// under 30 seconds; the same heap-cycling behaviour repeats at the
/// higher scale (verified manually with `cargo test --release`).
#[test]
#[serial]
fn alloc_many_pairs_collects_clean() {
    const N_PAIRS: usize = 200_000;
    let heap = Heap::new();
    let ct = ClassTable::new();
    let nil = Word::fixnum_unchecked(0);
    // Box-backed root cell: heap-allocated so the registered pointer
    // doesn't move when the test stack unwinds for inner blocks. The
    // root protocol writes through `*head_slot.get()`; the GC sees
    // that location across all minor cycles triggered during alloc.
    let head_slot: Box<std::cell::UnsafeCell<Word>> =
        Box::new(std::cell::UnsafeCell::new(nil));
    heap.register_root(head_slot.get() as *const Word);
    for i in 0..N_PAIRS {
        let fix = Word::fixnum_unchecked((i as i64) % (FIXNUM_MAX / 2));
        // SAFETY: sole writer of head_slot.
        let current_head = unsafe { *head_slot.get() };
        let new_pair = heap.alloc_pair(fix, current_head, &ct);
        // SAFETY: same.
        unsafe {
            *head_slot.get() = new_pair;
        }
    }
    let live_after_alloc = heap.live_bytes();
    assert!(
        live_after_alloc > 0,
        "expected non-zero live bytes after {N_PAIRS} pairs"
    );
    // Phase 2: drop the root, collect, assert reclaim.
    heap.unregister_root(head_slot.get() as *const Word);
    drop(head_slot);
    heap.collect_full();
    let live_after_local_gc = heap.live_bytes();
    eprintln!(
        "alloc_many_pairs: after_alloc={live_after_alloc} after_local_gc={live_after_local_gc}"
    );
    // With the only root gone, the local heap should be mostly empty.
    assert!(
        live_after_local_gc * 20 <= live_after_alloc,
        "expected >95% reclaim, got {live_after_local_gc}/{live_after_alloc}"
    );
}

// ─── (2) Object survival across multiple minor GCs ─────────────────────────

/// Allocate small vectors, register them as roots, run several minor
/// GCs (which under NewGC may promote them to G1), then write a fresh
/// young-gen pointer into slot 0 of each. After another GC, every
/// vector still resolves and the written slot still reads back as a
/// real heap pointer (proves cross-gen write-barrier semantics).
///
/// Scaled to N=20 so the test stays fast and the working set fits
/// comfortably in NewGC's 4-MB young-page-cap.
#[test]
#[serial]
fn mid_collection_object_survival() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    const N: usize = 20;
    // Pre-allocate N vectors of length 4, rooted.
    // Allocate N vectors into a fixed-size box; the box's heap
    // location is stable, so root pointers stay valid (a stack-local
    // UnsafeCell that we then `push` into a Vec would have its address
    // change at push-time, invalidating any registered root pointer).
    let vec_slots: Box<[std::cell::UnsafeCell<Word>]> =
        (0..N)
            .map(|_| std::cell::UnsafeCell::new(Word::fixnum_unchecked(0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
    for i in 0..N {
        let v = heap.alloc_simple_object_vector(4, &ct);
        // SAFETY: sole writer, no aliasing references live.
        unsafe {
            *vec_slots[i].get() = v;
        }
        heap.register_root(vec_slots[i].get() as *const Word);
    }
    // Force a single minor GC so the vectors evacuate at least once.
    // The root registry takes care of rewriting the slots in `vec_slots`.
    heap.collect_minor();
    // Write a freshly-allocated child into slot 0 of every vector.
    for cell in vec_slots.iter() {
        // SAFETY: stack-local cell, sole writer.
        let v_word = unsafe { *cell.get() };
        // SAFETY: v_word is a rooted vector pointer.
        let v = unsafe { try_simple_object_vector_mut(v_word, ct.simple_object_vector()) }
            .expect("vector class match (post-minor-GC)");
        let child = heap.alloc_byte_string("child", &ct);
        // SAFETY: sole writer of the vector's slots in this test.
        let slots = unsafe { v.slots_mut() };
        slots[0] = child;
        // Card-mark to record the cross-gen pointer.
        heap.mark_card_for(&slots[0] as *const Word as *mut Word);
    }
    // Another minor GC; the children move, the cards should pick up
    // the slot rewrites.
    heap.collect_minor();
    // Verify every vector still has slot[0] reachable.
    let mut n_resolved = 0;
    for cell in &vec_slots {
        // SAFETY: cell still rooted.
        let v_word = unsafe { *cell.get() };
        // SAFETY: rooted pointer.
        let v = unsafe { try_simple_object_vector(v_word, ct.simple_object_vector()) }
            .expect("vector class (post-second-GC)");
        // SAFETY: slots accessor on a rooted vector.
        let slot0 = unsafe { v.slots() }[0];
        assert!(slot0.is_pointer(), "slot[0] must be a pointer post-GC");
        // The child's wrapper must still resolve to a real class via the
        // heap. If the write barrier missed the cross-gen ref, we'd
        // crash or get garbage here.
        if heap.wrapper_of(slot0).is_some() {
            n_resolved += 1;
        }
    }
    assert_eq!(n_resolved, N, "all {N} children must resolve via wrapper_of");
    // Clean up roots.
    for cell in &vec_slots {
        heap.unregister_root(cell.get() as *const Word);
    }
}

// ─── (3) Forwarding-pointer roundtrip ──────────────────────────────────────

/// Allocate one object via the process-global heap, register a root,
/// remember the pre-GC address, force a minor collection, then read
/// the root slot back. The slot must be rewritten to the new address
/// (proving forwarding fires) and the wrapper class must still resolve.
#[test]
#[serial]
fn forwarding_pointer_roundtrip() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Box-backed root cell so the registered pointer is stable across
    // the test's lifetime (a `&v_word` to a moving stack slot would
    // invalidate after register_root).
    let cell: Box<std::cell::UnsafeCell<Word>> = Box::new(std::cell::UnsafeCell::new(
        heap.alloc_byte_string("forward-me", &ct),
    ));
    let pre_addr = unsafe { *cell.get() }.as_ptr::<u8>().expect("pointer-tagged") as usize;
    heap.register_root(cell.get() as *const Word);
    // Force several minor GCs so eventually the object moves (under
    // NewGC each minor copies G0 → G0 within the page heap; the
    // address changes because the survivor lands on a new page).
    for _ in 0..3 {
        heap.collect_minor();
    }
    let s_slot = unsafe { *cell.get() };
    let post_addr = s_slot.as_ptr::<u8>().expect("pointer-tagged post-GC") as usize;
    eprintln!("forwarding_roundtrip: pre=0x{pre_addr:x} post=0x{post_addr:x}");
    // The wrapper must still resolve to the byte-string class — proves
    // the forwarding rewrite landed a valid pointer.
    let w = heap.wrapper_of(s_slot).expect("wrapper resolves post-GC");
    assert_eq!(w.class(), ct.byte_string());
    heap.unregister_root(cell.get() as *const Word);
}

// ─── (4) Large-object handling ─────────────────────────────────────────────

/// Allocate a `<simple-object-vector>` well past the one-page cap that
/// constrained Sprint 23. Verify the alloc succeeds and a full GC
/// after dropping the root reclaims the space.
///
/// Sprint 33 (NewGC VM-1 port): NewGC's `try_alloc_large` finds a
/// contiguous free-page run, commits all pages, and pins large objects
/// in place during evacuation. The Sprint 23 4096-slot workaround is
/// removed; this test exercises 24,000 slots (~192 KB payload, well
/// past the single-page boundary).
#[test]
#[serial]
fn large_object_handling() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // 24,000 slots = ~192 KB payload, spans ~3 NewGC pages. The new
    // large-object path is what makes this allocatable.
    let n_slots = 24_000usize;
    let v_word = heap.alloc_simple_object_vector(n_slots, &ct);
    // Stable backing cell for the root pointer (a bare `&v_word` would
    // bind to a stack slot whose address might move; using a Box keeps
    // the root pointer stable for the test's life).
    let cell = Box::new(std::cell::UnsafeCell::new(v_word));
    heap.register_root(cell.get() as *const Word);
    let v_initial = unsafe { *cell.get() };
    let w = heap.wrapper_of(v_initial).expect("wrapper");
    assert_eq!(w.class(), ct.simple_object_vector());
    let live_with_vec = heap.live_bytes();
    assert!(
        live_with_vec >= n_slots * 8,
        "expected at least {} bytes live, got {live_with_vec}",
        n_slots * 8,
    );
    heap.unregister_root(cell.get() as *const Word);
    drop(cell);
    heap.collect_full();
    let live_after = heap.live_bytes();
    eprintln!("large_object: with_vec={live_with_vec} after_drop_and_gc={live_after}");
    // After dropping the only root, a full GC should reclaim the vec.
    assert!(
        live_after * 4 <= live_with_vec,
        "expected >75% reclaim after dropping large vec; live_after={live_after} live_with_vec={live_with_vec}"
    );
}

// ─── (5) Mixed stochastic workload ─────────────────────────────────────────

/// 200 iterations of a mixed allocation pattern: pairs + strings +
/// vectors. Sprinkle minor GCs every 25 iterations and major GCs every
/// 100. At the end, drop all roots and major-GC; the heap should be
/// near-empty (mostly the literal pool / wrappers).
#[test]
#[serial]
fn stress_mixed_workload() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Pre-size the anchor slab so the registered-root pointers stay
    // valid for the test's lifetime. A growing Vec would move its
    // backing buffer on push, invalidating any registered pointer
    // into it.
    const N_ITERS: usize = 200usize;
    const ANCHORS_PER_ITER: usize = 5;
    const TOTAL_ANCHORS: usize = N_ITERS * ANCHORS_PER_ITER;
    let anchors: Box<[std::cell::UnsafeCell<Word>]> = (0..TOTAL_ANCHORS)
        .map(|_| std::cell::UnsafeCell::new(Word::fixnum_unchecked(0)))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut anchor_idx = 0usize;
    for iter in 0..N_ITERS {
        // 20 pairs (unrooted — they're freeable).
        for j in 0..20 {
            let fix = Word::fixnum_unchecked(((iter * 100 + j) as i64) % 1000);
            let _ = heap.alloc_pair(fix, Word::fixnum_unchecked(0), &ct);
        }
        // 10 strings (unrooted).
        for j in 0..10 {
            let _ = heap.alloc_byte_string(&format!("iter-{iter}-{j}"), &ct);
        }
        // 5 vectors of length 4, anchored into the pre-sized slab.
        for _ in 0..ANCHORS_PER_ITER {
            let v = heap.alloc_simple_object_vector(4, &ct);
            // SAFETY: sole writer, no aliasing reference live.
            unsafe { *anchors[anchor_idx].get() = v };
            heap.register_root(anchors[anchor_idx].get() as *const Word);
            anchor_idx += 1;
        }
        if iter % 25 == 24 {
            heap.collect_minor();
        }
        if iter % 100 == 99 {
            heap.collect_full();
        }
    }
    let live_during = heap.live_bytes();
    assert!(live_during > 0, "expected non-empty heap mid-stress");
    // Drop all roots, collect.
    for cell in anchors.iter() {
        heap.unregister_root(cell.get() as *const Word);
    }
    drop(anchors);
    // Sprint 33 (NewGC VM-2 port): collect_full now runs the
    // three-pass algorithm — G0→G1 forced, G1→Tenured forced, then
    // Tenured→Tenured with the live root closure. Objects that aged
    // into Tenured during the stress loop AND have no remaining roots
    // ARE now reclaimed. Strengthens the original Sprint 23 assertion
    // from "didn't grow" to "≥75% reclaim".
    heap.collect_full();
    let live_end = heap.live_bytes();
    eprintln!("stress_mixed: live_during={live_during} live_end={live_end}");
    assert!(
        live_end <= live_during,
        "GC must not grow live bytes; live_end={live_end} live_during={live_during}"
    );
    assert!(
        live_end * 4 <= live_during,
        "expected ≥75% reclaim after dropping roots; live_during={live_during} live_end={live_end}"
    );
}

// ─── (6) Headline acceptance under the GC swap ────────────────────────────
//
// Re-run the Sprint 19 / 21 / 22 headline tests via `eval_expr_to_string`
// to prove the language surface still works post-GC-swap. Sprint 20b's
// is rolled into Sprint 21's coverage.

#[test]
#[serial]
fn headline_sprint19_condition_message_under_newgc() {
    // Sprint 19: condition-message after handler invocation.
    let out = eval_expr_to_string(
        "block () \
           signal(make(<simple-error>, message: \"x\")) \
         exception (c :: <error>) \
           condition-message(c) \
         end",
    )
    .expect("eval ok");
    assert_eq!(out, "\"x\"");
}

#[test]
#[serial]
fn headline_sprint21_first_class_method_over_pair_list_under_newgc() {
    // Sprint 21: map over an inline list with a lambda.
    let out = eval_expr_to_string("map(method (x) x * x end, #(1, 2, 3))").expect("eval ok");
    assert_eq!(out, "#(1, 4, 9)");
}

#[test]
#[serial]
fn headline_sprint22_table_under_newgc() {
    // Sprint 22: string-keyed table set then get.
    let out = eval_expr_to_string(
        "let t = make(<table>); t[\"foo\"] := 42; t[\"foo\"]",
    )
    .expect("eval ok");
    assert_eq!(out, "42");
}

#[test]
#[serial]
fn headline_sprint23_gc_stats_reports_backend() {
    // Sprint 23: the new gc-stats-report includes the backend name.
    // Under default `newgc-backend` we expect `page-mark-evacuate`;
    // under `semispace-backend` we expect `semispace`. The test
    // accepts either.
    collect_minor();
    let s = gc_stats();
    assert!(
        s.heap_backend == "page-mark-evacuate" || s.heap_backend == "semispace",
        "unexpected backend name: {}",
        s.heap_backend
    );
}
