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

## Body-shaped macro head with a custom separator does not parse

* **Symptom**: a `define macro` whose head is a parenthesised `(?var:name SEP
  ?expr:expression)` group fails unless `SEP` is `for-each`'s special-cased
  `in`. For example a `dotimes (i below 5) … end` macro signals
  `<simple-error>: expected ) after arguments` during expansion: the head paren
  group `(i below 5)` is parsed as an ordinary expression (where `below` is not
  an operator) rather than as macro-head fragments.
* **Workaround**: shape the macro body-style with a *literal keyword* separator
  outside any paren group (e.g. `repeat N times … end`, modelled on
  `with-cleanup`'s `cleanup`), or use a call-shaped macro with simple args. The
  shipped `inc!`/`dec!`/`repeat` macros use these working shapes.
* **Planned fix**: teach the macro head matcher to treat a parenthesised head as
  fragment patterns (name/sep/expr) rather than re-parsing it as an expression,
  generalising the `for-each` `in` path to arbitrary separators.
* **Scope**: medium (macro engine — `compiler/dylan-macro.dylan`,
  `src/nod-macro/src/lib.rs`).
* **Status**: open.

## An unhandled signalled condition panics the JIT eval engine

* **Symptom**: when front-end/expansion code signals a `<condition>` that no
  handler catches (e.g. the macro-head parse error above), the process panics at
  `src/nod-runtime/src/conditions.rs` (`unhandled signalled condition: …`)
  instead of reporting a clean diagnostic and a non-zero exit.
* **Workaround**: none needed for valid input; relevant only when a tool hits an
  internal error path.
* **Planned fix**: install a top-level condition handler in the `eval`/dump entry
  points that renders the condition as a diagnostic and exits non-zero.
* **Scope**: small.
* **Status**: open.

## `define test`/`define suite` keyword (property-list) heads don't expand

* **Symptom**: testworks heads its tests with a keyword property list —
  `define test foo (description: "...") … end`, `(title: "...")`,
  `(expected-failure?: #t)`, `(when: method () … end)`. These fail: the test/suite
  macro doesn't expand, so the test function is never defined ("unknown callee").
  Affects ~24 corpus test files. A NON-keyword paren head (`(foo)`, `(a, b)`,
  `(x :: <integer>)`) DOES work once a `?opts:parameter-list` rule is added — the
  macro matcher's `ParameterList` kind matches ANY paren `Fragment::Group`,
  keyword content included. The blocker is upstream of the macro layer.
* **Root cause (traced 2026-06-14)**: `parse_module_with_macros_rust` returns
  `Err` if ANY `parse_top_item` errors. For a keyword head, a parse pass parses the
  head as an EXPRESSION and aborts at the `KeywordColon` (`unexpected token
  KeywordColon`, parse_atom in `parser.rs`) — even though `parse_define_other`
  itself token-skips the head cleanly and returns a well-formed `DefineOther`. The
  module-level `Err` makes the whole pipeline bail BEFORE macro expansion runs, so
  the test is never expanded.
* **Note on ROI**: fixing this alone yields ~0 corpus *compile* gain — those files
  have independent downstream blockers (`<bit-vector>`, parser features, etc.).
  It's a correctness/foundation fix (the test macro should accept real testworks
  shapes), not a metric mover.
* **Planned fix**: ensure a definition-macro head is never expression-parsed at the
  module level (token-skip only, as `parse_define_other` already does) so keyword
  property lists don't trip parse_atom; then add the `?opts:parameter-list` rule to
  `test`/`suite`/`benchmark`. (Two agents mis-fixed this with
  `?opts:parameter-list` alone, which can't help while the parse still aborts.)
* **Scope**: medium (`src/nod-reader/src/parser.rs` parse path + the stdlib macros).
* **Status**: open.

## Cold-build AOT EXE link `LNK2005 nod_user_main` — RESOLVED (2026-06-14)

* **Was**: AOT EXE linking intermittently failed with `LNK2005: nod_user_main
  already defined` — Cargo's CGU partitioner could merge nod-runtime's
  `aot_user_main_stub.rs` into an always-pulled object, defeating MSVC's
  on-demand archive extraction. A warm incremental build often linked by luck;
  any non-trivial `nod-runtime` edit could re-partition and break it.
* **Fix**: moved the default `nod_user_main` stub into its own crate
  (`src/nod-aot-stub`). A separate crate is always compiled to its own object,
  regardless of nod-runtime's CGU layout, so on-demand extraction reliably drops
  it when the user supplies their own `nod_user_main` (every real AOT EXE).
  `nod-runtime` links it via `extern crate nod_aot_stub as _;` (side-effect symbol
  only, never force-pulled). `codegen-units = 1` was the WRONG fix (one object for
  the whole crate → stub always pulled). Guard: `tools/smoke-aot.sh`.

## A one-line function body containing a `for` loop fails to build

* **Symptom**: a function whose body is written on a single line and includes a
  `for` loop — e.g. `define function s (n) let a = 0; for (i from 1 to n) a := a
  + i end; a end function;` — fails AOT codegen with `codegen: unknown callee
  's'` (the function silently fails to lower, so callers can't resolve it). The
  byte-identical **multi-line** form builds and runs correctly (→ 5050). Found by
  `tools/smoke-aot.sh`.
* **Workaround**: write the function body across multiple lines (the normal form
  for real code; the corpus is unaffected).
* **Planned fix**: trace the newline-sensitivity in the Rust-parser/`for`-desugar
  path (the one-line statement sequence around `for … end; …` parses to a body
  the lowerer drops). Add the one-line case to the regression suite once fixed.
* **Scope**: medium (`src/nod-reader/src/parser.rs` and/or the `for` desugar in
  `src/nod-sema/src/lower.rs`).
* **Status**: open.

## Pre-existing failures in the oracle / parser-parity nod-tests suites

* **Symptom**: a full `cargo test -p nod-tests` run shows failures in
  `c3_oracle` (`c3_linearisation_matches_rust_reference`), `lexer_oracle`
  (`oracle_factorial`/`oracle_hello`), and `dylan_parse_translate`
  (`dylan_parser_translation_gate` — divergence on `dylan-c3.dylan` /
  `dylan-lexer.dylan`, around character-literal and `#:` parsing). These are
  **pre-existing** (present at the import commit / session start) and are NOT
  covered by the working regression guard (the 55 in-tree fixtures via
  `dump-ast`/`dump-dfm` on both parser paths, `tools/smoke-aot.sh`, and the
  `nod-runtime`/`nod-sema` unit suites + the functional `nod-tests` suites —
  classes/collections/tables/conditions/closures/first-class-functions/byte-string,
  which all pass).
* **Nature**: these compare the Dylan-parser shim against the Rust reference
  parser, or against checked-in oracle snapshots, on the compiler's OWN sources —
  they are sensitive to long-standing parser-parity gaps (char literals, `#:`
  symbols) and stale snapshots, not to feature work.
* **Planned fix**: regenerate the lexer/c3 oracle snapshots and close the
  Dylan-vs-Rust parser-parity gaps on character literals / `#:`; then re-enable
  them as part of the guard. Tracked separately from corpus iteration.
* **Status**: open (pre-existing; not a regression from the 2026-06 iterations).

## Notes

* A live Sprint 60 corpus + improvement backlog (ranked) is recorded in
  [the Sprint 60 plan](../sprints/sprint-60.md#4-continue-to-improve-the-compiler).
* The IDE and the in-Dylan lexer are collectively the highest-pressure
  correctness tests the compiler has — every gap they surface is a gap real
  users will hit. Time spent fixing these gaps is time well spent.
* When a gap is fully closed and leaves no residual workaround and no standing
  design constraint, it is removed from this page; its regression test remains in
  the test suite as the durable record.

---

Reference: [Platforms](platforms.md) | [Performance](performance.md) | [Tracing](tracing.md) | [Architecture](../architecture.md) | [Codegen](../compiler/codegen.md) | [GC](../compiler/gc.md) | [Glossary](../glossary.md)
