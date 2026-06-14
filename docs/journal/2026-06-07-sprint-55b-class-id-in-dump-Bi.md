# 2026-06-07 — Sprint 55b (B-i): class ids in the DFM dump → sealed dispatch flips

*The decided fix for the sealed-dispatch crux. The DFM dump now carries class
ids on function params/returns/block-params (`<class:N>`, was the id-dropping
`<class>`), making the dump lossless for class-typed temps. With that,
`richards-shape` (sealed generic + methods on class-typed param receivers) lowers
in Dylan and flip-matches — the first sealed program through the flip.*

## Why (the crux, recap)

Params were formatted via `TypeEstimate::name()` → `<class>` (id dropped) →
reconstructed as `Class(0)`. The dispatch resolver (host-side, post-reconstruction)
couldn't match a sealed method to a `Class(0)` receiver (`is_subclass(0, <idler>)`
is false), so a sealed dispatch that pure-Rust resolves to `DirectCall g$id`
stayed `Dispatch` in the flip → byte mismatch. (Invisible for open generics —
Dispatch on both sides.) Two fixes were weighed; we chose **B-i** (format change)
over a host-side param re-stamp, because the re-stamp would have the host
re-derive param types from the AST — masking Dylan-side param-typing bugs and
defeating "Dylan is load-bearing, *verified* byte-identical". B-i keeps the dump
the source of truth.

## What

- **`format.rs`**: params (L58), return (L60), block-params (L70) now use
  `type_label(...)` (which renders `Class(id)` → `<class:N>`, `Singleton` →
  `<singleton:0xN>`, else `name()`) instead of `.name()`. The dump is now lossless
  for class-typed temps.
- **`parse.rs` `parse_type`**: a `<class:...>` with a NON-numeric payload (e.g.
  `<class:<idler>>`) resolves the inner class name via a `resolve_class` closure →
  `Class(id)`. Threaded `resolve_class` through `parse_type` → `parse_temp_decl` /
  `parse_temp_decls` / `parse_block_header` / `parse_function_header`. New unit
  test `class_type_by_name_resolves`. (Numeric `<class:N>` still parses directly;
  an unresolvable name is a hard error.)
- **`dylan-lower.dylan` `label-for-type-name`**: a user class (AST set) or a
  registered builtin class (`%is-class?`) now emits `<class:<NAME>>` (by name);
  `parse_type` resolves it at the seam → `Class(id)` → reformats to the numeric
  `<class:N>` = byte-identical to Rust. Removed the `has-sealed-modifier?` bail
  (and the function): sealed generics/methods/classes now lower (the crux is
  fixed). Other bails (MI / slot-bearing supers / constant slots / non-bare-class
  specialisers / `%`-prims) kept.
- **`nod-llvm/src/cache.rs`**: `NOD_RUNTIME_ABI_VERSION` 2→3 — the dump (= the JIT
  cache key, `format_for_cache_key`) changed, so the same source hashes to a new
  key; the bump makes invalidation of pre-B-i cached objects explicit.
- **Gate re-classification**: a fixture with a class-typed param/return now dumps
  `<class:N>` (by id) via Rust, but standalone `dump-dylan-dfm` emits
  `<class:<name>>` (by name) — they reconcile ONLY through the flip (which
  resolves the name). So `translate-class` + `lower-slot-assign` move PHASE0 →
  `FLIP_ONLY_LOWER_FIXTURES`, and `richards-shape` joins it.

## Verification

Whole-corpus survey: **0 mismatches**, 27 fixtures Dylan-lowered through the flip
(richards-shape BAIL→MATCH). `richards-shape` `dump-dfm` byte-equals
`dump-dfm --lower-with-dylan`. `nod-dfm` 14/14 (incl. the new test + the existing
round-trips, which now exercise `<class:5>` on params), `sema_topnames` 6/6,
`codegen` 8/8, `nod-runtime` 144/144 (`-j1`). No hardcoded-`<class>` snapshots
needed re-blessing (the byte-match gates reformat both sides through the same
formatter, so they self-survive).

## Where it leaves us

The Dylan front end is now load-bearing through lowering for sealed AND open class
programs (richards both ways). Remaining: the `%`-prim → `nod_` name map (unlocks
`gap-007` + helps rope/ide, which mix `%`-prims with the now-working sealed
methods) and 55c closures/blocks. Pre-existing note: `cargo test -p nod-llvm`'s
*test* code is stale (`DirectCall` literals miss the Sprint-48 `is_no_alloc`
field) — unrelated to this change (confirmed on the pristine tree).
