# Sprint 60 — Year Three Kickoff

*Opens year three of NewOpenDylan.*

The compiler JITs and AOT-compiles non-trivial Dylan, the front-end is written
in Dylan, and the back-end is Rust + LLVM split at [DFM](../compiler/dfm.md).
This sprint proves the compiler can **rebuild itself**, then hardens the two
things that gate everything downstream: a correctly-structured, correctly-parsed
**standard library**, and **corpus coverage**.

## Theme

> Prove the bootstrap. Clean the stdlib. Widen the corpus.

## Goals

1. [Prove we can rebuild ourselves](#1-prove-we-can-rebuild-ourselves)
2. [Move the stdlib into its own folders](#2-move-the-stdlib-into-its-own-folders)
3. [Fix the stdlib's expression precedence](#3-fix-the-stdlibs-expression-precedence)
4. [Continue to improve the compiler](#4-continue-to-improve-the-compiler)
5. [Add more macros](#5-add-more-macros)
6. [Cover the Dylan corpus](#6-cover-the-dylan-corpus)

---

## 1. Prove we can rebuild ourselves

**Objective.** Demonstrate a reproducible self-rebuild: the Dylan front-end,
compiled by our own back-end into the [shim](../compiler/self-hosting.md), can be
rebuilt from source and produces a byte-identical artifact and identical
front-end output across the corpus.

**Tasks.**
- Script the full bootstrap from a clean checkout: `cargo build --workspace`,
  then `nod-driver build --library --project compiler/dylan-lex-shim.prj` to
  produce `compiler/dylan-lex-shim.lib.obj`, then relink `nod-driver`.
- Rebuild the shim a second time from the freshly-built driver and confirm the
  `.obj` is reproducible (same symbols; diff the disassembly/section layout).
- Run the front-end (lex → parse → expand → sema → lower) over the corpus with
  the shim-built driver and confirm output is stable across the rebuild.
- Capture the procedure as a `rebuild` check (a script or test) so the bootstrap
  is exercised, not just asserted.

**Acceptance.** A documented, repeatable command sequence rebuilds the front-end
shim and the driver from a clean tree, and the rebuilt compiler reproduces the
same shim artifact and the same front-end output on the corpus.

**Status — done.** `tools/self-rebuild.ps1` (and a `tools/self-rebuild.sh`
variant) script the loop: build the driver → rebuild the Dylan front-end shim
*from it* → relink the driver → verify. A key gotcha is captured: the shim must
be built with `--parse-with-rust`, otherwise the already-linked Dylan front-end
re-registers the shim's own classes and the build fails with "class redefinition
refused". Proven reproducible first-hand: two fresh shim builds **and** the
committed on-disk artifact are byte-identical (`sha256 92f36804…`), and the
front-end dumps (`dump-dylan-ast`/`dump-tokens`/`dump-dfm`) are byte-stable
across the rebuild.

## 2. Move the stdlib into its own folders

**Objective.** Replace the single `stdlib/*.dylan` with
a structured `stdlib/` directory split by concern, so the standard library can
grow without one monolithic file.

**Tasks.**
- Design the layout (e.g. `stdlib/{core,collections,strings,math,io,conditions,
  format}.dylan`) and a LID/project file that lists them in dependency order.
- Split `stdlib.dylan` along those seams; keep each move a behaviour-preserving
  cut with a full test sweep after each.
- Update the build/driver paths that reference the stdlib source location.
- Keep the [stdlib boundary rules](../compiler/self-hosting.md#the-stdlib-boundary)
  intact: new API in Dylan, Rust only for the gated primitives.

**Acceptance.** The stdlib lives in a multi-file `stdlib/` tree described by a
project/LID file; the full build and test sweep are green; no behaviour change.

**Status — done.** The monolithic `stdlib.dylan` is split into
`stdlib/`: `macros.dylan`, `collections.dylan`,
`strings.dylan`, `ffi-callbacks.dylan`, `structs.dylan`, `streams.dylan`, and the
generated `win32-constants.dylan`. The ordered file list lives in `STDLIB_FILES`
(`src/nod-sema/src/stdlib.rs`) — `macros.dylan` is first so it owns every stdlib
macro for the Dylan-side expander (`stdlib_macro_source`). The `generate_constants`
tool now emits into `stdlib/`. Verified behaviour-preserving: the 128 top-level
definitions are partitioned exactly (define-set + contiguous-byte oracles), the
workspace builds, and the corpus is unchanged (54/55 `dump-ast`, 55/55 `dump-dfm`)
with predicates and macro fixtures evaluating correctly.

## 3. Fix the stdlib's expression precedence

**Objective.** Dylan precedence is **flat and left-associative** per the DRM
([syntax](../language/syntax.md)); the stdlib currently leans on the legacy
`Precedence: c` header pragma and/or carries expressions written for C-style
precedence. Make the stdlib correct under the DRM rule and retire the crutch.

**Tasks.**
- Audit every stdlib file for the `Precedence: c` pragma and for expressions that
  only parse correctly under C precedence (mixed arithmetic/comparison/`:=`).
- Rewrite those expressions with explicit parentheses so they are correct under
  flat precedence, then remove the `Precedence: c` pragma from each file.
- Add focused regression cases for the precedence shapes that were wrong (the
  parser already treats `3 + 4 * 5 == 35`; assert the stdlib agrees).
- Confirm the [reader](../compiler/reader.md) needs no change — this is a source
  correction, not a grammar change.

**Acceptance.** No stdlib file uses `Precedence: c`; all stdlib expressions parse
and evaluate correctly under flat DRM precedence; regression tests cover the
fixed shapes.

**Status — done.** `stdlib.dylan` and `win32-constants.dylan` are pragma-free.
The precedence-sensitive expressions were the 13 character-class predicates
(`ascii-digit?`, `ascii-hex-digit?`, `ascii-bin-digit?`, `ascii-oct-digit?`,
`ascii-uppercase?`, `ascii-lowercase?`, `ascii-alpha?`, `ascii-alphanumeric?`,
`ascii-whitespace?`) plus the `as-uppercase` / `as-lowercase` range checks; each
comparison is now explicitly parenthesized. Verified behaviour-preserving:
`dump-ast` of the parenthesized, pragma-free source is byte-identical (function
bodies) to the original C-precedence AST, and JIT `eval` of every predicate
returns correct results. `win32-constants.dylan` had no precedence-sensitive
expressions (proven by an identical AST), so only the pragma was removed.

## 4. Continue to improve the compiler

**Objective.** Steady forward progress on correctness and capability across the
front-end and the DFM lowering.

**Tasks.**
- Close the highest-leverage entries in
  [known limitations](../reference/known-limitations.md) that block corpus or
  stdlib work.
- Grow the Dylan [AST → DFM lowering](../compiler/sema.md) coverage toward the
  full corpus.
- Tighten diagnostics where the work above surfaces unclear errors.

**Acceptance.** A measurable reduction in the known-limitations list and/or an
increase in lowered-construct coverage, each backed by tests.

**Status — in progress (item 1 landed).** A corpus triage (55 fixtures ×
`dump-ast`/`dump-dfm`/Dylan-side lowering) plus a known-limitations review
produced a ranked backlog. Highest-leverage items, in order:

1. ✅ **Done.** Seed the stdlib macro table in the `dump-ast` diagnostic path so
   it matches the real pipeline — `dump-ast` now derives the stdlib macro names
   via `stdlib_macro_names()` and recognises `with-cleanup`/`repeat`/etc.,
   fixing the only hard corpus parse failure (`macro-when-cleanup.dylan`).
   Corpus parse coverage **54/55 → 55/55**; the byte-identical translation gate
   (`dylan_parse_translate`) stays green; independently verified, no regression.
   *(small)*
2. Lower `case` / `select` in the back-end (currently a hard "not lowered" hole
   hit by both parsers). *(medium)*
3. Extend the AST-wire translator (`dylan_to_ast.rs`) to cover
   `begin`/`when`/`unless`/`for`/`case` statements — the largest single bucket of
   Dylan-parser fall-backs (9/55 fixtures). *(medium)*
4. Grow the Dylan-side lowering past `if`/`while`/`until`/`begin` (add
   `cond`/`case`/`for`/`block`) and stop bailing the whole module on a single
   unsupported item, to raise the byte-match self-hosting coverage (41/55 today).
   *(medium/large)*

Two new robustness gaps were filed this sprint in
[known-limitations](../reference/known-limitations.md): the body-shaped
macro-head separator limitation, and an unhandled signalled condition panicking
the `eval` engine.

**Parser-coverage push (landed).** Three corpus-driven parser fixes — block-depth
tracking when skipping unknown `define`-macro bodies, an optional `;` after a
return signature, and escaped names (`\name`) in import specs — lifted the
OpenDylan corpus parse rate **101 → 121 / 161**, with the in-tree fixtures
unchanged (55/55). Remaining gaps (for-iteration clause forms, nested body-macro
`end`s, keyword-symbol atoms, no-`end` define-forms) are tracked in
[the corpus assessment](../reference/opendylan-corpus.md).

## 5. Add more macros

**Objective.** Grow the surface syntax defined as Dylan
[`define macro`](../language/macros.md) forms rather than hardcoded AST nodes,
per the macro/sema boundary in the [macro expander](../compiler/macro-expander.md).

**Tasks.**
- Identify control-flow / `with-*` / iteration forms still missing or hardcoded
  and express them as stdlib macros.
- Exercise each new macro through the expander and the corpus.
- Note any expander gaps the new macros expose (e.g. `*` repetition, aux rules)
  as known limitations rather than working around them silently.

**Acceptance.** New macros land in the stdlib, expand correctly, and are covered
by tests; any expander gap they reveal is recorded.

**Status — 3 shipped, gaps recorded.** Added to `stdlib/macros.dylan` and
verified by JIT `eval`: `inc!` / `dec!` (in-place `x := x ± n`, call-shaped) and
`repeat N times … end` (counted loop, body-shaped via a `times` keyword
separator like `with-cleanup`). Two further designs — `dotimes` (indexed loop)
and `when-let` — hit a real macro-engine limitation (a parenthesised head with a
custom separator, e.g. `(i below 5)` / `(v = expr)`, fails to parse), and
`assert` depends on `%error` which isn't linked in the JIT `eval` engine. These
are deferred and filed in
[known-limitations](../reference/known-limitations.md). Corpus unchanged;
existing macros intact.

## 6. Cover the Dylan corpus

**Objective.** Increase the share of the test corpus that lexes, parses,
macro-expands, lowers, and runs cleanly through the Dylan front-end.

**Tasks.**
- Run the corpus and triage failures by phase (lex / parse / expand / sema /
  lower).
- Drive coverage up by feeding the failures into goals 3–5 (precedence, compiler,
  macros).
- Track the covered count as a sprint metric.

**Acceptance.** A documented corpus-coverage number at sprint start and end, with
a net increase and the remaining gaps triaged into follow-up work.

**Status — in-tree corpus + upstream corpus assessed.** In-tree fixtures:
**55/55** `dump-dfm`, **54/55 → 55/55** `dump-ast` (after the goal-4 macro-seeding
fix). The upstream OpenDylan corpus (`opendylan-tests/`, language + stdlib suites,
161 files) was assessed end-to-end: **lex 161/161**, **parse 101/161 (63%)**, but
**0 real tests compile** yet — blocked by (1) the testworks harness (not in tree)
and (2) the unported OpenDylan library stack, plus a tight cluster of concrete
parser gaps and a panic-instead-of-diagnostic robustness issue. Full findings and
a ranked unlock plan: [OpenDylan corpus](../reference/opendylan-corpus.md).

---

## Definition of done

- The self-rebuild procedure is scripted and reproduces the shim + front-end
  output.
- The stdlib is multi-file, precedence-correct, and `Precedence: c`-free.
- Corpus coverage is higher than at sprint start, with the delta recorded.
- The full build and test sweep are green.

## Out of scope

- Porting any part of the back-end (codegen, GC, JIT, linker) to Dylan — DFM is
  the floor. See [architecture](../architecture.md).
- macOS / Linux ports — see [platforms](../reference/platforms.md).
