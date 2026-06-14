# Known limitations and language-feature gaps

This page records Dylan-language features or compiler behaviors that have been
surfaced by dogfooding — writing real Dylan to drive real Dylan tooling (the
IDE, the in-Dylan lexer, the eventual in-Dylan parser/sema). Each entry keeps
its symptom, workaround, planned fix, and scope so that a residual workaround
can be audited and removed once the underlying issue is fully closed.

The majority of the historical gaps surfaced this way have been fixed; only
entries that still represent a live limitation, a residual in-tree workaround,
or a standing design trade-off are kept here.

## Entry format

```
## short title

* Symptom: minimal code that fails / unexpected behaviour.
* Workaround: what to do instead (if any).
* Planned fix: what the compiler should ultimately do.
* Scope: rough size estimate (small / medium / large).
* Status: open | fixed.
```

---

## Loop-carried heap locals across heavy allocation (residual workaround)

* **Symptom**: a function holds a heap-object reference in a `let` local (a
  `<stretchy-vector>`, a `<string-stream>`, a `<byte-string>`) and threads it
  through a loop that calls into other functions that allocate. Historically,
  after enough iterations the local's word turned into garbage and the next use
  tripped either `stretchy_vector_push: not a <stretchy-vector>` or a spurious
  `<no-applicable-methods-error>` with a per-run-varying class id — classic
  stale-pointer behaviour. Function parameters showed the same failure as `let`
  locals; passing the value through a helper's parameter slot did not save it.
  Module-level `define variable` cells survived because they live in cell-backed
  slots registered as GC roots.

* **Root cause**: phi-incoming wiring in `src/nod-llvm/src/codegen.rs` resolved
  jump-arg `TempId`s to SSA values at end-of-function instead of at jump-emit
  time. Because every safepoint reload mutated `state.temps`, the phi for a
  loop-carried temp ended up taking the last in-loop reload SSA value on both
  incoming edges — the entry edge then could not dominate it. This surfaced
  either as a build-time `Instruction does not dominate all uses!` verifier
  error (when both phi incomings referenced an in-loop `%gc.reload` value) or as
  a runtime stale pointer (when block layout happened to satisfy dominance but
  the entry edge read an uninitialised slot).

* **Fix**: snapshot SSA values at jump-emit time instead of resolving at
  phi-wiring time — resolve `args` to `BasicValueEnum` before the branch and
  push the resolved values onto `pending_incoming`, then iterate the
  pre-resolved values in the wiring loop. Snapshotting at emit-time captures the
  SSA value as it flowed out of the actual predecessor.

* **Residual workaround in tree**: the lexer fixture
  (`tests/nod-tests/fixtures/dylan-lexer.dylan`) still stashes its
  heaviest-trafficked heap roots as module variables (`*tokens*`,
  `*dump-stream*`) from before the fix. These can revert to natural `let`-locals;
  the acceptance gate for that retirement is the lexer self-dump
  (`nod-driver dump-tokens compiler/dylan-lexer.dylan`)
  succeeding end-to-end on the lexer's own source.

* **Scope**: small (the code fix was ~10 lines plus regression tests; the
  residual is a fixture cleanup).

* **Status**: fixed (root cause). The broader class of GC phi-wiring and
  env-scope-leak bugs is closed; the five-file IDE compiles, opens a window, and
  runs the Win32 message loop without crash. The fixture workaround retirement is
  the one open cleanup remaining.

---

## AOT end-safepoint root accounting and permanent roots (design note)

* **Symptom**: an AOT EXE panicked at end-of-safepoint with
  `lost active roots before end: current N baseline M expected 0` when a call
  bracketed by `nod_aot_begin_safepoint` / `nod_aot_end_safepoint` legitimately
  *added* permanent roots during its body.

* **Root cause**: the Win32 callback layer
  (`install_gc_roots_for_this_thread`) registers permanent `ROOT_STACK` entries
  on the first call from a thread (one per registered window-procedure callback
  cell). That first-touch registration happens inside a bracketed safepoint, so
  the post-call root count is `baseline + N` rather than `baseline + 0`. The
  end-safepoint assertion used `assert_eq!`, which treats any *increase* as an
  error, even though permanent roots being added during a call are legitimate.

* **Fix**: the end-safepoint assertions use `assert!(current >= baseline +
  expected)` rather than `assert_eq!`. The "too few roots" check (a callee
  removing roots it should not) is preserved; the "too many roots" rejection is
  removed.

* **Standing trade-off (the reason this stays documented)**: the `>=` relaxation
  permanently removes the ability to catch "caller registered MORE roots than
  expected and never cleaned them up" leaks. The strict equality can only be
  restored once `install_gc_roots_for_this_thread`'s permanent-root injection is
  moved to *before* the first safepoint (for example a first-call init at the AOT
  main wrapper start), at which point the permanent roots are already in the
  baseline count.

* **Scope**: small (the relaxation); medium (restoring strict equality requires
  moving permanent-root injection earlier).

* **Status**: fixed (the crash); the weaker leak detection is an open design
  constraint until permanent-root injection moves before the first safepoint.

---

## Notes

* The IDE and the in-Dylan lexer are collectively the highest-pressure
  correctness tests the compiler has — every gap they surface is a gap real
  users will hit. Time spent fixing these gaps is time well spent.
* When a gap is fully closed and leaves no residual workaround and no standing
  design constraint, it is removed from this page; its regression test remains in
  the test suite as the durable record.

---

Reference: [Platforms](platforms.md) | [Performance](performance.md) | [Tracing](tracing.md) | [Architecture](../architecture.md) | [Codegen](../compiler/codegen.md) | [GC](../compiler/gc.md) | [Glossary](../glossary.md)
