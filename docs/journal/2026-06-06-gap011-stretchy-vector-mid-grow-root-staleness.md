# 2026-06-06 — GAP-011: `stretchy_vector_push` mid-grow root staleness

*Resuming the SEMA phase (finish the 53.2 Dylan sema recording walk). The
walk turned out to be blocked by a real GC-root-staleness bug in the
runtime — a genuine root cause, not a workaround. Fixing it unblocks the
self-hosted lexer/parser from crashing on large files. Follows
[Sema in Dylan](2026-06-03-sema-in-dylan-53.md).*

## Goal

Get `dylan-sema.exe factorial.dylan` to emit the `=== top-names ===`
section that byte-matches the Rust `format_sema_model` oracle, so the
53.2 gate (`sema_topnames.rs`) can land. The previous session had
localised a runtime crash in `collect-top-names` (`%byte-string-size` on
a stale `source`). Picking that back up.

## What I found (the bigger blocker)

Before the runtime crash even matters, **the project no longer builds**:
`nod-driver build --project dylan-sema.prj` (release) aborts during the
*build itself*. The driver parses each source with the Dylan parser by
default (`main.rs:567`, `cfg!(dylan_lex_shim_linked) && !--parse-with-rust`),
so building the four big self-host files (lexer ~1800 LOC, parser, macro,
sema) JIT-runs the Dylan lexer over them — and that crashes:

```
thread 'main' panicked at src\nod-runtime\src\collections.rs:1108:14:
stretchy_vector_push: sv evacuated mid-grow
  ... nod_stretchy_vector_push ← lex-into ← lex ← dylan-parse-emit
```

This is **GAP-011**, reproduced cleanly and pinned to a *specific,
localized* spot — not the codegen/lowering rewrite the earlier
hypotheses chased.

## Root cause

`nod_stretchy_vector_push(sv, value)` grows the backing storage when
full: it allocates a 2× SOV, which can trigger a **moving** minor GC.
The function already *tried* to survive this — it puts a `RootGuard` on
`sv_local`/`value_local` (registering `&sv_local` in the thread-local
root stack) and re-reads `stretchy_vector_fields(sv_local)` after the
alloc, with a deliberate `.expect("sv evacuated mid-grow")` guard.

The evacuator **does** rewrite the registered slot (`heap.rs`
`minor_forward_word` writes `*slot = new_addr` for a young, evacuated
pointer; `run_minor` casts each `*const Word` root to `*mut` and forwards
it). So the *memory* at `&sv_local` holds the post-GC address.

But `RootGuard::new(slot: &Word)` takes a **shared** reference. At
`-O2`/`-O3` LLVM is entitled to assume a value behind `&Word` does not
change, so it reuses the **pre-collection register copy** of `sv_local`
across the allocating call. The re-read at line 1107 then sees the stale
(evacuated) address — which now holds a forwarding pointer, not a
`<stretchy-vector>` — and the guard fires. (At `-O0` the local is
reloaded from the stack slot, so debug builds hid this. It is a
release-path bug.)

`value_local` has the identical exposure in the grow branch (the pushed
element can itself be evacuated by the grow alloc).

## Fix

A volatile reload of the rooted slot, applied at every post-allocation
re-read. `core::ptr::read_volatile` always emits a load instruction, so
the compiler cannot substitute the cached register — the caller observes
the collector's in-memory rewrite.

- `make.rs`: new `RootGuard::reload(&self) -> Word` (volatile read of the
  registered slot), with a doc comment spelling out the
  shared-reference-caching hazard.
- `collections.rs` `stretchy_vector_push`: name the guards (were `_`),
  and in the grow path read `sv_guard.reload()` before
  `stretchy_vector_fields` and the `%storage`/`%capacity` `write_slot`s;
  before the final element write + `%length` bump, read
  `value_guard.reload()` + `sv_guard.reload()`. On the non-grow path no
  GC fires, so `reload()` returns the original value unchanged.

This is the root-cause fix for the class: *a rooted value used after a
GC-triggering allocation must be reloaded from its slot, never read from
the original local.*

## Validation

- Release driver rebuilt; the `dylan-sema.prj` build's **parse step now
  passes** — the Dylan lexer JIT-runs over all four big self-host files
  with no "evacuated mid-grow" abort. (The build now reaches the linker;
  see "Still open" below.)
- `cargo test -p nod-runtime --lib -- --test-threads=1` → **144 passed,
  0 failed** (includes `stretchy_vector_push_and_size`, which forces two
  grows). The runtime is single-threaded with a global heap + per-thread
  roots, so allocating tests must run serially; the parallel run's lone
  failure was that expected artefact, not a regression.
- Full `nod-tests` serial sweep (`--test-threads=1`): **every suite 0
  failed** — no regressions across the integration suite.

## Still open (next steps)

1. **Release AOT link fragility (pre-existing).** With the GC crash gone,
   `dylan-sema.exe` now fails at link: `LNK2005 nod_user_main already
   defined` — `nod_runtime.lib`'s default stub (`aot_user_main_stub.rs`)
   collides with the user obj in release, because Cargo's CGU partitioner
   colocates the stub with a hot std monomorphization. Documented release-
   only issue (`jit-and-aot.md:309`); the promised fix is a
   `codegen-units = 1` pin on `nod-runtime`. Debug AOT links cleanly.
2. **#2 — a SECOND, distinct bug (the real year-3 blocker).**
   **CORRECTION (later same day):** #2 turned out to be a *codegen
   wrong-value miscompilation, NOT a GC bug* — zero collections fire
   before the crash (tiny input, 4 MB nursery). Full re-diagnosis +
   fix design in
   [`2026-06-06-gap011-2-codegen-wrong-value-diagnosis.md`](2026-06-06-gap011-2-codegen-wrong-value-diagnosis.md).
   The GC-staleness reading below is superseded; kept for the trail.
   Rebuilt `dylan-sema.exe` (debug, `--parse-with-rust`) and ran on
   `factorial.dylan` with probes. Findings:
   - `source` is **valid throughout** (size=221 at after-load, after-lex,
     **after-parse**, preloop, and at both body-defns). So #2 is NOT a
     `source`-staleness bug — `source` survives the whole walk. The #1 fix
     plus the existing `param_homes` mechanism handle the params fine.
   - The crash is `%byte-string-size` on a *different* byte-string during
     the output construction. With NO probes it crashes in the per-`fn`
     `line = concatenate(...)`; adding stepwise bindings + `format-out`
     probes makes that survive and moves the crash to the post-loop
     `out := concatenate(out, …)` accumulator. **A crash that relocates
     when you add allocating probes is a GC-timing staleness (Heisenbug).**
   - It is a *debug* crash, so it is NOT the #1 `RootGuard` register-cache
     class. It is the **codegen precise-root path for loop-carried /
     cross-block heap locals** — the `NOD_DIAG_MERGE_DIVERGENCE` class
     (the in-progress GAP-011 codegen liveness/phi-threading work). The
     liveness *computation* (`populate_safepoint_roots`) is correct; the
     gap is codegen installing a stale carried value at a merge/loop-header
     for a heap local that lowering didn't thread through a block param.
   - **Pervasive, not workaroundable.** Both the straight-line `line`
     concatenate and the loop-carried `out` accumulator are vulnerable
     depending on GC timing, so you cannot write meaningful loop/branch
     Dylan (string building, accumulators, locals held across calls)
     without tripping it. The sema walk's last mile therefore depends on
     fixing #2 in codegen, not on restructuring the Dylan.
   - **Fix candidates:** (A) lowering threads every live-across-block
     heap local through block params (the documented codegen contract;
     large change to `lower.rs` CFG construction); or (B) codegen extends
     the `param_homes` home-alloca mechanism to ALL GC-typed locals
     (uniform, codegen-local, but changes the SSA model + perf). Gate
     either with the merge-divergence detector going to zero.
3. **Harden sibling grow/rehash primitives.** `tables.rs` (14 RootGuards),
   `lists.rs`, `closures.rs` re-read rooted locals after allocation in the
   same shape. They are not crashing today but carry the identical latent
   bug; sweep them onto `RootGuard::reload()` as a follow-up.
4. Then the original 53.2 finish: strip the `dylan-sema.dylan` DBG probes,
   write `sema_topnames.rs`.

## Lesson (for memory)

A precise-root slot the GC rewrites must be read back through the slot
(volatile), not through the `&Word`-shared local that was registered —
otherwise `-O2`/`-O3` serves a cached pre-GC register and you get a
non-deterministic "evacuated mid-X" / "not a `<class>`" crash that
vanishes in debug. This is distinct from the JIT/AOT precise-root path
(`param_homes` + safepoint slabs) which governs compiled *Dylan* locals.
