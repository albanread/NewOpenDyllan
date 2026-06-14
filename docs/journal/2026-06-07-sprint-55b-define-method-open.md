# 2026-06-07 — Sprint 55b: `define method` / `define generic` (open scope)

*The method machinery. `define generic` + `define method` now lower Dylan-side
(open/unsealed scope), unlocking `richards-shape-open`. Implemented per the
decided roadmap; two blockers the design didn't foresee surfaced and were closed
conservatively.*

## What

- **`build-generic-names`** now also collects `define generic` and `define
  method` names (mirrors Rust `collect_generic_names`, lower.rs ~4737). LOAD-
  BEARING: when the Dylan lowering runs, the host hasn't registered the module's
  own generics yet, so `%is-generic?` returns false for them — a same-module
  method call must be recognized as a Dispatch from the AST, else it emits a
  wrong DirectCall.
- **`define generic`** → no-op (emit nothing, don't bail); the host registers it
  from the AST. **`define method`** → `lower-method`.
- **`lower-method`** = the shared `lower-defn-body-into` (extracted from
  `lower-function`) plus the method-body NAME: `{generic}${spec0}_{spec1}_…`
  where each spec is the required param's specialiser **class name** (`<object>`
  for an unannotated required param). Emitted BY NAME (`run-task$<idler>_<integer>`)
  — the Dylan side can't know ids at lowering time. Bails if any specialiser
  isn't a bare class-name ref (singleton/union/expr).
- **`parse_function_header`** (parse.rs) resolves the by-name suffix to the
  numeric Rust scheme (`run-task$1082_1`): split on the first `$`, resolve each
  `<…>` token via the existing `resolve_class` at the reconstruction seam (same
  precedent as `make`'s `ClassMetadataPtr`-by-name). Numeric/regular names pass
  through unchanged (round-trip preserved). Call sites need no change — a
  resolved callee comes from the Rust-registered `body_fn_name`.

## Two blockers the roadmap missed (both needed for richards-shape-open)

1. **List builtins.** `head`/`tail`/`empty?`/`pair`/`nil` → `%pair-head`/
   `%pair-tail`/`%empty?`/`%pair-alloc`/`%nil` DirectCalls (mirrors Rust
   `lower_list_builtin`; arity-checked, correct dst types). `visit-list`/`main`
   use them; without this they emitted wrong `DirectCall head(...)`. This is a
   specific 5-name set, distinct from the deferred general `%`-prim/
   LOWER_PRIMITIVE_TABLE map.
2. **Zero-inherited-slot superclasses.** `<idler> (<task>)` where `<task>` is
   empty. `class-is-simple?` now accepts a single-inheritance chain of module
   user classes that contribute ZERO inherited slots (own slots still start
   @8), via a recursive `class-inherited-slot-count`. Conservatively bails on
   MI, builtin/unknown supers, and any slot-bearing super (we don't reimplement
   the runtime's most-specific-first inherited-slot layout).

## Sealed stays bailing (the crux, deferred)

`richards-shape` (sealed) differs from `-open` only by the `sealed` modifiers. A
`define sealed generic` on a class-typed PARAM receiver resolves to
`DirectCall g$id` in pure Rust but stays `Dispatch` in the flip — because the
param reconstructs as `Class(0)` (the dump drops the id) and the resolver can't
match. So sealed is guarded out (`has-sealed-modifier?` bails sealed
generic/method/class). It needs the `<class:N>` format change (the decided B-i),
the next increment.

## Verification

Independent whole-corpus survey: **26 fixtures Dylan-lowered through the flip, 0
mismatches** (richards-shape-open BAIL→MATCH; `richards-shape` correctly BAILS,
not mismatches). `nod-dfm` 13/13 (new `method_body_name_suffix_resolves` test),
`sema_topnames` 6/6, `codegen` 8/8. Committed fixtures: `lower-method-open.dylan`
(generic + 2 methods); `richards-shape-open` + `lower-method-open` added to
`FLIP_ONLY_LOWER_FIXTURES`.
