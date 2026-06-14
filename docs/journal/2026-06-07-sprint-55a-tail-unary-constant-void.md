# 2026-06-07 — Sprint 55a-tail: unary `-`, `define constant`, void functions

*Three small lowering forms, the first increment gated through the new flip.
Notable mainly because one of them (void functions) carries loop safepoints, so
it can only be verified through `--lower-with-dylan` — the flip's first payoff.*

## What

- **Unary `-x`** → `PrimOp NegInt` (integer) / `NegFloat` (float), dst typed via
  `primop-result-label` (extended to cover `NegInt` and the float ops, which it
  previously mis-typed as `<boolean>`).
- **`define constant NAME [:: T] = INIT`** → a 0-arg initializer function
  `fn NAME () -> <ret>: <init>; Return t`, emitted in source order with the user
  functions (Rust emits exactly this thunk per constant). Single binder only;
  multi-binder or unsupported init bails. `define variable` still bails.
- **Void functions** (`=> ()`, or a body whose last statement is a loop) → the
  function types `<unit>` with a bare `Return` (no value). Previously a missing
  return value bailed the whole function.

## The gate split this forced

`kernel-arith` (constant + unary, pure integer) is pre-pass-clean, so it joins
`PHASE0_LOWER_FIXTURES` and passes the standalone `dump-dylan-dfm` text gate.
But `translate-loop`'s void functions wrap `until`/`while` loops whose calls get
`safepoint=[…]` populated by the host liveness pass — the *standalone* Dylan
dump (pre-pass, empty safepoints) can't match `dump-dfm`. It's correct only
*through the flip*, where the host runs the same passes on the reconstructed DFM.

So fixtures now split two ways: `PHASE0_LOWER_FIXTURES` (text-gateable, used by
both the standalone gate and the flip gate) and a new `FLIP_ONLY_LOWER_FIXTURES`
(`translate-loop` — verified only via `--lower-with-dylan`). The flip gate
iterates both. This is the general pattern going forward: any form touched by a
post-lowering pass (safepoints, dispatch resolution) is flip-only.

## Verification

`kernel-arith` byte-matches both the standalone text gate and the flip;
`translate-loop` byte-matches the flip (void functions + loop safepoints
populated host-side). Both lowering gates green; the 13 prior fixtures
unregressed; parser round-trip still green (kernel-arith added to its corpus).

## Where it leaves us

Coverage creeps up via the flip. The remaining bails are dominated by make /
generic dispatch (class-ids) and macro-using fixtures (`cond`/`when`/`unless`/
`for` — the Dylan lowering doesn't yet run the macro expander). make/dispatch is
the next big one — its forms are flip-only (class metadata + dispatch resolution)
and need class refs threaded from the Dylan side (it emits names; the
reconstruction resolves them against the live registry).
