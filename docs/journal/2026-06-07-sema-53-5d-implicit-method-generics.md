# 2026-06-07 — Sprint 53.5d: implicit generics from bare `define method`

*Closing 53.5b (anon-method lifting) exposed that the 53.5(1) survey had
mis-attributed the `rope` / `ide_rope` / `unified_ide` divergence: those
three carry two further gaps beyond anon-methods. This entry closes the
first — implicit generics — and pins the second down to a single line.*

## Goal

After 53.5b the `__anon-method-N` lines matched on all four rope-family
fixtures, but `rope` / `ide_rope` / `unified_ide` still diverged on a block
of `generic <name>` lines: `for-each-leaf`, `rope-concatenate`,
`rope-element`, `rope-size`, … — every name carried by a bare `define
method` with no explicit `define generic`. Close it.

## What

The Rust oracle's `collect_generic_names` (`nod-sema/src/lower.rs`) seeds
the generics set from three sources:

- `Item::DefineGeneric { name }` — explicit generics;
- `Item::DefineMethod { name }` — **every** method name (a bare method
  implicitly defines a generic of its name);
- `Item::DefineClass` — slot accessors (`<slot>` getter, `<slot>-setter`).

The Dylan walk (`collect-top-names` in
`tests/nod-tests/fixtures/dylan-sema.dylan`) already reproduced the first
and third. It was missing only the second. One branch added: a `define
method` item now inserts its `defn-method-name` into `generics`, deduped
against the explicit / slot generics via `bs-member?`. (Lifted
`__anon-method-N` thunks are `DefineFunction`s, not methods, so they are
correctly excluded — they never become generics on either side.)

No `fn` line is emitted for a method — that was already correct (53.4
dropped the spurious method `fn` line); this change touches only the
`=== generics ===` section.

## Why it can't gate the three fixtures yet

After this change the rope family is **one line** from byte-identical:

```
< fn empty-rope arity=0 return=Top          (Dylan walk)
> fn empty-rope arity=0 return=Class(1082)   (oracle)
```

`empty-rope () => (r :: <rope-leaf>)` returns a user class. The oracle's
`type_from_expr` resolves `<rope-leaf>` to its registered class-id (1082)
and `format_sema_model` prints it through `TypeEstimate`'s `Debug` —
`Class(1082)`. The Dylan walk has no runtime class-id, so it emits `Top`.

Two things are true here, and both point away from chasing the id:

1. **It's the Sprint 54 problem.** Reproducing `Class(1082)` in the Dylan
   walk means reproducing the runtime's class-id assignment — exactly the
   class-id-determinism keystone the 54–56 roadmap calls out.
2. **The raw id is a portability leak in the oracle itself.** Everywhere
   else, `format_sema_model` refers to classes *by name*
   (`sema_class_name(id)` for parents / cpl / slot origin / sealing) —
   precisely because (53.1) "ids are process-global". The `return=Class(id)`
   line is the one place a raw id escapes into the supposedly-stable dump,
   so the oracle's own output is non-deterministic across builds that
   register classes in a different order. The clean fix is to render the
   return class **by name** on both sides (`Class(<rope-leaf>)`), not to
   teach the Dylan walk the id. That is a change to the oracle's dump
   contract, so it's surfaced for decision rather than slipped in here.

So the three stay ungated, now blocked by this single, well-understood line.

## Verification (re-run, not trusted)

- Rebuilt `dylan-sema.exe`; diffed against `--parse-with-rust dump-sema`:
  `rope` / `ide_rope` / `unified_ide` now differ by exactly the one
  `empty-rope` return line; `nod-ide` remains a full match.
- `sema_topnames` (`--ignored`): **35/35 MATCH**, 1 passed, 0 failed — no
  regression. The change only adds method-name generics, deduped; every
  already-gated fixture's method names were already covered by an explicit
  generic or a slot accessor (else it would have been diverging, not
  gated), so the pre-existing 34 are unaffected.
- `sema_self_host` (`--ignored`): 1 passed.

Dylan-only fixture change (+ a doc-comment edit in `sema_topnames.rs`): the
full Rust sweep is correctly skipped.

## Where it leaves us

Two of the three rope-family divergences are now closed (anon-methods in
53.5b, implicit method generics here); the third is a single
`return=Class(id)` line that wants either the Sprint 54 class-id work or a
small by-name fix to `format_sema_model`. After that the rope family gates
and the only sema work left before the load-bearing `--sema-with-dylan`
step is whatever the broader corpus still surfaces.
