# Dylan Sema Wire Format — Sprint 53

Sibling of [`DYLAN_TOKEN_WIRE.md`](DYLAN_TOKEN_WIRE.md) and
[`DYLAN_AST_WIRE.md`](DYLAN_AST_WIRE.md). The contract by which the Dylan
front-end hands its computed **sema recording model** (`SemaModel`) back
to the Rust host, so the host's `lower_with_model` (Sprint 54 — the
DFM/CFG construction) consumes a Dylan-computed model instead of
recomputing it.

Drafted before either side's wire code (the verify path — `dump-sema`
byte-match — lands first and needs no wire). Status: **design**; the
emit/reconstruct code is Sprint 53.5.

---

## 1. What the model is

`SemaModel` is the *recording* half of `nod-sema`'s `lower_module_full`
(see `lower.rs`): the exhaustive record of what one module declares, which
every later phase reads instead of re-poking the AST. Four parts (the
four `dump-sema` sections):

- **top-names** — top-level `define function`/`method`/`constant`/
  `variable` names, each with arity and a return-type estimate; plus the
  constant/variable sets (for bareword-vs-call lowering).
- **generics** — the generic-function name set (incl. auto-generated slot
  accessors' generics).
- **classes** — per `define class`: name, parents, C3-linearised CPL,
  slot layout (name, byte offset, has-setter, origin class), own/inherited
  slot counts, sealed flag. (C3 is already Dylan — `dylan-c3-smoke.dylan`.)
- **sealing** — sealed classes, sealed generics, sealed `domain` tuples.

## 2. Verification is via `dump-sema`, not the wire

The byte-identical oracle is the textual `dump-sema` dump
(`nod_sema::format_sema_model`), NOT a binary diff of the wire. The Dylan
side must produce a `SemaModel` whose `dump-sema` rendering is
byte-identical to the Rust side's. This reuses the token/AST verify
discipline: two implementations, one deterministic text dump, byte-compare.

**Names, not ids.** Unlike the token/AST wires (spans not values), the
sema dump references classes by **name**, because class ids are assigned
from a process-global counter and will NOT match across the Rust and
Dylan implementations. The cross-impl-stable invariants are: names, slot
**offsets**, CPL **order**, and the flag sets. The dump (and any future
wire) keys on those; numeric class ids are deliberately omitted from the
comparison.

## 3. Calling convention (integration, Sprint 53.5)

```c
// Exported by the sema shim, alongside dylan-expand-source / dylan-parse-emit.
uint64_t dylan_sema_emit(uint64_t expanded_source_bs);
```

The Dylan front-end already holds the post-expansion AST (Sprint 52). It
runs the sema recording walk Dylan-side and serialises the `SemaModel`
into a `<stretchy-vector>` of fixnums the host reconstructs — same shape
as the AST wire. Records are packed by section; every name is a
**(span_lo, span_hi)** pair into `expanded_source_bs` (the host slices the
name out), so the wire carries spans, not interned strings.

### 3.1 Record layout (per section)

```
top-name:   kind(0=fn|1=const|2=var) · name_lo · name_hi · arity · return_estimate
generic:    name_lo · name_hi
class:      name_lo · name_hi · sealed · n_parents · n_cpl · n_slots
            then n_parents × (parent_name_lo · parent_name_hi)
            then n_cpl     × (cpl_name_lo · cpl_name_hi)            // C3 order
            then n_slots   × (slot_name_lo · slot_name_hi · offset · has_setter · origin_name_lo · origin_name_hi)
sealing:    kind(0=class|1=generic|2=domain) · name_lo · name_hi · [n_specialisers · (spec_name_lo·spec_name_hi)…]
```

`return_estimate` is the `TypeEstimate` discriminant (the small lattice:
Top/Bottom/Integer/SingleFloat/DoubleFloat/Character/Boolean/String/…),
stable by ordinal. A leading header record carries the four section
counts so the host knows how many records of each to read.

> Why spans for names but a discriminant for `return_estimate`: names are
> source text (the host already holds the source); type estimates are a
> closed enum with no source span, so they cross as their stable ordinal.

## 4. Composition

Multi-file builds **compose at the AST** — the ASTs are concatenated into
one module and analysed once (`compile_files_for_aot`). The Dylan sema
walk runs on the merged AST, producing ONE `SemaModel`; never per-file
models merged afterward (that would invalidate slot layouts + the CPL
order baked into the record). This mirrors the Rust path.

## 5. Authoritative-model discipline

`SemaModel` is the authoritative record: `lower_with_model` (Sprint 54)
reads only the model, never the raw AST. The wire is the only channel by
which the model reaches the host; anything lowering needs that isn't on
the wire is a missing-fact bug, surfaced as a compile error rather than a
silent gap. (Today's `lower_module_full` still fuses recording +
DFM-construction; the structural split that enforces this is a later
53.1 step.)

---

*Companion to [`DYLAN_AST_WIRE.md`](DYLAN_AST_WIRE.md) (the prior stage's
wire) and [`54-lowering-dylan.md`](../../specs/54-lowering-dylan.md) (the
DFM wire — the next and final front-end seam, after which the Dylan→Rust
boundary is the DFM/CFG handoff, crossed once per compile).*
