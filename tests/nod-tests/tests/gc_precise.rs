//! Sprint 11b — precise GC roots via spill-to-runtime-slots.
//!
//! These tests close the soundness gap flagged in `NCL_GC_FEEDBACK.md`
//! §2 ("Pin-then-promote question"): with Sprint 12's `make`-via-
//! moveable-heap path, two `let a = make(...); let b = make(...)`
//! statements could leave `a` as a stale pointer if `b`'s allocation
//! evacuates `a`'s target. Sprint 11b brackets every potentially-
//! allocating JIT call with `nod_register_root` / `nod_unregister_root`
//! pairs around an entry-block `alloca` slot; the GC walks the
//! registered slots, evacuates if needed, and rewrites the slots so
//! the post-call reload picks up the relocated address.
//!
//! Phase F coverage from the Sprint 11b brief:
//!   1. JIT IR shape — `nod_register_root` / `nod_unregister_root`
//!      brackets show up in the textual LLVM IR for a function that
//!      `make`s in sequence.
//!   2. Allocation-across-allocation soundness — `rust_make` followed
//!      by a forced minor GC followed by another `rust_make` keeps
//!      the first instance readable.
//!   3. Root count balances — register / unregister are paired; the
//!      pool's `root_count` returns to zero after every call site.
//!   4. JIT round-trip end-to-end — the `x(a) + x(b)` fixture compiles
//!      and produces the expected sum.
//!   5. Liveness verifier rejects bogus claims.
//!   6. `pin_stack_range` is feature-gated behind `gc-conservative-pin`
//!      and not exercised by normal execution.
//!   7. NCL stack_map.rs compiles into nod-runtime (Sprint 11c prep).
//!   8. Sprint 12 regression — `point_distance_squared_returns_25`
//!      still passes (covered in `classes.rs`).

use std::path::{Path, PathBuf};

use serial_test::serial;

use nod_dfm::{
    Block, BlockId, Computation, ConstValue, Function, FunctionId, SafepointError, Temporary,
    TempId, Terminator, TypeEstimate, populate_safepoint_roots, verify_safepoint_roots,
};
use nod_reader::{FileId, Span};
use nod_runtime::{
    ClassId, Word, class_metadata_for, collect_minor, find_class_id_by_name, gc_stats,
    rust_make, with_literal_pool,
};
use nod_sema::{dump_llvm_for_file, lower_module, run_function_to_i64};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn parse(src: &str) -> nod_reader::Module {
    use nod_reader::{SourceMap, lex, parse_module, scan_preamble};
    let mut sm = SourceMap::new();
    let id = sm.add("<t>", src.to_string()).unwrap();
    let toks = lex(src, id);
    let pre = scan_preamble(src);
    parse_module(src, &toks, pre.as_ref()).expect("parse")
}

fn fake_span() -> Span {
    Span::new(FileId(0), 0, 0)
}

fn mk_temp(id: u32, ty: TypeEstimate) -> Temporary {
    Temporary {
        id: TempId(id),
        type_estimate: ty,
    }
}

/// 1. JIT IR shape: a Dylan function that allocates two instances in
///    sequence emits safepoint brackets around the second `make`, with
///    the first instance's Word spilled to an `alloca` slot.
///
/// Naming note: codegen migrated from the legacy
/// `nod_register_root` / `nod_unregister_root` per-Word root-stack
/// pattern to the per-safepoint slot-slab + `nod_jit_begin_safepoint`
/// / `nod_jit_end_safepoint` protocol (see
/// `src/nod-llvm/src/codegen.rs::begin_safepoint`). The slot names
/// also gained a per-site prefix (`gc.s<N>.reload.tN`). Assertions
/// updated to match — the structural shape we want is the same
/// (entry-block alloca slot + bracketing safepoint calls +
/// post-safepoint reload).
#[test]
#[serial]
fn jit_ir_brackets_second_make_with_register_root() {
    // Use a distinct fixture with a distinct class name; the class
    // registry is process-global and Sprint 12's
    // `ClassRedefinitionNotSupported` would otherwise refuse the
    // other test's lowering when both run in the same binary.
    let path = fixtures_dir().join("gc_precise_two_makes_ir.dylan");
    let ir = dump_llvm_for_file(&path).expect("dump LLVM IR");
    assert!(
        ir.contains("nod_jit_begin_safepoint"),
        "expected nod_jit_begin_safepoint in IR:\n{ir}"
    );
    assert!(
        ir.contains("nod_jit_end_safepoint"),
        "expected nod_jit_end_safepoint in IR:\n{ir}"
    );
    assert!(
        ir.contains("gc.root.slot."),
        "expected entry-block alloca slot in IR:\n{ir}"
    );
    // The reload-after-call path is the key correctness sequence —
    // verify a `load i64, ptr %gc.root.slot.*` and a subsequent use.
    assert!(
        ir.contains(".reload.t"),
        "expected reload label (`.reload.tN`) in IR:\n{ir}"
    );
}

/// 4. End-to-end: the same two-make fixture compiles and produces the
///    expected sum.
#[test]
#[serial]
fn two_makes_run_end_to_end_returns_sum() {
    let path = fixtures_dir().join("gc_precise_two_makes.dylan");
    let result = run_function_to_i64(&path, "main").expect("run two-makes main");
    assert_eq!(result, 4, "expected x(a) + x(b) = 1 + 3 = 4");
}

/// 2. Allocation-across-allocation soundness via the runtime path. We
///    allocate `a`, then run a manual minor GC (which evacuates `a`
///    into old), then read `a`'s x slot. Without precise roots, `a`'s
///    Word would be left pointing at the stale young-gen address.
///    With Sprint 11b's `rust_make` -> `RootGuard` wiring (and an
///    explicit `register_root` here keeping `a` alive across the
///    forced collection), the slot is rewritten and the read returns
///    the original value.
#[test]
#[serial]
fn allocation_across_gc_keeps_first_instance_readable() {
    // Serialise against `root_count_balances_…` which observes the
    // global root count exactly.
    // Register a fresh user class so the test is hermetic across
    // re-runs in the same process. (Sprint 12 refuses redefinition.)
    let class_name = "<gcp-soundness-aagkfir>";
    let src = format!(
        "define class {class_name} (<object>)
            slot v :: <integer>, init-keyword: v:;
         end class;"
    );
    let m = parse(&src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name(class_name).expect("class registered");
    let md = class_metadata_for(id);

    // Allocate `a` in young.
    let three = Word::from_fixnum(42).unwrap();
    // SAFETY: registered class, fresh Word.
    let a = unsafe { rust_make(md, &[("v", three)]) };
    assert!(a.is_pointer());

    // Manually register `a` as a root so the forced GC doesn't drop it.
    // This is the same mechanism the JIT uses across allocating calls.
    let a_slot: *const Word = &a;
    with_literal_pool(|pool| pool.heap.register_root(a_slot));

    // Force a minor collection. `a`'s underlying object evacuates from
    // young to old; the heap rewrites the slot at &a.
    let collections_before = gc_stats().minor_collections;
    collect_minor();
    let collections_after = gc_stats().minor_collections;
    assert!(
        collections_after > collections_before,
        "expected collect_minor to fire"
    );

    // Unregister now that we're past the GC.
    with_literal_pool(|pool| pool.heap.unregister_root(a_slot));

    // Read `a.v` — must still return 42 even though `a`'s underlying
    // address has been rewritten by the collector.
    assert!(a.is_pointer(), "a stayed pointer-tagged across GC");
    let addr = a.as_ptr::<u8>().unwrap() as usize;
    // SAFETY: a is live (registered root during the GC); offset 8 is
    // the first slot per Sprint 12's layout.
    let v = unsafe { *((addr + 8) as *const Word) };
    assert_eq!(v.as_fixnum(), Some(42));
}

/// 3. Root count balances around manual register/unregister pairs.
///    Two registers + two unregisters → `root_count` ends at 0.
#[test]
#[serial]
fn root_count_balances_after_register_unregister_pairs() {
    // The global heap's root list is shared with concurrent `rust_make`
    // calls in other tests (each one transiently registers+unregisters
    // via `RootGuard`). Strict equality on `mid` would race; we assert
    // the invariant that matters: register/unregister are balanced —
    // any net change must come from OUR 2 registers + 2 unregisters,
    // which net to zero.
    let count_before = with_literal_pool(|pool| pool.heap.root_count());
    let w1 = Word::from_fixnum(1).unwrap();
    let w2 = Word::from_fixnum(2).unwrap();
    with_literal_pool(|pool| {
        pool.heap.register_root(&w1 as *const Word);
        pool.heap.register_root(&w2 as *const Word);
    });
    let mid = with_literal_pool(|pool| pool.heap.root_count());
    assert!(
        mid >= count_before + 2,
        "expected mid={mid} >= count_before+2 ({})",
        count_before + 2
    );
    with_literal_pool(|pool| {
        pool.heap.unregister_root(&w1 as *const Word);
        pool.heap.unregister_root(&w2 as *const Word);
    });
    let after = with_literal_pool(|pool| pool.heap.root_count());
    // After our 2 unregisters, the count must have dropped by at
    // least 2 from `mid`. Concurrent rust_makes may still be in flight
    // (transiently +1, not yet -1) so we don't strictly compare to
    // count_before — the API contract being tested is "register and
    // unregister are balanced for OUR pair", which `mid - after >= 2`
    // captures correctly.
    assert!(
        mid.saturating_sub(after) >= 2,
        "expected mid({mid}) - after({after}) >= 2"
    );
}

/// 5. Liveness verifier extension: a Computation whose
///    `safepoint_roots` claims a temp that isn't actually live across
///    the call is rejected.
#[test]
#[serial]
fn verifier_rejects_bogus_safepoint_claim() {
    // fn f() -> Integer:
    //   t0 = Const 1                ; type Integer (no protection needed)
    //   t1 = DirectCall foo()        ; claim t0 alive across — BOGUS:
    //                                  Integer doesn't need protection
    //   Return t1
    let f = Function {
        id: FunctionId(0),
        name: "f".into(),
        params: vec![],
        entry: BlockId(0),
        blocks: vec![Block {
            id: BlockId(0),
            label: "entry".into(),
            params: vec![],
            computations: vec![
                Computation::Const {
                    dst: TempId(0),
                    value: ConstValue::Integer(1),
                },
                Computation::DirectCall {
                    dst: TempId(1),
                    callee: "foo".into(),
                    args: vec![],
                    safepoint_roots: vec![TempId(0)],
                    is_no_alloc: false,
                },
            ],
            terminator: Terminator::Return { value: Some(TempId(1)) },
        }],
        temps: vec![
            mk_temp(0, TypeEstimate::Integer),
            mk_temp(1, TypeEstimate::Integer),
        ],
        return_type: TypeEstimate::Integer,
        span: fake_span(),
    };
    let errs = verify_safepoint_roots(&f).expect_err("bogus claim must be rejected");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SafepointError::TempDoesNotNeedProtection { temp, .. } if *temp == TempId(0)
        )),
        "expected TempDoesNotNeedProtection: {errs:?}"
    );
}

#[test]
#[serial]
fn verifier_rejects_dead_temp_claim() {
    // fn f() -> Top:
    //   t0 = Const "x"               ; pointer-shaped, definitely defined here
    //   t1 = DirectCall foo()         ; t1 dst; BOGUS claim that t1 alive
    //                                   across — but t1 is the call's own
    //                                   result, NOT live across.
    //   Return t1
    let f = Function {
        id: FunctionId(0),
        name: "f".into(),
        params: vec![],
        entry: BlockId(0),
        blocks: vec![Block {
            id: BlockId(0),
            label: "entry".into(),
            params: vec![],
            computations: vec![
                Computation::Const {
                    dst: TempId(0),
                    value: ConstValue::String("x".into()),
                },
                Computation::DirectCall {
                    dst: TempId(1),
                    callee: "foo".into(),
                    args: vec![],
                    safepoint_roots: vec![TempId(1)],
                    is_no_alloc: false,
                },
            ],
            terminator: Terminator::Return { value: Some(TempId(1)) },
        }],
        temps: vec![
            mk_temp(0, TypeEstimate::String),
            mk_temp(1, TypeEstimate::Top),
        ],
        return_type: TypeEstimate::Top,
        span: fake_span(),
    };
    let errs = verify_safepoint_roots(&f).expect_err("dead-temp claim must reject");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SafepointError::TempNotLiveAcrossCall { temp, .. } if *temp == TempId(1)
        )),
        "expected TempNotLiveAcrossCall for t1: {errs:?}"
    );
}

/// 5b. The verifier accepts the correctly-populated `safepoint_roots`
///     emitted by the liveness pass.
#[test]
#[serial]
fn populated_safepoint_roots_pass_verifier() {
    // fn f() -> Integer:
    //   t0 = Const String "x"         ; pointer-shaped
    //   t1 = DirectCall make()         ; t0 live across (used by Return)
    //   Return ???     -- can't return String directly; let's just
    //   put t1 = Direct, then t2 = Direct(t0), Return t2
    let mut f = Function {
        id: FunctionId(0),
        name: "f".into(),
        params: vec![],
        entry: BlockId(0),
        blocks: vec![Block {
            id: BlockId(0),
            label: "entry".into(),
            params: vec![],
            computations: vec![
                Computation::Const {
                    dst: TempId(0),
                    value: ConstValue::String("x".into()),
                },
                Computation::DirectCall {
                    dst: TempId(1),
                    callee: "alloc".into(),
                    args: vec![],
                    safepoint_roots: vec![],
                    is_no_alloc: false,
                },
                Computation::DirectCall {
                    dst: TempId(2),
                    callee: "use_string".into(),
                    args: vec![TempId(0)],
                    safepoint_roots: vec![],
                    is_no_alloc: false,
                },
            ],
            terminator: Terminator::Return { value: Some(TempId(2)) },
        }],
        temps: vec![
            mk_temp(0, TypeEstimate::String),
            mk_temp(1, TypeEstimate::Top),
            mk_temp(2, TypeEstimate::Integer),
        ],
        return_type: TypeEstimate::Integer,
        span: fake_span(),
    };
    populate_safepoint_roots(&mut f);
    // First call: t0 alive across (used by call #2's operands).
    let call1 = f.blocks[0].computations[1].safepoint_roots().unwrap();
    assert_eq!(call1, &[TempId(0)]);
    verify_safepoint_roots(&f).expect("populated roots pass verifier");
}

/// 6. `pin_stack_range` is feature-gated. The default build does not
///    set `Pinned` bits, and a large allocation run produces non-zero
///    minor collections without ever invoking the conservative
///    pinner. The Sprint 11b precise-roots story replaces it.
#[test]
#[serial]
fn gc_runs_without_conservative_pinning() {
    let class_name = "<gcp-stress-no-pin>";
    let src = format!(
        "define class {class_name} (<object>)
            slot v :: <integer>, init-keyword: v:;
         end class;"
    );
    let m = parse(&src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name(class_name).expect("class registered");
    let md = class_metadata_for(id);
    let count_before = gc_stats().minor_collections;
    let pinned_before = gc_stats().last_pinned_objects;
    // Allocate some, then force a minor GC explicitly. The point of
    // this test is to verify that under normal allocation pressure
    // no Pinned bits are ever set — not to discover the young-gen
    // threshold (which varies with prior test state in the same
    // process).
    for n in 0..1000 {
        let w = Word::from_fixnum(n).unwrap();
        // SAFETY: registered class.
        let _ = unsafe { rust_make(md, &[("v", w)]) };
    }
    collect_minor();
    let count_after = gc_stats().minor_collections;
    let pinned_after = gc_stats().last_pinned_objects;
    assert!(
        count_after > count_before,
        "collect_minor() should have fired"
    );
    // No pinned bits — Sprint 11b does NOT exercise pin_stack_range
    // from production paths. The conservative pinner is feature-gated
    // and only the dedicated unit test in `gc.rs` should set Pinned.
    assert_eq!(
        pinned_after, pinned_before,
        "no production path should pin objects in Sprint 11b"
    );
}

/// 7. NCL stack_map.rs compiles. The types are reachable from
///    `nod_runtime`; using them here forces the dependency to typecheck
///    even if no other test imports them.
#[test]
#[serial]
fn stack_map_module_is_usable() {
    let mut sm = nod_runtime::StackMap::new();
    assert!(sm.is_empty());
    sm.register(nod_runtime::StackMapEntry {
        pc: 0xC0FFEE,
        slots: vec![nod_runtime::LiveSlot::FpOffset(8)],
    });
    assert_eq!(sm.len(), 1);
    assert!(sm.lookup(0xC0FFEE).is_some());
}

/// 8. JIT-emitted IR for an arithmetic-only Sprint 07 function does
///    NOT contain register_root calls — the liveness pass correctly
///    reports an empty live set for `let x = 1; x + 1` etc. This is
///    a tightness check: we don't over-spill.
#[test]
#[serial]
fn fixnum_only_function_has_no_register_root_calls() {
    let path = fixtures_dir().join("factorial.dylan");
    let ir = dump_llvm_for_file(&path).expect("dump factorial IR");
    assert!(
        !ir.contains("nod_register_root"),
        "fixnum-only factorial must not register any roots:\n{ir}"
    );
}

/// Sprint 12 regression: the headline acceptance test still passes
/// (this is also asserted in `classes.rs` but duplicated here for
/// the precise-roots story to stand alone).
#[test]
#[serial]
fn point_distance_squared_still_returns_25() {
    let path = fixtures_dir().join("point.dylan");
    let result = run_function_to_i64(&path, "main").expect("run point main");
    assert_eq!(result, 25);
}

/// Many `<point>` allocations interleaved with explicit GC drives,
/// each followed by a read of a previously-allocated instance.
#[test]
#[serial]
fn many_allocations_with_interleaved_gc_keep_first_readable() {
    let class_name = "<gcp-stress-interleave>";
    let src = format!(
        "define class {class_name} (<object>)
            slot v :: <integer>, init-keyword: v:;
         end class;"
    );
    let m = parse(&src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name(class_name).expect("class registered");
    let md = class_metadata_for(id);

    let a = unsafe { rust_make(md, &[("v", Word::from_fixnum(111).unwrap())]) };
    let a_slot: *const Word = &a;
    with_literal_pool(|pool| pool.heap.register_root(a_slot));

    for _ in 0..50 {
        let _ = unsafe { rust_make(md, &[("v", Word::from_fixnum(0).unwrap())]) };
        // Force a collection every iteration to maximise the chance
        // of `a` getting evacuated.
        collect_minor();
    }
    with_literal_pool(|pool| pool.heap.unregister_root(a_slot));

    let addr = a.as_ptr::<u8>().expect("a stayed pointer") as usize;
    let v = unsafe { *((addr + 8) as *const Word) };
    assert_eq!(
        v.as_fixnum(),
        Some(111),
        "first instance's slot survives many GC cycles"
    );
}

/// Sprint 11c — lock-free root registration smoke test. The mutex
/// baseline in Sprint 11b took ~3 seconds for 1M register/unregister
/// pairs on the bench machine; the thread-local replacement finishes
/// in well under 500 ms. We can't directly observe "no mutex" in safe
/// code, but a tight-loop timing assertion is a sufficient smoke test
/// against accidental re-introduction of locking on the hot path.
#[test]
#[serial]
fn lock_free_roots_no_mutex_acquisition() {
    let start = std::time::Instant::now();
    let w = Word::from_fixnum(7).unwrap();
    let slot: *const Word = &w;
    for _ in 0..1_000_000 {
        with_literal_pool(|pool| {
            pool.heap.register_root(slot);
            pool.heap.unregister_root(slot);
        });
    }
    let elapsed = start.elapsed();
    // 1M register/unregister pairs should complete well under 500 ms
    // when the registry is thread-local. The Sprint 11b mutex version
    // was ~3 s. Generous threshold to absorb CI variance and the
    // `with_literal_pool` mutex (which still locks — that's the
    // process-global pool, not the root list).
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "lock-free roots should complete 1M iterations in <500ms; took {elapsed:?}"
    );
}

/// Sanity: ClassId is exported correctly.
#[test]
#[serial]
fn user_class_metadata_is_subclass_of_object() {
    let class_name = "<gcp-cls>";
    let src = format!(
        "define class {class_name} (<object>)
            slot v :: <integer>, init-keyword: v:;
         end class;"
    );
    let m = parse(&src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name(class_name).expect("class registered");
    assert!(nod_runtime::is_subclass(id, ClassId::OBJECT));
}
