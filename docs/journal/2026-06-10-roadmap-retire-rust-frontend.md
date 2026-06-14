# 2026-06-10 — Roadmap: retiring the Rust front-end (agent-planned, code-grounded)

*Produced by a 4-agent planning workflow (3 parallel code-grounded investigations
— expander integration, class-table consume, remaining tables + cutover — then a
synthesis), then reviewed. Every file:line was verified against the code at plan
time. This is the authoritative forward plan for the self-hosting endgame; the
2026-06-07 design doc is its conceptual parent.*

# Sprint 56 Roadmap: Retiring the Rust Front-End (Expander + Class Consume + Table Ownership → Cutover)

## TL;DR

**Done.** The Dylan parser is default (`--parse-with-rust` opts out). Three independent, composable load-bearing opt-ins exist and are byte-match-gated on `dump-dfm`: `--sema-with-dylan` (Dylan sema model), `--lower-with-dylan` (Dylan AST→DFM lowering for ~27–32/62 fixtures, module-granular bail to Rust otherwise), and `NOD_EXPAND_WITH_DYLAN` (Dylan expander in the **parse** path only). The class `=== classes ===` dump is a **checked** input (`verify_dylan_classes`, lower.rs:1520-1522) but still **derived in Rust** (`register_module_classes`, lower.rs:1502). The DFM dump replaces only `out`/`functions` (lower.rs:2453-2454) — `methods`/`blocks`/`variables`/`closures` are always Rust-computed.

**What remains.** Three workstreams, partially independent:
1. **Expander into lowering** — `--lower-with-dylan` re-parses **raw, unexpanded** source (main.rs:1489 `lower_emit_via_shim(src)`), so every macro-using fixture lowers to `""` and bails vacuously. One host-seam call unlocks the macro cluster.
2. **Class consume (56a→full)** — grow the class wire with the four lossy `SlotInfo` fields, mirror them in the Dylan walk, then **install from Dylan records** instead of deriving in Rust.
3. **Table ownership + cutover (56b–56e)** — shadow-verify then consume `methods`/`blocks`/`variables`/`closures` from the Dylan lowering dump, replay the three pre-flip side effects, add the combined `--frontend-with-dylan` flag, skip Rust Phase-3/4, make Dylan default, **delete the Rust phases**.

**End state.** `--frontend-with-rust` becomes the opt-OUT (mirroring `--parse-with-rust`); Rust Phase-3/4 body-lowering, `register_module_classes` on the load-bearing path, and the dead `lower_module_full_with_model` (lower.rs:1615, zero callers) are deleted. The module-granular bail survives **only** for c-functions and MI, routed to a **retained minimal** Rust path — never deleted while anything real bails.

---

## The dependency graph (why this order)

```
56b-EXPAND (host-seam expand) ──┐ unlocks macro cluster, near-zero risk, cheap
                                ├──> widens lowering coverage  ──> feeds 56e precondition
56b-T (verify variables/closures)─┤      (more fixtures exercise the table verify)
56c-T (verify methods/blocks) ───┘
                                          56a-CLASS-WIRE (grow slot fields, verify)
                                                │ (independent of tables)
                                                ▼
                                          56a-CLASS-CONSUME (install from Dylan)
                                                │
   (all verify+consume green) ─────────────────┴──> 56d-SIDE-EFFECTS (replay 3 mutations)
                                                          │
                                                          ▼
                                                  56e-COMBINED-FLAG (skip Phase-3/4)
                                                          │
                                                          ▼
                                                  56f-DEFAULT + DELETE Rust phases
```

**DO THIS FIRST: 56b-EXPAND (host-seam expand-then-lower).** Rationale:
- **Highest leverage / lowest risk.** It is a single Rust call in `dylan_dfm_dump_provider` (main.rs:1487-1490), reusing the **already-byte-match-gated** expander (`expand_source_via_shim` + `stdlib_macro_source()`, proven in the parse path at main.rs:386-388). **No shim rebuild.** The Sprint 52 gate `macro_file_expand.rs` already proves AST-equivalence-after-round-trip for `unless`/`for-range`.
- **De-risks the whole roadmap.** It widens the lowering coverage (the 56f "near-complete coverage" precondition is the actual gate to delete Phase-3/4), so doing it first front-loads the coverage work that everything else waits on. The other steps mostly add verification scaffolding that is *exercised only on covered fixtures* — more coverage = more test signal for 56b-T/56c-T.
- It is **orthogonal** to the class/table work and can land while those are in flight.

---

## Ordered sprints

### 56b-EXPAND — Wire the Dylan expander into the lowering path *(DO FIRST)*

**Goal.** Make `--lower-with-dylan` expand Dylan-side before lowering, so macro-call fixtures (`unless`/`when`/`cond`/`for-each`) lower in Dylan instead of bailing to `""`.

**Concrete changes.**
- `src/nod-driver/src/main.rs:1487-1490` — in `dylan_dfm_dump_provider`, after `dylan_lex_jit::init()?` (keep this FIRST — the shim resolver must fire before `expand-source`, the 52.6c AOT-name-literal bug), call `dylan_parse_wire::expand_source_via_shim(src, nod_sema::stdlib::stdlib_macro_source())`, then pass the **expanded** source to `lower_emit_via_shim`. Fall back to raw `src` on expansion error (mirror main.rs:400-403).
- Design **(a)** at the host seam, NOT the shim — `dylan-lower-emit` stays unchanged (still lexes+parses+lowers, now over kernel-shaped source). Reject design (b) (pass an already-expanded Rust `Module`): the provider contract is `fn(&str)->Result<String,String>` (lib.rs:215-216); the Rust `Module` is not wire-serializable to the lowering shim, which deliberately re-parses from text.
- This expand seam is **always-on under `--lower-with-dylan`** and **independent of `NOD_EXPAND_WITH_DYLAN`** (which gates the parse path). It MUST be always-on: the Rust oracle always expands (lib.rs:244), so the Dylan path must too to byte-match.
- `tests/nod-tests/tests/sema_topnames.rs:421-438` — move `macros-unless`, `macro-when-only`, `cond_smoke`, `expand-pipeline-smoke` into `FLIP_ONLY_LOWER_FIXTURES` (they expand to `if`, which `lower-if-expr` handles). They are **flip-only, NOT PHASE0** — the standalone `dump-dylan-dfm` path (main.rs:1507, raw source) does not expand, so their standalone dump stays empty. Do NOT add to `PHASE0_LOWER_FIXTURES`.

**Gate.** `cargo test -p nod-tests --test sema_topnames dump_dfm_lower_with_dylan_whole_corpus_survey -- --ignored --nocapture` stays **0 mismatches**; `dump_dfm_lower_with_dylan_byte_match` green with the four macro fixtures now producing **non-empty** Dylan dumps that byte-equal plain `dump-dfm`.

**Risk.** *Medium-low.* (1) **Hygiene nonce** — shim pins `"0"` (dylan-lex-shim.dylan:1356) vs Rust's per-expansion counter; safe for `if`-expanding macros (no template-introduced binders per Sprint 52.4) but any future macro with a hygienic binder silently breaks byte-match — the survey is the net. Per-fixture, confirm no `__nod_hyg_` text in the expansion. (2) **Idempotence on macro-free source** — forcing expand on the 32 already-lowered fixtures must be a byte no-op; `render-frags` reshapes whitespace but re-lexing is whitespace-insensitive, so the AST is unchanged — **verify by running the survey with expand forced on ALL fixtures**, not just macro ones. (3) **Does NOT unlock**: `for` (a kernel `<ast-for-clause>` statement, not a macro — Rust returns `Unsupported` at lower.rs:7294-7297, so there's no oracle), `block`-expanding macros (`macro-when-cleanup`, `block` Unsupported lower.rs:7298-7301), or the rope/IDE family (they use `case`/blocks/multi-value-bind on other axes). **Set expectations: ~+4 flip-covered fixtures, not a headline jump.**

---

### 56b-T — Shadow-emit + verify `variables` and `closures` (the clean cases)

**Goal.** Make the implicit name-coupling of two Rust-computed tables an explicit checked invariant — verify-only, no consume (the 56a pattern).

**Concrete changes.**
- `tests/nod-tests/fixtures/dylan-lower.dylan` (~line 2843, `dylan-lower-emit`) — append `=== variables ===` (`variable NAME init=INIT_FN_NAME`) and `=== closures ===` (`closure LIFTED_NAME arity=N` + `cell_locals_per_function` sets) after the function dump.
- `src/nod-sema/src/lower.rs` — add parsers (mirror `parse_sema_dump`/`parse_sema_classes`, lower.rs:2722/2834) and `verify_dylan_variables`/`verify_dylan_closures` (mirror `verify_dylan_classes`, lower.rs:2935), called in `lower_module_full_inner` **after** the Rust tables build and the DFM replace (lower.rs:2455), comparing against `VariableRegistration{name,init_fn_name}` (lower.rs:705, built 2270) and `ClosureInfo{lifted_name,arity,captured}` (lower.rs:4283, from the lift pre-pass 1686).

**Gate.** `dump_dfm_lower_with_dylan_byte_match` and `dump_dfm_lower_with_dylan_whole_corpus_survey` stay **0-mismatch** (a verify mismatch fails the compile loudly on a covered fixture). Add a pure-Rust unit test pinning the new grammar (mirror `sprint56a_class_parse_tests`).

**Risk.** *Low* — verify-only, tables still Rust-computed, behavior unchanged. Only risk is emit/parse grammar drift, caught by the unit test. (Rebuild `dylan-lex-shim.lib.obj` via the no-shim bootstrap after touching `dylan-lower.dylan`.)

---

### 56c-T — Shadow-emit + verify `methods` and `blocks` (names + param_count; ids ride the class byte-match)

**Goal.** Make the method-body-name coupling (`methods[].body_fn_name` recomputed at lower.rs:3769-3782 — the current *implicit* contract that lets `register_methods` bind pointers) an explicit checked invariant.

**Concrete changes.**
- `tests/nod-tests/fixtures/dylan-lower.dylan` — add `=== methods ===` (`method GENERIC_NAME body=BODY_FN_NAME params=N specialisers=[<name>,...]`) and `=== blocks ===` (`block body=NAME cleanup=NAME? afterwards=NAME? handlers=[<class-name>:HANDLER_FN,...]`).
- `src/nod-sema/src/lower.rs` — verify against Rust `methods` (`MethodRegistration`, lower.rs:229, built 1779-2190) and `blocks` (`BlockRegistration`, lower.rs:1390, drained 2472). **Compare specialisers/handlers BY CLASS NAME** (resolve via `sema_class_name`, lower.rs:2598), NOT raw `ClassId` — ClassId correctness already rides the 56a class byte-match + AOT drift assert. **Normalize out `block_id`** (runtime-allocated, lower.rs:1393) and any `entry_ptr`. param_count + by-name body/cleanup/afterwards/handler symbols ARE compared.

**Gate.** Both lower-with-dylan gates 0-mismatch. Spot-check a method-heavy fixture (richards-shape) and a block/exception fixture for the handler list.

**Risk.** *Medium.* (1) **Coverage thinness** — blocks exist only for covered fixtures; most block/handler fixtures bail today (survey: 35 bail), so this verify is under-tested until 56b-EXPAND + axis-1 widen. (2) A method whose specialiser is a **singleton/union** (not a bare class) has no class name → skip-verify those rows, matching the shim's own bail (`lower-method` returns `#f`, dylan-lower.dylan:2493). (3) **Seed condition classes** in block handler lists (`ensure_conditions_registered`, lower.rs:1647) are NOT user classes and NOT in the 56a byte-match — their ClassId correctness rides the seed-registration order being identical host-vs-EXE (see 56d/cutover).

---

### 56a-CLASS-WIRE — Grow the class wire with the four lossy `SlotInfo` fields (verify-total)

**Goal.** Make the class dump **non-lossy** so the Dylan records can later be the install source. Today `format_sema_model` emits only `slot NAME @OFFSET setter=BOOL origin=ORIGIN` (lower.rs:2676-2683), dropping `type_kind`/`init_keyword`/`required_init_keyword`/`default_init`. `nod_make` (make.rs:174-216), GC precision (classes.rs:551-557), and AOT (lib.rs:1340-1384) all read these.

**Concrete changes.**
- **A1** `src/nod-sema/src/lower.rs:2676-2683` — widen the slot line additively (keep the existing four tokens FIRST): `... type=<KIND> init-keyword=<KW|-> required=<BOOL> default=<TAG[:bits]>`. Encode **class-typed slots by NAME** via `sema_class_name` (NOT `SlotType::Class(id)` — not name-stable, re-introduces the 53.5e id-determinism bug); scalar slots by a fixed bucket label matching dylan-lower's `slot-kind-label`/`slot-return-label` (dylan-lower.dylan:2000-2017). Encode `default` in the GAP-009 tag space AOT already uses (lib.rs:1355-1360): `unbound`/`true`/`false`/`nil`/`value:bits`.
- **A2** `tests/nod-tests/fixtures/dylan-sema.dylan` — extend `<class-rec>` (lines 277-284, already has `rec-slot-ests`) with init-keyword/required/default-tag vectors, populated in `build-class-rec` (346-360) from parser fields `slot-init-kw`/`slot-required?`/`slot-init`/`slot-type` (dylan-parser.dylan:752-760). Extend the classes-emit loop (760-781) to print the widened line byte-for-byte. **Default-tag derivation must EXACTLY mirror Rust `register_class` (lower.rs:3356-3373)**: only integer + bool literals → `Value`, everything else → `Unbound`, **including the fixnum `try_into` overflow → Unbound edge.**
- **A3** `src/nod-sema/src/lower.rs:2817-2823,2899-2923` — add the four fields to `ParsedSemaSlot`, parse them in `parse_sema_slot_line`, and extend `verify_dylan_classes` (lower.rs:2975-2989) to check them against Rust `SlotInfo`. This turns the lossy verify into a **total** structural check of `SlotInfo` — the precondition for 56a-CONSUME.

**Gate.** `dump_dfm_sema_with_dylan_byte_match` (sema_topnames.rs:579), `dylan_sema_in_process_byte_match` (:296), and the standalone EXE sema gate stay green; verify now fails loudly on any A1/A2 field divergence. Add a `parse_sema_classes` round-trip unit test for a slot with init-keyword + required + integer default.

**Risk.** *Medium-high* — the **default-init tag derivation is the trickiest mirror**: the Dylan walk must reproduce Rust's partial literal-recognition including the fixnum-overflow→Unbound edge, or the sema byte-match breaks. (Rebuild the shim.) Independent of the table work — can run in parallel with 56b-EXPAND/56b-T/56c-T.

---

### 56a-CONSUME — Install classes from Dylan records; stop deriving in Rust on the load-bearing path

**Goal.** Replace `register_module_classes(m)` in `analyse_module_from_dump` (lower.rs:1502) with an install driven by the now-total Dylan records — making the Dylan class table load-bearing, not just verified.

**Concrete changes.**
- **B1** `src/nod-sema/src/lower.rs` — add `install_dylan_classes(&[ParsedSemaClass]) -> Result<Vec<UserClassRegistration>, LoweringError>`: in **dump declaration order**, resolve parent + CPL names to ClassIds via `resolve_class_id_by_name`, reconstruct the full `Vec<SlotInfo>` from the total `ParsedSemaSlot`, compute `slot_origin` ClassIds by name, and call **the existing** `nod_runtime::register_user_class_metadata(UserClassSpec{..})` (lib.rs:894 — the SAME entry the AOT shim `nod_aot_register_user_class` uses; **no new runtime API needed**). Use `ClassId(u32::MAX)` self-sentinel for cpl[0]/own-slot origin (matches `register_mi_user_class`, lib.rs:1049-1062). Capture the minted id into a `UserClassRegistration` shaped like register_module_classes' output (lower.rs:1560-1569).
- **B2** `analyse_module_from_dump` — parse `parse_sema_classes(dump)` FIRST, call `install_dylan_classes` to register + obtain `classes`, then `parse_sema_dump` (name resolution now works against the installed registry). **Preserve the sealed-flag flip** (currently register_module_classes Phase 1c, lower.rs:1583-1599) by replaying it from the dump's `=== sealing ===` `sealed-class` lines via `mark_sealed()`. **Gate behind `NOD_CLASSES_FROM_DYLAN`** initially (opt-in discipline, mirroring the other flips).
- **B3 — the AOT canonical-order invariant.** `install_dylan_classes` MUST register in **exactly** the dump's declaration order (= the order register_module_classes walked `m.items`). `allocate_user_class_id` (classes.rs:759) is monotonic; the EXE resolver replays in merged-LM order and asserts `assigned_id == expected_class_id` (aot.rs:1088-1095). **Preserve the Sprint 54 eager seed-registration ordering** (lower.rs:1647-1677: `ensure_*_registered` + float/c-ffi-error) which runs BEFORE class install on BOTH host and EXE — do not move it relative to the flip. Add an in-`install_dylan_classes` assertion that the minted id equals the position-derived expected id, mirroring the AOT assert, to catch drift at compile time.

**Gate.** With the flag ON: `dump_dfm_sema_with_dylan_byte_match` (:579), `dump_dfm_lower_with_dylan_byte_match` (:640), `dump_dfm_lower_with_dylan_whole_corpus_survey` (:719) all byte-green — installed ClassIds + metadata must equal Rust-derived for every fixture (any one-off id shift cascades through every `<class:N>` reference and explodes the byte-match). **Plus a runtime-behavior gate (mandatory):** the five named gates are all `dump-dfm` TEXT gates — none run `nod_make` or execute an EXE, so they CANNOT catch wrong init_keyword/required/default/type_kind wiring. Add a focused execution test: compile a fixture with `make(<C>, kw: v)` + slot getter + required-init-keyword + integer/`#t` default, run it (EXE or JIT), assert observed slot values — run BOTH flag-off (Rust) and flag-on (Dylan), assert identical.

**Risk.** *High.* (1) **Inherited slots / MI** — the current Dylan emit assumes no slot-bearing superclass and origin==self (dylan-sema.dylan:760-762). `register_class`'s MI slot-merge + offset-patch (lower.rs:3472-3531) is non-trivial; inherited-slot classes must keep bailing to Rust until the merge is ported — **scope 56a to SI / no-inherited-slot classes**, see open questions. (2) **Class-id drift** — THE invariant (B3); any reorder re-introduces the Sprint 54 3-class drift. (3) The behavior gate is the only net for the lossy fields — non-negotiable. (4) **register_module_classes likely cannot be fully deleted** — the plain (no-flag) `dump-dfm` path (the byte-match ORACLE) still derives in Rust and has no dump; register_module_classes survives as the no-flag fallback.

---

### 56d-SIDE-EFFECTS — Replay the three pre-flip side effects from the Dylan tables (table data ≠ side effects)

**Goal.** Before Phase-3/4 is *skipped* (56e), wire the three global mutations a table byte-compare cannot detect to read from the now-owned Dylan tables, while Phase-3/4 still runs (so it's exercised before the skip).

**Concrete changes.** `src/nod-sema/src/lower.rs`, behind the combined flag:
- **(a)** Dispatch pre-registration of null-body method stubs (lower.rs:2409-2431) that `resolve_dispatches` (lower.rs:2461-2465) depends on — route to read from the Dylan `methods` table.
- **(b)** `install_sealing_facts(&sealing)` (lower.rs:2459) — already fed by the Dylan sema model under `--sema-with-dylan`; **confirm it still fires** in the right order (before dispatch resolution, the historical point).
- **(c)** c-fn stub-table static allocation (lower.rs:2037) — **N/A**: c-fn modules bail wholly (`define c-function` → `all-ok? := #f` → `""`, dylan-lower.dylan:2812-2814), so the c-fn pre-pass + `c_function_call_map` (lower.rs:2065/2091, read by Phase-4 at 2123/2148/2177/2236) never run on the Dylan path. **Confirm and preserve this bail.**

**Gate.** **Full codegen/runtime/aot suite** under the combined flag — NOT just `dump-dfm`. The side-effect bugs surface only as wrong RUNTIME behavior (wrong dispatch from a mis-ordered null-body registry, or sealing installed after dispatch resolution), never as a table diff. Run the dispatch-resolution and block/exception runtime tests specifically.

**Risk.** *High* — the design's explicitly-called-out trap. Must be validated by behavior, not bytes.

---

### 56e-COMBINED-FLAG — Add `--frontend-with-dylan` and SKIP Rust Phase-3/4 (consume, not verify)

**Goal.** One flag that composes expand+sema+lower; when the Dylan DFM dump is present, STOP building Rust Phase-3/4 tables and build `methods`/`blocks`/`variables`/`closures` from the Dylan dump sections (graduating 56b-T/56c-T's verify to a **consume**).

**Concrete changes.**
- `src/nod-driver/src/main.rs` — add clap flag `--frontend-with-dylan` (+ `NOD_FRONTEND_WITH_DYLAN=1`), mirroring the three flag blocks (main.rs:541-576), setting all three: installs sema+lower providers AND sets the expand path. (They already compose — independent `OnceLock`s.)
- `src/nod-sema/src/lower.rs` — in `lower_module_full_inner`, when `dfm_dump` is `Some(non-empty)`, skip Rust Phase-3/4 and build the four tables from the Dylan sections, driving the 56d side effects from them. **Keep the module-granular bail**: empty dump (c-fn, MI, any uncovered form) → full Rust Phase-3/4 (the `out = parsed` branch at lower.rs:2441 is skipped).
- `src/nod-sema/src/lib.rs` — route `compile`/`build`/`eval` through `lower_module_full_choice`. Today **only `dump-dfm`** uses the choice seam; `compile_file_for_aot` (lib.rs:1013), `eval` (lib.rs:378), `dump-llvm` (lib.rs:280) call `lower_module_full` directly. The AOT path's ClassId-drift eager-init dance (lib.rs:986-1001, lower.rs:1662-1677) must survive the reroute.

**Gate.** Whole-corpus survey 0-mismatch AND the **FULL suite** (`cargo test --workspace` + the `#[ignore]` integration gates) green under `--frontend-with-dylan`. This is the real cutover gate.

**Risk.** *High* — the actual skip-Phase-3/4 flip. Any table the consume path forgets to build (closures' `cell_locals_per_function`, the warnings vec, c_function tables) breaks codegen/AOT. The Sprint-44 merge-bug (lib.rs:1081-1102) is the cautionary precedent for silently-dropped fields.

---

### 56f-DEFAULT + DELETE — Make Dylan default, delete the Rust phases

**Goal.** Invert the flag and remove the now-unreachable Rust code.

**Concrete changes.**
- `src/nod-driver/src/main.rs` — invert: `--frontend-with-rust` opts OUT (mirror `--parse-with-rust`, main.rs:578-581).
- `src/nod-sema/src/lower.rs` — DELETE the now-unreachable Rust Phase-3/4 body-lowering in `lower_module_full_inner`; DELETE `lower_module_full_with_model` (lower.rs:1615, **zero call sites — verified, only doc/journal refs**).
- `tests/nod-tests/fixtures/dylan-lower.dylan:4-16` — fix the stale "toy subset" header.
- **Retain a minimal Rust path** for the module-granular bail (c-fn + MI) — the bail's fallback IS the deleted Phase-3/4, so it must be re-pointed at a retained minimal Rust lowering. `register_module_classes` survives for the no-flag oracle path.

**Gate.** `cargo build --workspace` clean (no dead-code warnings for deleted fns); full suite green with Dylan default; `--frontend-with-rust` still byte-identical (the regression oracle). The AOT ClassId-drift assert (aot.rs:1088-1095) stays as the canary.

**Risk.** *High* — deleting Phase-3/4 removes the fallback for everything still bailing. **Do NOT delete until the survey shows the REAL corpus (not `_tmp/` scratch) is fully covered OR its bails route to a retained path.**

---

## Cutover checklist — exact conditions to flip default + delete Rust phases

Flip `--frontend-with-dylan` to default and delete Rust Phase-3/4 ONLY when **all** hold:

1. **Coverage** — the real corpus (excluding the `dylan-*` compiler self-sources and the `_tmp/` MI scratch) is fully actively-lowered, OR every remaining bail routes to a **retained minimal Rust path**. Today 27/62 active, 35 bail; 56b-EXPAND clears the macro cluster (~+4); the residual axis-1 backlog (%-prims, `for`/`case`/block lowering, list literals, rope/IDE family) must shrink first. *(Run the standalone bail survey and categorize the 35 — see open questions.)*
2. **All five `dump-dfm` byte-match gates green under the combined flag for several sessions** — `dump_dfm_sema_with_dylan_byte_match` (:579), `dump_dfm_lower_with_dylan_byte_match` (:640), `dump_dfm_lower_with_dylan_whole_corpus_survey` (:719), `dylan_sema_in_process_byte_match` (:296), the standalone EXE sema gate.
3. **The full codegen/runtime/AOT suite green under `--frontend-with-dylan`** — NOT just `dump-dfm`. This is the only net for 56d's side effects (dispatch ordering, sealing timing) and 56a-CONSUME's lossy slot fields (`make`/GC/AOT), which no text gate exercises.
4. **The 56a-CONSUME runtime behavior gate green flag-on AND flag-off, identical results** — `make(<C>, kw: v)` + getter + required-init-keyword + default observed correctly.
5. **Module-granular bail still catches c-functions + MI + any uncovered form** — so default can never emit a wrong artifact. The c-fn whole-module bail (dylan-lower.dylan:2812-2814) and the MI `class-is-simple?` guard (dylan-lower.dylan:2787-2792) confirmed live.
6. **AOT ClassId-drift assert (aot.rs:1088-1095) never fires** across a build+run of a class-bearing fixture (e.g. point.dylan / translate-class.dylan) — the canary that the host-vs-EXE registration order is identical.
7. **`--frontend-with-rust` produces byte-identical output** as a retained regression oracle (mirroring `--parse-with-rust`).

Then, and only then: invert the flag, delete Phase-3/4 + `lower_module_full_with_model`, re-point the bail at the retained minimal Rust path. **Perf is NOT a gate** (memory `perf-waits-for-whole-frontend`: the ~50× reclaim from collapsing per-stage shims is deferred).

---

## Open questions / decisions for the architect

1. **`register_module_classes` — delete or retain?** The plain (no-flag) `dump-dfm` path is the byte-match ORACLE for every gate and still derives classes in Rust with no dump available. Strong implication: register_module_classes **survives** as the no-flag fallback and is only removed from the `--sema-with-dylan` path. Confirm the project does not intend to drive the no-flag path from a Dylan dump too.

2. **56a scope — SI-only or port the MI slot-merge now?** Inherited-slot / MI classes can't be installed from the current Dylan records (which assume origin==self, dylan-sema.dylan:760-762). Decide: scope 56a-CONSUME to SI/no-inherited-slot and keep Rust (bail) for the rest, OR port `register_class`'s slot-merge + C3 + offset-patch (lower.rs:3472-3531) into the Dylan walk now. The corpus has **zero real MI** (only `_tmp/mi-*.dylan` scratch), which argues for SI-only + bail.

3. **Canonical `type=` label vocabulary** for the widened slot line — `SlotType` variant name (Integer/Object/…), source type name (`<integer>`), or the dual kind+return labels dylan-lower already emits? Must be **total and agreeing** on BOTH the Rust `SlotInfo`→label and Dylan AST→label sides; class-typed slots **must** be by NAME (via `sema_class_name`) to avoid the 53.5e id-determinism trap. GC needs only `is_pointer_shaped`; make/AOT need the precise `SlotType` incl. `Class(name)`.

4. **Does the 56e consume need the class wire grown first?** 56e consumes `methods`/`blocks`/`variables`/`closures` from the lowering dump, but if classes stay Rust-derived (56a-CONSUME not landed), is that a coherent partial cutover, or must 56a-CONSUME land first? (Methods reference specialiser **class names** — they resolve against whatever registry is live, so a partial cutover *may* be coherent, but the slot type-kind/init-keyword/default that codegen+make need still come from the Rust-derived class table until 56a-CONSUME.)

5. **Seed condition-class ClassId validation across the flip.** Block handler lists reference seed condition classes (`ensure_conditions_registered`, lower.rs:1647), NOT user `define class`es, so they're NOT in the 56a class byte-match. Their ClassId correctness rides the seed-registration order being **identical host-vs-EXE** — confirm this is asserted somewhere (the AOT drift assert covers user classes; is there an equivalent for seeds?).

6. **`stdlib_macro_source()` vs the Rust expander's macro table** — is `stdlib::stdlib_macro_source()` (returns `STDLIB_FILES[0].1`) the SAME stdlib text the Rust expander collects from (`stdlib::stdlib_macros()`)? If they diverge (file ordering), the Dylan and Rust macro tables differ and 56b-EXPAND's byte-match fails. They appear to share `STDLIB_FILES[0]` but assert it.

7. **`for-each` fip-prims** — for the rope/IDE family, even correct `for-each` expansion to `until`+`%fip-*` prims is **necessary but not sufficient**: `%fip-init`/`%fip-current-element`/`%fip-advance!` are NOT in the `LOWER_PRIMITIVE_TABLE` mirror (they bail as unknown `%`-prims). Decide whether adding them is in 56b-EXPAND scope or a follow-on axis-1 task.

8. **Flag staging** — should `--frontend-with-dylan` default-on be staged per-subcommand (dump-dfm → eval → build) or flipped globally? And is the `--frontend-with-rust` inversion symmetric with `--parse-with-rust` the intended final shape?

---

**Files that will change, by sprint** (all absolute):
- 56b-EXPAND: `e:\NewOpenDylan\NewOpenDylan\src\nod-driver\src\main.rs`, `e:\NewOpenDylan\NewOpenDylan\tests\nod-tests\tests\sema_topnames.rs`
- 56b-T / 56c-T: `e:\NewOpenDylan\NewOpenDylan\tests\nod-tests\fixtures\dylan-lower.dylan`, `e:\NewOpenDylan\NewOpenDylan\src\nod-sema\src\lower.rs`
- 56a-WIRE: `e:\NewOpenDylan\NewOpenDylan\src\nod-sema\src\lower.rs`, `e:\NewOpenDylan\NewOpenDylan\tests\nod-tests\fixtures\dylan-sema.dylan`, `...\fixtures\dylan-lower.dylan`
- 56a-CONSUME: `e:\NewOpenDylan\NewOpenDylan\src\nod-sema\src\lower.rs`, `...\src\lib.rs`, `...\tests\nod-tests\tests\sema_topnames.rs`, `...\fixtures\`
- 56d / 56e / 56f: `e:\NewOpenDylan\NewOpenDylan\src\nod-driver\src\main.rs`, `...\src\nod-sema\src\lib.rs`, `...\src\nod-sema\src\lower.rs`, `...\fixtures\dylan-lower.dylan`

**Start with 56b-EXPAND** — one host-seam call (main.rs:1487-1490) reusing the already-gated expander, no shim rebuild, unlocks the macro cluster, and front-loads the coverage that 56f's delete-gate depends on.
