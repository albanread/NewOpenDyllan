# 2026-06-10 — Sprint 56a (step 1): the Dylan class derivation becomes a *checked* input on the live path

*First increment of Sprint 56a (the "make classes load-bearing" centerpiece).
Promotes the offline 53.3/53.4 class byte-match oracle to a **live invariant**
on `--sema-with-dylan`, without yet changing behaviour. The precondition for
retiring the Rust class derivation.*

## Goal

Sprint 56's corrected centerpiece (see `2026-06-07-sprint-56-design-table-ownership.md`)
is that **classes are still re-derived in Rust on every live path** — the Dylan
`=== classes ===` section (parents / CPL / `slot @offset origin=…`, computed and
byte-matched by 53.3/53.4) is *thrown away* by `parse_sema_dump`. 56a wants to
consume it. But a full consume is genuinely multi-step: the dump carries only
name / parents / CPL / slot offset+setter+origin — it does **not** carry slot
*type-kinds*, *init-keywords*, or *defaults*, which codegen and `make` need. So
the runtime install can't yet be fed purely from the dump; the wire has to grow
first (a later step).

The safe, high-value first move is to make the Dylan derivation a **checked
input**: parse the classes section on the live path and assert it matches the
host's `register_module_classes` — so a Dylan-vs-Rust class-derivation
divergence becomes a loud compile failure instead of being silently masked by
Rust. This is the design's own verification backbone ("`ClassId` correctness is
validated by the existing class-section byte-match"), now made *live*.

## What we did

All in `src/nod-sema/src/lower.rs` (commit on `master`, not pushed):

- **`parse_sema_classes(dump) -> Vec<ParsedSemaClass>`** — the inverse of the
  classes block of `format_sema_model`. New `ParsedSemaClass` / `ParsedSemaSlot`
  structs carry exactly what the dump prints (everything by name). A small
  `parse_sema_slot_line` handles `NAME @OFFSET setter=BOOL origin=ORIGIN`.
  Section-gated (`=== classes ===` … until the next `=== … ===` header), so the
  top-names / generics / sealing sections can't leak in.
- **`verify_dylan_classes(rust, dylan)`** — structural equality by NAME:
  declaration order + name, direct parents, the C3 CPL, and the slot layout
  (name / offset / setter / origin, resolving the host's `ClassId`s through
  `sema_class_name`). Returns a precise `Err` naming the first divergent class /
  slot field.
- **Wired into `analyse_module_from_dump`** (the `--sema-with-dylan` seam, the
  only caller): after `register_module_classes` + `parse_sema_dump`, parse the
  dump's classes and verify against the registration; a mismatch fails the
  compile (`sema-with-dylan: class <X> …mismatch…`). `parse_sema_dump`'s own
  contract is untouched — it still ignores classes; the new function is a
  separate, clearly-scoped 56a addition.
- **Five pure-Rust unit tests** (`sprint56a_class_parse_tests`) pin the text
  grammar without the shim: single class with mixed `setter=true/false`, a
  two-class hierarchy with an *inherited*-slot origin (the `slot_origin` case
  the verifier checks), an empty classes section, a slot-less class, and a
  malformed-slot-line error.

## Verification

- `cargo test -p nod-sema sprint56a_class_parse` — 5/5 pass.
- **`dump_dfm_sema_with_dylan_byte_match`** (the Sprint-54 load-bearing gate,
  the 38-fixture corpus) — **38/38 MATCH**, 0 failed, ~57s. This is the real
  proof: `verify_dylan_classes` now runs inside `analyse_module_from_dump` for
  every class-bearing fixture (`point`, `gc_precise_two_makes`, `richards-shape`,
  `richards-shape-open`, `translate-class`, `rope`, `ide_rope`, `unified_ide`,
  `nod-ide`) under `--sema-with-dylan`, and **none** tripped the verifier — the
  Dylan class derivation byte-matches the Rust registration on the *live* path,
  not just in the offline oracle EXE.
- Default paths untouched: the verifier only runs when the `--sema-with-dylan`
  provider is installed; `analyse_module_from_dump` has no other caller.

## Discovered

- **The dump is lossy for a full consume, on purpose.** The classes section was
  designed as a by-name *oracle*; it omits slot type-kind / init-keyword /
  default because the oracle only ever compared what `format_sema_model`
  printed. Retiring `register_module_classes` (56b+) therefore needs the wire to
  grow those fields first — confirming the design's "bounded but real" framing,
  and ruling out a naive "just parse and install" shortcut.
- **A live verifier is strictly stronger than the offline gate.** The offline
  53.x oracle compares the standalone `dylan-sema.exe` / `dump-dylan-sema`
  output to the Rust oracle. The live path runs the in-process shim *and* the
  host registration in the *same* process. This verifier closes the gap between
  those two worlds: if the in-process dump ever diverged from what the host
  registered, the offline gate could still be green while the live compile
  silently trusted Rust. Now it can't.

## Where it leaves us

56a step 1 is in: the Dylan class derivation **gates** the live `--sema-with-dylan`
path (divergence ⇒ hard failure), but classes are still *derived* in Rust. The
next step toward "classes load-bearing" is to **extend the lowering/sema wire**
to carry slot type-kinds / init-keywords / defaults, then flip the runtime
install to consume the Dylan-derived CPL + slot layout (host still allocating
`ClassId`s by name in canonical order). MI stays on the Rust bail path (zero MI
in stdlib + corpus, per the design challenge).

## Follow-on (same session): the whole-corpus lowering-flip survey is now a gate

The lowering memory + several journal entries keep invoking a discipline —
*"after any change that widens what's accepted, run the whole-corpus survey
(dump-dfm vs dump-dfm --lower-with-dylan over all fixtures); the curated gate
can be green while the broader invariant (0 mismatches = never a wrong dump) is
violated. That's how the unknown→DirectCall trap was caught."* — but there was
**no committed survey**; it was re-run ad-hoc each session.

Codified it as `dump_dfm_lower_with_dylan_whole_corpus_survey` (in
`sema_topnames.rs`). It enumerates **every** `*.dylan` fixture (no curated list),
and asserts `dump-dfm --lower-with-dylan` byte-equals plain `dump-dfm` for each:
a bail falls back to Rust (⇒ identical), a covered fixture must match Rust (⇒
identical), so the only way they differ is a Dylan lowering bug emitting a
*wrong* DFM. Fixtures whose Rust `dump-dfm` itself fails (no baseline — only
`dylan-lex-shim`, not a dump fixture) are skipped, not failed.

Current state captured by the gate: **61 compared, 1 skipped, 0 mismatches**;
the standalone bail survey shows **27/62 fixtures actively lowered** by the
Dylan path, 35 still bailing (the big `dylan-*` compiler sources, the macro
fixtures, `%`-prim repros like `gap-007-repro`, `#(…)` list literals like
`stdlib-size-call`, and the rope/IDE family). Those bails are the axis-1
body-lowering grind; this gate is the standing safety net that each unlock must
keep green.
