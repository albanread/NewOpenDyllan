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

## GC pointer staleness across allocation-triggered collection

* **Symptom**: a program that builds a large heap structure in a tight allocating
  loop silently TRUNCATES it. E.g. `let acc = #(); for i: acc := pair(i, acc)` over
  N nodes returns `size(acc) < N`. Reproduces with a plain cons loop (no
  `apply`/`curry`); `curry(\+,1)(2)` before a 200k loop made it surface earlier by
  shifting GC scheduling. Originally mis-attributed to AOT roots, then to a within-gen
  G0→G0 evac dest-page release — both refuted by instrumentation.
* **Two distinct bugs, traced 2026-06-15:**
  1. **`alloc_pair` argument staleness — FIXED.** `Heap::alloc_pair` (lists.rs) called
     `alloc_object` (which can fire a moving minor GC) and then wrote its BY-VALUE
     `head`/`tail` parameters into the new pair. The caller's `RootGuard`s update the
     caller's slots, but `alloc_pair`'s own copies were never re-read, so a
     `pair(x, acc)` whose allocation evacuated `acc` stored a STALE tail into a freed
     page. Fix: `RootGuard` + `reload()` `head`/`tail` across `alloc_object` inside
     `alloc_pair`. This fixed the `curry`+200k repro (25289→200000) and raised the
     plain-cons corruption threshold from ~175k to ~520k nodes. (Same bug class as the
     `code_ptr`-stale crash fixed in `invoke_rest_closure`: any runtime fn that holds a
     pointer across its own allocation must root+reload it.)
  2. **Chunked-evacuator backward cross-chunk sever — FIXED (Cheney defer-release).**
     Beyond ~520k nodes a multi-chunk major/promotion truncated by a fixed prefix. `acc`
     is correctly rooted+updated (a module-`define variable` accumulator truncated too),
     so NOT root/codegen staleness: the evacuator copied live objects in CHUNKS and
     `phase3_reclaim` released+`zero_whole_page`d each chunk's source pages IMMEDIATELY,
     destroying in-page forward markers; those freed pages were re-acquired as dest by
     later chunks. A BACKWARD cross-chunk pointer (cons `cdr`: newer/high-addr →
     older/low-addr; chunks processed low→high) into an already-released chunk could
     never be rewritten — `maybe_rewrite` (evac.rs:487) bails on a `Free` target — so it
     dangled, and a later mark reclaimed the orphaned prefix. A **side forwarding table
     was rejected** (3-agent adversarial review): cell indices are global, so a
     released-then-reacquired source page reuses addresses and a raw dangling backward
     pointer can't be disambiguated (old vs new occupant) — no keying/epoch fixes a
     reused raw address. **Fix (Cheney discipline):** the chunk loop now runs only
     phase1-copy + phase2-rewrite per chunk; `phase3_reclaim` runs ONCE at end-of-cycle
     over the whole `from_pages` set. Source pages (and their forward markers) live until
     every chunk's rewrite is done, so no source address is reused mid-cycle and every
     backward pointer is rewritable; `maybe_rewrite`'s `Free`-bail never fires on a live
     source. Verified: plain 600k cons 119520→**600000**, 1M→**1000000**; new
     newgc-core regression test `backward_chain_survives_multi_chunk_evac` (single-rooted
     backward chain forced multi-chunk) fails-before / passes-after.
* **Residual limit (by design, not corruption):** a correct copying collector needs
  transient ~2× space (live source held while the copy is built), so for a same-gen
  compaction whose live set exceeds ~half the reservation the evac raises the EXISTING
  **loud** `GcStallError(MidEvacOOM)` instead of silently corrupting. `DEFAULT_OLD_BYTES`
  was raised 12 MB → 48 MB (address space only — pages commit lazily, so committed RAM
  still tracks the live set) so the common large workloads (≤~1M-node lists) grow and
  succeed; a genuinely-too-big build (e.g. a 2M-node tight-loop list) loud-stalls. A 1×
  in-place mark-compact for the old gen would remove the 2× requirement — future work.
* **Scope**: both fixed (lists.rs ~14 lines; newgc-core evac.rs defer-release +
  heap.rs reservation). Pre-existing on `origin/main`; strictly improves correctness
  (silent truncation → correct, or loud stall when the heap truly can't fit a copy).

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

## Multi-binding `dynamic-bind` does not restore prior values

* **Symptom**: `dynamic-bind (a = 1, b = 2) … end` (two or more bindings)
  forward-sets each place and runs the body, but does **not** restore the prior
  values on exit. The single-binding form `dynamic-bind (a = 1) … end` *does*
  restore (verified by AOT build+run: a value bound inside reads back as its
  prior value after the block).
* **Cause**: the stdlib macro (`stdlib/macros.dylan`) implements the
  single-binding case with `block ()/cleanup` save-and-restore, but the
  multi-binding case expands via the `{ ?place := ?val; } ...` sequence splice,
  which can set every place but cannot generate a per-place saved temporary or a
  reverse-order restore (the macro engine has no rest/recursion pattern — only
  `expression`/`name`/`body`/`variable`/`parameter-list` kinds and `{ } ...`
  sequences).
* **Workaround**: nest single-binding `dynamic-bind`s, which compose correctly
  and each restore.
* **Planned fix**: a macro-engine rest/recursion pattern, or a runtime
  dynamic-binding stack the macro pushes/pops.
* **Scope**: small–medium (`stdlib/macros.dylan` once the engine supports it).
* **Status**: open (single-binding works).

## `for … in coll using PROTOCOL` ignores the protocol (forward only)

* **Symptom**: `for (i in s using backward-iteration-protocol) … end` parses and
  compiles, but iterates **forward** — the `using` protocol expression is parsed
  and discarded.
* **Cause**: the `for`-loop lowering only emits the default forward iteration
  protocol (`%fip-init`/`%fip-finished?`/`%fip-current-element`/`%fip-advance!`).
  There is no backward / custom-protocol FIP plumbing yet.
* **Workaround**: none for backward iteration; forward `in` loops are correct.
* **Planned fix**: a `%fip-init-with-protocol` family (or honour an explicit
  `<iteration-protocol>` object) so `using` selects the direction/protocol.
* **Scope**: small parser change already done (parse + ignore); medium runtime
  work to honour it.
* **Status**: open (parses; semantics forward-only).

## Multi-fragment argument in a comma-separated call-macro pattern doesn't match

* **Symptom**: a call-shaped macro rule `{ m(?a:expression, ?b:expression) }`
  matches `m(x, 3)` but NOT `m(x, f(y))` or `m(x, a + b)` — "call to macro `m`
  matched none of its N rules". A single-argument call-macro (`assert(f(x))`)
  works; only the *comma-separated* multi-argument case fails when a non-first
  slot is more than one fragment (an `ident(args)` call is two fragments, a
  binop is three). Wrapping the arg in parens (`m(x, (f(y)))`) works around it.
* **Cause**: the macro engine's argument matcher for a parenthesised
  comma-separated call pattern binds each comma slot to a single fragment
  (token or balanced group), rather than greedily matching all fragments up to
  the next top-level comma.
* **Impact**: the condition-testing testworks helpers (`check-condition`,
  `assert-signals`, `check-no-errors`, `assert-no-errors`, `check-no-condition`)
  cannot be the faithful `block … exception …` macros they should be (the tested
  form is a call), so they are shipped as plain **functions** that evaluate the
  form eagerly — the files COMPILE but the helpers don't actually catch a
  signal. `with-pretty-print-to-string`, `collecting(as ?type)`, and other
  multi-arg call macros hit the same wall.
* **Planned fix**: make the comma-slot matcher greedy (match fragments up to the
  next depth-0 comma / closing paren), in `src/nod-macro/src/lib.rs`.
* **Scope**: medium (macro engine).
* **Status**: open (functions ship as eager stubs meanwhile).

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

## `define test`/`define suite` keyword (property-list) heads don't expand — RESOLVED (2026-06-15)

* **Symptom (was)**: testworks heads its tests with a keyword property list —
  `define test foo (description: "...") … end`, `(expected-to-fail-reason: "...")`,
  etc. These appeared to fail: the test function was never defined ("unknown
  callee foo").
* **Real root cause (traced 2026-06-15)**: TWO things, and the previously-recorded
  "module-level parse aborts at `KeywordColon` in parse_atom" diagnosis was itself
  an artifact, not the cause:
  1. **The macro rule was too narrow.** Rule 1 was the literal `{ define test
     ?name:name () ?body:body end }`; a keyword head `(description: "...")` doesn't
     match `()`, so it fell through to rule 2 (`define test ?name:name ?body:body
     end`), which then tried to parse `(description: "...")` as the start of the
     BODY → `KeywordColon` in expression position → parse error. The fix is simply
     to generalise rule 1's head to `?opts:parameter-list` (the matcher's
     `ParameterList` kind matches ANY paren `Fragment::Group`, incl. keyword
     content); the head is then token-skipped and discarded, never
     expression-parsed. **No parser change was needed.**
  2. **The investigation fixtures were malformed.** They had no blank line after
     the `Module:` header. `scan_preamble` treats any line containing `key:` as a
     header entry, so `define test foo (description: "x")` (the `description:`
     colon) was swallowed INTO the preamble; the lexer skipped it and the parser
     began at the dangling `end test;` → `unexpected token KwEnd`, and `foo` was
     never defined. A `(bar)` head (no colon) wasn't swallowed — which is exactly
     why `(bar)` "worked" and the keyword head looked categorically broken. With a
     well-formed header the keyword head parses, expands, and the function is real.
* **Fix shipped**: one line in `stdlib/macros.dylan` — `define test`'s head rule
  `()` → `?opts:parameter-list` (commit a4ba2dc). Strictly more permissive (every
  old `()` head still matches); build+run verified `define test foo (description:
  "x") format-out("%d\n",42); end` + `main foo()` → prints **42** (callable, not
  hollow). Corpus dump-dfm 71 → 74 (collections.dylan, test-functional.dylan,
  recursive-locks.dylan), all emitting real `fn`s.
* **Lesson**: always test corpus-shaped fixtures with a real `Module:` header AND a
  blank line; a missing blank line silently routes code into the preamble and
  produces misleading "parse abort" symptoms.

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
