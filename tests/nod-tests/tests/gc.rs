//! Sprint 11 — generational copying GC.
//!
//! Tests cover:
//!
//!   1. Allocate-and-trace: 100 byte-strings, traced from a root vector.
//!   2. Minor GC frees young after roots drop.
//!   3. Minor GC promotes root-held survivors into old.
//!   4. Major GC reclaims old after roots drop.
//!   5. Forwarding works: pre-GC and post-GC reads of the same root
//!      give the same logical value.
//!   6. Conservative stack pin: a synthetic stack-shaped Word array
//!      pins its target across a minor GC.
//!   7. Allocation stress: enough small allocations to trigger several
//!      minor GCs; the process doesn't OOM.
//!   8. Sprint 10 regression: existing literal / eval paths still work.
//!   9. Immediates survive GC: the pinned singletons keep stable
//!      addresses across multiple major GCs.
//!  10. Literal-pool stability: a JIT-baked string literal's address
//!      survives several GC cycles.

use nod_runtime::{
    ClassTable, Heap, RootSet, SymbolTable, Word, collect_full, collect_minor, gc_stats_report,
    intern_string_literal, literal_pool_immediates, trace_heap, try_byte_string,
    try_simple_object_vector, try_simple_object_vector_mut,
};
use nod_sema::eval_expr_to_string;

// ─── (1) Allocate-and-trace ───────────────────────────────────────────────

#[test]
fn allocate_100_byte_strings_and_trace() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Vector to hold 100 byte-strings.
    let vec_word = heap.alloc_simple_object_vector(100, &ct);
    for i in 0..100 {
        let s = heap.alloc_byte_string(&format!("s{i}"), &ct);
        // SAFETY: vec_word is unique; single-threaded test.
        let v = unsafe { try_simple_object_vector_mut(vec_word, ct.simple_object_vector()) }
            .expect("vector class");
        // SAFETY: same.
        let slots = unsafe { v.slots_mut() };
        slots[i] = s;
    }
    let mut roots = RootSet::new();
    roots.add_static(&vec_word as *const Word);
    let trace = trace_heap(&roots, &heap, &ct);
    // 1 vector + 100 strings.
    assert_eq!(trace.objects.len(), 101);
    assert_eq!(trace.count_of(ct.byte_string()), 100);
    assert_eq!(trace.count_of(ct.simple_object_vector()), 1);
}

// ─── (2) Minor GC frees young (no roots) ─────────────────────────────────

#[test]
fn minor_gc_reclaims_young_with_no_roots() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Allocate 1000 strings; don't register any of them as roots.
    for i in 0..1000 {
        let _ = heap.alloc_byte_string(&format!("transient{i}"), &ct);
    }
    let live_before = heap.young_used_bytes();
    assert!(live_before > 0, "young should hold the 1000 strings");
    heap.collect_minor();
    let live_after = heap.young_used_bytes();
    // No roots → everything in young should be reclaimed.
    assert_eq!(live_after, 0);
    let old_after = heap.old_used_bytes();
    // Survivors copy into old; with no roots, nothing copies.
    assert_eq!(old_after, 0);
}

// ─── (3) Minor GC promotes root-held survivors ───────────────────────────

// Sprint 23: semispace-specific — NewGC's first minor cycle does
// within-G0 evacuation, not G0→old promotion (promotion fires on
// cycle `G0_PROMOTION_THRESHOLD = 3`). The semantic assertion this
// test makes — "young is empty post-minor" — is exactly what
// NewGC's age-based cohort promotion was designed to NOT do.
#[cfg(feature = "semispace-backend")]
#[test]
fn minor_gc_promotes_survivors_into_old() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Allocate 100; hold roots on 50 of them via a vector.
    let vec_word = heap.alloc_simple_object_vector(50, &ct);
    for i in 0..100 {
        let s = heap.alloc_byte_string(&format!("s{i}"), &ct);
        if i < 50 {
            // SAFETY: vector is unique, single-threaded.
            let v = unsafe { try_simple_object_vector_mut(vec_word, ct.simple_object_vector()) }
                .expect("vector class");
            // SAFETY: same.
            let slots = unsafe { v.slots_mut() };
            slots[i] = s;
        }
    }
    heap.register_root(&vec_word as *const Word);
    let young_before = heap.young_used_bytes();
    let old_before = heap.old_used_bytes();
    assert!(young_before > 0);
    heap.collect_minor();
    let young_after = heap.young_used_bytes();
    let old_after = heap.old_used_bytes();
    assert_eq!(young_after, 0, "young should be empty post-minor");
    // 50 strings (24 bytes each) + 1 vector (424 bytes) survived.
    assert!(
        old_after > old_before,
        "old should grow from survivor promotion"
    );
    heap.unregister_root(&vec_word as *const Word);
}

// ─── (4) Major GC reclaims old ────────────────────────────────────────────

// Sprint 23: semispace-specific — same "every survivor tenures on
// first minor" assumption as `minor_gc_promotes_survivors_into_old`
// above. NewGC keeps cohorts in G0 for 3 minor cycles before
// promotion, so `old_with_roots > 0` after a single minor is false
// by design.
#[cfg(feature = "semispace-backend")]
#[test]
fn major_gc_reclaims_old_after_roots_drop() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Build up old via a minor GC of root-held allocations.
    let vec_word = heap.alloc_simple_object_vector(20, &ct);
    for i in 0..20 {
        let s = heap.alloc_byte_string(&format!("k{i}"), &ct);
        // SAFETY: unique.
        let v = unsafe { try_simple_object_vector_mut(vec_word, ct.simple_object_vector()) }
            .expect("vector class");
        // SAFETY: same.
        let slots = unsafe { v.slots_mut() };
        slots[i] = s;
    }
    heap.register_root(&vec_word as *const Word);
    heap.collect_minor();
    let old_with_roots = heap.old_used_bytes();
    assert!(old_with_roots > 0);
    // Drop the root.
    heap.unregister_root(&vec_word as *const Word);
    heap.collect_full();
    let old_after = heap.old_used_bytes();
    assert_eq!(old_after, 0, "no roots → nothing live in old");
}

// ─── (5) Forwarding works: post-GC read gives same logical value ─────────

#[test]
fn forwarding_preserves_byte_string_content() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Vector holds the string root through GC.
    let vec_word = heap.alloc_simple_object_vector(1, &ct);
    let s_word = heap.alloc_byte_string("forwarded!", &ct);
    // SAFETY: unique.
    let v = unsafe { try_simple_object_vector_mut(vec_word, ct.simple_object_vector()) }
        .expect("vector class");
    // SAFETY: same.
    let slots = unsafe { v.slots_mut() };
    slots[0] = s_word;
    heap.register_root(&vec_word as *const Word);
    let s_addr_before = s_word.raw() & !1;
    heap.collect_minor();
    // The original `vec_word` Word on the stack was rewritten by the
    // collector (we registered &vec_word as a root). Read it back.
    // SAFETY: vec_word is a live `Word` on the test's stack.
    let root_word = unsafe { *(&vec_word as *const Word) };
    // SAFETY: root_word is now the post-GC tagged pointer.
    let v2 = unsafe { try_simple_object_vector(root_word, ct.simple_object_vector()) }
        .expect("vector class");
    // SAFETY: v2 is the surviving copy.
    let slots = unsafe { v2.slots() };
    let s_word_after = slots[0];
    let s_addr_after = s_word_after.raw() & !1;
    assert_ne!(
        s_addr_before, s_addr_after,
        "minor GC should have moved the string into old"
    );
    // SAFETY: s_word_after points at the surviving copy.
    let bs = unsafe { try_byte_string(s_word_after, ct.byte_string()) }
        .expect("class still byte-string post-GC");
    // SAFETY: live.
    assert_eq!(unsafe { bs.as_str() }, Some("forwarded!"));
    heap.unregister_root(&vec_word as *const Word);
}

// ─── (6) Conservative stack pin smoke test ───────────────────────────────

// Sprint 23: semispace-specific — NewGC is compiled without the
// conservative-pin Cargo feature (we're a precise-roots-only client
// via Sprint 11c's lock-free registry). `pin_stack_range` is a
// no-op on the newgc-backend, returning 0 unconditionally.
#[cfg(feature = "semispace-backend")]
#[test]
fn conservative_stack_pin_keeps_object_alive() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let s_word = heap.alloc_byte_string("pinme", &ct);
    // Build an artificial "stack" — a Vec<Word> on the heap whose
    // first slot holds s_word's raw bits.
    let stack: Vec<u64> = vec![s_word.raw()];
    let lo = stack.as_ptr() as usize;
    let hi = lo + stack.len() * 8;
    // SAFETY: stack is a properly-aligned Vec<u64>.
    let n_pinned = unsafe { heap.pin_stack_range(lo, hi) };
    assert!(n_pinned >= 1, "expected at least one pinned object");
    let minor_before = heap.minor_collection_count();
    heap.collect_minor();
    let minor_after = heap.minor_collection_count();
    assert!(minor_after > minor_before);
    // Pinned object got promoted into old (Sprint 11 design choice).
    assert!(heap.old_used_bytes() > 0);
    // Stack-borrowed variable here keeps `stack` live across the GC.
    let _ = stack;
}

// ─── (7) Allocation stress triggers minor GCs ────────────────────────────

// Sprint 23: semispace-specific — the semispace's 4MB young region
// forces a minor cycle every ~250K small allocations; NewGC's 1GB
// reservation has thousands of fresh pages, so 50K small allocations
// never approach a page-shortage trigger. Sprint 24's
// "should_collect_auto" wiring will give NewGC an allocation-budget
// trigger that fires on a comparable cadence.
#[cfg(feature = "semispace-backend")]
#[test]
fn many_allocations_trigger_minor_gcs() {
    let heap = Heap::with_capacity(2 * 1024 * 1024); // small heap → forces GC
    let ct = ClassTable::new();
    // Allocate a lot of throwaway strings; no roots → minor GCs reclaim.
    for i in 0..50_000 {
        let _ = heap.alloc_byte_string(&format!("throwaway-{i}-content"), &ct);
    }
    let n = heap.minor_collection_count();
    assert!(
        n > 0,
        "expected at least one minor GC; got {n}"
    );
    // The heap should still be alive — we walk live_bytes without panicking.
    let _ = heap.live_bytes();
}

// ─── (8) Sprint 10 regression: eval still works ──────────────────────────

#[test]
fn sprint_10_eval_smoke_tests_post_gc() {
    let s1 = eval_expr_to_string("1 + 2 * 3").expect("eval ok");
    // Sprint 51e flat (DRM) precedence: all binary operators share one
    // left-associative level, so `1 + 2 * 3` is `(1 + 2) * 3 = 9`, NOT
    // the C-style `1 + (2 * 3) = 7`. This assertion was stale from the
    // flat-precedence default landing (cf. the matching, already-updated
    // assertion in `codegen.rs::eval_arithmetic_precedence`).
    assert_eq!(s1, "9");
    let s2 = eval_expr_to_string("instance?(#t, <boolean>)").expect("eval ok");
    assert_eq!(s2, "#t");
    // Trigger a few collects on the process-global heap.
    collect_minor();
    collect_minor();
    collect_full();
    // Re-evaluate — same answer. Flat (DRM) precedence: `(1 + 2) * 3 = 9`.
    let s3 = eval_expr_to_string("1 + 2 * 3").expect("eval ok");
    assert_eq!(s3, "9");
    let s4 = eval_expr_to_string("instance?(#t, <boolean>)").expect("eval ok");
    assert_eq!(s4, "#t");
}

// ─── (9) Immediates survive multiple major GCs ───────────────────────────

#[test]
fn immediates_addresses_stable_across_major_gcs() {
    let imm_a = literal_pool_immediates();
    collect_full();
    collect_full();
    collect_full();
    let imm_b = literal_pool_immediates();
    assert_eq!(imm_a.true_, imm_b.true_);
    assert_eq!(imm_a.false_, imm_b.false_);
    assert_eq!(imm_a.nil, imm_b.nil);
    // And `instance?(#t, <boolean>)` still works.
    let s = eval_expr_to_string("instance?(#t, <boolean>)").expect("eval ok");
    assert_eq!(s, "#t");
}

// ─── (10) Literal-pool string-literal address survives GC ────────────────

#[test]
fn literal_pool_string_address_stable_across_gcs() {
    let w_a = intern_string_literal("a literal that lives forever");
    let addr_a = w_a.raw() & !1;
    // Hammer the heap to force several minor GCs.
    let local_heap = Heap::new();
    let ct = ClassTable::new();
    for i in 0..10_000 {
        let _ = local_heap.alloc_byte_string(&format!("hammer-{i}"), &ct);
    }
    collect_full();
    collect_full();
    let w_b = intern_string_literal("a literal that lives forever");
    // intern_string_literal currently allocates a fresh copy each call
    // (no per-string dedup in the static-area path). What we're really
    // checking is that the first call's Word remains valid + the bytes
    // are still readable.
    let _ = w_b;
    let s_word_after = Word::from_raw((addr_a as u64) | 1);
    // SAFETY: addr_a is a pinned-static byte-string allocation; its
    // bytes are still alive.
    let bs = unsafe { try_byte_string(s_word_after, ct.byte_string()) }
        .expect("class stable post-GC");
    // SAFETY: bs points at live pinned storage.
    assert_eq!(
        unsafe { bs.as_str() },
        Some("a literal that lives forever")
    );
}

// ─── (Bonus) Fibonacci-style allocation stress through eval ─────────────

/// Dial scaled to be reasonable for `cargo test` on a dev machine. The
/// SPRINTS.md acceptance criterion talks about "1M `<byte-string>`
/// objects"; in practice we hit the same heap-cycling behaviour at
/// 10× lower scale with much shorter test time.
#[cfg(feature = "semispace-backend")]
const STRESS_ALLOCATIONS: usize = 100_000;

// Sprint 23: semispace-specific — relies on automatic minor-GC
// triggering (same reasoning as `many_allocations_trigger_minor_gcs`).
// Under NewGC the 100K allocations spread across the 1GB reservation
// without any GC firing; the "process doesn't OOM" guarantee still
// holds, but the "many minor GCs" assertion doesn't.
#[cfg(feature = "semispace-backend")]
#[test]
fn allocation_stress_completes_without_oom() {
    // Small young capacity so GCs fire frequently within the test run.
    let heap = Heap::with_config(nod_runtime::GcConfig {
        young_bytes: 64 * 1024,
        old_bytes: 1024 * 1024,
    });
    let ct = ClassTable::new();
    // Allocate STRESS_ALLOCATIONS short-lived byte-strings; no roots
    // → minor GCs reclaim each round. Process must not OOM.
    for i in 0..STRESS_ALLOCATIONS {
        let _ = heap.alloc_byte_string(&format!("stress-{i}"), &ct);
    }
    let n_minor = heap.minor_collection_count();
    assert!(
        n_minor > 10,
        "expected many minor GCs over {STRESS_ALLOCATIONS} allocations, \
         got {n_minor}"
    );
    // Live total should be modest — most of the allocations were
    // throwaway, so each GC reclaimed nearly all.
    let live = heap.live_bytes();
    assert!(
        live < 1024 * 1024,
        "live={live} bytes after stress; expected to fit in 1 MB"
    );
}

// ─── (Bonus) Sprint 10 regression: format-out still prints ───────────────

#[test]
fn format_out_hello_post_gc() {
    nod_runtime::install_test_writer();
    collect_full(); // GC before
    let _ = eval_expr_to_string("format-out(\"hello\\n\")").expect("eval ok");
    let buf = nod_runtime::take_test_writer().unwrap_or_default();
    nod_runtime::uninstall_test_writer();
    assert_eq!(&buf, b"hello\n");
}

// ─── (Bonus) Symbol intern still works ────────────────────────────────────

#[test]
fn symbol_intern_round_trip_after_gc() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let st = SymbolTable::new();
    let _ = st.intern("alpha", &heap, &ct);
    heap.collect_minor();
    let b = st.intern("alpha", &heap, &ct);
    // Same name → same word (table dedups).
    let c = st.intern("alpha", &heap, &ct);
    assert_eq!(b, c);
}

// ─── (Bonus) gc_stats_report produces a non-trivial string ───────────────

#[test]
fn gc_stats_report_includes_backend_and_counters() {
    collect_minor();
    let r = gc_stats_report();
    eprintln!("--- GC STATS REPORT ---\n{r}--- /GC STATS REPORT ---");
    assert!(r.contains("backend"), "report:\n{r}");
    // Sprint 23: accept either backend name. Default build runs the
    // NewGC `page-mark-evacuate` backend; the `--no-default-features
    // --features semispace-backend` escape hatch keeps the old name.
    assert!(
        r.contains("page-mark-evacuate") || r.contains("semispace"),
        "report:\n{r}"
    );
    assert!(r.contains("minor collections"), "report:\n{r}");
    assert!(r.contains("major collections"), "report:\n{r}");
}

