# 2026-06-07 — Sprint 56 design: retiring the Rust front-end (table + class ownership)

*Converts "Dylan lowering is load-bearing **alongside** Rust" into "the Rust
front-end is **deleted**." Grounded by a read-only `FunctionId`/registration
lifecycle map, then **adversarially challenged** by a second agent and revised.
The original draft's claim that classes were "already owned" was REFUTED — the
correction is the centerpiece below. Rulings are recorded at the end.*

## TL;DR

- **`FunctionId` is vestigial — CONFIRMED by the challenge** (exhaustive: zero
  `FunctionId`-keyed map/vec anywhere; none in codegen/jit/aot/runtime; not in
  the DFM dump or the cache key; the Sprint-37/38 cache + cross-process replay
  re-resolve everything **by name**, `lib.rs:702-730`, `symbols.rs:183-184`).
  The `--lower-with-dylan` flip already ships functions whose ids differ from
  Rust's (`parse_dfm_module` mints its own, `parse.rs:92`) and passes. So
  `FunctionId` alignment is a **non-issue** — there is no blocker there.
- **But the premise "classes are already owned via the 54c sema flip" is
  FALSE** (the challenge's killer finding). 54c made top-names / generics /
  **sealing** load-bearing from Dylan; **classes are still re-derived in Rust**
  on every live path. `analyse_module_from_dump` (`lower.rs:1501`) calls the
  Rust `register_module_classes` → `register_class` → `c3_linearise` → slot
  layout (`lower.rs:1524, 3064, 3230, 3268`), and `parse_sema_dump` **discards**
  the Dylan `=== classes ===` section (`lower.rs:2703-2706, 2772-2773`). The
  Dylan class derivation (53.3/53.4: parents / CPL / `slot @offset origin=…`,
  byte-matched to Rust) exists **as an oracle only** — thrown away on the live
  path. So Sprint 56 is **not** pure "emit tables + finishing engineering."
- **Two axes to "delete Rust", both must complete:**
  1. **function-BODY lowering coverage → ~100% of corpus** — the visible grind
     (%-prims, 55c closures, macros-in-lower). Today substantial (55b lowers
     control flow / slots / single+sealed dispatch / `make`), **but partial**.
  2. **registration-table + class-table OWNERSHIP** — this doc.

## What's owned vs Rust today (corrected)

| Output (`LoweredModule`, `lower.rs:587`) | Keyed on | Source under both Dylan flags today |
|---|---|---|
| `functions` | name | **Dylan** (the dump) — for the covered subset; rest bail to Rust |
| `top_names`, `generics`, `sealing` | name | **Dylan** (injected sema model, 54c) |
| `user_classes` (CPL, slots, offsets, slot_origin, counts) | name / `ClassId` | **RUST** — re-derived from the AST; the Dylan `=== classes ===` section is computed + byte-matched but **discarded** |
| `methods` (`generic_name`, `specialisers`, `body_fn_name`, `param_count`) | name + `ClassId` | **Rust** |
| `blocks` (`block_id`, body/cleanup/afterwards names, handlers) | name + runtime id | **Rust** |
| `variables` (`name`, `init_fn_name`) | name | **Rust** |
| `closures` (lifted-name → source arity) | name | **Rust** |
| `c_functions` + `c_function_stub_table` | name + opaque sig + live `entry_ptr` | **Rust** (stateful pre-pass) |

## The work (revised scope)

1. **Own the class table — the corrected centerpiece.** Make
   `analyse_module_from_dump` **consume** the Dylan `=== classes ===` section
   (parents / CPL / slots / offsets / slot_origin — already computed and
   byte-matched by 53.3/53.4) instead of re-running `register_module_classes`.
   The host still **allocates `ClassId`s by name in canonical order** (so the
   `nod_aot_register_user_class` drift invariant holds, `aot.rs:1036-1046`).
   This is "wire the already-verified Dylan derivation into the live path,"
   exactly analogous to what 54c did for sealing — **bounded** (the derivation
   exists and is gated), but **real** (a new consume path + by-name id
   resolution for CPL/slot refs). It is **not** the from-scratch
   class-semantics port the challenge estimated — the challenge assumed the
   Dylan derivation didn't exist; it does (53.3/53.4) and byte-matches. SI only;
   MI deferred (see below).
2. **Own the four function-side tables** — `methods`, `blocks`, `variables`,
   `closures`: emit from Dylan **by name**, extending the lowering dump (W2
   wire — reuse the sema wire for classes/sealing, add `=== methods ===` etc.
   to the lowering dump; do NOT build a unified wire).
3. **Replay the pre-flip side effects** *(added after the challenge — table data
   ≠ side effects)*. Skipping Phase 3/4 drops three **global mutations** that a
   table byte-compare cannot detect, and they must be replayed from the Dylan
   tables: (a) the dispatch pre-registration of null-body method stubs
   (`lower.rs:2397-2418`) that `resolve_dispatches` (post-flip,
   `lower.rs:2448-2453`) **depends on**; (b) `install_sealing_facts`
   (`lower.rs:2447`); (c) the c-function stub-table static allocation
   (`lower.rs:2025`).
4. **c-functions stay wholly on Rust initially.** The c-fn pre-pass builds
   `c_function_call_map` that the **body lowering reads** (`lower.rs:2047-2087`)
   — so a c-fn module must bail **entirely** to Rust (the Dylan lowering already
   does a module-granular bail). Do not attempt "skip Phase 3/4 but keep the
   c-fn pre-pass."

## Principles (upheld by the challenge)

- **Never key a table on `FunctionId`.** The latent harmless id divergence
  becomes a bug the moment any table joins by id. Tables reference bodies by
  name; keep it so.
- **`ClassId` / `block_id` allocation stays host-side, by name.** The runtime
  registry is permanent (back-end), so it is the correct owner of runtime ids;
  Dylan refers by name, host mints in canonical order.
- **W2 wire** (reuse + extend, not unify).

## Verification (revised — "strong-but-partial," not "zero-risk")

The challenge correctly downgraded the "zero-risk shadow-compare" claim:
- Shadow byte/struct-compare is **clean** for `variables` / `closures` and for
  `methods`'/`blocks`' **names + `param_count`**.
- It **cannot validate `ClassId` correctness** — the persisted specialiser /
  handler ids are raw `u32` (`sidecar.rs:406,428`), stable only because the host
  allocated them; comparing them presupposes the Rust class registration we want
  to retire. **`ClassId` correctness is validated by the existing class-section
  byte-match** (53.3/53.4: Dylan CPL/slots/origin already equal Rust) **+ the
  AOT drift assert**, not by the table compare.
- `block_id` (runtime-allocated) and `entry_ptr` (live pointer) need
  normalization / exclusion from the compare.
- The real cutover gate is the **full test suite** (codegen/runtime/aot) behind
  `--frontend-with-dylan`, because the side-effect bugs (work item 3) only
  surface as wrong behaviour, never as a table diff.

## Hard / uncertain

- **MI class tables — deferred, and safe to defer.** The challenge verified
  **zero multiple-inheritance** `define class` anywhere in stdlib + corpus +
  the headline programs (nod-ide, ide_rope, richards). Keep MI on the Rust bail
  path; it cannot gate the cutover for real programs. (But note: since SI is
  universal, the SI class derivation *is* the work — MI being deferrable does
  not shrink it.)
- **Axis-1 body-lowering coverage** (%-prims, 55c closures, macros-in-lower) is
  a separate, ongoing grind; "delete Rust" needs it near-complete too.

## Phasing (revised)

- **56a** — wire the Dylan `=== classes ===` section into
  `analyse_module_from_dump` (consume + host-allocate by name); make classes
  load-bearing. *The corrected centerpiece.*
- **56b** — shadow-emit + compare `methods` / `variables` / `closures` /
  `blocks` (names + arities; ids via the class byte-match).
- **56c** — side-effect replay (dispatch pre-reg, sealing) from the Dylan
  tables, behind `--frontend-with-dylan`.
- **56d** — the cutover: skip Phase 3/4; collapse the per-stage shims into one
  front-end → one handoff (the deferred ~50× reclaim). c-fn + MI stay Rust-bail.
- **56e** — default + delete Phase 3/4 + the dead `lower_module_full_with_model`
  (`lower.rs:1603`); full sweep green; drift assert is the `ClassId` canary.

## Adversarial review outcome (2026-06-07) — rulings

A fresh agent challenged this design against the code. Verdicts + my rulings:

| Area | Challenge verdict | Ruling |
|---|---|---|
| `FunctionId` vestigial | CONFIRMED (exhaustive) | **Upheld** — keep the crux |
| "classes already owned (54c)" | **REFUTED** (`lower.rs:1501,2703-2706`) | **Conceded** — corrected; work item 1, the centerpiece |
| Side-effect replay missing | MISSED | **Conceded** — added work item 3 |
| Shadow-compare "zero-risk" | WEAKER | **Conceded** — downgraded to strong-but-partial; ids via class byte-match |
| c-fn pre-pass coupling | MISSED | **Conceded** — c-fn modules bail wholly to Rust (item 4) |
| W2 wire / MI deferral | CONFIRMED | **Upheld** |

One challenge point **rejected**: it read a stale Phase-0 header comment in
`dylan-lower.dylan:4-16` and called the body lowering a "toy subset (Const/
PrimOp/DirectCall + Return)." That comment is stale — 55b lowers control flow /
slots / single + sealed dispatch / `make` (verified via the flip gate on
`richards-shape`). The body lowering is **substantial-but-partial**, not a toy;
the remaining grind is axis-1. (The stale header is worth fixing.)

## Bottom line (revised)

Sprint 56 is **bounded but bigger** than "emit five tables, compare, flip,
delete." The `FunctionId` crux is a non-issue. The real centerpiece is making
the **already-computed, already-byte-matched Dylan class derivation
load-bearing** — wiring it into the live path so Rust's
`register_module_classes` can be retired — plus owning four function-side
tables, **replaying three pre-flip side effects**, and the separate ongoing
body-lowering coverage grind. No new invention is required: the hardest piece
(class derivation) already exists in Dylan and is gated; it is merely discarded
today. The challenge's effort warning stands directionally — this is more than
"finishing engineering" — but its "entire class-semantics port from scratch"
estimate is too high, because the derivation is already written and verified.
