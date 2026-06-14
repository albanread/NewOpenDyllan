# 2026-06-02 — The Dylan parser is the default front-end (51e.6)

*Sprint 51e.6 — the parser-phase capstone. With the shim-AOT class-id
drift fixed, the gate (full sweep green under the shim) finally opens, so
the Dylan parser flips from opt-in to the **default** real-pipeline
front-end. Follows [the class-id resolution](2026-06-02-class-id-drift-attempt-rejected.md).*

## What we did

Flipped the default in `nod-driver`:

- The Dylan parse override now installs **by default** — `if
  !want_parse_with_rust && cfg!(dylan_lex_shim_linked)`. Every
  `compile`/`eval`/`build` in a shim-linked driver routes
  `parse_module_with_macros` through the Dylan parser, with whole-file
  fall-back to Rust for anything it can't translate yet.
- New `--parse-with-rust` / `NOD_PARSE_WITH_RUST=1` opt-out restores the
  legacy Rust parser as the authoritative path.
- **Gated on shim availability** via the build.rs-set
  `dylan_lex_shim_linked` cfg: a fresh checkout without the static shim
  cleanly stays on Rust — no install, no per-file fall-back noise. The
  override JIT-straps the shim lazily on first parse, so non-parsing
  commands (`--version`, etc.) pay nothing.
- The `dump-ast` gate is untouched: `run_dump_ast` calls
  `parse_module_with_macros_rust` directly and gates its inline Dylan path
  on the explicit env, so plain `dump-ast` stays the Rust oracle and the
  `dylan_parse_translate` byte-identical gate keeps working.

## Validation

- `nod-driver dump-dfm hello.dylan` (NO flag) prints
  `Dylan parser override installed (real pipeline active)` — the default
  is genuinely on.
- **Default full sweep** (`cargo test -p nod-tests -- --test-threads=1`,
  no env): **35 test binaries green**, only `ide_shell_infra` fails — the
  pre-existing environmental Win32/D2D access-violation (`0xC0000005`),
  excluded as before. This matches the under-shim-flag sweep exactly,
  confirming the default routes through Dylan with no regressions.
- `dylan_parse_translate` 28/36, 0 divergences (unregressed). clippy clean
  on the changed file.

## Where it leaves us — Sprint 51e complete

The parser phase is done: the Dylan-written parser is the **default**
front-end of the real compile pipeline, with Rust demoted to the verify
oracle + per-file fall-back. The lexer (live) and parser (default) are now
both Dylan.

The 8 remaining corpus fall-backs are all **macro-phase** work — `define
macro` definitions, and `when`/`cond` body-macro call sites — which the
Dylan parser declines today and Rust handles. These close in **Sprint
52**, where the Dylan front-end gains macro parse + expand (locus B:
expand Dylan-side, on the parser's own tree, before the AST-wire emit),
which subsumes parse-level macro recognition. So 28/36 → toward 36/36 is a
52 outcome, not a 51e gap.

**Next:** Sprint 52 — port `nod-macro` to Dylan, building on
`dylan-macro-smoke.dylan`, per `specs/52-macro-expander-dylan.md`. The
shim-AOT path is now clear (class-id band), the parser is a trusted
default source of the AST, and the same 5-step migration pattern applies.
