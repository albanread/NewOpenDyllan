# 2026-06-07 — Sprint 55: the lowering flip (Dylan AST→DFM is load-bearing)

*The keystone. The Dylan-side AST→DFM lowering now FEEDS the Rust back-end: under
`--lower-with-dylan` the Phase-3/4 Rust functions are replaced by the ones the
Dylan `dylan-lower-emit` shim produces, reconstructed host-side, and the SAME
back-end passes run on them. Byte-identical DFM on the covered subset. This is
the lowering analogue of 54c's sema flip.*

## The shape (mirrors 54c)

54c made the Dylan *sema* authoritative via a text-transport provider: Dylan
emits a name-keyed dump, the host reconstructs the `SemaModel` (registering
classes to assign ids), and lowering consumes it. The lowering flip is the same
move one stage later:

1. **Parser** (committed separately): `nod_dfm::parse_dfm_module` — the exact
   inverse of `format.rs`. Parses the indented `dump-dfm` text back into
   `Vec<Function>`. Validated by 11 hand-built round-trips (full variant surface)
   + a corpus round-trip over all 13 lowering fixtures' real dumps.
2. **Provider** (`nod-sema`): `DFM_DUMP_PROVIDER` + `set_dfm_dump_provider`,
   twin of `SEMA_DUMP_PROVIDER`. `lower_with_sema_choice` now picks the sema
   source AND the lowering source independently (they compose).
3. **Seam** (`lower_module_full_inner`): right before the back-end passes
   (`install_sealing_facts` → `narrow_function` → `resolve_dispatches` →
   `populate_safepoint_roots`), if a non-empty Dylan DFM dump was supplied,
   `out` (the Phase-3/4 Rust functions) is REPLACED by the reconstruction. Class
   labels resolve through the live registry (`resolve_class_id_by_name`) —
   classes are already registered by Phase 1 / `analyse_module[_from_dump]`. The
   passes then run on the Dylan-produced DFM exactly as on the Rust one. An empty
   dump (the Dylan lowering bailed) leaves the Rust output untouched.
4. **Driver**: `--lower-with-dylan` / `NOD_LOWER_WITH_DYLAN`, mirroring
   `--sema-with-dylan`; installs `dylan_dfm_dump_provider` (→ `lower_emit_via_shim`).

## Why this dissolves the text-gate ceiling

The standalone `dump-dylan-dfm` text gate could only ever match where the
post-lowering passes are no-ops and no class-id is printed (straight-line
integer code, slot accessors, `instance?`). Through the flip, BOTH sides run the
**same** passes on DFM carrying the **same** host-assigned class-ids — so once
the Dylan lowering emits make/dispatch (next), those forms byte-match too,
without stretching the text gate. The flip is the right verification surface for
everything id-bearing or pass-touched.

## Verification

New gate `dump_dfm_lower_with_dylan_byte_match`: `dump-dfm` vs
`dump-dfm --lower-with-dylan` byte-identical across all 13 PHASE0_LOWER_FIXTURES
(it asserts the Dylan path produced non-empty DFM — no silent fallback). The
earlier whole-corpus survey (0 mismatches, 15 match / 27 bail / 3 Rust-error)
already proved the Dylan lowering never emits a wrong dump, so bails fall back to
Rust safely (confirmed on `point.dylan`). Full `sema_topnames` 6/6 (four sema
gates + the two lowering gates + the parser round-trip) and `codegen` 8/8 — no
regression in the shared `lower_module_full` compile path.

## Known limitations (deferred)

- **Reconstructed `out` only.** The flip replaces the functions; the
  `LoweredModule`'s method/c-function/variable registrations still come from the
  Rust Phase 4 (and its FunctionIds). Fine for `dump-dfm` (functions only);
  codegen consumption needs id alignment — a later step.
- **Wasted Rust Phase 3/4.** The Rust lowering still runs (it provides the model
  + class registration) and is then discarded. A cheap optimisation later.
- **Class-typed temps.** Bare `<class>` in the text loses its id (the formatter
  prints `name()`); irrelevant on the covered subset (no class temps) but the
  make/dispatch increment will need class refs threaded by a means the text
  doesn't drop — likely emitting `<class:id>` from the Dylan side via a
  class-id-by-name primitive, since the host registry is live when it runs.

## Where it leaves us

The Dylan front end is now load-bearing through lowering for the covered subset
— lexer → parser → macros → sema → **lowering**, all Dylan, all gated
byte-identical. The remaining 55b forms (make/dispatch/slot-`:=`) now have their
verification surface (this flip gate) and their id story (the live registry +
the upcoming class-id primitive). That's the next increment.
