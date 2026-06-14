# 2026-06-10 — Sprint 56a-WIRE: the class dump becomes non-lossy

*The precondition for retiring `register_module_classes`: the `=== classes ===`
sema dump now carries the slot type-kind / init-keyword / required / default
that `nod_make` + GC + AOT need, co-evolved across both emitters + parser +
verifier, byte-match-gated. Agent-implemented from the roadmap recipe, then
independently verified.*

## What we did

The slot line grew (additively, the original four tokens first):

```
slot NAME @OFF setter=BOOL origin=ORIGIN type=<KIND> init-keyword=<KW|-> required=<BOOL> default=<TAG>
```

- **`type=`** — the canonical source type name, **class-typed slots by NAME**
  (via `sema_class_name`, never a numeric ClassId — that would re-introduce the
  53.5e id-nondeterminism). Rust `slot_type_label(SlotType)` and the Dylan
  `slot-type-label` apply the SAME collapsing rules `slot_type_from_expr` uses
  (`<byte-string>`→`<string>`, `<single-float>`/`<float>`→`<double-float>`,
  `<simple-object-vector>`→`<vector>`, untyped→`<top>`), so both sides emit
  identical text. Total + agreeing.
- **`init-keyword=`** — the colon-free keyword, or `-`.
- **`required=`** — `true`/`false`.
- **`default=`** — the GAP-009 tag space the AOT serializer uses
  (`unbound`/`true`/`false`/`nil`/`value:<bits>`), derived EXACTLY as Rust
  `register_class` does: only int + bool literals become a value, `init-function:`
  thunks + non-literals → `unbound`.

Changes: `format_sema_model` (Rust emitter, + `slot_type_label`/`slot_default_tag`
helpers); `ParsedSemaSlot` + `parse_sema_slot_line` + `verify_dylan_classes` (the
four fields parsed + checked against `SlotInfo`); `dylan-sema.dylan`'s
`<class-rec>` + `build-class-rec` + classes-emit (the Dylan side, mirroring the
default-tag derivation); a small `<ast-slot-spec> slot-init-fn?` flag in
`dylan-parser.dylan` so the Dylan default-tag faithfully discards `init-function:`
like Rust. The shim `make(<class-rec>)` kept ≤8 keywords (Sprint-12 cap) — the 4
new parallel vectors set via setters.

## Verification

- Sema byte-match (`dump-dylan-sema` vs `--parse-with-rust dump-sema`): MATCH on
  `point`, `richards-shape`, `richards-shape-open`, `gc_precise_two_makes`,
  `translate-class`, AND the full-corpus gates `dylan_sema_in_process_byte_match`
  + `dump_dfm_sema_with_dylan_byte_match` (the widened line agrees corpus-wide).
- Live: `--sema-with-dylan` / `--lower-with-dylan` / `--frontend-with-dylan`
  `dump-dfm` all exit 0 + byte-match plain `dump-dfm` on point/richards-shape/
  gc_precise_two_makes — `verify_dylan_classes` now checks the four new fields
  and passes; no regression in lowering/frontend.
- 12 nod-sema unit tests (incl. a new widened-slot test: init-keyword +
  `required=true` + integer default `value:84`).

## Scope note

The gated corpus has no slot defaults (all `default=unbound`), so the
`value:<bits>` path is covered only by the unit test. Negative defaults / the
fixnum-overflow→Unbound edge aren't reachable from a Dylan-parsed literal and
aren't in any fixture; the now-live `verify_dylan_classes` would catch any real
divergence loudly.

## Where it leaves us

The class derivation is now LOSSLESSLY dumped + verified — the precondition for
the CONSUME (install classes from the Dylan records via
`register_user_class_metadata`, retiring `register_module_classes`, the last
load-bearing Rust front-end logic). Verify-only here; nothing installed yet.
