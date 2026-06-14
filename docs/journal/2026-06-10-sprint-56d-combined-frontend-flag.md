# 2026-06-10 — Sprint 56d (a): the combined `--frontend-with-dylan` flag

*A small, low-risk step on the cutover path: one flag that composes all three
front-end opt-ins, so the WHOLE Dylan front-end can be exercised — and verified
byte-identical to Rust — in one switch. The foundation for the skip-Phase-3/4
cutover.*

## What we did

Added `--frontend-with-dylan` / `NOD_FRONTEND_WITH_DYLAN` (`main.rs`). It implies
all three existing opt-ins: it sets `NOD_EXPAND_WITH_DYLAN` (so the parse-path
expander is Dylan too — the lowering path already expands Dylan-side under
`--lower-with-dylan` since 56b), and ORs into `want_sema_with_dylan` +
`want_lower_with_dylan`. So lex (default) + parse (default) + expand + sema +
AST→DFM lowering all run in Dylan, with the back-end taking the Dylan handoff.
The lowering still bails per-module to Rust on any uncovered form.

This does NOT yet skip Rust Phase-3/4 — the methods/blocks/variables/closures
tables are still Rust-built (from the Dylan-parsed AST), and classes are still
Rust-derived (verified-from-Dylan). Those consume steps are next. The flag is
the composition + the test handle.

## Verification

`--frontend-with-dylan dump-dfm` byte-matches plain `dump-dfm` on `point`,
`richards-shape`, `factorial`, `macros-unless`, `gap-007-repro`, `hello` (a
spread of classes / methods / macros / %-prims / strings) — i.e. the fully
composed Dylan front-end produces byte-identical DFM to the all-Rust front-end
on the covered subset. Host-only change (no shim rebuild).

## Where it leaves us

Next: the actual cutover — under this flag, SKIP the Rust Phase-3/4 table build
and consume `methods` (now emit+verified, 56c-T) from the Dylan dump, with
blocks/variables/closures empty for the covered subset, replaying the dispatch
pre-registration + sealing side-effects. That's the high-risk, runtime-tested
step; this flag is its entry point.
