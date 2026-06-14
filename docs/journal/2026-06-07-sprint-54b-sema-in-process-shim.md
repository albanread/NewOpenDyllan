# 2026-06-07 — Sprint 54b: the Dylan sema walk goes in-process (shim-bundled)

*54a split the recording phase into `analyse_module` → `SemaModel`. 54b makes
that model obtainable from the Dylan side **inside the host process**: bundle
the sema walk into the statically-linked shim, export a `dylan-sema-emit`
entry, and gate its output byte-identical to the Rust oracle — run in-process,
not via the standalone EXE. This is the channel 54c flips load-bearing.*

## What

The recipe came straight from the existing AST/expander wire (the agents
mapped it): the export needs **zero new infrastructure** — a bundled Dylan
`define function` keeps its source symbol name in AOT `StaticLibrary` mode, and
the host declares a matching `extern "C"` with `#[link_name]`, covered by the
same one-time `nod_aot_resolve_relocs`.

- **`tests/nod-tests/fixtures/dylan-lex-shim.prj`** — bundle `dylan-c3.dylan` +
  `dylan-sema.dylan` into the shim (c3 before sema; both after the parser). The
  disjoint shim class-id band absorbs their classes — the `.obj` rebuilt with
  **zero** "class redefinition refused" lines.
- **`dylan-lex-shim.dylan`** — new `dylan-sema-emit (source) => <byte-string>`:
  `lex` → `parse-dylan-with-precedence` (honouring `Precedence: c` exactly as
  `dylan-parse-emit`) → `collect-top-names`, returning the four-section model
  dump.
- **`src/nod-driver/src/dylan_parse_wire.rs`** — `extern "C" dylan-sema-emit` +
  `sema_emit_via_shim(src)`, mirroring `expand_source_via_shim` (text transport
  via `read_dylan_byte_string`).
- **`src/nod-driver/src/main.rs`** — `dump-dylan-sema <file>` subcommand:
  `dylan_lex_jit::init()` (fires the shared resolver once) → `sema_emit_via_shim`
  → print.

## Why text transport (not the binary fixnum wire yet)

`DYLAN_SEMA_WIRE.md` §3 designs a fixnum wire; 54b deliberately uses **text**
(the model dump) instead, for two reasons. (1) The dump is *our own*
line-oriented format — it round-trips losslessly, unlike source text (the
52.6 lesson was about round-tripping *source*, preamble and all, not a
structured dump). (2) It reuses `collect-top-names` verbatim — no parallel
fixnum serializer to keep in sync. The binary wire's payoff is perf, which is
deferred until the whole front end is Dylan (the locked decision). 54c will
parse this dump back into a `SemaModel` host-side; if that ever needs the
richer structured form, the fixnum wire is the upgrade, not a rewrite.

## Verification (re-run, not trusted)

- Shim `.obj` bootstrap-rebuilt (no-shim driver → `build --library` → relink):
  "compiled (library)", **0** redefinition/collision lines.
- New gate `dylan_sema_in_process_byte_match` (in `sema_topnames.rs`, reuses
  the 38-fixture `FIXTURES` + `normalize`): `dump-dylan-sema` (in-process shim)
  vs `dump-sema --parse-with-rust` (oracle) — **38/38 MATCH**. Skips cleanly
  when the shim isn't linked (probes `hello`, detects "shim init").
- No regressions from the enlarged shim: standalone `dylan_sema_top_names_byte_match`
  green; `dylan_parse_coverage` / `dylan_parse_translate` green (parser shim
  path unaffected); `lexer_oracle` 2/2 (`--test-threads=1`); `codegen` 8/8.

So the SAME Dylan `collect-top-names` now matches the oracle both as a
standalone EXE (53.x gate) and **in-process through the shim** (this gate) —
the host can obtain the Dylan model at compile time.

## Where it leaves us

The load-bearing channel exists and is proven. 54c is the flip:

- Host-side, parse the `dump-dylan-sema` text back into a `SemaModel` (reusing
  `analyse_module`'s class registration for `ClassId`s — the Dylan model is
  name-keyed; the host still registers classes from the AST to assign ids, then
  populates `top_names` / `generics` / `sealing` from the Dylan dump, applying
  the deterministic const/var-in-`fns` rule the dump filters out).
- Behind `--sema-with-dylan` / `NOD_SEMA_WITH_DYLAN`, the one-line seam
  `let model = analyse_module(m)?` in `lower_module_full` takes the Dylan model
  instead.
- Gate: `dump-dfm` byte-identical with the flag on vs off — the Dylan sema
  becomes authoritative for the back-end, retiring the Rust recompute.
