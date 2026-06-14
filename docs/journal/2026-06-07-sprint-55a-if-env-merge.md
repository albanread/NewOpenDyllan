# 2026-06-07 — Sprint 55a: generalize the `if` env-merge

*Adding `:=` (in the loops increment) exposed two latent `if` bugs, both
confirmed against Rust `dump-dfm` and fixed here by giving `if` the same
env-merge discipline the loops already use.*

## The two bugs (confirmed, not theoretical)

1. **Assigned vars weren't threaded.** `if (x>0) y := 1 else y := 2 end; y`:
   Rust threads `y` through the join (`Jump join3(t4, t4)` / `join3(t6, t7)` /
   `Return t7`); the old `if` dropped it and returned the stale arm temp →
   wrong output. (Latent because no gate fixture assigned in an arm — but a
   real miscompile once `:=` existed.)
2. **Join created before the arms.** A nested `if` in an arm: Rust creates the
   outer join AFTER the arm's blocks (GAP-010), so its id is highest; the old
   `if` created the join up front, so block ids/order diverged.

## The fix

`lower-if-expr` now mirrors `lower_if` fully:
- **Merge set** = vars assigned in either arm (`collect-assigned`) ∪ GC-typed
  env names, sorted lexically. Param order = jump-arg order = **value first,
  then merge vars**.
- **Snapshot/restore env** around the arms (the else arm starts from the pre-if
  bindings; each arm captures its own merge temps).
- **Join created AFTER both arms**; each arm's END block (which may differ from
  then/else if the arm branched) jumps to it with `[value, merge…]`.
- At the join: value param first, then a param per merge var (type = lattice
  join of the two arms' temps); env restored to pre-if then merge vars rebound
  to the join params (evicting arm-local lets).

This also removes the old "bail on GC-typed env" guard — GC-typed env vars are
now threaded (added to the merge set), matching Rust.

## Verification

`ifasg` (assignment in both arms) and `ifnest` (nested if in an arm) now
byte-match. Committed `lower-if-merge.dylan` (both shapes) added to the gate →
**11 fixtures**, no regressions (the simple-arm fixtures have an empty merge set
and single-block arms, so their output is unchanged).

## Where it leaves us

`if` is now fully general (assignments, nesting, GC-typed env). Short-circuit
(`|`/`&`) still has the analogous latent shape (it creates its join up front
and doesn't thread merged vars) — correct for the common simple-RHS case (the
gate), wrong for RHS-assignment / nested-RHS. Generalizing short-circuit the
same way is the next step; then multi-binder `let`, more call intrinsics, and
55b (classes/dispatch).
