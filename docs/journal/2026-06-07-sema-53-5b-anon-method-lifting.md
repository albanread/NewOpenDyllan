# 2026-06-07 — Sprint 53.5b: anonymous-method lifting (`__anon-method-N`)

*53.5(1) broadened the Dylan-side sema byte-match gate to 33 corpus
fixtures and scoped the last two divergences; 53.5c closed the
body-shaped-statement-macro one. This entry closes the other — anonymous
method lifting — by teaching the Dylan sema walk to lift `method (…) …
end` literals to synthetic `__anon-method-N` top-level functions, the
same way the Rust lowering pre-pass does.*

## Goal

The Rust lowering pre-pass `lift_anonymous_methods`
(`src/nod-sema/src/lower.rs`) rewrites every `method (…) … end` literal in
**expression position** into a synthetic top-level `define function` named
`__anon-method-N`, where `N` is a process counter incremented once per
literal during a depth-first, source-order walk of the module. Those
synthetic functions land in `top_names.fns` (arity = the literal's
parameter count, return = `Top` — the lifter always sets `return_: None`),
so they surface as `fn __anon-method-N arity=A return=Top` lines in
`dump-sema`. The Dylan walk (`collect-top-names` in
`tests/nod-tests/fixtures/dylan-sema.dylan`) only scanned **top-level**
items, never expression positions, so it dropped these lines entirely.
Close the divergence and gate the fixtures that need it.

## What

A lift pre-pass added to `dylan-sema.dylan`, mirroring the Rust traversal:

- `collect-anon-methods (items, source, fns)` — walks the top-level items
  in declaration order. Descends `define function` / `define method`
  bodies and `define constant` / `variable` initialisers; **skips** class
  supers / slot defaults and generic signatures, exactly as `lift_item`'s
  match arms do.
- `lift-anon-node` / `lift-anon-body` / `lift-anon-for-clause` — the
  recursive walk. It mirrors `dump-node`'s child-visit order
  (`dylan-parser.dylan`), which is the same source order the Rust
  `lift_statement` / `lift_expr` see. On an anonymous method literal — an
  `<ast-statement>` whose `stmt-word` is `method`/`function` and whose
  `stmt-method-name` is `#f` — it emits the next `__anon-method-N` **before**
  descending the literal's body (pre-order: a parent is numbered before the
  methods nested inside it), so sibling literals number left-to-right and
  nested ones get higher indices. `local method` bodies are skipped (Rust's
  `Statement::Local` binds the names but does not lift their bodies).

The counter is a one-element `<stretchy-vector>` threaded through the
recursion as a mutable box (two top-level functions cannot share a mutable
`let`, so the count must live on the heap).

Gate: `nod-ide` added to `FIXTURES` in
`tests/nod-tests/tests/sema_topnames.rs` (now **35**).

## The arity / order proof

Ground truth, from `--parse-with-rust dump-sema`:

| fixture | literals | `__anon-method-N arity` |
|---|---|---|
| `rope`, `ide_rope` | 1 | `0:1` |
| `unified_ide` | 5 | `0:1 1:0 2:1 3:4 4:4` |
| `nod-ide` | 4 | `0:0 1:1 2:4 3:4` |

Every literal in these fixtures sits in one of two positions — a call
argument (`for-each-leaf(r, method (leaf-bytes) … end)`) or a `let` value
(`let wp = method (hwnd, msg, wparam, lparam) … end`) — so the indices
follow plain source order, and the Dylan walk now reproduces all of them
byte-for-byte (verified: the `__anon-method-N` lines are identical on both
sides for all four fixtures).

## The survey was wrong about three of the four

53.5(1) attributed the `rope` / `ide_rope` / `unified_ide` / `nod-ide`
divergence **solely** to anon-method lifting. Ground truth says otherwise:
only `nod-ide` diverged on anon-methods *alone*, and it now byte-matches
end-to-end and is gated. The other three carry **two further, independent
gaps** the anon-method work does not touch, so they stay ungated:

1. **Implicit generics from bare `define method`.** The oracle records a
   `generic <name>` entry for every `define method` name (`rope-size`,
   `for-each-leaf`, …); the Dylan walk only emits generics from `define
   generic` + slot accessors. This is exactly the DEFERRED note already
   sitting in `collect-top-names` — its own future step (tractable: collect
   method names, dedupe, sort).
2. **User-class return estimates.** `empty-rope () => (r :: <rope-leaf>)`
   dumps `return=Class(<id>)` in the oracle (the registered class-id of
   `<rope-leaf>`) vs `return=Top` here. Reproducing `Class(<id>)` needs the
   runtime class-id replicated in the Dylan walk — the **Sprint 54
   class-id-determinism** problem flagged as the migration keystone, not an
   anon-method concern.

This is, again, the recurring lesson: ground the brief by running both
sides, don't trust a prior survey's attribution. The clean scoping holds —
53.5b is anon-methods, full stop; the two new gaps are their own work.

## Verification (re-run, not trusted from the build summary)

- `sema_topnames` (`--ignored`): **35/35 MATCH**, incl. `nod-ide`; 1 passed,
  0 failed. The `__anon-method-N` lines match for `rope` / `ide_rope` /
  `unified_ide` too (those fixtures still differ only on the two gaps above).
- `sema_self_host` (`--ignored`): 1 passed (`dylan-sema.exe factorial.dylan`
  still exit 0 with the expected two-function table — no anon methods, no
  regression).
- The 34 previously-gated fixtures stayed green — none contains an anonymous
  method (if one had, it would already have been in the diverging set), so
  the pre-pass is a no-op for them.

Dylan-only fixture change (+ a test-only edit to `sema_topnames.rs`): the
full Rust sweep is correctly skipped; the two sema gates above are the
relevant guards.

## Where it leaves us

The Dylan sema recording model now reproduces anonymous-method lifting in
lockstep with the Rust oracle, gated on `nod-ide`. The remaining sema
divergences are the two scoped gaps on the rope family (implicit method
generics; user-class return = `Class(id)`), the latter dovetailing with the
Sprint 54 class-id work. After those, the load-bearing step is the
`--sema-with-dylan` + verify-mode integration (retire the oracle crutch),
then Sprint 54 (`lower_with_model`).
