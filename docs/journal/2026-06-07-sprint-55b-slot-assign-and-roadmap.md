# 2026-06-07 — Sprint 55b: slot-`:=` + the design/challenge roadmap

*A small form (`slot(obj) := v`), but the entry that records the
design→challenge→decide pass for the remaining Sprint 55 lowering work, so the
roadmap outlives this session.*

## slot-`:=` (this commit)

`lower-assign` now handles a call LHS: `slot(obj) := v` → `Dispatch
<slot>-setter(obj, value)` (obj lowered first, then value, dst last). This
mirrors Rust `lower_assign`, where `try_resolve_slot_offset` ALWAYS returns
`None`, so a slot assignment is a setter Dispatch — never a `StoreSlot` at a user
site (StoreSlot appears only inside the synthesized accessor body). No class-id
involved; the `<slot>-setter` generic is already in `build-generic-names`.
Committed fixture `lower-slot-assign.dylan` (`counter-val(c) := n` →
`Dispatch counter-val-setter(t0, t1)`); standalone + flip byte-match. Gate → 17
PHASE0. (Class named `<counter>`, not `<cell>` — `<cell>` is a builtin closure
class; redefinition is refused.)

## The roadmap (design + adversarial challenge, decided)

Two agents ran: a design for `define method`/`define generic` lowering, and an
adversarial review. Key outcomes (verified against code):

- **The crux (verified):** class-typed *params* reconstruct as `Class(0)` (the
  dump prints `<class>`, id dropped via `TypeEstimate::name()`), which breaks
  **sealed** dispatch resolution — `is_subclass(ClassId(0), <idler>)` is false,
  so a sealed generic on a param receiver stays `Dispatch` in the flip but is
  `DirectCall g$id` in pure Rust → mismatch. INVISIBLE for open/unsealed
  generics (Dispatch on both sides). So the sealed path (richards-shape) is
  separable from everything else.
- **Gap the challenge caught:** `build-generic-names` collects only slot
  generics; it must also collect `define generic`/`define method` names (mirror
  `collect_generic_names`, lower.rs ~4737), else method calls emit wrong
  DirectCalls. Prerequisite for method work.
- **Method names:** call-site callees come from the Rust-registered
  `body_fn_name` (no parser work needed there); only the method-body *header*
  needs the numeric `g$id_id` scheme. The Dylan side emits it BY NAME
  (`g$<idler>_<integer>`) and `parse_function_header` resolves the `$<class>`
  suffixes via the existing `resolve_class` (same seam/precedent as `make`).
- **Crux fix — decided: B-i (format change), not host re-stamp.** Emit
  `<class:N>` for params/returns (`type_label` in format.rs); Dylan emits
  `<class:name>`, `parse_type` resolves. Rejected the cheaper host-side
  param re-stamp because it has the host re-derive param types from the AST,
  which MASKS Dylan-side param-typing bugs (the gate would pass even if the
  Dylan side emitted the wrong type) — that defeats "Dylan is load-bearing,
  verified byte-identical". B-i keeps the dump the source of truth. Cost: a
  `CACHE_KEY_VERSION` bump + snapshot re-bless; the byte-match gates
  self-consistently survive (both sides reformat through the same formatter).

**Execution order (clean wins first, crux last):** (1) slot-`:=` ✓ [this commit];
(2) `define generic`+`define method` open/unsealed (+ build-generic-names fix +
method-header resolution) → unlocks `richards-shape-open`; (3) `%`-prim → `nod_`
name map (+ a `no_alloc` flag on `<dfm-comp>`) → unlocks `gap-007`/rope/ide;
(4) B-i format change → unlocks sealed `richards-shape` (architect can veto the
format/cache-key change before it lands).

## Verification

Whole-corpus survey 0 mismatches, 24 fixtures Dylan-lowered through the flip.
Both lowering gates + parser round-trip green.
