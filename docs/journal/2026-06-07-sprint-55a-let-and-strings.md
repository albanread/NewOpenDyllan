# 2026-06-07 — Sprint 55a (first forms): `let` bindings + string-debug escaping

*Phase 0 proved the Dylan AST→DFM lowering byte-matches on the simplest
functions. 55a grows it form by form, each re-greening the `dump-dfm` gate
before the next. This entry adds local `let` bindings and fixes string-literal
rendering — two small forms, both confirmed byte-identical.*

## What

- **`let` bindings.** `lower-function` now lowers a *sequence* of body
  statements (not just one), the last statement's value being the return.
  `lower-let` handles `let binder = init`: lower the init, bind the binder name
  to its temp in the `LocalEnv`. A non-captured `let` is exactly that — a
  name→value-temp binding, **no extra computation** (cell promotion for
  captured lets is 55c) — so `let y = x + x; y` lowers to `t1 = AddInt t0 t0;
  Return t1`, byte-identical to Rust.
- **String-debug escaping.** `format-dfm`'s `String(…)` const rendered the raw
  decoded bytes; Rust's `format.rs` uses `{:?}` (str Debug). Added
  `escape-string-debug` matching it: `"`/`\` backslash-escaped, `\n`/`\t`/`\r`
  letter escapes, printable ASCII through, else `\u{<hex>}`. This was the *only*
  difference on `hello` (`String("hello\n")`), which now matches.

## Verification (gate, re-run)

`dylan_lower_phase0_dump_dfm_byte_match` grew to **5 fixtures**, all
byte-identical (`dump-dylan-dfm` vs `dump-dfm`):
- `sprint09-add`, `mutual` (Phase 0), now **`hello`** (string + call) and
  **`gap011-jcs-min-crash`** (40 functions, chained direct calls) — both
  unlocked this pass;
- **`lower-let`** — a new dedicated fixture (chained `let` + arithmetic), since
  no corpus fixture exercises `let` without also using control flow / classes /
  primitives.

All 4 `sema_topnames` tests green (the lowering gate + the three sema gates).

## Discovered

- The `let` lowering was easy to *write* but the oracle still earned its keep:
  the first synthetic `let` smoke fixture failed on *both* sides because it
  omitted the blank line after `Module:` — `scan_preamble` keeps consuming
  lines as preamble continuations until a blank line, so the `define function`
  got swallowed (a Rust parse error, not a lowering bug). The committed
  `lower-let.dylan` has the blank line. Iterating on fixtures needs no shim
  rebuild — only the Dylan-side lowering does — so fixture shapes are cheap to
  probe.

## Where it leaves us

The lowering handles straight-line functions with literals, binops, direct
calls, var/param reads, and `let`. Next in 55a is the first **control flow**:
`if` — the block-parameter SSA + deterministic sorted env-merge that is the
brutal core of the port, where every temp/block id and merge-param order must
reproduce exactly. Then short-circuit `|`/`&`, `while`/`until`, multi-binder
`let`; then 55b (classes/dispatch), 55c (closures/blocks), and the structured
DFM wire for the load-bearing flip.
