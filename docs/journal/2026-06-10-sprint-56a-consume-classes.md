# 2026-06-10 — Sprint 56a-CONSUME: classes installed from Dylan (register_module_classes retired)

*The centerpiece of the cutover. Under `--frontend-with-dylan`, the module's
classes are now INSTALLED from the Dylan `=== classes ===` records (via the
runtime `register_*_user_class` entry) instead of being re-derived in Rust by
`register_module_classes` — retiring the last load-bearing Rust front-end logic
on that path. Agent-implemented, then independently verified by compile-and-run.*

## What we did

- **`install_dylan_classes(&[ParsedSemaClass]) -> Vec<UserClassRegistration>`**
  (lower.rs): in dump declaration order, resolve parents / CPL / slot-origins to
  `ClassId` by name; reconstruct `Vec<SlotInfo>` from the now-lossless
  `ParsedSemaSlot` (via `slot_type_from_label` + `slot_default_from_tag`, the
  inverses of the 56a-WIRE emitters); call the explicit-shape runtime install
  (`register_mi_user_class`, the same entry `register_class`/the AOT shim use,
  which patches the `ClassId(u32::MAX)` self-sentinel); register direct-subclass
  links; assert the canonical-order id invariant (the analogue of the AOT drift
  assert). MI (`parents.len() > 1`) returns `Err` → fall back to Rust (zero MI in
  the real corpus).
- **`analyse_module_from_dump`** flipped: under `NOD_FRONTEND_WITH_DYLAN`, install
  from Dylan + replay the sealed-flag flip from the dump's sealing facts + skip
  `verify_dylan_classes` (vacuous when consuming). On install `Err`, fall back to
  `register_module_classes`. The no-flag path is byte-for-byte unchanged
  (register + verify-only).
- **`lib.rs`** (necessary discovery): `build`/`eval` call `lower_module_full`
  DIRECTLY, not the choice seam — so the single-file AOT build was rerouted
  through `lower_with_sema_choice` under the flag (preserving the eager
  `nod_runtime_init` + shim-band toggle + stdlib merge). Without this the consume
  would only be reachable from `dump-dfm` and the runtime test would be vacuous.

## Verification

- **dump-dfm byte-match** under the flag: MATCH on point / richards-shape /
  gc_precise_two_makes / translate-class / lower-class-accessors (+ no-class
  controls). The consume is non-vacuous — `install_dylan_classes` fires with
  point=1, **richards-shape=5** (the full sealed hierarchy, ids 1081-1085),
  translate-class=1, lower-class-accessors=2. Drift assert silent everywhere.
- **COMPILE-AND-RUN (the acceptance gate)** — built real EXEs WITH and WITHOUT
  the flag and ran them; runtime behaviour IDENTICAL:
  - a `<gcp-pt>` with a defaulted slot (`x :: <integer> = 7`) made with only
    `y:`, then `y(a) := y(a) + 100`, prints `x=7 y=102` under BOTH paths — slot
    DEFAULT, init-keyword, getter, and setter all behave identically.
  - `richards-shape` (5-class sealed hierarchy, inheritance, sealed-generic
    dispatch) builds + runs identically.
- 14 nod-sema unit tests (incl. the new inverse-helper tests).

## Findings

- Class ids start at **1081**, not `FIRST_USER` (1024) — seed-registration mints
  user-band ids first, so the canonical-order assert anchors on the observed
  first id, not a hard constant. Correct + required.
- A hand-written 2-LEVEL-inheritance probe (own slots at each level) exposed a
  PRE-EXISTING, unrelated offset bug in the `--lower-with-dylan` shim (`@8` vs
  `@24` for an inherited-slot access) — NOT in the consume, and outside the gated
  corpus (which has zero such classes). Noted for later; set aside.

## Where it leaves us

Under `--frontend-with-dylan` the front-end is now Dylan-load-bearing end to end:
functions (DFM), methods (consumed), and classes (installed) all come from Dylan;
`register_module_classes` is retired on that path (kept as the no-flag oracle +
the MI fallback). The Rust Phase-3/4 walk still RUNS redundantly (its
function/method outputs are replaced, its class derivation bypassed) — skipping
+ deleting it (56e/f) is the remaining cleanup, gated on near-complete axis-1
coverage so the module-granular bail's Rust fallback can be retired safely.
