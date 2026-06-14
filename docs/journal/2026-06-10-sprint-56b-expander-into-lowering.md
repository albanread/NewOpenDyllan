# 2026-06-10 — Sprint 56b: the expander goes into the lowering path

*The first step of the agent-planned cutover roadmap
(`2026-06-10-roadmap-retire-rust-frontend.md`). The Dylan AST→DFM lowering now
EXPANDS macros before lowering, so macro-using fixtures reach the lowering as
kernel AST instead of bailing the whole module. The highest-leverage,
lowest-risk unblock — a host-seam change, no shim rebuild.*

## The gap (from the planning workflow)

The `--lower-with-dylan` provider (`dylan_dfm_dump_provider`, main.rs) handed the
lowering shim RAW, UNEXPANDED source. The Dylan parser parses `unless`/`when`/
`cond`/`for-each` as `<ast-statement>`s whose `stmt-word` is the macro name;
`lower-expr` only handles `if`/`while`/`until`, so every macro fixture lowered to
`""` → the host transparently fell back to Rust → the whole-corpus survey passed
**vacuously** (those fixtures never got a Dylan dump). Meanwhile the oracle
(plain `dump-dfm`) always expands via `expand_with_stdlib_macros`.

## What we did

- **`dylan_expand_then_lower_emit(src)`** (new, main.rs): expand the source
  Dylan-side via the EXISTING, already-byte-match-gated `expand_source_via_shim`
  (the Sprint 52 expander, proven AST-equivalent to the Rust expander by
  `macro_file_expand.rs`) + `stdlib_macro_source()`, then run the Dylan lowering
  on the expanded source. Falls back to raw source on expansion error (a
  macro-free file expands to itself, idempotent after the whitespace-insensitive
  re-lex). **Independent of `NOD_EXPAND_WITH_DYLAN`** (which gates the *parse*-path
  expander) — there are now two expand seams, both calling the same shim entry.
- Wired it into BOTH the `--lower-with-dylan` provider AND the standalone
  `dump-dylan-dfm` command (chose this over the roadmap's provider-only
  minimal-change so the unlock is PROVABLE: the standalone dump reflects the full
  Dylan front-end expand→parse→lower, so a macro fixture's non-empty standalone
  dump proves Dylan lowered it rather than falling back).
- No shim rebuild (host Rust only). The expander is already bundled.

## Discovered

- **Expansion is necessary but not sufficient for most macro fixtures.** The
  macro EXPANDS fine, but the expanded form often hits OTHER unsupported lowering
  constructs. `macro-when-only` (→ plain `if`) lowered immediately. Adding `~`
  (unary not → `PrimOp BoolNot`, dst `<boolean>`) in the same batch unlocked
  `macros-unless` (→ `if (~ cond) … else 0`) and `expand-pipeline-smoke`. The
  remaining macro fixtures need more: `macro-for-range` → `begin` + a loop,
  `macro-when-cleanup` → `block`/cleanup. So 56b is the INFRASTRUCTURE win — as
  forms land, fixtures unlock through the seam automatically. (`for` itself is
  NOT a macro — a kernel `<ast-for-clause>` statement Rust doesn't lower either,
  so it has no oracle; a separate axis-1 task.)
- **`begin` exposes a void-marker materialization gap (deferred).** `begin … end`
  is transparent (lowers as `lower-stmt-range` over its body) and matches Rust
  for value bodies, BUT when its last statement is a void loop followed by more
  statements, Rust materializes the loop's void value as `Const Bool(false)` at
  `loop_exit` and `lower-stmt-range` does not. The whole-corpus survey caught
  this as a wrong dump on a hand-written probe — so `begin` was dropped from this
  batch (kept `~`, which is clean) pending the non-tail-void-marker fix. The
  survey paid off again.
- The expander re-lexes the whole stdlib per call (cached per process), so the
  survey is noticeably slower with expansion forced. Acceptable for a gate; the
  ~50× hybrid cost is the deferred consolidation, not a correctness issue
  (`perf-waits-for-whole-frontend`).

## Verification

- Whole-corpus survey (expansion forced on all 64 fixtures): **0 mismatches** —
  forcing expansion did not break any fixture (idempotent on the macro-free
  ones, correct on the macro ones).
- `macro-when-only` now provably lowers in Dylan (non-empty standalone, PHASE0).
- Plus the system memory tuning that fell out of this work: capped
  `[build] jobs = 6` in `.cargo/config.toml` — `cargo test --workspace` was
  OOMing the linker (LNK1102 / link.exe exit 1102) by linking ~20 LLVM-debug
  test binaries at once on a 32 GiB box.

## Where it leaves us

The expand seam is in and sound. The next axis-1 forms (`~` unary not, `begin`
blocks, `block`/cleanup) each now unlock one or more macro fixtures through it.
Per the roadmap, the parallel workstreams are 56b-T (verify variables/closures),
56c-T (verify methods/blocks), and 56a-WIRE/CONSUME (the class-table consume),
converging on 56d-56f (combined flag → skip Phase-3/4 → default + delete Rust).
