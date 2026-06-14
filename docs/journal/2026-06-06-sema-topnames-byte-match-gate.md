# 2026-06-06 — Sema top-names byte-match gate (Sprint 53.2 close)

*The last piece of 53.2: a gate that proves the Dylan-side
`collect-top-names` walk byte-matches the Rust oracle's top-names
section on class-free fixtures. Follows [Sema in Dylan](2026-06-03-sema-in-dylan-53.md).*

## Goal

Lock in the 53.2 Dylan port (`tests/nod-tests/fixtures/dylan-sema.dylan`,
already written and bug-fixed in the empty-table GAP-011 #2 work) with a
regression gate, and reconcile the one fixture that diverged.

## What

- **New gate `tests/nod-tests/tests/sema_topnames.rs`** (`#[ignore]`,
  `#[serial]`), modeled on `sema_self_host.rs`. Builds `dylan-sema.exe`
  once from `dylan-sema.prj`, then per class-free fixture runs the EXE
  (the Dylan walk, which prints *only* the `=== top-names ===` section)
  and the Rust oracle (`nod-driver --parse-with-rust dump-sema`), slices
  the oracle output to the top-names prefix (up to, not including,
  `=== generics ===`), normalizes CRLF/trailing-whitespace identically on
  both sides, and asserts byte-equality. Mismatches print both blocks.
  Gates six fixtures: `factorial`, `sprint09-add`, `mutual`, `hello`,
  `stdlib-size-call`, `kernel-arith`.

- **One-line classification fix in `format_sema_model`** (`nod-sema/src/lower.rs`).

## Why (the kernel-arith divergence)

`kernel-arith.dylan` has `define constant *answer* = 42;`. The Dylan walk
correctly emits a single `constant *answer*` line and **no** `fn` line.
The Rust oracle was emitting `fn *answer* arity=0 return=Top` **as well
as** `constant *answer*` — a spurious extra `fn` line.

Root cause is by-design recording, not a bug in `collect_top_level_names`:
`define constant` / `define variable` names ARE inserted into
`TopNames.fns` (arity 0) because they lower to zero-arg getter functions,
and `Expr::Ident` resolution consults `top_names.contains()` / `.arity()`
to pick the right shape (the GAP-002 path: a bareword constant must
evaluate via a zero-arg DirectCall, not become a function-ref). That
`fns` membership is **load-bearing for codegen** and must stay.

The mismatch was purely in the *dump*: `format_sema_model` iterated all
of `fns`. Fix = filter constant/variable names out of the `fns` listing
**in the dump only**:

```rust
let mut fns: Vec<(&String, &TypeEstimate)> = lm.top_names.fns.iter()
    .filter(|(name, _)| !lm.top_names.constants.contains(*name)
                     && !lm.top_names.variables.contains(*name))
    .collect();
```

`TopNames` population is untouched. Confirmed `LoweredModule.top_names`
is **recording-only**: written once (`top_names.clone()` in
`lower_module_full`), read back solely by `format_sema_model`
(grep of `\.top_names` / `top_names:`). Codegen reads the separate
`top_names` *local* threaded through `LowerCtx`, which this change does
not touch. So this is a pure dump-presentation change with zero codegen
impact — the safer fix than re-bucketing in `collect_top_level_names`
(which would have changed what `.contains()`/`.arity()` see).

## Discovered

- The task framing said the oracle emitted the `fn` line and *no*
  `constant` line. Actual ground truth (run, not assumed): the oracle
  emitted **both** — the bug was the *extra* `fn *answer*`, with the
  `constant` line already correct. Re-confirming by running rather than
  trusting the brief mattered here.
- `hello`'s `main` has no explicit `=>` and both sides say `return=Top`;
  the explicit-type path covers the gated fixtures (body-inference of
  return type is a later concern, not exercised by these six).

## Gates

- `cargo build` — clean (pre-existing warnings only).
- `cargo test -p nod-tests --test sema_topnames -- --ignored --nocapture`
  — **1 passed**, all six fixtures MATCH.
- `cargo test -p nod-sema` — 23 passed. `sema_dump` — 1 passed.
- Full serial sweep `cargo test -p nod-tests --no-fail-fast --
  --test-threads=1` — **0 failed** across all binaries.

## Where it leaves us

53.2 is closed: the Dylan top-names walk is gated byte-for-byte against
the oracle on the class-free corpus, and the constant/variable
classification now agrees between the two implementations. Next is 53.3
(classes + slot accessors): the auto-generated `<C>-getter-x` `fn`
entries that 53.2 intentionally omits, reusing the already-ported C3 for
CPL. Class fixtures join the gate then.
