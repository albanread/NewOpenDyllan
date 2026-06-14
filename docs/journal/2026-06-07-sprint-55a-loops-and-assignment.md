# 2026-06-07 — Sprint 55a: `while`/`until` loops + `:=` (the env-merge)

*The hardest 55a step: loops bring the env-merge the plan flagged — loop-carried
variables threaded through a header block-param — plus `:=` assignment.
Implemented by a subagent from a precise recipe + exact target dumps, then
reviewed function-by-function and independently byte-verified here.*

## What

- **`:=`** (`lower-assign`): lower the RHS, and if the LHS is a plain env-bound
  variable, **rebind** the name to the RHS temp — emitting *no* computation for
  the assignment itself (SSA rebind, most-recent-wins). Value = RHS temp.
  (`bump`: `let x = n; x := x + 1; x` → just the `AddInt`, then `Return` it.)
- **`while`/`until`** (`lower-loop`): a `loop_header` / `loop_body` / `loop_exit`
  CFG. The **carried (phi) set** = env names assigned via `:=` in the body ∪
  used in cond/body ∪ GC-typed env names, **sorted lexically**. That one order
  governs the header block-params, the entry-edge `Jump` args (pre-loop temps),
  and the back-edge `Jump` args (post-body env temps) — all aligned. `until`
  vs `while` differ *only* in the `If` branch-label order (the cond primop is
  not negated). The loop is valueless; a `#t` **void marker** flows back so
  `lower-function`/`lower-stmt-range` know not to treat it as the return value
  (vs `#f` = bail, `<integer>` = a value).
- Block/temp order is load-bearing: `loop_header` created first (id H); its
  phis consume temp ids *before* the cond lowers; `loop_body`/`loop_exit`
  created *after* the cond (so a short-circuit cond's `sc_*` blocks precede
  them). Plus `collect-used` / `collect-assigned` walks + a local lexical
  string sort (mirroring dylan-sema's `bs-le?`/`sort-strings!`).

## Agent + review

The subagent implemented it against the recipe/targets and iterated to
byte-match. Review (read every new function: `lower-assign`, `lower-loop`,
`collect-used`, `collect-assigned`, the sort/set helpers): correct and
idiomatic — reuses the existing builder API (`fb-new-block` /
`fb-add-block-param` / `fb-terminate-jump` / env), respects the reserved-word
and 8-keyword constraints, and matches the recipe's ordering exactly.
Independently re-verified byte-match (not trusting the summary).

## Verification

`fac` (until + `:=`), `sumto` (while + `:=`), `bump` (bare `:=`) byte-match the
Rust `dump-dfm` (header params sorted `[i, n, result]`; `n` rides as a
self-feeding phi; until polarity `If … loop_exit loop_body`). Committed
`lower-loop.dylan` (all three shapes) added to the gate →
`dylan_lower_phase0_dump_dfm_byte_match` **10 fixtures**, no regressions. The
agent also spot-checked nested `while` (matches) and `for` (bails cleanly to
the Rust path).

## Where it leaves us

The lowering now covers literals, binops, calls, var/param reads, `let`, `if`,
short-circuit, `while`/`until`, and `:=` — the bulk of 55a's control flow, with
the env-merge discipline proven on loops. Note: `if`/short-circuit still bail
to Rust when an arm *assigns* an env var (their join doesn't thread merged vars
yet — only loops do); generalizing that env-merge to `if`/`sc` is the next
cleanup, using the same sorted-carried-set machinery. Then multi-binder `let`,
more call intrinsics, 55b (classes/dispatch), 55c (closures/blocks), and the
structured DFM wire for the load-bearing flip.
