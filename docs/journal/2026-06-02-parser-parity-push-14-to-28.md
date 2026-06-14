# 2026-06-02 — Parser parity push: 14 → 28/36, and two traps

*Sprint 51e, continued. A supervised push on the Dylan-parser →
`ast::Module` translator to close the `dylan_parse_translate` fall-back
punch-list, plus the front-end-migration sprint specs (51e–54) and a git
consolidation. Follows [the translator payoff](2026-06-01-translator-payoff-paren-and-assign.md).*

## Goal

Drive the remaining front-end migration (parser → macro → sema → lowering
to Dylan). This session: author the sprint specs for the band, then grind
the parser translation gate from 14/36 toward parity.

## What we did

1. **Sprint specs (planning agent).** Wrote `specs/51e-parser-to-default.md`,
   `52-macro-expander-dylan.md`, `53-sema-dylan.md`, `54-lowering-dylan.md`,
   and a `README-frontend-migration.md` roadmap. The key design call, in 52:
   **expand macros entirely Dylan-side, on the parser's own tree, *before*
   the AST-wire emit** (locus B) — no new wire, the host stays oblivious,
   reuses 51e's byte-identical gate. Corrections the agent found vs the
   brief: `lower.rs` is 7,769 lines (not 7,515), the DFM `Computation` enum
   has 10 variants (not 11), there is no `dump-sema` subcommand yet (53 must
   build it).

2. **51e.1 — `Precedence: c` pragma in the Dylan parser** (976a6b8). A
   faithful mirror of nod-reader's `parse_or…parse_pow` C-ladder, gated on
   the header (re-scanned from the preamble in the shim, since the lexer
   drops it). Verified the ladder against `parser.rs` line-for-line.

3. **51e.2 — `operator_in_gap` strips comments** (6e5e97d) + **HashLit /
   DefineBinding wire kinds** (9b93f1f). The translator recovered a
   BinaryOp's operator from the source gap by stripping delimiters +
   whitespace, but a gap can hold a multi-line `//` comment (dylan-parser's
   `|`-chain has a 6-line block between operands) — it scooped the comment
   text as a garbage operator. Made it comment-aware.

4. **51e.4 — definition modifiers on the wire + DefineGeneric** (aeef9ad).
   New `Modifier` wire kind (38) emitted as leading children of every
   definition; the translator reads them via `Modifier::from_word` in
   source order. Rewrote the generic emit (was a bare leaf) to a full
   signature, added `translate_generic`. `richards-shape{,-open}` translate.

## Discovered — two traps that cost real time

1. **Fall-back reasons are first-reported-reason artifacts.** The gate's
   "9 Precedence: c file" bucket did NOT lift the tally when the pragma
   landed — each of those 9 files had a *second* downstream blocker that the
   `Precedence: c` early-return had masked. Eliminating one `Unsupported`
   reason just rebuckets the file to its next one. So a sub-task's headline
   number ("→ 23/36") can be unreachable in isolation even when the sub-task
   is correct and complete. Read the gate as "which reason is *first* per
   file," not "N files need exactly this fix."

2. **The gate's `ensure_driver_built()` can measure a stale binary.** The
   gate test runs `cargo build -p nod-driver` itself, then spawns the exe.
   Early on this reported 14/36 with reasons (`expression UnaryOp`, `if
   clause elseif`) that *cannot* come from the on-disk translator (those
   arms exist). A clean rebuild showed the true tally was already 24/36 —
   51e.1 had moved it 14→24; the "no movement" reading was an incremental-
   build timing artifact. **Lesson: before trusting a tally, `cargo build`
   to completion and re-run; never compare a number from one build against
   a reason-list from another.** Verifying empirically (running the driver
   on individual fixtures) is what surfaced both this and the comment bug.

3. **Git: a stray `docs/doccrate-manual` branch.** The repo had been left on
   a feature branch carrying the DocCrate manual; a commit landed there
   instead of master. Resolved by fast-forwarding master over it (linear
   history, no divergence) and pushing. Lesson for the autonomous loop:
   `git rev-parse --abbrev-ref HEAD` before committing, not after.

## Where it leaves us

`dylan_parse_translate`: **28/36, 0 divergences, 0 regressions.** master ==
origin/master, clean. The remaining 8 fall-backs are all macro-phase work:
3 `define macro`, 2 `when`, 1 `cond` (Error/seeding) — these want the
Dylan front-end to parse + recognise macros, which is **Sprint 52**, where
locus-B parse+expand subsumes parse-level macro recognition — plus
`ide_win_calls` (`define c-function`, FFI) and `unified_ide` (a nested
Error to chase). **Next:** 51e.5 — wire `--parse-with-dylan` into the real
compile pipeline (`compile`/`eval`/`build`) with Rust fall-back + verify-mode
(the full-sweep gate), then 51e.6 default; the macro fixtures close in 52.
