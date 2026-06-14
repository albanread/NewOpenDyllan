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
| OpenDylan corpus parse (`dump-ast`) | 123 / 161 | language + stdlib suites (DUIM/etc. excluded) |
| OpenDylan corpus lower (`dump-dfm`) | _baseline TBD_ | |
| OpenDylan corpus build/run | 0 | the headline goal to move |

## Iterations

*(newest first)*

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
