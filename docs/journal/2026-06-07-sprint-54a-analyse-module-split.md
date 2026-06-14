# 2026-06-07 — Sprint 54a: the analyse | lower split (`analyse_module` → `SemaModel`)

*Sprint 54 makes the Dylan sema model load-bearing. The on-ramp is done
(53.x sema walk byte-matches the corpus; the shim class-id drift is fixed),
so 54 begins with the structural prerequisite the wire doc §5 flagged: draw
the sema | DFM boundary in `nod-sema` so a model can be **produced** by one
phase and **consumed** by the next. This entry is 54a — the split — done as a
behavior-preserving refactor. 54b (the wire) and 54c (the load-bearing flip)
build on it.*

## What

`lower_module_full` was a ~1000-line fused function: it interleaved the sema
*recording* (register classes, flip sealed flags, collect top-names /
generics, compute sealing) with *DFM/CFG construction* (slot accessors, per-
item lowering, dispatch resolution). The recording outputs were snapshotted
onto `LoweredModule` but, per its own comment, "Lowering does not read these
back ... the structural `lower_with_model` split that enforces that is a later
step." That step is now here.

- **New `SemaModel` struct** (`top_names`, `generics`, `classes`, `sealing`) —
  the four `dump-sema` sections, with a `user_class_map()` helper that rebuilds
  the `name -> ClassId` map lowering needs. `classes` keeps `ClassId`s for
  lowering; the dump/wire key on names, not ids (53.1 invariant).
- **New `analyse_module(m) -> Result<SemaModel, …>`** — the recording phase,
  lifted verbatim out of `lower_module_full`'s Phase 1a (register classes) /
  Phase 1c (sealed flags) / Phase 2 (names + generics), plus the sealing-fact
  *computation*.
- **`lower_module_full` now calls `analyse_module`** and rebuilds the exact
  same local names (`user_classes`, `user_class_registrations`, `top_names`,
  `generics`, `sealing`) from the returned model, so the entire DFM
  construction (Phase 3/4 + optimise) is byte-for-byte unchanged.

## The two behavior-preserving subtleties

Both were the difference between "refactor" and "regression":

1. **`errors` is a shared accumulator.** It collects class-registration errors
   in Phase 1 AND per-item lowering errors in Phase 4, but the two are always
   separated by early-returns (`if !errors.is_empty()` after each phase). So
   `analyse_module` gets its own `errors` (returns `Err` on a bad class), and
   `lower_module_full` declares a *fresh* `errors` for Phase 4. Net behavior
   identical — Phase-1 failure still short-circuits before Phase 4 ever runs.

2. **Sealing: compute in `analyse`, install in `lower`.** `collect_sealing_facts`
   is pure (reads `m.items` + the class map), so its *computation* moved into
   `analyse_module`. But `install_sealing_facts` is a global side effect that
   the historical code ran *after* Phase 3/4 lowering and *before* dispatch
   resolution — Phase 3/4 lowering runs against the *previous* install. Moving
   the install earlier would change what lowering sees, so it stays exactly
   where it was; only the model's precomputed `sealing` feeds it.

The seed-registrations (`ensure_*_registered`) and the anonymous-method lift
pre-pass still run in `lower_module_full` before `analyse_module` (analyse
assumes `__anon-method-N` are present and seed classes resolve).

## Verification (re-run, not trusted)

Behavior-preserving by construction; proven by the suite. Critically,
`sema_topnames` is a real regression check here — the Dylan walk is unchanged,
so any drift in the Rust *recording* output would fail it:

- `cargo build -p nod-sema` clean, no warnings.
- nod-sema units 23/23; `sema_dump` 1; **`sema_topnames` 38/38 MATCH**;
  `sema_self_host` 1.
- Codegen-critical (lowering → DFM → codegen → run): `codegen` 8, `tables` 14,
  `c3_oracle`, `sealing` 17, `gc` 9, `heap_objects` 16 — all pass.
- **Full `cargo test -p nod-tests --no-fail-fast` sweep: every functional
  binary green.** The lone failure is `lexer_oracle::oracle_hello` — the known
  parallel-build flake (`LNK1104`: two tests in the binary link to the same
  temp `dylan-lexer.exe` concurrently). It passes `--test-threads=1` (2/2) and
  is documented as the last open test-infra item in the 2026-06-05
  consolidation; not a product regression.

## Where it leaves us

The sema | DFM boundary is now real at the data level: `analyse_module`
produces the authoritative `SemaModel`; the DFM construction consumes it. This
is the seam the rest of Sprint 54 plugs into:

- **54b** — the sema wire: `dylan_sema_emit` (Dylan serialises a `SemaModel`)
  + host reconstruct + `--sema-with-dylan` verify-mode (in-process byte-match
  of the reconstructed model vs the Rust one). The `SemaModel` struct is now
  the serialisation target.
- **54c** — flip load-bearing: have the DFM construction consume the
  Dylan-produced model (extracting a named `lower_with_model` as needed); gate
  `dump-dfm` byte-identical with the flag on vs off. This is also where the
  two corpus gaps from the consolidation survey (stdlib-class returns;
  expand-before-sema) resolve — the host holds the class registry and the real
  expander runs ahead of the model.
