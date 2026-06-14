# 2026-06-07 — Sprint 55a: `if` — the block-parameter SSA core

*The first control flow, and the form the Sprint 55 plan flagged as the
brutal core: `if` lowers to a block-parameter-SSA diamond where every block
id/label, temp id, and the join merge-param must reproduce the Rust emission
order exactly. It now byte-matches — and unlocks recursion (`factorial`).*

## What

`lower-if-expr` mirrors `lower_if`'s value-merge (non-mutating) case: a 3-block
diamond — `then` / `else` / `join` — created in id order (labels `then<id>` /
`else<id>` / `join<id>`), with the merged value as the single `join`
block-param. Emission order reproduces Rust: cond temps (entry) → then-arm
temps → else-arm temps → join param. A missing `else` synthesizes `Const
Bool(false)`; the join param type is the lattice join of the arms (equal →
that type, else `<top>`). The `if` is an expression — its value is the join
param, and lowering continues in the `join` block, so `let y = if … end; y * 2`
lowers the trailing `y * 2` into `join` (matching Rust exactly).

To stay correct without the full env-merge yet, `lower-if-expr` **bails to the
Rust path** on: any GC-typed binding in the enclosing env (so reassigned /
heap values that would need threading through join params don't), `elseif`
chains, and any unsupported arm (e.g. `:=`, which isn't lowered, bails
naturally). Integer-only `if`s — the common case — need no env threading, so
they reproduce exactly.

## Two bugs the build caught (both pre-`dump-dfm`, at shim compile)

1. **`cond` is a reserved keyword-token.** I named the condition temp `cond`;
   the parser treats `cond` as the `cond`-macro keyword and skipped to the next
   `define` ("unexpected KwDefine"). Renamed to `cnd`. (Lesson: avoid bare
   stdlib-macro / control-word names — `cond`/`when`/`block`/`select`/… — as
   identifiers in the self-hosted sources.)
2. **`make` supports ≤8 keyword pairs.** Adding `term-a`/`term-b`/`term-args`
   to `<dfm-block>` pushed its `make` to 9 keywords ("Sprint 12 supports up to
   8 keyword pairs, got 9"). Factored the terminator into a separate
   `<dfm-term>` object (5 keywords) held by one `block-term` slot — also keeps
   every slot supplied at `make` time, sidestepping the slot-default GAP.

## Verification

`if1` (`if (x>0) 1 else 2 end`), `if2` (if as a `let` value with arithmetic in
each arm + a trailing `* 2` in the join), and `if3` (no else → `Const
Bool(false)`, `<top>` join) all byte-match Rust. The gate
`dylan_lower_phase0_dump_dfm_byte_match` grew to **8 fixtures**: + `lower-if`
(committed), and two real corpus fixtures `if` unlocked — **`factorial`**
(recursion + `if`) and **`jit_cache_sample_items`**. No regressions.

## Where it leaves us

The hardest single form is done; the byte-exact block-param SSA discipline
holds. Remaining 55a: short-circuit `|`/`&` (same diamond shape with a
trampoline edge), `while`/`until` loops (header/body/exit with a back-edge —
brings in the env-merge for loop-carried vars + `:=`), multi-binder `let`, more
call intrinsics. The GC-typed-env merge threading (deferred here) lands with
loops, since loop-carried GC values must thread through the header block param.
Then 55b (classes/dispatch), 55c (closures/blocks), and the structured DFM wire
for the load-bearing flip.
