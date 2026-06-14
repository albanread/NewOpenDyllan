# NewGC env-reclaim — investigation notes

**Status:** Investigation in progress. No code change yet.
**Companion to:** [`NEWGC_BUG_ENV_RECLAIM_UNDER_CALLBACK.md`](NEWGC_BUG_ENV_RECLAIM_UNDER_CALLBACK.md) (the original bug report).
**Date:** 2026-05-24.

This document captures the static-analysis pass over the reachability
chain from "callback registry root" through "`<function>` closure" to
"`<environment>` → cells SOV → `<cell>` objects" in light of the panic
at `nod_env_cell` (env raw `0x27054af0569`, 16 bytes inside a recycled
`<byte-string>`). The intent is to keep the next engineer from
re-tracing what's already been traced and to point at the remaining
suspect paths.

## Reachability chain — what should happen

```
ROOT_STACK[i] = &registry.closures[slot_id]   (registered, stable, Box<UnsafeCell<Word>>)
  ↓ dereferenced by visit_roots in Mark mode
G1_closure (<function>)
  ↓ user_class_layout returns (7, 1, 7) → cells 1..6 are pointer cells
  ↓ cell 5 = env-ptr = tagged G1_env Word
G1_env (<environment>)
  ↓ cells slot is a tagged <simple-object-vector> Word
SOV
  ↓ vector_scan visits each element
<cell> objects
  ↓ value slot
captured values
```

Every link is a pointer slot the GC should walk. The panic says env was
reclaimed → some link broke.

## What was verified (traced)

These code paths were read line-by-line and checked against the panic:

| Path | Verdict |
|---|---|
| `callbacks.rs::install_gc_roots_for_this_thread` registers all 32 registry slot pointers as GC roots on the current thread (idempotent per-thread flag). | ✓ correct |
| `callbacks.rs::register_callback` calls `install_gc_roots_for_this_thread` BEFORE writing the closure into the slot; no Dylan allocation happens between root registration and the slot store, so a GC can't fire mid-window. | ✓ correct |
| `functions.rs::make_function` packs `env_ptr` as `Word::from_raw(env_ptr)` and passes it through `rust_make` as the `env-ptr` init-keyword. | ✓ correct |
| `make.rs::rust_make` collects init-keyword values into a Vec, registers each via `RootGuard::new` BEFORE `alloc_object`, then reads the rooted Vec slot back when writing the instance's slot. A minor GC fired during alloc will rewrite the rooted Vec entry; the slot store carries the new address. | ✓ correct |
| `closures.rs::make_environment` allocates the cells SOV via `pool.heap.alloc_simple_object_vector` (the moveable heap, NOT the static area), guards each cell Word with `RootGuard::new` across the SOV alloc, and uses `write_barrier` to fill each SOV slot. | ✓ correct |
| `dylan_layout.rs::DylanLayout::header_layout` reads the wrapper, looks up `class_metadata_ptr`, and returns `(total_cells, pointer_cells_start, pointer_cells_end)` from the metadata's layout function. For `<function>` the layout reports `(7, 1, 7)`. | ✓ correct |
| `classes.rs::user_class_layout` returns `(total_cells, 1, total_cells)` for any user class with >1 cell — env-ptr at cell 5 IS treated as a pointer cell during scan. | ✓ correct |
| Cheney-style BFS mark in `evac.rs::mark_visit_slot` only marks objects whose page's CURRENT generation matches `from_gen`. For G0 minor it correctly marks G0 closure and walks through G0 closure.env-ptr to mark G0 env. | ✓ correct for single-cycle |
| `phase1_copy_chunk` does a verbatim byte copy from G0 to dest_gen and writes a forwarding marker at source cell 0. | ✓ correct |
| `phase2_rewrite` step 2a runs `visit_roots` in Rewrite mode (including dirty-card scan). Step 2b walks ONLY dest_gen pages and rewrites stale source-gen pointers via `maybe_rewrite_word`. | ✓ correct under single-chunk, full-mark path |
| `maybe_rewrite_word` reads the cell, classifies, looks up the target's forwarding marker via `is_real_forward_target_at`, and returns `Word::from_raw(L::rewrite_pointer_addr(old_raw, new_addr))` which re-applies the tag bit. The rewritten Word is correctly tagged. | ✓ correct |
| `space.rs::rebuild_cards_for_old_gens` rescans every live page after the cycle and marks a card dirty iff any cell in that 512-byte range classifies as `PointerCons` or `PointerHeader`. G1 closure has a pointer-tagged env-ptr at cell 5 → its card stays dirty after every cycle. | ✓ correct |
| `coordinator_api.rs::scan_dirty_cards_as_roots` filters by `descs_at_scan_time`, skipping pages whose snapshot generation is `Free` or `== from_gen`. For a G0 minor with the closure already in G1, the G1 dirty card IS scanned. | ✓ correct |
| `cycle.rs::collect_minor` snapshots `descs_at_scan_time` BEFORE `evacuate_with_roots`. The cascade (G1→Tenured) re-uses the same snapshot. `mark_visit_slot` uses CURRENT descs, not the snapshot, so newly-promoted-this-cycle G1 pages are correctly seen as G1 by the cascade's BFS. | ✓ correct on the marked path |
| `heap.rs::collect_minor` snapshots `ROOT_STACK` BEFORE acquiring the heap mutex, but in a single-threaded runtime nothing else can modify the stack between snapshot and GC. The single-thread assumption holds for the IDE because Win32 dispatches WNDPROC on the message-pump thread (same as the mutator) — the backtrace's `RtlUserThreadStart` is just how every Windows thread starts, not evidence of a separate thread. | ✓ correct under single-thread assumption |

## What was ruled out

- **Multi-chunk rewrite drop.** Multi-chunk evacuation would only fire
  when the destination free-list is near-exhausted. The 256 MB
  reservation can't reach that state in 3-5 keypresses with the IDE's
  allocation pattern.
- **Cascade-snapshot bug** (newly-promoted G1 pages showing as Free in
  the cascade's `descs_at_scan_time`). `mark_visit_slot` reads CURRENT
  descs, not the snapshot. The snapshot only gates `scan_dirty_cards_as_roots`,
  which is for finding cross-gen pointers; the BFS walks roots
  independently.
- **Missing write barrier on closure.env-ptr slot store.** `<function>`
  is immutable post-construction; the slot is written exactly once
  inside `rust_make`, which is the freshly-allocated G0 path — no
  cross-gen write happens.
- **Cells SOV allocated in static area.** Confirmed `make_environment`
  uses `pool.heap.alloc_simple_object_vector`, which is the moveable
  heap. The cells SOV is scanned by the GC like any other object.
- **Sprint 11b JIT alloca roots.** Bug report §6 confirms this is
  working; the env is reclaimed via a cross-generation edge, not a
  missing in-frame root.
- **Local `closure` variable in `wndproc_dispatch` going stale.** No
  Dylan allocation runs between the registry read (under mutex) and the
  `nod_funcall4` call, so no GC fires in that window. Inside
  `nod_funcall4`, env-ptr is extracted from `f` BEFORE the JIT body
  runs (which is where GC can fire). The JIT then receives env as a
  Sprint-11b-rooted argument.

## Suspect paths that warrant a synthetic reproducer

The static analysis closes every algorithmic gap I can find for the
single-chunk, single-thread, well-below-full-heap scenario described
in the bug report. The bug exists, so one of the following is true:

1. **A subtle ordering or filter bug I'm missing in `phase2_rewrite`** —
   most likely candidate is the `descs_at_scan_time` filter in
   `scan_dirty_cards_as_roots` skipping a card it shouldn't, OR
   `rewrite_page` skipping G1_closure because something on its page
   looks like a forwarding marker (start-bit confusion on a recycled
   page).
2. **`class_metadata_ptr` returning null for `<function>` at some
   intermediate cycle**, causing `header_layout` to fall through to the
   defensive `ObjectLayout::opaque(1)` branch — env-ptr at cell 5 is
   then NOT scanned. This is the most plausible silent failure: it
   doesn't crash, it just stops walking pointer slots. The defensive
   fallback was added (per the comment in `dylan_layout.rs:107-128`)
   for stale-start-bit and test-contamination cases; in production for
   a long-lived registered class it should never fire, but if it does,
   the symptom matches exactly.
3. **A WNDPROC-specific path** that the synthetic test won't hit — e.g.
   the Win32 message pump re-entering Dylan code in a state the
   runtime doesn't expect.

## Synthetic reproducer result

The reproducer ([tests/nod-tests/tests/gc_callback_env.rs](../tests/nod-tests/tests/gc_callback_env.rs))
covers four variants of escalating fidelity to the WNDPROC path:

1. `callback_closure_env_survives_promotion` — closure + env + 13
   cells, registered via `heap_register_root` of a `Box<UnsafeCell<Word>>`,
   8 minor cycles with ~1 MB byte-string pressure each.
2. `callback_closure_env_survives_heavy_pressure` — same shape, 6
   cycles with ~12 MB pressure each (exercises near-full young).
3. `callback_closure_env_survives_cell_mutation_under_promotion` —
   same shape AND mutates every captured cell via `nod_cell_set`
   (write_barrier path) on every cycle, mimicking the IDE's per-
   keypress mutation pattern.
4. `callback_dispatched_via_trampoline_keeps_env_alive` — uses the
   **production** `callbacks.rs::register_callback` to register the
   closure (so `install_gc_roots_for_this_thread` runs and the
   registry's 32 slot pointers become GC roots), then looks up the
   per-slot trampoline (`wndproc_slot_<i>`) via `callback_slot_address`,
   casts to `extern "system" fn(u64, u32, u64, u64) -> u64`, and calls
   it 50 times. Each invocation runs through `wndproc_dispatch`'s
   registry-slot read into a local, into `nod_funcall4`, into a
   Rust closure body that walks env → cells → values, mutates every
   cell, allocates ~1 MB of byte-strings, and explicitly drives a
   minor GC — all while the trampoline frame is still on the stack.
   After 50 dispatches the test reads cells back from outside and
   confirms every mutation accumulated.

**All four pass in both debug and release builds.** The
trampoline variant runs in ~0.8 s with ~600 MB of total allocation
pressure across the dispatch loop; the cell mutations roundtrip
exactly.

This is a very strong negative result: the runtime/GC layer in
isolation — including the production callback-registry path, the
real `wndproc_dispatch` function pointer, GC cycles fired from inside
the callback body, and write-barrier-path cell mutations — does not
lose env. The bug requires something none of these tests have.

## Remaining differentiators (what's left to explain the crash)

The trampoline variant rules out the "cross-callback dispatch is
buggy" hypothesis. What's left is everything that's specific to a
**JIT-compiled** WNDPROC body:

1. **JIT body on the stack with Sprint 11b alloca roots active when
   GC fires.** In the IDE, `nod_funcall4` calls a JIT-compiled
   function. The JIT prelude spills `env_ptr` to an alloca slot and
   registers that slot via `nod_register_root`. If the Sprint 11b
   register/unregister pairs are MISSING for the env_ptr argument
   in the WNDPROC body specifically (e.g. nod-sema's lowering
   doesn't treat the env_ptr arg as a live root across every
   allocation site), env could be dropped when the JIT allocates.
   Bug report §6 asserts Sprint 11b is working — but "working in
   general" is not "working for the env_ptr arg of a closure body".
2. **Closure created via `nod_make_closure` (JIT shim), not via
   `make_function` directly.** The shim allocates a `<vector>`-shaped
   env wrapper through a slightly different keyword path; worth
   diffing against `make_function`. (The Rust trampoline test calls
   `make_function` directly because that's what `nod_make_closure`
   itself ultimately calls — but if `nod_make_closure` does extra
   work that fails to root something across alloc, the test misses it.)
3. **Allocation pattern depth**: rope-edit code in the IDE produces
   nested allocations (rope-node holds child rope-node refs, etc.).
   The synthetic body allocates flat byte-strings; nothing has
   payload pointers into other heap objects.
4. **Win32 message-pump re-entry**: ruled out by test #4 — the
   trampoline call is a plain function-pointer dispatch whether
   the OS or our test code invokes it.

## Next steps (revised after trampoline test)

1. **Compile a Dylan WNDPROC body via nod-driver** and run it as the
   closure code-ptr. Sprint 11b alloca roots will then be active on
   the stack when GC fires inside the body. This is the smallest
   step that closes the remaining gap. Likely shape: a `tests/nod-
   tests/fixtures/wndproc-callback.dylan` that captures N cells,
   reads them, mutates them, allocates pressure, returns 0 — then a
   test that compiles it via `nod_sema::lower_module`, registers the
   resulting closure via `register_callback`, calls the trampoline
   in a loop, asserts mutations roundtrip.
2. **Audit nod-sema's lowering of closure-body env_ptr arg roots.**
   Specifically: does the codegen wrap every potentially-allocating
   call in the closure body with `nod_register_root(&env_ptr_alloca)`
   / `nod_unregister_root` brackets? Grep nod-sema for `register_
   root` emission and check that env_ptr is in the live-roots set
   at every JIT call site inside a closure body.
3. **In parallel: ship workaround §10.1** (force callback closures
   + their env graph to Tenured) so the IDE is unblocked. That's a
   ~10-line change in `register_callback`: walk closure → env → SOV
   → cells, call a "promote to Tenured" path on each. The runtime
   already has the promote machinery (used by `collect_full`'s
   final pass).
4. **If steps 1-2 reproduce the bug:** the fix lands in nod-sema's
   alloca-root emission for closure bodies. The synthetic test
   becomes the regression guard.
5. **If steps 1-2 don't reproduce:** add `eprintln!`-based
   instrumentation to the real IDE per the original bug report §8
   (G0→G1 promotion events, post-promote pointer-slot dumps,
   dirty-card-scan trace).

## Pointers into the codebase

| File | Lines | Why |
|---|---|---|
| [src/nod-runtime/src/dylan_layout.rs](../src/nod-runtime/src/dylan_layout.rs) | 97-141 | `header_layout` with the defensive `opaque(1)` fallback — suspect #2 |
| [src/nod-runtime/src/classes.rs](../src/nod-runtime/src/classes.rs) | 662-673 | `class_metadata_ptr` linear scan |
| [src/nod-runtime/src/callbacks.rs](../src/nod-runtime/src/callbacks.rs) | 147-175, 345-377 | `install_gc_roots_for_this_thread`, `wndproc_dispatch` |
| [src/nod-runtime/src/functions.rs](../src/nod-runtime/src/functions.rs) | 216-257, 622-640 | `make_function`, `nod_funcall4` |
| [src/nod-runtime/src/closures.rs](../src/nod-runtime/src/closures.rs) | 201-232, 261-284 | `make_environment`, `nod_env_cell` (panic site) |
| `E:\NewGC\crates\newgc-core\src\page_heap\cycle.rs` | 127-217 | `collect_minor` and the cascade |
| `E:\NewGC\crates\newgc-core\src\page_heap\evac.rs` | 838-896, 937-997, 1012-1032, 1269-1286 | `phase2_rewrite`, `rewrite_page`, `maybe_rewrite_word`, `is_real_forward_target_at` |
| `E:\NewGC\crates\newgc-core\src\page_heap\coordinator_api.rs` | 590-644 | `scan_dirty_cards_as_roots` |
| `E:\NewGC\crates\newgc-core\src\page_heap\space.rs` | 813-862 | `rebuild_cards_for_old_gens` |
