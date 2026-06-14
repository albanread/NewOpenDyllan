# 2026-06-10 — Sprint 56c-T: the methods table becomes a checked input

*Second step of the cutover roadmap (after 56b-EXPAND). The Dylan AST→DFM
lowering now EMITS a `=== methods ===` section, verified against the Rust
`methods` table on the `--lower-with-dylan` path — making the implicit
method-body-name coupling an explicit, load-bearing invariant. Verify-only;
agent-implemented from a precise spec, then independently checked. The
precondition for consuming the methods table (retiring Rust's
`register_methods`-fed build).*

## What we did

- **Dylan side** (`dylan-lower.dylan`): `dylan-lower-emit` accumulates a methods
  list during the lowering walk and appends a `=== methods ===` section after
  the function dump (only when `all-ok?`; a bail still returns `""`). Two
  sources, in Rust walk order: pass-1 slot accessors (per own slot, getter
  `method <slot> body=<C>-getter-<slot> params=1 specialisers=[<C>]` then, iff
  setter, `<slot>-setter body=<C>-setter-<slot> params=2 specialisers=[<C>,
  <object>]`), then pass-2 user `define method`s (generic / body / param-count /
  specialiser class-names). `method-specialiser-names` mirrors `method-body-name`'s
  required-param derivation.
- **Host side** (`lower.rs`): at the dfm-dump seam, `split_once("\n=== methods
  ===\n")` peels the methods section off so `parse_dfm_module` only sees the
  functions; `parse_dylan_methods` reconstructs `Vec<ParsedMethod>`;
  `verify_dylan_methods` (mirrors `verify_dylan_classes`) asserts same count +
  order, equal generic_name / body_fn_name / param_count, and specialisers
  compared BY NAME (Rust `ClassId`s via `sema_class_name`). A mismatch fails the
  compile loudly. Plus 6 pure-Rust parser unit tests.

## The one real finding (byte-match)

User-method `body_fn_name` is encoded differently on the two sides: Rust bakes
the **numeric ClassId** into the suffix (`run-task$1082_1`), while the Dylan side
emits **by name** (`run-task$<idler>_<integer>`) — the established by-name
convention that `parse_function_header` resolves at the reconstruction seam. So
`verify_dylan_methods` canonicalises the Rust body name to the by-name form
(`expected_dylan_body_fn_name`: when it starts with `<generic>$`, rebuild the
suffix from the specialiser names joined by `_`) before comparing. Accessor body
names (`<C>-getter-<slot>`) already matched byte-for-byte on both sides.

## Verification

- `cargo test -p nod-sema sprint56` → 11/11 (5 class + 6 method parser tests).
- Independently re-checked the byte-match (the verify runs internally, so a
  mismatch makes `--lower-with-dylan dump-dfm` FAIL): `point`, `richards-shape`,
  `richards-shape-open`, `lower-method-open`, `translate-class`,
  `gc_precise_two_makes`, `lower-class-accessors` all exit 0 + byte-match plain
  `dump-dfm`; controls `factorial`/`hello`/`macros-unless` unregressed.
- Non-vacuous: `richards-shape`'s emitted section is 8 accessors (getter+setter
  per class, walk order) + 4 `run-task` user methods (source order) — real
  multi-method data.

## Where it leaves us

The methods table is now a checked, by-name input on the load-bearing path — the
56a pattern extended to the lowering wire (classes were on the sema wire). Next
on the cutover path: CONSUME the methods table (build `methods` from the Dylan
section instead of from the AST, under the combined flag), and the class-table
wire growth + consume. Then the `--frontend-with-dylan` flag that skips Rust
Phase-3/4 for covered modules.
