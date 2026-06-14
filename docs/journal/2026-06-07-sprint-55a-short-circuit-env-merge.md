# 2026-06-07 — Sprint 55a: generalize the short-circuit env-merge

*The short-circuit twin of the `if` env-merge fix. `|`/`&` had the same latent
shape — join created up front, merged vars not threaded — correct for a simple
RHS but wrong for an RHS that assigns or itself branches. Now generalized to
match Rust's `lower_short_circuit`.*

## What

`lower-short-circuit` now threads the env-merge like `if`:
- **Merge set** = vars assigned in the RHS (`collect-assigned`) ∪ GC-typed env
  names, sorted; join-param / jump-arg order = value first, then merge vars.
- The **`sc_edge`** arm (short-circuit taken, RHS not evaluated) jumps with the
  **pre-RHS** merge temps (captured before the RHS lowers); **`sc_rhs`** jumps
  with the **post-RHS** temps.
- The **join is created AFTER the RHS** (so a nested-short-circuit / control-flow
  RHS gets the right block ids). The RHS is an expression (no `let`s to evict),
  so unlike `if` no env snapshot/restore is needed — only the merge vars change,
  and they're rebound to the join params at the join.
- Removed the old bail-on-GC-typed-env guard (GC vars are threaded now).

`env-has-gc-typed?` is now unused (both `if` and short-circuit thread instead of
bailing); left in place as harmless, removable dead code.

## Verification

Byte-identical vs Rust: `a | b`, `a & b` (simple — regression), `a | (x := 5)`
(RHS assignment threaded), `a | (b & c)` (nested short-circuit, join-after-RHS
ordering). The committed `lower-shortcircuit.dylan` grew to cover all five
shapes; the gate holds at **11 fixtures**, no regressions.

## Where it leaves us

All three diamond/loop control-flow forms — `if`, short-circuit, and
`while`/`until` loops — now carry the full env-merge (assigned-var threading,
GC-typed threading, correct join-after-arms ordering). 55a's hard control-flow
is done. Remaining 55a: multi-binder `let`, more call intrinsics (`%`-prims,
`instance?`, `make`-shaped). Then 55b (classes / dispatch — `LoadSlot` /
`StoreSlot` / `Dispatch`), 55c (closures / blocks), and the structured DFM wire
for the load-bearing flip.
