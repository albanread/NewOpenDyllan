//! Synthetic reproducer for `NEWGC_BUG_ENV_RECLAIM_UNDER_CALLBACK.md`.
//!
//! Builds a closure-shaped object graph identical to what the Win32
//! WNDPROC callback path constructs (closure `<function>` → env-ptr →
//! `<environment>` → cells SOV → `<cell>` objects), registers the
//! closure Word as a GC root the same way `callbacks.rs::install_gc_
//! roots_for_this_thread` does, then drives several minor cycles with
//! byte-string allocation pressure between them.
//!
//! Per bug report §9: if env is reclaimed without the WNDPROC path, the
//! bug is in the runtime/GC layer alone — no Win32 dependency.
//!
//! All tests are `#[serial]` because they share the process-global
//! `LITERAL_POOL` heap (`with_literal_pool`).

use std::cell::UnsafeCell;

use serial_test::serial;

use nod_runtime::{
    CallbackSignature, Word, _reset_callbacks_for_tests, callback_slot_address, collect_minor,
    function_env_ptr, heap_register_root, heap_unregister_root, make_cell, make_environment,
    make_function, nod_cell_get, nod_cell_set, nod_env_cell, register_callback, with_literal_pool,
};

/// Captured cells in the synthetic closure. Matches the "~13 captured
/// cells" figure from the bug report §3 for the IDE's WNDPROC.
const N_CELLS: usize = 13;

/// Allocate `n` byte-strings of `payload_bytes` bytes each into the
/// moveable heap. Pure garbage — no roots, exists only to push the
/// G0-promotion counter forward and exercise destination pressure.
fn churn_byte_strings(n: usize, payload_bytes: usize) {
    let filler = "x".repeat(payload_bytes);
    for _ in 0..n {
        let _ = with_literal_pool(|pool| {
            pool.heap.alloc_byte_string(&filler, &pool.classes)
        });
    }
}

/// Build a closure-shaped graph: returns `(closure_w, env_w)`.
/// `code_ptr` is the Rust function the closure body executes when
/// invoked via `nod_funcall4`; the cell-graph tests pass a no-op (we
/// never call the closure), the trampoline-dispatch test passes the
/// real `synthetic_callback_body`.
fn build_closure_graph_with_code(code_ptr: *const u8) -> (Word, Word) {
    let mut cells: Vec<Word> = Vec::with_capacity(N_CELLS);
    for i in 0..N_CELLS {
        cells.push(make_cell(Word::fixnum_unchecked(i as i64)));
    }
    let env = make_environment(&cells);
    // kind_tag = 2 (FUNCTION_KIND_CLOSURE) so any reader sees a closure.
    let closure = make_function("synthetic-wndproc", 4, code_ptr, 2, env.raw());
    (closure, env)
}

/// No-op closure body for tests that never actually invoke the closure.
extern "C-unwind" fn noop_closure_body(
    _e: u64, _a: u64, _b: u64, _c: u64, _d: u64,
) -> u64 { 0 }

fn build_closure_graph() -> (Word, Word) {
    build_closure_graph_with_code(noop_closure_body as *const u8)
}

/// Walk env-ptr → env → cells SOV → cell[i] → value through the
/// `nod_env_cell` / `nod_cell_get` path — i.e. exactly what `nod_
/// funcall4` hands the JIT closure body, which is the panic site.
fn assert_graph_intact(closure_w: Word, label: &str) {
    let env_raw = function_env_ptr(closure_w).expect("env-ptr slot present");
    assert_ne!(env_raw, 0, "[{label}] env-ptr is zero (closure not a closure?)");
    let env_w = Word::from_raw(env_raw);
    assert!(
        env_w.is_pointer(),
        "[{label}] env-ptr is not pointer-tagged (raw = {env_raw:#x})",
    );
    for i in 0..N_CELLS {
        let idx_raw = Word::fixnum_unchecked(i as i64).raw();
        // SAFETY: env_raw came from a live <function>'s env-ptr slot;
        // idx_raw is a fixnum-tagged Word in range. This calls the
        // exact panic path from the bug report — if env was reclaimed,
        // the `try_simple_object_vector` check inside `nod_env_cell`
        // will panic.
        let cell_raw = unsafe { nod_env_cell(env_raw, idx_raw) };
        let cell_w = Word::from_raw(cell_raw);
        assert!(
            cell_w.is_pointer(),
            "[{label}] cells[{i}] not pointer-tagged (raw = {cell_raw:#x})",
        );
        // SAFETY: cell_w is a live <cell> Word returned by nod_env_cell.
        let v_raw = unsafe { nod_cell_get(cell_raw) };
        let v_fix = Word::from_raw(v_raw)
            .as_fixnum()
            .unwrap_or_else(|| panic!("[{label}] cells[{i}].value is not a fixnum"));
        assert_eq!(
            v_fix, i as i64,
            "[{label}] cells[{i}].value mismatch (expected {i}, got {v_fix})",
        );
    }
}

/// Headline reproducer. Build a closure graph, register the closure
/// Word as a GC root via a stable `Box<UnsafeCell<Word>>` (the exact
/// shape the callback registry uses for its slots), then drive minor
/// cycles with byte-string pressure, asserting the graph is intact
/// after each. The bug report says the crash fires within 5
/// keypresses; we drive 8 cycles to comfortably cross G0_PROMOTION_
/// THRESHOLD (3) and into the cascade window.
#[test]
#[serial]
fn callback_closure_env_survives_promotion() {
    let (closure_w, _env_w) = build_closure_graph();
    let slot: Box<UnsafeCell<Word>> = Box::new(UnsafeCell::new(closure_w));
    heap_register_root(slot.get() as *const Word);

    // Sanity check before any GC.
    let cur = unsafe { *slot.get() };
    assert_graph_intact(cur, "pre-GC");

    for cycle in 1..=8 {
        // ~1 MB of throwaway byte-strings per cycle (4096 × ~240 B).
        churn_byte_strings(4096, 240);
        collect_minor();
        let cur = unsafe { *slot.get() };
        assert!(
            cur.is_pointer(),
            "[cycle {cycle}] closure Word no longer pointer-tagged (raw = {:#x})",
            cur.raw(),
        );
        assert_graph_intact(cur, &format!("cycle {cycle}"));
    }

    heap_unregister_root(slot.get() as *const Word);
    drop(slot);
}

/// Mirrors the IDE's WNDPROC body: each "keypress" cycle MUTATES
/// every captured cell (`%cell-set!` → `nod_cell_set` → `write_barrier`)
/// before triggering a minor GC. Per the bug report, the IDE
/// "mutates several captured `<cell>` variables (cursor offset, rope
/// edits, cached flat-string, …)" on each keypress. If the
/// write-barrier path has a bug that corrupts memory near the slot, or
/// fails to mark a card that needs marking, this is the variant that
/// surfaces it.
#[test]
#[serial]
fn callback_closure_env_survives_cell_mutation_under_promotion() {
    let (closure_w, _env_w) = build_closure_graph();
    let slot: Box<UnsafeCell<Word>> = Box::new(UnsafeCell::new(closure_w));
    heap_register_root(slot.get() as *const Word);

    let cur = unsafe { *slot.get() };
    assert_graph_intact(cur, "pre-GC");

    for cycle in 1..=8 {
        // 1. Mutate every captured cell via the full JIT-callable path.
        //    nod_cell_set runs write_barrier on the slot before storing.
        let cur = unsafe { *slot.get() };
        let env_raw = function_env_ptr(cur).expect("env-ptr");
        for i in 0..N_CELLS {
            let idx_raw = Word::fixnum_unchecked(i as i64).raw();
            // SAFETY: env_raw + idx_raw are the same shape as in
            // assert_graph_intact.
            let cell_raw = unsafe { nod_env_cell(env_raw, idx_raw) };
            // Write a fresh fixnum value derived from the cycle so we
            // can verify the mutation roundtrips. New value = i + cycle * N_CELLS.
            let new_val = Word::fixnum_unchecked((i + cycle * N_CELLS) as i64);
            // SAFETY: cell_raw is a live <cell> Word.
            let _ = unsafe { nod_cell_set(new_val.raw(), cell_raw) };
        }

        // 2. Allocate ~1 MB of throwaway byte-strings, mimicking the
        //    IDE's per-keypress rope-edit churn (rope-node, rope-leaf,
        //    cached flat-string).
        churn_byte_strings(4096, 240);

        // 3. Force a minor GC.
        collect_minor();

        // 4. Verify the closure is intact AND the mutations roundtrip.
        let cur = unsafe { *slot.get() };
        assert!(
            cur.is_pointer(),
            "[mut cycle {cycle}] closure Word no longer pointer-tagged (raw = {:#x})",
            cur.raw(),
        );
        let env_raw = function_env_ptr(cur).expect("env-ptr");
        assert!(
            Word::from_raw(env_raw).is_pointer(),
            "[mut cycle {cycle}] env-ptr not pointer-tagged (raw = {env_raw:#x})",
        );
        for i in 0..N_CELLS {
            let idx_raw = Word::fixnum_unchecked(i as i64).raw();
            let cell_raw = unsafe { nod_env_cell(env_raw, idx_raw) };
            let v_raw = unsafe { nod_cell_get(cell_raw) };
            let got = Word::from_raw(v_raw).as_fixnum().expect("fixnum");
            let expected = (i + cycle * N_CELLS) as i64;
            assert_eq!(
                got, expected,
                "[mut cycle {cycle}] cells[{i}] mutation lost \
                 (expected {expected}, got {got})",
            );
        }
    }

    heap_unregister_root(slot.get() as *const Word);
    drop(slot);
}

/// Variant that floods young-gen aggressively to force the multi-chunk
/// / cascade paths if they're reachable at this heap size.
#[test]
#[serial]
fn callback_closure_env_survives_heavy_pressure() {
    let (closure_w, _) = build_closure_graph();
    let slot: Box<UnsafeCell<Word>> = Box::new(UnsafeCell::new(closure_w));
    heap_register_root(slot.get() as *const Word);

    for cycle in 1..=6 {
        // ~12 MB per cycle (3000 × ~4 KB).
        churn_byte_strings(3000, 4080);
        collect_minor();
        let cur = unsafe { *slot.get() };
        assert_graph_intact(cur, &format!("heavy cycle {cycle}"));
    }

    heap_unregister_root(slot.get() as *const Word);
    drop(slot);
}

// ─── Win32-shaped reproducer ──────────────────────────────────────────────
//
// The tests above register the closure Word directly via
// `heap_register_root` of a Box-cell. The IDE registers via
// `callbacks.rs::register_callback`, which installs the registry's 32
// slot pointers as GC roots and writes the closure into one of those
// slots. When the OS calls `CallWindowProcW`, the per-slot trampoline
// forwards to `wndproc_dispatch`, which reads the closure back from
// the registry slot into a LOCAL `closure` Word and then calls
// `nod_funcall4(closure.raw(), ...)`.
//
// We don't need the OS for this path — the trampoline is a plain
// `extern "system" fn(u64, u32, u64, u64) -> u64`. Casting its address
// to a function pointer and calling it from Rust reproduces the exact
// dispatch sequence the OS uses.

type WndprocFn = unsafe extern "system" fn(u64, u32, u64, u64) -> u64;

/// Body for the synthetic Dylan closure. Reads + increments every
/// captured cell, allocates ~1 MB of throwaway byte-strings, and
/// triggers a minor GC — all while the trampoline frame is on the
/// stack. Receives `env_ptr` as its first argument under the closure
/// ABI (`nod_funcall4` passes env before the four arity args when
/// `function_kind_tag == CLOSURE`).
extern "C-unwind" fn synthetic_callback_body(
    env_ptr: u64,
    _a: u64,
    _b: u64,
    _c: u64,
    _d: u64,
) -> u64 {
    // 1. Walk env → cells. If env was reclaimed before nod_funcall4
    //    read closure.env-ptr, the first nod_env_cell call panics —
    //    same panic site as the bug report.
    for i in 0..N_CELLS {
        let idx_raw = Word::fixnum_unchecked(i as i64).raw();
        // SAFETY: env_ptr came from nod_funcall4 reading closure.env-ptr;
        // idx_raw is a fixnum in range.
        let cell_raw = unsafe { nod_env_cell(env_ptr, idx_raw) };
        // SAFETY: cell_raw is a live <cell> Word returned by nod_env_cell.
        let v_raw = unsafe { nod_cell_get(cell_raw) };
        let v = Word::from_raw(v_raw).as_fixnum().unwrap_or(0);
        let new_val = Word::fixnum_unchecked(v + 1);
        // SAFETY: same.
        let _ = unsafe { nod_cell_set(new_val.raw(), cell_raw) };
    }
    // 2. ~1 MB of throwaway byte-strings → forces a minor GC while
    //    THIS body's frame is on the stack.
    churn_byte_strings(4096, 240);
    // 3. Also explicitly drive a minor cycle so the test doesn't
    //    depend on the heap's auto-trigger threshold.
    collect_minor();
    0
}

/// Win32-shaped reproducer. Registers the closure via the production
/// `register_callback` path, looks up the trampoline address, casts to
/// `WndprocFn`, and calls it in a loop — exactly the sequence the OS
/// executes when it dispatches `WM_KEYDOWN` to a WNDPROC.
#[test]
#[serial]
fn callback_dispatched_via_trampoline_keeps_env_alive() {
    // Clean registry → deterministic slot allocation and clear
    // per-thread "roots installed" flag.
    _reset_callbacks_for_tests();

    let (closure_w, env_w) = build_closure_graph_with_code(
        synthetic_callback_body as *const u8,
    );

    // Root env_w from the OUTSIDE so we can read cells back post-dispatch
    // for the round-trip assertion. (The registry will root the closure;
    // the closure transitively keeps env alive — but if the bug is what
    // it claims, that transitive link is exactly what fails. Rooting env
    // here separately means a panic inside the body localises the
    // failure to "closure.env-ptr is stale" vs "env was never reachable".)
    let env_slot: Box<UnsafeCell<Word>> = Box::new(UnsafeCell::new(env_w));
    heap_register_root(env_slot.get() as *const Word);

    // Register via the production path. After this returns:
    //   • the registry's 32 slot pointers are GC roots on THIS thread;
    //   • the closure Word lives at `registry.closures[slot_id]`;
    //   • `callback_slot_address(Wndproc, slot_id)` is the trampoline.
    let slot_id = register_callback(closure_w, CallbackSignature::Wndproc)
        .expect("register_callback should succeed on a fresh registry");
    let trampoline_addr = callback_slot_address(CallbackSignature::Wndproc, slot_id);
    assert_ne!(trampoline_addr, 0, "trampoline address must be non-zero");
    // SAFETY: `callback_slot_address` returns the address of a
    // statically-defined `extern "system" fn(u64, u32, u64, u64) -> u64`.
    let trampoline: WndprocFn = unsafe { std::mem::transmute(trampoline_addr) };

    // Drive 8 "keypresses". Each iteration:
    //   1. Allocates pressure OUTSIDE the callback (mimics other Dylan
    //      work happening before the OS dispatches a message).
    //   2. Calls the trampoline. Inside: registry → wndproc_dispatch →
    //      nod_funcall4 → synthetic_callback_body → mutate + alloc +
    //      collect_minor.
    // A passing iteration means env + closure survived a full
    // registry-rooted GC cycle. A failing iteration panics inside the
    // body's first nod_env_cell, matching the production crash.
    const WM_KEYDOWN: u32 = 0x0100;
    const N_DISPATCHES: usize = 50;
    for _iter in 1..=N_DISPATCHES {
        churn_byte_strings(2048, 240);
        // SAFETY: trampoline points at `wndproc_slot_<slot_id>`,
        // matching the signature.
        let _lresult = unsafe { trampoline(0, WM_KEYDOWN, b'A' as u64, 0) };
    }

    // Each dispatch incremented every cell by 1. Confirm the mutations
    // accumulated — proves the body actually ran N times AND env
    // survived every dispatch.
    let env_cur = unsafe { *env_slot.get() };
    let env_raw_cur = env_cur.raw();
    for i in 0..N_CELLS {
        let idx_raw = Word::fixnum_unchecked(i as i64).raw();
        // SAFETY: env_cur is rooted; idx in range.
        let cell_raw = unsafe { nod_env_cell(env_raw_cur, idx_raw) };
        let v_raw = unsafe { nod_cell_get(cell_raw) };
        let got = Word::from_raw(v_raw).as_fixnum().expect("fixnum");
        let expected = (i + N_DISPATCHES) as i64;
        assert_eq!(
            got, expected,
            "cells[{i}] expected {expected} after {N_DISPATCHES} dispatches, got {got}",
        );
    }

    heap_unregister_root(env_slot.get() as *const Word);
    drop(env_slot);
    // Don't leak the registry slot into the next test.
    _reset_callbacks_for_tests();
}
