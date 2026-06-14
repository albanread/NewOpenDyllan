# 2026-06-07 — Sprint 55 plan: porting AST→DFM lowering to Dylan

*Sprint 54 made the Dylan sema load-bearing. Sprint 55 ports the LAST
front-end stage — lowering (AST→DFM construction) — to Dylan. This is the
most logic-dense stage by far; this entry is the plan (from a full map of
`lower.rs` / `nod-dfm`), to be executed sub-phased and oracle-gated. After 55
every front-end stage is Dylan-written; the Dylan→Rust boundary becomes the
permanent DFM/CFG handoff.*

## The shape of the target

A lowered `Function` (`nod-dfm/src/ir.rs`) is a phi-free **block-parameter SSA
CFG**: `blocks: Vec<Block>`, each with `params: Vec<TempId>` (join values),
`computations`, and exactly one `Terminator`. The surface a Dylan lowering must
emit is smaller than it looks:

- **Computation**: 10 variants, but lowering only ever *emits* ~8 — it **never**
  emits `SealedDirectCall`, **never** sets `is_no_alloc`, and **always** passes
  `safepoint_roots: []`. Generics lower to `Dispatch`; the Rust **dispatch
  resolver** rewrites `Dispatch`→`SealedDirectCall`/`DirectCall` and the
  **liveness pass** populates `safepoint_roots` — both run AFTER lowering and
  **stay Rust**.
- **Terminator**: 3 variants — `Return`, `If`, `Jump { target, args }`.
- The back-end (liveness → verify → resolve → codegen/GC/LLVM) is **permanent
  Rust** and runs on the Dylan-emitted `Vec<Function>` unchanged.

## The oracle: `dump-dfm` byte-match (2-process)

`format_dfm_module` (`nod-dfm/src/format.rs`) is frozen and **doubles as the
JIT-cache key** — byte-drift silently invalidates cached objects, so the gate is
strict. Discipline mirrors `sema_dump.rs` / the 54 gates: run Rust-lowering and
Dylan-lowering in **separate processes** (class registration isn't idempotent),
byte-compare across the corpus. Two format quirks the port must mirror exactly:
`Dispatch`/`SealedDirectCall` dsts render `Class(id)` as `<class:id>` (everywhere
else `Class(_)` → `<class>`); `SlotTypeKind` prints via `{:?}`.

The flag is `--lower-with-dylan` / `NOD_LOWER_WITH_DYLAN` (mirrors
`--sema-with-dylan`). The hardest infra question — how Dylan-emitted DFM crosses
back to the host — is a **DFM wire** (Dylan serializes `Vec<Function>` →
host reconstructs), the analogue of the sema/AST wires but richer
(blocks/computations/terminators/temps). Decide text-vs-fixnum at Phase 0; the
sema lesson says a structured *own-format* text round-trips losslessly, so a
`dump-dfm`-shaped text wire is viable and reuses the formatter as the contract.

## What stays Rust (do NOT port)

The lift pre-pass aside, everything after lowering: liveness
(`safepoint_roots`), the verifier, dispatch resolution (`Dispatch`→sealed/direct,
`is_no_alloc`), narrowing, codegen, GC, linker. Also a **defer candidate**: the
FFI/winapi materialization (~877 LOC — Win32 DB lookup + stub-table allocation);
keep it in the Rust `lower_module_full` shell, port only the `nod_winffi_call_N`
emission once the call-map is available.

## Sub-phasing (each independently `dump-dfm`-gateable)

**Phase 0 — scaffold (prereq).** Port `FunctionBuilder` primitives
(`lower.rs:4583-4682`: monotonic `fresh_temp`/`new_block`, entry = `BlockId(0)`)
+ `LocalEnv`; stand up the `NOD_LOWER_WITH_DYLAN` flag + the DFM wire +
the 2-process `dump-dfm` gate. First corpus: leaf fixtures that lower to a
single straight-line block (literal-returning fns). **Temp/block ids are
monotonic counters — reproducing the exact emission order IS the byte-match.**

**55a — statements + expressions (~2.2–2.8k LOC, the bulk).** `lower_expr`
(the 20 `Expr` variants; the `Ident` resolution cascade is the densest single
arm and its order is load-bearing), `lower_call` (intrinsic cascade →
DirectCall/Dispatch trichotomy), and the three control-flow builders —
`lower_if`, `lower_short_circuit`, `lower_while_like` — where the
**block-parameter SSA + deterministic *sorted* env-merge + `needs_gc_protection`
phi inclusion** lives (5 GAP fixes embedded). Plus `select_binop`/`select_unop`,
`type_from_expr`, the primitive table, the `collect_assigned/used_*` analyses.
Sub-gate incrementally: literals → binops → calls → `if` → short-circuit →
loops → multi-binder `let`.

**55b — classes + dispatch (~400–600 LOC).** Slot getter/setter builders + the
Phase-3 accessor loop, `lower_method_item`, `lower_make`, `lower_instance_check`,
the slot-store path of `lower_assign`, `try_resolve_slot_offset`. Emits
`LoadSlot`/`StoreSlot`/`Dispatch`/`TypeCheck`/make-shaped DirectCalls (reads
class metadata from the runtime registrations established in 54).

**55c — closures + blocks (~1.2k LOC, port last).** The lift pre-pass
(`lift_anonymous_methods` & friends — already partly mirrored in the Dylan sema
walk's `collect-anon-methods`; here it must produce the synthetic items + a
`ClosureRegistry`), `lower_block_form` + `lift_block_stage*`, the cell/env
machinery, and the cell-promotion arms threaded through `lower_expr`/`lower_assign`
/`lower_function_inner`. The deterministic `block_id` SipHash
(`lower_block_form`) must reproduce bit-exactly or cache keys drift.

## Honest assessment

~4.8–5.4k LOC of Rust lowering logic to reproduce in Dylan, the irreducible
core being the ~1.4k LOC of `lower_expr`/`lower_call` + the three control-flow
builders. This is multi-session. The byte-match (every temp/block id + merge
param order) is the brutal part — it's why the sub-phasing + incremental
sub-gating matters: never port more than one form before re-greening `dump-dfm`.

## Sequencing

Phase 0 (scaffold + wire + gate, trivial fixtures) → 55a (sub-gated form by
form) → 55b → 55c (lift first, then block-form) → FFI emission threaded last.
Then Sprint 56 consolidates the per-stage shim round-trips into one Dylan
front-end handing a single DFM module to the Rust back-end — where the deferred
~50× perf returns.
