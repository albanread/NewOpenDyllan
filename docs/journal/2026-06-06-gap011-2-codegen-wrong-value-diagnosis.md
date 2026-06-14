# 2026-06-06 — GAP-011 #2: it is a codegen WRONG-VALUE bug, not GC (diagnosis + design)

*Diagnose-and-design pass on the second sema-walk crash, after the
[stretchy-vector mid-grow fix](2026-06-06-gap011-stretchy-vector-mid-grow-root-staleness.md)
(#1) unblocked the build. User scoped this as "diagnose + design only" —
no risky codegen/GC change. The headline: the earlier "GC stale-reload"
framing was WRONG. #2 is a deterministic codegen miscompilation.*

## The crash

`dylan-sema.exe factorial.dylan` aborts in `collect-top-names`:

```
%byte-string-size: expected <byte-string> Word; got raw 0x248ff762821
  at src\nod-runtime\src\strings.rs:157
```

i.e. a non-`<byte-string>` pointer reaches `%byte-string-size`.

## Why it is NOT GC (the correction)

Three independent signals say no collection happens before the crash:

1. **Crash-dump GC metrics: `minor collections: 0`, `major: 0`.** The
   shadow is updated only at collection end, so 0 means no cycle ran.
2. **`NOD_GC_TRACE` emits no `[GC minor]` line** before the abort.
3. **`factorial.dylan` is 221 bytes.** The whole sema walk allocates a
   few KB; the young gen is 4 MB. There is no allocation pressure to
   trigger a collection.

And the faulting value differs every run — `0x12c6a42a311`,
`0x220d45c4131`, `0x248ff762821` — all pointer-tagged, varying by ASLR /
heap layout. That is a pointer to the *wrong object*, selected
deterministically by miscompiled code; the address varies, the bug does
not.

So #2 is a **deterministic codegen wrong-value miscompilation**: at some
control-flow merge, a temp is bound to the wrong SSA value (a heap
pointer that isn't a `<byte-string>`), and a later `%byte-string-size`
on it crashes. The "Heisenbug" (crash relocates when probes are added)
is NOT GC timing — adding probes changes the IR shape, which moves where
the miscompilation surfaces. `source` is never involved (probes confirm
size=221 throughout); the `param_homes` + safepoint machinery is not
involved (no GC).

## Mechanism

`nod-llvm` codegen (`emit_function`) binds cross-block temp values two
ways:

- **Block params** get real LLVM phis, wired from `pending_incoming`
  (resolved SSA values captured at jump-emit). Correct.
- **Non-block-param temps** that are live into a block are restored from
  `block_entry_temps`, populated by `note_successor_entry_temps` with
  **first-writer-wins** across edges. If such a temp reaches a block from
  ≥2 predecessors with *different* SSA values, codegen installs whichever
  edge was emitted first — a missing-phi miscompilation. For a pointer
  temp that's the wrong object → the `%byte-string-size` crash.

The contract comment in `emit_function` already states this:
> the lowering MUST thread any value live across a [block] through a
> block-arg phi … if lowering fails to thread a live ref, this reset will
> silently install the [wrong] LLVM value. **The fix is always in the
> lowering, not here.**

So the bug is **lowering (`nod-sema/lower.rs`) failing to thread some
live-across-block temp through a block param** in `collect-top-names`'s
shape (a `define function` with nested `if`/`let` + `until` + a stretchy
vector of user objects + sort + an output loop).

Note this can mis-bind **non-pointer** temps too (a wrong integer would
compute silently-wrong, not crash). The pointer crash is the visible tip;
correctness for all types depends on the same threading.

## The `NOD_DIAG_MERGE_DIVERGENCE` detector OVER-REPORTS

The existing detector flags "GC-typed temp arrives at a block from ≥2
predecessors with different values, not a block param." It is an
imprecise *screen*, not an oracle:

- It does **not** skip *function params* (only *block* params). Function
  params are handled by `param_homes`, so those reports are false
  positives. (`map-type-estimate`, `type-node-name` reports are ALL the
  param `t0`/`t1` → all false.)
- It flags non-param locals that nonetheless run correctly. A minimal
  `pick(base)` with a heap local `tag` live across an `if`/`elseif`
  join is flagged (`t2` at join5/join6) yet runs 40 000 iterations
  clean — both edges carry an equal value, so first-writer-wins is right.

So the detector cannot pinpoint #2. Reproducers built from its flagged
shapes (`build-simple`, `build-nested`, `pick` in
`tests/nod-tests/fixtures/gc_loop_accum.dylan`) all RUN CORRECTLY. The
real bug needs a merge where the predecessors carry *genuinely different*
SSA values for a *used* temp — present in `collect-top-names`, absent in
the toy cases.

## Next diagnostic step (to pin the exact temp)

The remaining unknown is *which* temp in `collect-top-names` is
mis-bound. To get there:

1. Dump `collect-top-names`'s DFM + the emitted LLVM IR. `dump-dfm` takes
   a single file and `dylan-sema.dylan` needs the parser context, so
   either (a) teach `dump-dfm` to accept the multi-file set like `build`
   does, or (b) add a one-shot `--dump-dfm-fn collect-top-names` to the
   project build path.
2. In the DFM, find each block with ≥2 predecessors and list temps that
   are live-in but not in `block.params` (the genuine missing-phi set).
   Cross-check against the LLVM IR for a temp bound by first-writer-wins
   rather than a phi.
3. Confirm by minimal reduction: shrink `collect-top-names` until the
   crash vanishes; the removed construct is the trigger shape.

A **precise** detector would help here and is a low-risk change: skip
function params, and only report a temp that is actually *used* in or
after the merge block with genuinely-distinct incoming values. That turns
the screen into an oracle and becomes the fix's gate.

## Fix approaches

Both are SSA-correctness changes (no GC machinery), so lower-risk than
the GC work this was first mistaken for.

### (A) Lowering threads live-across-block temps through block params

The contract's intended fix. In `lower.rs` CFG construction, when a temp
defined in block A is live into block B reached by a jump, add it to B's
`params` and pass it as a jump arg on every edge into B (standard
block-argument SSA construction). Covers all types, kills the class.

- **Pro:** correct by construction; keeps the clean SSA model; matches
  the codegen contract; fixes silent non-pointer mis-binds too.
- **Con:** touches the 7.7k-line `lower.rs`; must get the live-in set
  right at every block (a real liveness pass over the lowered CFG, which
  `nod-dfm` already has — `compute_global_live_out` — and could feed back
  into a "legalize block params" post-pass).
- **Cleanest form:** a `nod-dfm` post-pass `legalize_block_params(f)`
  that, using the existing global liveness, adds missing block params +
  jump args so every live-across-edge temp is threaded. Lowering stays as
  is; the pass runs alongside `populate_safepoint_roots`. This localizes
  the fix to `nod-dfm` rather than scattering it through `lower.rs`.

### (B) Codegen synthesizes phis for divergent non-param temps

In `emit_function`, instead of first-writer-wins for a temp that diverges
across edges, materialize a real LLVM phi at the merge and wire its
incomings from `pending_incoming`.

- **Pro:** codegen-local; no lowering change.
- **Con:** re-implements SSA repair in the backend (block ordering,
  dominance, incoming wiring) — the kind of thing the DFM block-param
  model exists to avoid. Higher chance of subtle dominance bugs.

### Recommendation

**(A), as a `nod-dfm` `legalize_block_params` post-pass.** It reuses the
already-correct global liveness, keeps the backend phi-wiring untouched
(it already handles block params correctly), fixes every type, and is
gated cleanly by the (made-precise) merge-divergence detector → 0. It is
the smallest correct change that matches the existing architecture.

## Verification plan (for the eventual fix)

1. Make the merge-divergence detector precise (skip function params;
   require genuine use + distinct incomings). Gate: **0 sites** across
   the dylan-sema build and the corpus.
2. `dylan-sema.exe factorial.dylan` produces the `=== top-names ===`
   output byte-matching the Rust oracle (the original 53.2 goal).
3. Full `nod-tests` serial sweep — 0 failed (regression gate; lowering /
   nod-dfm change).
4. Keep `gc_loop_accum.dylan` as a negative-control fixture; add the
   reduced `collect-top-names` shape (once found) as a positive
   regression test that crashed before and passes after.

## Status

- #1 (stretchy-vector mid-grow) fixed, committed, pushed, sweep-clean.
- #2 re-diagnosed: **codegen wrong-value (missing phi), not GC.** Design
  above. No code change made this pass (per "diagnose + design only").
- Repro fixture `gc_loop_accum.dylan` added (negative controls).

---

## RESOLUTION (later same day) — it was NOT a compiler bug

The "missing phi" hypothesis above was **wrong**. Bisecting the crash with
`DBG` markers inside `collect-top-names` localised it precisely: the abort
fires *between* `sort-fns!` and the output loops — inside `sort-strings!`
called on the **empty** `consts` / `vars` tables (factorial.dylan has no
`define constant` / `define variable`).

The bug is a **source-level off-by-one in the fixture's insertion sorts**:

```dylan
let i = 1;
until (i = n)        // n = 0 for an empty table → 1 = 0 is #f → body runs
  let x = v[i];      // reads v[1] on a size-0 vector → out of bounds
  ...
```

`i` starts at 1, so the `= n` guard never holds for `n = 0`; the body runs
and indexes `v[1]` on an empty `<stretchy-vector>`. The out-of-bounds read
returns a stray non-`<byte-string>` Word, which flows into the comparator
and aborts in `nod_byte_string_size`. This explains every signal that was
read as "deterministic codegen miscompilation":

- **0 collections** — there is no GC; it is a plain bad read.
- **ASLR-varying fault address** — the OOB read returns whatever Word sits
  past the vector, which moves with the heap layout.
- **"Heisenbug" under probes** — added `format-out`s shift the heap so the
  past-the-end Word differs, not because of phi/GC timing.

The team writeup's standing warning was right: *"Do not implement the
SSA-renaming / per-temp-slot rewrite — it targets a mechanism that does not
fire here."* An SSA `legalize_block_params` pass was prototyped, confirmed
it threaded the flagged `sc_join` sites, **and did not fix the crash** (0 GC
⇒ no reload divergence ⇒ first-writer-wins already carried the single
correct value). It was reverted.

### Fix

`tests/nod-tests/fixtures/dylan-sema.dylan`: guard both insertion sorts with
`until (i >= n)` so an empty table is a no-op. Also removed the leftover
`DBG …` 53.2 scaffolding from `collect-top-names` so the `=== top-names ===`
output is clean. `dylan-sema.exe factorial.dylan` now exits 0 and prints the
correct two-function table; a multi-def input sorts functions / constants /
variables correctly.

Regression: `tests/nod-tests/tests/sema_self_host.rs`
(`dylan_sema_handles_input_with_empty_const_and_var_tables`, `#[ignore]`)
builds the `dylan-sema` project and runs it on `factorial.dylan`, asserting
exit 0 + the expected entries — it crashed before the fix, passes after.

### Note on the latent codegen first-writer-wins hole

The reload-divergence class the design above targets (a GC-typed temp live
across a merge from ≥2 edges that carry *different* reloaded SSA values,
bound by `note_successor_entry_temps`' first-writer-wins) is a *real* codegen
unsoundness in principle, but measurement keeps showing it does not fire on
the current corpus (this crash, the parser corpus, and the `pick` 40k-iter
control all run clean). It remains a deferred hardening item, not a live
bug; `legalize_block_params` (DFM post-pass) is the recommended shape if a
genuine reproducer ever surfaces.
