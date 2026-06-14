# Corpus iteration journal

Autonomous, goal-driven iteration over the OpenDylan test corpus
(`opendylan-tests/`). Loop: pick a test → run it → diagnose → fix the compiler
bug *or* add the reasonable missing stdlib feature (no massive subsystems like
DUIM) → re-run → on a pass, record it here and keep going. Verify no regression
(in-tree fixtures stay green) and commit each win.

## Status

| Metric | Value | Notes |
|--------|-------|-------|
| In-tree fixtures (`dump-ast`/`dump-dfm`) | 55 / 55 | regression guard — must stay green |
| OpenDylan corpus parse (`dump-ast`) | 139 / 161 | language + stdlib suites (DUIM/etc. excluded) |
| OpenDylan corpus lower (`dump-dfm`) | _baseline TBD_ | |
| OpenDylan corpus build/run | 0 | the headline goal to move |

## Iterations

*(newest first)*

### 2026-06-14 — Iteration 2: nested body-macro `end` balancing (parser bug)

- **Target:** the dominant `KwEnd` cluster (28 files) — gabriel benchmarks
  (`define benchmark` wrapping `benchmark-repeat (…) … end`), and threads/io/
  system tests using `with-lock` / `with-open-file` / `printing-object` /
  `collecting` / `timing` / `profiling`.
- **Diagnosis (agent):** *two* causes. (1) A structural bug —
  `parse_body_shaped_macro_call` (the parsed-body counterpart to
  `skip_body_to_matching_end`) tracked only grouping depth, not block depth, so
  **any** body-macro whose body contained a nested `end`-block closed early
  (even already-known macros: `unless (t) if (t) … end; end;` failed). (2) Several
  library/test body-macros weren't in the block-opener set.
- **Fix:** gave `parse_body_shaped_macro_call` the same block-depth tracking as
  `skip_body_to_matching_end`; extended `is_block_opener_kw` with the
  library/test body-macros (`benchmark-repeat`, `with-lock`, `with-open-file`,
  `printing-object`, `collecting`, `timing`, `profiling`, …).
- **Result:** corpus parse **123 → 139**; `KwEnd` failures 28 → 12; 11 gabriel
  benchmarks (boyer, ctak, dderiv, deriv, div2, fft, puzzle, tak, takl, browse,
  destru) now parse. In-tree fixtures unchanged (55/55 ast + dfm).
- **Residual:** a few context-(b) body-macros (e.g. `dynamic-bind` in
  stak/traverse/triang) still parse as plain calls because they aren't *routed*
  as macro calls (not in the parser's known-macro set) — next.

### 2026-06-14 — Iteration 1: `for` iteration-clause forms (parser bug)

- **Target:** gabriel benchmarks `div2`, `browse`, `triang`, `cl-stubs`, `cn2`,
  `destru`, `traverse` — parse-fail `expected ) after for-clauses, got Equal` /
  `got KeywordColon`.
- **Diagnosis:** compiler bug. `parse_for_clause` only handled `var in …`,
  `var from … [to/below/above/by]`, and bareword `until`/`while`. It rejected
  three real Dylan for-clause forms: `var = init then next` (explicit step),
  `until:` / `while:` keyword clauses, and `var keyed-by key in coll`.
- **Fix:** added `ForClause::Step` + `ForClause::Keyed` AST variants
  (`ast.rs`, `format_dylan.rs`) and extended `parse_for_clause` to accept all
  three forms (`parser.rs`).
- **Result:** every "for-clauses" parse error eliminated; corpus parse
  **121 → 123**. `cl-stubs` now fully parses; the others advance past the
  for-clause and surface the next blocker (the `KwEnd` body-macro cluster,
  next iteration). In-tree fixtures unchanged (55/55 ast + dfm).
