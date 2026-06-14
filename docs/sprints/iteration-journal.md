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
| OpenDylan corpus parse (`dump-ast`) | 150 / 161 | language + stdlib suites (DUIM/etc. excluded); 101 at session start |
| OpenDylan corpus lower (`dump-dfm`) | _baseline TBD_ | |
| OpenDylan corpus build/run | 0 | the headline goal to move |
| Macro engine | definition macros ✅ | first one (`benchmark`) builds+runs; was: only body/call macros |
| Evidence | `tak`/`benchmark` build to `.exe` and run | pure benchmark computation compiles + runs correctly (=7) |

## Iterations

*(newest first)*

### 2026-06-14 — Iteration 5: testworks-compat stdlib helpers + matcher nested-`end` fix

- **Demand:** real gabriel benchmarks (`tak.dylan`) call `benchmark-repeat
  (iterations: N) … end` and `assert-equal` — from `testworks`, which is a
  separate package not vendored in the reference tree.
- **Added (minimal, faithful):** `assert-equal(expected, actual)` (equality
  check) to `stdlib/collections.dylan`; `benchmark-repeat ?opts:expression
  ?body:body end` (runs the body, yields its value — drops only the repeat-count
  timing) to `stdlib/macros.dylan`.
- **Found + fixed a *third* nested-`end`-balancing bug:** the macro **matcher's**
  `?body` termination (`nod-macro`) had its own hardcoded block-opener list,
  missing the library body-macros (`benchmark-repeat`, `with-lock`, …). Extended
  it to match the parser's set, so a matched macro's body can contain nested
  library-macro `end`s.
- **Verified:** `assert-equal(7,7)`→`#t`, `(7,8)`→`#f`; `benchmark-repeat` in a
  normal function body builds + runs (=7); the `benchmark` definition macro now
  matches real `tak`'s body. In-tree fixtures unchanged (55/55).
- **Remaining blocker for `tak.dylan`:** definition-macro-produced items use a
  span shortcut (lexed with the call-site file id), so RECURSIVE expansion of a
  body-macro *inside* the produced item (`benchmark-repeat` inside the
  `benchmark`-produced function) re-lexes the wrong tokens. Needs an origins-based
  span rewrite of the produced item (scratch SourceMap + `rewrite_spans_item`) —
  next.

### 2026-06-14 — Iteration 4: definition macros (engine feature) — `define benchmark`

- **Demand:** the gabriel benchmark files wrap pure functions in
  `define benchmark NAME () body end` — a *definition* macro. Our engine had **no
  definition-macro support** (the marquee missing macro feature). A hand-extracted
  `tak` already builds to an exe and runs correctly, so the wrapper was the blocker.
- **Research (agent):** the collect→expand two-pass and the rule parser already
  handle this shape; the gap is recognising a top-level `define <word> … end`
  (an `Item::DefineOther` whose keyword is a registered macro) and expanding it by
  re-parsing the substituted expansion as a top-level **item**, not an expression.
- **Fix (`src/nod-macro/src/lib.rs`):** added `expand_definition_macro` (mirrors
  `expand_one` but re-parses via `parse_module_with_macros_rust` → `Item`) and a
  span-based `call_site_fragments_span`; `expand_module` now rewrites
  `DefineOther{keyword ∈ table}` through it. Added the first definition macro,
  `benchmark`, to `stdlib/macros.dylan`.
- **Result:** `define benchmark foo () 3 + 4 end` → `define function foo` →
  **builds to an `.exe` and runs (prints 7)**. The first definition macro in
  NewOpenDylan. In-tree fixtures unchanged (55/55).
- **Caveats / follow-ups:** verified via `--parse-with-rust`; the DEFAULT pipeline
  routes through the Dylan parser, which doesn't yet recognise definition-macro
  calls (panics "define: expected a define-body word") — needs Dylan-parser
  awareness or a Rust fallback in the lowering path. Nested body-macro expansion
  inside a definition-macro body + fine-grained diagnostics need an origins-based
  span rewrite. Real gabriel files additionally need `benchmark-repeat` +
  `assert-equal`.

### 2026-06-14 — Iteration 3: route library body-macros as macro calls (parser bug)

- **Target:** the residual `KwEnd` (context-(b) body-macros in *parsed* function
  bodies) — gabriel `stak`/`traverse`/`triang` (`dynamic-bind`), plus
  `with-lock`/`with-open-file`/etc. used inside `define function`/`method`.
- **Diagnosis:** these are block-openers (`is_block_opener_kw`) but were not in
  the parser's `known_macros` set, so the statement dispatch parsed them as plain
  calls (no body) and their `end` dangled.
- **Fix:** route any `is_block_opener_kw` word (not just `known_macros`) to
  `parse_body_shaped_macro_call` when the call shape matches — one-line guard
  change at the expression/statement dispatch.
- **Result:** corpus parse **139 → 150**; `KwEnd` failures 12 → 1. In-tree
  fixtures unchanged (55/55). The remaining 11 failures are long-tail singletons
  (keyword-symbol atoms, no-`end` define-forms, adjacent strings, `==`/operator
  names, a couple of param-list `,` cases).

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
