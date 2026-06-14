# 2026-06-06 — Sprint 53.5(1): broaden the sema byte-match across the corpus

*After 53.2–53.4 grew the Dylan-side sema recording walk section by
section (top-names → generics → classes → sealing), 53.5(1) steps back and
asks: how much of the **whole fixture corpus** does the Dylan walk already
reproduce, byte-for-byte, vs the Rust oracle? Caps the 53.2–53.4 gate work.*

## What I did

Built `dylan-sema.exe` once and ran a survey: for every `.dylan` fixture
(excluding the 8 giant self-host bundle/shim files), compare the EXE's full
output against `nod-driver --parse-with-rust dump-sema <fx>`, normalised
identically (CRLF→LF, per-line trim, whole-block trim). (First pass had an
asymmetric-normalisation bug that flagged even the 9 known-good gated
fixtures as DIFFER — fixed it, re-ran; the 9 came back MATCH, confirming the
harness.)

**Result: 44 fixtures surveyed → 39 MATCH, 5 DIFFER, 0 FAIL.**

So the Dylan sema walk **already byte-matches the Rust `SemaModel` for the
great majority of real inputs** — including macro-using surface
(`cond_smoke`, `macros-unless`, `macro-when-only`, `macro-for-range`), the
macro-engine's own test inputs (`dylan-macro-collect/expand/file/match/walk`,
`expand-pipeline-smoke`), GAP/GC repros, `jit_cache_sample*`, `translate-*`,
`stdlib-min`, the `richards-shape-open` open-class variant, and
`ide_helpers/ide_syntax/ide_win_calls`. None of the class/generic shapes I'd
flagged as "deferred" (inherited slots, MI, sealed-domain, define-variable)
actually diverged — they either already work or aren't isolated by a fixture.

I broadened the gate `tests/nod-tests/tests/sema_topnames.rs` from the 9
hand-picked fixtures to a **curated 33** of the verified-matching set
(skipping only transient `_tmp_*` / `gc_loop_accum` / `*-ir` variants). Gate
re-run: **33/33 MATCH, 1 passed.**

## The 5 divergences — two clean, scoped gaps

1. **Anonymous-method lifting `__anon-method-N`** — `rope`, `ide_rope`,
   `unified_ide`, `nod-ide`. The Rust sema lifts anonymous `method (…) … end`
   literals (e.g. a lambda passed to `for-each-leaf`) to synthetic top-level
   `Item::DefineFunction`s named `__anon-method-NNNN` (a process-global
   counter, `lower.rs:22-33,1443`), which appear in `top_names.fns`. The Dylan
   walk only scans top-level items, not nested expression positions, so it
   emits the enclosing named function instead. Matching this byte-for-byte
   needs a recursive expression walk **plus** replicating the lifter's exact
   traversal order (it sets the `N`). Tractable but fiddly → its own sprint
   (53.5b).

2. **Macro-expansion-before-sema** — `macro-when-cleanup`. The oracle parses
   **and macro-expands**, then records, so a macro that expands to a
   definition (`test-with-cleanup`) shows up as a `fn`. The Dylan walk runs
   on the un-expanded AST (`sema-main` does lex → parse → collect, no expand
   step), so it misses macro-generated top-level definitions. Closing this
   means wiring the (already-ported, already-bundled-elsewhere) Dylan macro
   engine into `sema-main` before `collect-top-names` — a pipeline step, not a
   walk tweak → its own sprint (53.5c), and it dovetails with the full
   front-end integration.

## Where it leaves us

The Dylan recording model is corpus-validated: **33 gated fixtures**, four
sections each, byte-identical to the Rust oracle, spanning functions,
constants, variables, slot accessors, generics (slot + `define generic`),
class hierarchies with C3 CPLs, and sealing. Remaining sema work is the two
scoped gaps above, then the **`--sema-with-dylan` + verify-mode** integration
that makes the Dylan sema load-bearing (retiring the oracle crutch), then
Sprint 54 (`lower_with_model`).

The working loop (ground the survey → curate → gate → document) keeps paying
off: this step was almost entirely "discover we're already correct and lock
it in," which is the best kind of sprint.
