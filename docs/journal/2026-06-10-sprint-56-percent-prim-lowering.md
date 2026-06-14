# 2026-06-10 — Sprint 56 (axis-1): `%`-primitive lowering in Dylan

*The next axis-1 body-lowering unlock from the lowering-flip roadmap: the Dylan
AST→DFM lowering stops bailing on `%`-primitive calls and emits the `nod_…`
runtime DirectCall, mirroring the Rust `LOWER_PRIMITIVE_TABLE`. Closes
`gap-007-repro` (the GAP-007 reproducer) through the flip.*

## Goal

Until now the Dylan lowering BAILED on any `%`-prefixed primitive call (the
soundness rule from `2026-06-07-sprint-55b-call-path-soundness.md`: never emit a
guessed callee). That left a cluster of corpus fixtures on the Rust fallback —
headlined by `gap-007-repro`, which is nothing but `%make-stretchy-vector` /
`%stretchy-vector-{size,element,push}` over a loop. Porting the prim table is
the listed next unlock (memory `lowering-flip-load-bearing`).

## What we did

- **Mirrored `LOWER_PRIMITIVE_TABLE`** (`src/nod-sema/src/lower.rs:261-498`, 127
  rows) into `tests/nod-tests/fixtures/dylan-lower.dylan` as three lookups —
  `prim-callee` (→ the literal `nod_…` symbol or `#f`), `prim-arity`, and
  `prim-result-label` (→ `<top>`/`<integer>`/`<boolean>`). The map is LITERAL
  (e.g. `%vector-size`→`nod_sov_size`, not a mechanical `%foo`→`nod_foo`), so the
  Dylan side was **generated from the Rust table** to eliminate transcription
  error, then committed as plain Dylan.
- **Replaced the bail branch** with the emission, modelled on the existing
  list-builtin branch: arity-check → lower args left-to-right (`unwrap-arg`) →
  mint the dst LAST (matching `fresh_temp(ret)` ordering) → push a `directcall`
  with the mapped callee + label. Crucially it keeps the `starts-with-percent?`
  gate: an **unknown** `%`-prim (not in the table — e.g. `%fip-state`,
  `%extract-symbol-value`) still BAILS rather than fall through to the plain
  DirectCall else, which would emit the raw `%foo` callee. Soundness preserved.
- **Rebuilt the shim** via the bootstrap (no-shim driver → `build --library` →
  relink); the `.lib.obj` grew 2.85→3.02 MB.
- **Gated** `gap-007-repro`, `gap-007-repro-ir`, and `dylan-macro-file` (the
  three fixtures the port newly unlocks) in `FLIP_ONLY_LOWER_FIXTURES`.

### Two findings worth keeping

- **No `[no_alloc]` on the live path.** The DFM formatter has a `[no_alloc]`
  suffix, but EVERY live `DirectCall` push site hardwires `is_no_alloc: false`
  (the flag only appears in `nod-dfm` unit-test fixtures). So byte-matching the
  dump needs the Dylan side to emit NO suffix — which it already does. No
  `no_alloc` field had to be added to `<dfm-comp>`. (This contradicted the
  memory's worry about a `<dfm-comp>` no_alloc flag — it's a non-issue.)
- **Safepoints stay host-side.** Lowering emits an empty safepoint set; the host
  liveness pass populates `safepoint=[…]` post-flip. `gap-007-repro`'s standalone
  dump has empty sets while its Rust `dump-dfm` shows populated ones — so it's
  FLIP-ONLY (reconciled only through `--lower-with-dylan`, which runs the host
  passes), not text-gateable.

## Verification

- `gap-007-repro` standalone now emits the four `nod_stretchy_vector_*`
  DirectCalls; `dump-dfm --lower-with-dylan` byte-matches plain `dump-dfm`.
- **Whole-corpus survey: 61 compared, 0 mismatches** — the new prim path emits a
  *wrong* DFM for no fixture. (This is exactly the net that caught the
  unknown→DirectCall trap; running it here is the discipline, not the curated
  gate.) Standalone lowered count **27 → 30 / 62**.
- Full `sema_topnames` ignored gate suite: **6/6 pass** (in-process sema,
  phase-0 lowering, sema-with-dylan, curated lowering flip incl. the 3 new
  fixtures, whole-corpus survey, standalone sema EXE). No regression from the
  shim rebuild.

## Where it leaves us

The full prim table is mirrored, so the ONLY remaining reason a `%`-prim fixture
bails is a *different* unsupported form (macros-in-lower, closures, `#(…)` list
literals, `define method` corners). The big IDE/rope fixtures (`rope`, `ide_*`,
`nod-ide`, `unified_ide`) use byte-string prims that now lower, but still bail on
those other forms — they'll fall out as axis-1 continues. Next axis-1 candidates:
`#(…)` list literals (`stdlib-size-call`), and the macro-using fixtures.
