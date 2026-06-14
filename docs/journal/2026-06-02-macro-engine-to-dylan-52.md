# 2026-06-02 — Porting the macro engine to Dylan (Sprint 52.1–52.5, 52.6 prep)

*Sprint 52 — the macro expander joins the lexer and parser in being
Dylan-written. Follows [the parser becoming the default front-end](2026-06-02-parser-is-the-default.md).
Spec: `specs/52-macro-expander-dylan.md`.*

## Goal

Port `nod-macro` (the ~1,900-line Rust macro engine) to Dylan, so the
front-end's third phase is self-hosted. Locus decision **(B)**: expand
entirely Dylan-side, before the AST-wire emit — no new wire.

## What we did

Drove the engine port sub-task by sub-task, each behind a Rust-parity or
hand-verified gate, committing every increment (reviewer pushes):

- **52.1** — locus decision (B) + `DYLAN_AST_WIRE.md` §7 "Parser+macro
  inputs" addendum. No new wire record/kind.
- **52.2** — promoted the engine out of the `dylan-macro-smoke` seed into
  `dylan-macro.dylan` (the production home) + a top-level `define macro …
  end macro` collector. Gate `macro_collect.rs`: name+rule-count parity
  with Rust `collect_macros` over stdlib (5 macros, cond=4 rules) + the
  fixtures.
- **52.3** — `match-pattern` to full parity: all seven `PatternKind`s
  (the seed had only expression/body). Gate `macro_match.rs`: 10 cases
  vs Rust `match_pattern`, identical bindings. Needed a tiny pub oracle
  helper `match_pattern_with_source` (the matcher reads literal text from
  a thread-local call-site source).
- **52.4** — substitution + **hygiene** (binder-only rename,
  `{name}__nod_hyg_{nonce}`). Gate `macro_expand.rs`: 4 cases incl. the
  real `for-each` (`%fip-state` renamed, `?var`/`?coll`/`?body` not), pinned
  nonce, byte-identical to `nod_macro::substitute`.
- **52.5** — multi-rule selection (`expand-call`) + the fragment-level
  module walk to fixpoint (`expand-fragments`/`expand-module-source`).
  Gates: `macro_expand.rs` extended to multi-rule `cond`; `macro_walk.rs`
  for embedded calls, passthrough, recursion-to-fixpoint, siblings.
- **52.6 prep** — strip `define macro` forms in the walk (compile-time
  only); call-shaped macro support (`name(args)`, no `end`).

## Why

The seed was already fragment-shaped (lex → fragments → match → substitute
→ re-lex), which is exactly locus (B)'s pipeline, so the "module walk over
the Dylan AST" the spec describes is really a **fragment-stream walk** —
simpler and the natural Dylan-side representation. Every gate drives the
SAME (def, call) cases through both engines so divergence is impossible to
miss; the cross-checks caught real things (the matcher's thread-local
source dependency; the lexer adapter silently dropping number/string
literals, which corrupts re-lexed expansions like `unless ?x (1) end`).

## Discovered

- **The lexer adapter was lossy.** `lex-token-to-tok` dropped every token
  kind it didn't explicitly handle (numbers, strings, chars, symbols).
  Harmless for collecting/matching the corpus (no literals in load-bearing
  positions) but it silently ate the `1` in a re-lexed `(1)`. Fixed by
  round-tripping all literal kinds as opaque `#"literal"` tokens.
- **Hygiene gensyms must be pinned to cross-check.** Rust's nonce is a
  per-expansion counter; the gate pins both sides to 42 so the rename text
  is deterministic. Neither corpus fixture actually has a template binder
  that isn't a pattern var, so the nonce never bites them — but the
  synthetic `let`/`method` cases exercise it.
- **The whole-file text round-trip is fidelity-limited — the real 52.6
  blocker.** `expand-module-source` renders expanded fragments back to
  text via `render-frags`, then the file would be re-parsed. Two leaks:
  the Dylan lexer keeps the `Module:` preamble (and `render-frags` drops
  the `:` off keyword-name tokens, so `Module: macros-unless` becomes
  `Module macros-unless`); and any keyword-name in the body (init-keywords,
  keyword args) loses its colon the same way. So rendering to text and
  re-parsing cannot be byte-faithful in general. The correct locus-(B)
  integration emits expanded **tokens** with synthesized spans (the
  span-rewrite piece deferred from 52.4) straight into the parser, never
  round-tripping through text.

## Update — 52.6 verify-mode PROVEN, then the production rollout hits class-id drift

Two more findings closed out the session:

- **Verify-mode works end-to-end (test level).** `macro_file_expand.rs`
  runs the Dylan expander over a whole file, re-parses the expanded source
  with the Rust parser, and asserts the AST is byte-identical to Rust
  `parse → expand` (modulo the compile-time-only `(Header …)`/
  `(DefineMacro …)` subtrees). `macros-unless.dylan` and
  `macro-for-range.dylan` both match exactly — hygiene and call-shape
  included. Two fidelity fixes made the text round-trip faithful:
  re-append the colon on keyword-name tokens (`Module:`, `x:`) when
  rendering, and strip the `Module:` preamble (host-side metadata) so the
  single-line render doesn't confuse preamble detection. Commit `a153d90`.
  The expander is correct; the source→source transform is the whole job.

- **Bundling the engine into the shim is SAFE — my first read was wrong.**
  Tried bundling `dylan-macro.dylan` into `dylan-lex-shim.prj` + a
  `dylan-expand-source` entry, saw `dump-dfm` crash, and jumped to "the
  engine's ~15 classes drift the AOT class-ids." **That was a
  misattribution.** Re-tested with reliable paths: with `dylan-macro`
  bundled, `macro_engine` (AOT compile+run), `tables` (14 codegen),
  `c3_oracle`, `gc_stress`, `sealing`, and `dylan_parse_coverage` all
  pass. The disjoint shim class-id band (`classes.rs` `next_shim_id`,
  `028f8ac`) already absorbs the engine's classes — and `dump-dfm`
  crashes the SAME way on the clean shim, so it isn't caused by the
  bundle. `aot.rs:1037` is the drift *assertion*; it fires because I
  rebuilt the shim `.lib.obj` with a shim-linked driver instead of the
  bootstrap (no-shim) sequence, baking a subtly-drifting `.obj`. The
  band is fine; the rollout is NOT blocked on it. The real care item is
  rebuilding the `.obj` via bootstrap.

## Where it leaves us

The engine is complete, parity-gated, AND its whole-file output is
verify-mode-proven against the Rust expander (`1c8cde4` … `a153d90`, five
macro gates green). What remains is the front-end **production rollout**, which is now
unblocked — the band already handles the engine's classes:

- **Rebuild the shim `.lib.obj` via the bootstrap** (no-shim driver):
  remove the `.obj`, `cargo build -p nod-driver` (no-shim), then
  `build --library` the shim, then `touch main.rs` + `cargo build` to
  relink. Rebuilding with a shim-linked driver bakes a drifting `.obj`
  (the `dump-dfm` `aot.rs:1037` symptom). This is the one care item.
- Then: bundle `dylan-macro.dylan` + the `dylan-expand-source` shim entry
  (written, reverted), a host-side byte-string read-back + the parse
  override calling expand first under `NOD_EXPAND_WITH_DYLAN`,
  stdlib-source delivery, the verify-mode AST comparison (normalising out
  Header/DefineMacro as the test gate does), the full-corpus sweep, and
  the default flip (52.7).

The remaining work is mechanical host wiring + the bootstrap `.obj`
rebuild discipline — not engine correctness, and not a class-id-band
extension. The engine is done and proven.

## Update 2 — 52.6c landed: the expander runs in the real pipeline

Wired end-to-end (commit `f29da53`). The shim gains
`dylan-expand-source(source, stdlib-source)`; `dylan-macro.dylan` is
bundled into `dylan-lex-shim.prj` (bootstrap-rebuild the `.obj`); the host
parse override (`dylan_parse_module`) expands `src` Dylan-side under
`NOD_EXPAND_WITH_DYLAN` before parsing, then parses the macro-free result.
Two things that bit and got fixed:

- **`expand-module-source` must PREPEND the verbatim preamble**, not strip
  it — the host needs the module name, and the single-line body still
  re-lexes because the header keeps its newlines.
- **Init the shim resolver before calling `dylan-expand-source`.** The
  expander lexes, which sets the lexer's `*src*`/`*pos*` module variables
  via AOT-resolved name literals; calling expand before `dylan_lex_jit::init()`
  panics in `nod_var_set_by_name` (garbage name Word). The existing parse
  path inits first; the expand path must too.

Headline: `expand-pipeline-smoke.dylan` (a `main` using stdlib `unless`)
builds under the flag — `expand-with-dylan: expanded` → `parse-with-dylan:
translated` → runs → `42`. A file that FELL BACK without the flag (the
parser declines `define macro`) now goes fully Dylan-side. Gate:
`macro_pipeline.rs`.

## Update 3 — render fidelity holds on real source, but perf is the 52.7 gate

Ran `macro_engine` (builds the lexer+parser+engine `.prj`, ~6k lines)
under `NOD_EXPAND_WITH_DYLAN=1`: **passes** — the expander re-renders and
round-trips the compiler's own complex source correctly (strings, the
whole lot). So `render-frags` fidelity is solid, not just on toy macros.

BUT it took **1077 s** vs ~22 s — ~50× slower. The cost is the
shim-strapped Dylan expander running per file, and especially
`collect-macro-defs(stdlib-source)` re-lexing the entire stdlib via the
(JIT'd, slow) Dylan lexer on EVERY expand call. So:

- **52.7 (flip the expander to default) is deferred — and not to a
  hybrid-perf pass.** A small stdlib-table cache landed (`1dfe6e4`;
  correct, gate green), but the real cost isn't the stdlib re-lex — it's
  the **per-parse AST-wire crossing** (Dylan builds the tree → serialize
  to 4-int records → Rust reconstructs `ast::Module`) plus the
  JIT-strapped shim, repeated per stage. Those intermediate crossings
  exist only because the front-end is *partially* ported.

## Update 4 — strategy: perf waits for the whole front-end

Decision (user): **do not optimize the shim/wire hybrid for speed.** When
lex → parse → expand → sema → lower are all Dylan, the intermediate
AST-wire crossings vanish — the Dylan→Rust boundary drops to a single
DFM/IR handoff (back-end stays Rust+LLVM), crossed once per compile, not
per stage. Performance comes from that consolidation, not per-step tuning
now.

So Sprint 52 is **complete as a correctness milestone**: the macro engine
is self-hosted, wired into the real pipeline, gated, and **opt-in**
(`NOD_EXPAND_WITH_DYLAN`); the Rust expander stays the default. The
"make-default" flip waits for the whole-front-end milestone, not a perf
pass. Next is **Sprint 53 — sema in Dylan** (`specs/53-sema-dylan.md`),
continuing the port down the pipeline; the wire and shim costs are
transient scaffolding that retire when the front-end is whole. Recorded
in memory `perf-waits-for-whole-frontend` so this isn't re-litigated.

1. Emit expanded tokens (not text) with synthesized spans — finish the
   52.4 span-rewrite, fragment→`<token>` flattening.
2. A shim entry that lexes → fragments → expands → feeds the parser, with
   the macro table seeded from the stdlib source (wire input (b)); bundle
   `dylan-macro.dylan` into `dylan-lex-shim.prj` (collision-free — checked)
   and rebuild the production shim `.lib.obj`.
3. `--expand-with-dylan` / `NOD_EXPAND_WITH_DYLAN` + verify-mode. Note the
   verify comparison must normalise out `define macro` items: locus (B)'s
   output is macro-free, but the Rust oracle's `format_ast_module` still
   prints `(DefineMacro …)`.
4. Flip the default + docs (52.7).

This rebuilds the production shim that the default parser uses, so it is
the point to confirm direction before proceeding.
