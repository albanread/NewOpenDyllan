# 2026-06-07 — Sprint 55b: slot-accessor emission (LoadSlot / StoreSlot)

*First 55b increment. `define class` now lowers its synthesized slot accessors
in Dylan, byte-identical to the Rust oracle. This is the slice of 55b that needs
**no** class-ids and **no** post-lowering passes — so it byte-matches the plain
`dump-dfm` text gate directly; the rest of 55b (make / dispatch / `instance?` on
user classes) does not, and is scoped separately below.*

## The key realization

`dump-dfm` reflects the DFM **after** the post-lowering passes —
`resolve_dispatches` (Dispatch → DirectCall/SealedDirectCall) and
`populate_safepoint_roots` (the `safepoint=[…]` annotations) — plus it bakes in
**process-global class-ids** (`make` → `Const ClassMetadataPtr(1081)`). Our Dylan
lowering produces the *pre-pass* DFM with empty safepoints and no ids. So the
text gate only byte-matches where those passes are no-ops and no class-id is
emitted: straight-line integer code (the 55a fixtures) **and slot accessors**.

Slot accessor bodies are exactly that island: a single `LoadSlot`/`StoreSlot`
keyed by a **deterministic offset** (own slot `i` → `@ 8 + 8i`) and a
`SlotTypeKind` derived from the declared type — no call, no GC root, no id. So
accessor emission is a clean, fully-gateable 55b sub-increment.

## What

`dylan-lower-emit` is now two-pass (mirroring Rust Phase 3 → Phase 4):

1. **Pass 1 (accessors).** For every `define class` (source order), for every
   own slot (source order, getter-then-setter), append a `<C>-getter-<slot>`
   (and, unless the slot is `constant`, a `<C>-setter-<slot>`) function. The
   getter is `t1 = LoadSlot t0 @off [kind]; Return t1`; the setter is
   `t2 = StoreSlot t0 @off := t1 [kind]; Return t2`. Offsets `8 + 8i`;
   `kind` = `Integer` for `<integer>`/`<character>` slots else `Object`
   (mirrors `slot_type_to_dfm_kind`); getter return label mirrors
   `slot_type_to_estimate` (`<integer>`, `<string>` from `<byte-string>`,
   `<boolean>`, `<character>`, `<double-float>`, else `<top>`). Slot metadata
   comes from the `<ast-class-definition>` accessors already used by
   `dylan-sema.dylan` (same shim module).
2. **Pass 2 (functions).** User `define function`s as before — but all
   accessors now precede all functions in the dump regardless of source order.

Two safety bails keep the Dylan side aligned with what the Rust oracle can
actually lower (never emit a dump Rust wouldn't):

- **Non-`<object>` super → bail.** Inherited slots would shift own-slot offsets
  off `@8`; only sole-super-`<object>` classes are handled.
- **`constant` slot → bail.** Rust lowering supports only `instance:` allocation
  and *errors* on a `Constant` slot; we'd otherwise emit a getter for a module
  Rust rejects.

And a new **generic/intrinsic call guard** in the call path: a call to a slot
generic (`x(p)`) is a `Dispatch` in Rust, not a `DirectCall`, and `make` /
`instance?` are intrinsics — none are plain DirectCalls. Until those forms land,
such a call bails the whole function to Rust. The builder carries the module's
generic-name set (`fb-generics`, built from the classes' slots) for this. This
is why `point.dylan` (class + `make` + `x(p)` dispatch) still correctly produces
`""` (Rust path) rather than a wrong dump.

## Verification

Byte-identical vs Rust `dump-dfm`: `lower-class-accessors.dylan` (committed,
gate **12 fixtures**) — two classes spanning every `SlotTypeKind`/return-type,
plus a trailing function proving accessors-before-functions. Spot fixtures:
single-slot class, varied-type class (offsets @8/@16/@24/@32), class+function.
Correct **bails** (empty → Rust path): `point.dylan` (make/dispatch), an
inheriting class (offset shift), a `constant`-slot class. Full `sema_topnames`
(4/4) and `dylan_parse_translate` green after the no-shim bootstrap rebuild — no
regressions from the enlarged shim.

## Where it leaves us

The accessor island of 55b is done. The rest of 55b — `make`
(`ClassMetadataPtr` needs the process-global class-id), generic `Dispatch`,
`instance?` on user classes, slot-`:=` (a `<slot>-setter` Dispatch, *not* a
StoreSlot), and the sealed-class resolver + safepoint passes — cannot byte-match
the standalone text gate: it carries class-ids and the post-pass rewrites.
`instance?` on *builtin* classes (`TypeCheck t0 <integer>` — literal label, no
id, no pass) is the one remaining id-free, pass-free piece and is the next
increment. The id-bearing forms want the structured DFM wire (Dylan lowering →
host reconstructs ids by name → host runs the passes), the 55 analogue of 54c.
