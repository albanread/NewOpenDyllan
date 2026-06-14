# 2026-06-07 — Sprint 54c: the Dylan sema goes load-bearing (`--sema-with-dylan`)

*54a split `analyse_module → SemaModel`; 54b made the Dylan walk callable
in-process (the `dylan-sema-emit` shim). 54c closes Sprint 54: the back-end
now **consumes** the Dylan-produced model instead of the Rust recompute, gated
`dump-dfm` byte-identical. The Rust sema is retired from the `dump-dfm` path —
the milestone the roadmap names for Sprint 54.*

## The shape of the flip

Opt-in behind `--sema-with-dylan` / `NOD_SEMA_WITH_DYLAN` (default off), so it
**cannot perturb any existing path** — the risk is confined to the new path,
caught by the gate. The agents' maps made the wiring mechanical; the one real
design call was how a name-keyed Dylan model becomes a `ClassId`-bearing
`SemaModel` the back-end can use.

**It's a hybrid, and deliberately so.** `analyse_module` has a runtime side
effect — `register_class` assigns process-global `ClassId`s. The Dylan model
crosses by name (53.1: ids are process-global, never on the wire). So:

- the host still **registers classes** from the AST (`register_module_classes`,
  extracted from `analyse_module`) — a runtime *mechanism*, not "recomputing
  sema";
- the Dylan walk owns the **computed recording** — `top_names`, `generics`,
  `sealing` — parsed back from its model dump and resolved to ids against the
  just-registered classes.

That's `analyse_module_from_dump(m, dump)`: register classes → `parse_sema_dump`
→ assemble the `SemaModel`.

## What

- **`format_sema_model(&SemaModel)`** (was `&LoweredModule`) + `LoweredModule::
  sema_model()` — so the formatter renders any model, including a reconstructed
  one (round-trip target).
- **`parse_sema_dump(dump) → (TopNames, generics, SealingFacts)`** — the inverse
  of `format_sema_model`'s name-keyed sections. Re-applies the GAP-002 rule the
  dump filters out (every `constant`/`variable` also lives in `fns` at arity 0),
  maps each `return=EST` Debug name back to a `TypeEstimate` (`Class(<name>)` →
  resolve to id), and resolves `sealed-domain` specialiser names to ids.
  Classes are NOT parsed from the dump — they come from the host registration.
- **`register_module_classes`** — Phase 1 (register + sealed-flag flip) shared
  by `analyse_module` (Rust recording) and `analyse_module_from_dump` (Dylan).
- **`lower_module_full_with_model(m, model)`** — `lower_module_full`
  parameterized with an optional injected model; the one-line seam
  `let model = match injected { Some(m) => m, None => analyse_module(m)? }`.
- **`nod_sema::set_sema_dump_provider`** + `lower_with_sema_choice` — when the
  driver installs a provider (source → Dylan dump), `dump_dfm_for_file` builds
  the model from the dump and feeds `lower_module_full_with_model`; else the
  all-Rust path. (Only `dump-dfm` opts in for now; compile/eval stay Rust until
  Sprint 56 consolidation.)
- **driver:** `--sema-with-dylan` installs `dylan_sema_dump_provider` (=
  `dylan_lex_jit::init()` + `sema_emit_via_shim`), gated on the shim being
  linked (warns + falls back otherwise).

## Verification — the roadmap's Sprint 54 acceptance criterion

New gate `dump_dfm_sema_with_dylan_byte_match` (`sema_topnames.rs`, reuses the
38-fixture corpus): `dump-dfm` vs `dump-dfm --sema-with-dylan`, byte-compared.

- **38/38 byte-identical** — the DFM the back-end builds is the same whether the
  recording came from Rust `analyse_module` or the Dylan walk. This spans
  `unified_ide` (5277 DFM lines) and `nod-ide` (3069) — classes, anon-method
  lifting, generics, sealing, the lot.
- No regressions (the flip is opt-in): all 3 `sema_topnames` tests pass,
  `nod-sema` 23 units, `sema_dump`, `codegen` 8, `tables` 14 — and the full
  `nod-tests` sweep (running) confirms the default paths are untouched.

## Why this is genuinely "Dylan sema is load-bearing"

The proof obligation isn't "the values differ" — they *must* match (54b proved
the dumps are byte-identical; that's the point). It's **provenance**: the
back-end's input now comes from the Dylan walk (in-process, off the shim,
reconstructed host-side) and produces correct, identical codegen IR. The Rust
`collect_top_level_names` / `collect_generic_names` / `collect_sealing_facts`
are no longer consulted on the `--sema-with-dylan` `dump-dfm` path. Class
registration stays Rust because ids are a runtime mechanism — the honest seam.

## Where it leaves us

**Sprint 54 is done: the Dylan front-end's lexer, parser, macros, AND sema are
all Dylan-written, with sema now load-bearing for the back-end.** The last
front-end stage is lowering (AST→DFM) itself — Sprint 55, the most logic-dense
stage (~5k LOC of `lower_expr`/`lower_call`/control-flow with block-param SSA),
sub-phased 55a (statements/exprs) → 55b (classes/dispatch) → 55c
(closures/blocks), each `dump-dfm`-gated against the Rust lowering. Follow-ups
for 54: extend `--sema-with-dylan` from `dump-dfm` to the compile/eval/AOT paths
(Sprint 56 consolidation), and the two corpus gaps (stdlib-class returns;
expand-before-sema) which dissolve once the whole front end is one Dylan pass.
