# 2026-06-07 — Sprint 55 Phase 0: the Dylan AST→DFM lowering scaffold

*Sprint 55 ports the last front-end stage — lowering — to Dylan. Phase 0
stands up the scaffold and proves the byte-match approach end-to-end on the
simplest functions: the Dylan-side lowering produces `dump-dfm` text
byte-identical to the Rust lowering. The hardest infrastructure question (can a
Dylan lowering reproduce the DFM dump at all?) is now answered yes.*

## What

New `tests/nod-tests/fixtures/dylan-lower.dylan` (~430 lines), bundled into the
shim, exporting `dylan-lower-emit (source) => <byte-string>` — the in-process
Dylan AST→DFM lowering for the **straight-line subset**:

- Dylan structs mirroring `nod-dfm/src/ir.rs`: `<dfm-func>` / `<dfm-block>` /
  `<dfm-comp>` (tagged: const / primop / directcall) / `<dfm-temp>`.
- `<fn-builder>` mirroring `FunctionBuilder` — monotonic `fb-fresh-temp` /
  block ids, entry = block 0, plus a `LocalEnv` (name→temp) for param/local
  resolution.
- `lower-expr` for the Phase-0 forms: integer/bool/string literals, binary ops
  (via a `select-binop` mirror → `*Int` opcodes), direct calls to known
  top-names, and **bare variable refs resolved through the env** (params).
- `lower-function` mirroring `lower_function_inner`'s straight-line path: param
  temps in declaration order (t0,t1,…) → single body expression → `Return`;
  `return_type` = declared label, else the final temp's type.
- `format-dfm` reproducing `nod-dfm/src/format.rs` **byte-for-byte** (the
  formatter is frozen and doubles as the JIT-cache key).

Transport is **text** (the `dump-dfm` dump), mirroring 54b's sema approach: the
gate compares text, so no structured DFM wire is needed yet (that's the
eventual load-bearing flip, the 55 analogue of 54c). Driver: a `dump-dylan-dfm`
subcommand + `lower_emit_via_shim`.

## Two bugs found by running the gate (the value of the oracle)

1. **No env for params.** The first draft's `lower-expr` had no case for a bare
   `<ast-variable-ref>`, so `x + y` (reading params) bailed. Added a `LocalEnv`
   to the builder; `lower-function` binds each param name to its temp.
2. **The preamble is parsed as items.** The Dylan parser lexes `Module:` /
   `Precedence:` as ordinary top-level forms (the host translator strips them
   via `scan_preamble`; the raw Dylan AST keeps them). `dylan-lower-emit` works
   on the raw AST, so it must **skip** non-definition items (mirroring
   `collect-top-names`) rather than bail on them. The first draft bailed on the
   preamble item → empty output for every fixture. Fixed: skip non-definitions,
   lower `define function`s, bail (return "") on any other definition
   (method/class/generic/constant) so Phase 0 never emits a WRONG dump — a
   fixture it can't fully lower stays on the Rust path.

## Verification

New gate `dylan_lower_phase0_dump_dfm_byte_match` (`sema_topnames.rs`):
`dump-dylan-dfm` vs `dump-dfm` over the Phase-0 fixtures — **`sprint09-add`
and `mutual` byte-identical** (plus a scratch literal `k() => 42`). `mutual`
exercises three functions, params, direct calls, integer consts, and binops in
one module. Skips cleanly when the shim isn't linked. The shim `.obj`
bootstrap-rebuilt with `dylan-lower.dylan` bundled (0 redefinition lines).

## Where it leaves us

The lowering port is real and gated. The scaffold reproduces the exact
emission order (monotonic temp/block ids) the byte-match demands. **55a** grows
it form by form, each re-greening the gate before the next: local `let`
bindings, `if` (the first control flow — block-param SSA + the sorted env-merge
that is the brutal core), short-circuit `|`/`&`, `while`/`until` loops,
multi-binder `let`, more call intrinsics. Then 55b (classes/dispatch) and 55c
(closures/blocks), then the structured DFM wire for the load-bearing flip.
