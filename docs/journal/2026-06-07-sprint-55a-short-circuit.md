# 2026-06-07 — Sprint 55a: short-circuit `|` / `&`

*A small form, the same block-param-SSA shape as `if`. `a | b` / `a & b` are
NOT PrimOps — they short-circuit, so they lower to an
`sc_edge` / `sc_rhs` / `sc_join` diamond.*

## What

`lower-short-circuit` (routed from `lower-expr`'s binop branch when the
operator is `|`/`&`, before `select-binop`): evaluate the LHS in the current
block, create the three blocks (in id order), then

- `|`: `If lhs sc_edge sc_rhs` — LHS true short-circuits to `sc_edge` carrying
  the LHS value; false falls to `sc_rhs`.
- `&`: `If lhs sc_rhs sc_edge` — the targets swap.

`sc_edge` jumps to `sc_join` with the LHS value; `sc_rhs` evaluates the RHS and
jumps with it; `sc_join`'s block-param is the result (type = lattice join of
the two). Same emission order + env-merge guard (bail on GC-typed env) as `if`.

## Verification

`a | b` and `a & b` (function bodies) byte-match Rust; the committed
`lower-shortcircuit.dylan` (both operators + `|` as an `if` condition) matches.
Gate `dylan_lower_phase0_dump_dfm_byte_match` → **9 fixtures**, no regressions.

## Where it leaves us

`if` + short-circuit cover the diamond-shaped control flow. Next and harder:
`while`/`until` loops (header/body/exit with a back-edge), which bring the
env-merge for loop-carried variables + `:=` assignment — currently being mapped
(exact dump-dfm targets + the header-param ordering rule) so the byte-match
holds. The deferred GC-typed-env merge generalises there too.
