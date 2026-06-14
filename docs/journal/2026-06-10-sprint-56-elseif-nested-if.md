# 2026-06-10 — Sprint 56 (axis-1): `if / elseif / else` in Dylan lowering

*The highest-leverage remaining lowering unlock — `elseif` is ubiquitous. The
Dylan lowering desugars `if / elseif / else` into NESTED ifs (mirroring Rust),
reusing the already-byte-matched single-`if` machinery. Notable for a
self-referential standalone-compile gotcha.*

## Goal

`lower-if-expr` bailed on any `elseif` clause (only a single `else` was handled).
Rust lowers `if (c1) b1 elseif (c2) b2 else b3` as nested ifs — the outer if's
else-arm is itself `if (c2) b2 else b3`. So the whole construct is a chain of
2-way diamonds. Since the Dylan `lower-if-expr` already lowers a nested-if
else-arm correctly (it recurses through `lower-stmt-range` → `lower-expr`), the
fix is to *desugar the elseif chain into a synthetic nested `if` statement* for
the else-arm and let the existing machinery handle it — guaranteeing the block
ids / temps / merge-set nesting match Rust, because it's the same code path.

## What we did

- **`lower-if-expr` clause resolution** (`dylan-lower.dylan`): for an `elseif`
  clause, build a synthetic `<ast-statement>` whose body is the elseif clause's
  body (which already carries its cond as the first constituent — the same shape
  as a leading `if` body) and whose clauses are the *remaining* clauses, then set
  it as the single statement of the else-arm. `else` resolves as before;
  unknown clause words (case/exception) still bail. `collect-assigned` already
  recurses into `<ast-statement>`, so the outer merge set picks up vars assigned
  in elseif/else arms — matching Rust's per-level merge sets.
- **`make-if-statement` factory in `dylan-parser.dylan`** — see the gotcha.

### The gotcha: standalone-compile self-reference

The first cut called `make(<ast-statement>, …)` directly in `dylan-lower.dylan`.
The shim built fine (it bundles `dylan-parser.dylan`, where `<ast-statement>` is
defined), and the elseif fixtures byte-matched — but the **whole-corpus survey
gate caught a regression**: `dump-dfm dylan-lower.dylan` (compiling the lowering
*as a standalone program*) started failing with `undefined ident
<ast-statement>`. `dylan-lower.dylan` references that class throughout
(`instance?`, accessors), but those are tolerated as unknown when out of scope;
`make(<class>)` is NOT — it forces class resolution to emit a `ClassMetadataPtr`.

Fix: a `make-if-statement(word, body, clauses)` factory in `dylan-parser.dylan`
(where the class is in scope). `dylan-lower.dylan` calls the *function* — tolerated
standalone, like the accessors — so it never references `<ast-statement>` by
class. Self-dump-dfm restored; the survey is back to ERR=1 (only `dylan-lex-shim`).
**Lesson: the whole-corpus survey earns its keep — it caught a self-referential
break a curated gate would have missed.** (This is the gate committed earlier
today; its first real catch.)

## Verification

- A 3-arm `if/elseif/elseif/else` byte-matches Rust exactly — joins nest
  `join7→join8→join9`, mirroring the nested desugaring. An arm-assigned var
  (`acc := …` in every arm) threads through both nested joins correctly.
- Unlocks **`gc_loop_accum`** (elseif + `concatenate` dispatch + loops → flip-only).
- New committed fixture `lower-elseif` (PHASE0, both shapes); `gc_loop_accum`
  added to FLIP_ONLY. Phase-0 + curated-flip + whole-corpus survey all green;
  **0 mismatches**; standalone lowered **31→32**.

## Where it leaves us

`if/elseif/else`, `while`, `until` are all covered; `case`/`begin`/blocks remain.
The remaining bails are the macro cluster (needs the gated expander), the big
self-hosting sources, and the IDE/rope family (other forms — `case`, blocks,
multi-value binds). Next clean axis-1 candidate: `case` (another nested-diamond
desugaring), or `begin … end` blocks.
