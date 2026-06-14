# Performance and Hardening Overview

*Drafted 2026-05-25. Windows-first scope. Companion to* `PLAN.md`,
`SPRINTS.md`, `DEFERRED.md`, `COMPILER_GAPS.md`, *and* `GC.md`.

## 1. Scope

This document covers two focused compiler/runtime tracks that can land
as ordinary NewOpenDylan sprints rather than open-ended research:

1. **Precise JIT safepoint maps on Windows x64**.
   Replace the current spill-and-reload-via-runtime-slots scheme with a
   callsite-indexed map emitted by codegen and consumed by the GC.
2. **Polymorphic inline caches (PICs)**.
   Extend the current monomorphic dispatch cache into a bounded
   cold -> mono -> poly -> megamorphic-shared state machine.

The scope here is intentionally **Windows-only for the first landing**.
Mac and Linux remain important ports, but they are treated as follow-on
ports after the Windows runtime, JIT, AOT pipeline, and IDE path are
structurally correct and fully supported.

Excluded from the first landing:

- cross-platform unwinder work
- executable-code compaction
- callstack return-address rewriting for moved code
- full register-root precision on day one
- deoptimization or OSR machinery

## 2. Why These Two Tracks

NewOpenDylan already has the beginnings of both stories, but in forms
that stop short of the structural fix.

### 2.1 Precise roots

Today the DFM and LLVM layers model safepoint protection as a list of
live temps attached to each call, then emit runtime bracketing around
the call:

- spill temp to a stack slot
- call `nod_register_root(slot)`
- make the allocating call
- call `nod_unregister_root(slot)`
- reload the slot and rebind the temp

That design is visible in:

- `src/nod-dfm/src/ir.rs`
- `src/nod-dfm/src/liveness.rs`
- `src/nod-llvm/src/codegen.rs`

It works, but it leaves GC correctness tied to mutable temp state and
post-call rewrites. The GAP-007 family documented in `COMPILER_GAPS.md`
shows why that is brittle even when individual local bugs get fixed.

The structural alternative is the standard VM design:

- every safepoint has a machine-PC identity
- codegen records which roots are live there
- each root is described by a location, not by a temp name
- the collector walks frames, looks up the PC, and scans only those
  precise locations

That is the model used by HotSpot, V8, and other production JITs, and
it is the part of Factor worth borrowing.

### 2.2 PICs

Today dispatch already has a real per-site cache slot with generation
checks and a fast path, but it is still monomorphic:

- one receiver class
- one cached method
- miss falls back to `nod_dispatch`

That design is visible in:

- `src/nod-runtime/src/dispatch.rs`
- `src/nod-llvm/src/codegen.rs`
- `docs/DEFERRED.md` (Sprint 13 follow-on PIC entry)

This is good enough for cold -> mono, but not for real Dylan callsites
that see a small stable set of receiver classes. Those sites miss on
every alternation and give up the main performance win that PICs are
supposed to provide.

The structural fix is also standard:

- cold -> mono
- mono -> poly with a tiny bounded entry set
- poly -> megamorphic shared fallback once the bound is exceeded

This is what Factor's `inline_cache.cpp` makes concrete.

## 3. Detailed Discovery

### 3.1 What Factor contributes

The code worth studying is not something to port line-for-line. The
transferable value is architectural.

#### Precise roots and safepoint maps

Factor's frame scanner in `factor-src/vm/slot_visitor.hpp` walks the
callstack, uses the return address to find the current safepoint inside
the owning code block, and then consults safepoint metadata to find the
exact live roots for that point.

Key ideas:

- safepoints are keyed by machine PC / return address
- live roots are a table lookup, not mutable temp state
- derived roots are modeled separately from base roots
- the collector does frame walking plus metadata lookup, not root-slot
  registration in the mutator fast path

#### PIC state machine

Factor's `factor-src/vm/inline_cache.cpp` implements the standard PIC
growth path:

- cold site compiles its first cache
- misses add entries while below the PIC cap
- once the cap is hit, the site becomes megamorphic and stops growing

The important thing here is the state machine and the bounded growth,
not the exact machine code templates.

#### Code heap compaction

Factor's `factor-src/vm/compaction.cpp` and
`factor-src/vm/code_blocks.cpp` are relevant as design reading, but not
as first-sprint implementation targets. The useful lesson is that code
addresses, inline caches, and frame metadata must be wired through
relocatable indirection if the runtime ever owns moving executable code.

NewOpenDylan is not there yet.

### 3.2 What NewOpenDylan already has

#### A dormant stack-map model already exists

`src/nod-runtime/src/stack_map.rs` is not dead code by accident. It is
already shaped around the right runtime abstraction:

- `StackMapEntry { pc, slots }`
- `LiveSlot::FpOffset(i32)`
- `LiveSlot::SavedRegister(u8)`
- `walk_parked_frame()`

That means the work is not "invent a root model". The work is:

- make codegen produce the records
- register them with installed JIT/AOT code
- make GC consult them on Windows

#### The JIT already has Windows unwind registration

`src/nod-llvm/src/jit_mm.rs` already registers Windows SEH unwind tables
with `RtlAddFunctionTable`. That matters because it means a
Windows-first precise-frame walk is a bounded engineering task, not an
unbounded portability project.

The current repo therefore already contains:

- a dormant frame-map data model in the runtime
- a Windows-specific unwind substrate in the JIT memory manager

That combination is enough to justify a Windows-only safepoint-map
landing now.

#### The current root path is still temp-oriented

The active implementation in `src/nod-llvm/src/codegen.rs` still uses:

- `begin_safepoint()`
- `end_safepoint()`
- `safepoint_slot_pool`
- calls to `nod_register_root` and `nod_unregister_root`

That is the exact machinery the safepoint-map sprint is supposed to
retire.

#### The current dispatch cache is monomorphic

`src/nod-runtime/src/dispatch.rs` defines a single `CacheSlot` with:

- `class`
- `method`
- `generation`
- `hits`
- `misses`
- `site_id`

That is a solid cold/mono substrate. It is also the natural place to
grow a bounded PIC without changing the high-level dispatch contract.

## 4. Design Decisions

### 4.1 Windows-first means Windows-first

The first implementation should target:

- Windows x64 JIT
- Windows x64 AOT-generated EXEs
- current-thread and callback-heavy paths that already matter for the
  IDE shell

It should not block on:

- DWARF or libunwind work
- macOS compact unwind
- Linux frame walking
- all-thread park protocol beyond what Windows support needs first

### 4.2 First safepoint-map landing should be stack-slot based

The first version should use frame slots as root homes, even if the
eventual end-state also tracks saved registers.

Reason:

- it keeps the codegen contract simple
- it lets the runtime and GC plumbing land first
- it still removes the temp-mutation correctness class that the current
  root shim suffers from

Register-root precision can be a follow-up once the map format and GC
integration are working.

### 4.3 Keep `nod_dispatch` as the correctness oracle

The PIC work should not fork the dispatch semantics. Fast paths should
stay a cache in front of the existing semantic authority.

That means:

- PIC miss still delegates to runtime dispatch logic
- generation invalidation remains authoritative
- sealed-direct and sealed-chain lowering continue to coexist with the
  fallback dispatch path

## 5. Proposed Sprint Sequence

These are written in the existing repo style: two weeks, one developer,
one demo, one concrete acceptance surface.

---

## Sprint 45c — Windows Safepoint Map Contract

**Goal:** Replace the temp-based safepoint contract with a Windows-first
PC-and-location contract without changing GC behavior yet.

### Detailed implementation map

**Compiler IR and analysis**

- `src/nod-dfm/src/ir.rs`
  is the first contract to change. The current call-shaped nodes carry
  `safepoint_roots: Vec<TempId>`, which bakes the spill/reload story
  into the IR surface. Sprint 45c should replace this with a
  safepoint-description payload that can name live values abstractly
  enough for codegen to assign stable homes.
- `src/nod-dfm/src/liveness.rs`
  should keep answering the same semantic question, but the output of
  `populate_safepoint_roots()` needs to evolve from "these temp ids are
  live across the call" to "these root-bearing values are live at this
  safepoint and must receive frame locations". The first change can be
  additive: compute the same live set, but hand it to a new lowering
  shape rather than the old spill shim.

**LLVM codegen**

- `src/nod-llvm/src/codegen.rs`
  is the owning implementation surface. Sprint 45c should add:
  1. a per-function safepoint-record accumulator keyed by post-call PC
     identity,
  2. a frame-layout helper for GC-visible stack slots,
  3. a debug dump path so tests can assert emitted safepoint metadata
     before the runtime consumes it.
- The existing `begin_safepoint()`, `end_safepoint()`, and
  `safepoint_slot_pool` machinery should remain intact during 45c.
  They are not the first edit slice; the first slice is metadata
  emission and introspection only.
- The concrete call emitters that will eventually switch over are the
  same ones that currently take `safepoint_roots`: direct call paths,
  builtin helper calls, and `emit_dispatch()`.

**JIT and AOT installation seams**

- `src/nod-llvm/src/jit.rs`
  will need a follow-on install hook for per-module safepoint maps,
  but in 45c the only required work is to define the data that will be
  passed later.
- `src/nod-llvm/src/aot.rs`
  likewise does not need full runtime consumption yet, but the 45c map
  shape should be chosen with both JIT and AOT serialization in mind so
  the contract does not become JIT-only by accident.

**Runtime seam**

- `src/nod-runtime/src/stack_map.rs`
  already provides the target runtime abstraction. Sprint 45c should
  not redesign it unless the compiler can prove one of its fields is
  materially wrong. The preferred move is to conform codegen to this
  shape.

### Likely test work for Sprint 45c

**New or expanded unit tests**

- Add focused `nod-llvm` tests in `src/nod-llvm/src/codegen.rs` or an
  adjacent test module that:
  1. compile a tiny function with two allocating callsites,
  2. assert two distinct safepoint records exist,
  3. assert each record names frame-slot locations rather than temp
     ids,
  4. assert the metadata is stable under later SSA rewrites such as phi
     wiring.
- Add runtime-side tests in `src/nod-runtime/src/stack_map.rs` for any
  new slot forms or serialization helpers introduced by the compiler
  contract. The existing `walk_parked_frame()` tests are already a good
  base and should stay green.

**Regression fixtures to keep in view**

- `tests/nod-tests/tests/gc_callback_env.rs`
  is the best callback/reentry stress anchor for later 45d/45e work.
  Sprint 45c does not need to change it, but any contract chosen now
  should be checked against this shape.
- The GAP-007 regression family and the Dylan lexer self-dump path are
  the best acceptance targets once metadata emission turns into active
  GC consumption.

### Smallest credible Sprint 45c edit slice

The smallest useful first patch set is:

1. define a location-based safepoint payload in `nod-dfm`,
2. thread that payload through `nod-llvm` without changing runtime GC,
3. emit a stable debug dump or test-visible record of safepoint maps,
4. add one focused codegen test proving the metadata shape.

That slice is narrow enough to validate quickly and still strong enough
to lock the architecture before touching GC behavior.

**Deliverables**

- Define the canonical safepoint metadata shape for JIT/AOT code using
  `src/nod-runtime/src/stack_map.rs`.
- Refactor `src/nod-dfm/src/ir.rs` and `src/nod-dfm/src/liveness.rs` so
  the compiler pipeline models live roots as safepoint facts, not as
  mutable spill/reload temp identities.
- Add a codegen-side frame-layout builder in `src/nod-llvm/src/codegen.rs`
  that can assign stable frame homes for root-bearing values.
- Add a debug/introspection path that dumps emitted safepoint records
  for a compiled function.

**Acceptance criteria**

- A focused `nod-llvm` test compiles a function with two allocating
  callsites and produces two distinct safepoint entries keyed by PC.
- The entries list frame-slot locations, not temps.
- No GC behavior changes yet.

**Dependencies**

- existing DFM liveness pass
- existing runtime `StackMap` model

**Demo**

- compile a tiny Dylan function and print a stable safepoint-map dump

---

## Sprint 45d — Windows GC Consumes Safepoint Maps

**Goal:** Make the Windows runtime and collector consult safepoint maps
for JIT frames at allocating callsites.

**Deliverables**

- Add JIT-side safepoint-map registration in `src/nod-llvm/src/jit.rs`.
- Add AOT metadata install/load support in `src/nod-llvm/src/aot.rs`.
- Extend `src/nod-runtime/src/heap.rs` to scan registered frame maps in
  addition to existing static roots.
- Wire the Windows-first parked-frame and frame-walk glue needed to use
  `StackMap::lookup(pc)` and `walk_parked_frame()`.
- Keep the existing spill/register_root path alive as fallback until
  the new path is verified.

**Acceptance criteria**

- A Windows-only integration test forces GC inside nested JIT calls and
  proves the collector consults the correct safepoint entry by PC.
- AOT and JIT both pass the same root-preservation regression fixture.
- Callback-heavy shapes still run correctly.

**Dependencies**

- Sprint 45c
- existing SEH registration in `src/nod-llvm/src/jit_mm.rs`

**Demo**

- end-to-end fixture survives allocation under JIT and AOT without the
  temp-root shim being semantically required

---

## Sprint 45e — Retire Spill/Reload Safepoints and Add Loop Polls

**Goal:** Remove the temp-slot root shim from the hot path and close the
remaining correctness gap for non-allocating loops.

**Deliverables**

- Delete `begin_safepoint()`, `end_safepoint()`, and the
  `safepoint_slot_pool` hot-path use from `src/nod-llvm/src/codegen.rs`.
- Remove JIT reliance on `nod_register_root` / `nod_unregister_root`
  for ordinary callsite protection.
- Add loop/back-edge safepoint poll emission in `src/nod-llvm/src/codegen.rs`.
- Update `docs/COMPILER_GAPS.md` and `docs/DEFERRED.md` to retire the
  temp-oriented root-protection story.

**Acceptance criteria**

- GAP-007-class loop-carried local fixtures still pass.
- A callback/reentry stress case continues to pass under repeated GC.
- No ordinary JIT callsite emits register/unregister root bracketing.

**Dependencies**

- Sprint 45d

**Demo**

- Dylan lexer self-dump runs cleanly under GC pressure without any
  module-variable workaround driven by temp-root fragility.
  **Status (Sprint 45h)**: the root cause is fixed (GAP-007–013
  closed). The `*tokens*`/`*dump-stream*` workaround in
  `dylan-lexer.dylan` is still in tree pending a retirement commit;
  the self-dump on the lexer's own source is the acceptance gate.

---

## Sprint 45f — Bounded PICs on Dispatch Sites

**Goal:** Extend dispatch caches from monomorphic to bounded
polymorphic.

**Deliverables**

- Replace the single-entry `CacheSlot` payload in
  `src/nod-runtime/src/dispatch.rs` with a bounded small-array PIC
  payload while preserving generation invalidation semantics.
- Update `src/nod-llvm/src/codegen.rs` fast-path emission from one class
  compare to a bounded chain of compares and direct calls.
- Add a shared megamorphic fallback path once the cap is exceeded.
- Preserve the existing per-site symbol and relocation scheme in
  `src/nod-llvm/src/symbols.rs`.

**Acceptance criteria**

- Tests cover cold -> mono, mono -> poly, poly hit, generation miss,
  and poly -> megamorphic transitions.
- Alternating 2-3 receiver-class dispatch sites stop taking the slow
  path every time.

**Dependencies**

- current Sprint 13 monomorphic cache slot

**Demo**

- a dispatch micro-benchmark shows a clear drop in misses for a stable
  2-class or 3-class callsite

---

## Sprint 45g — Dispatch and GC Stabilization

**Goal:** Close the remaining correctness and measurement gaps after the
structural changes land.

**Deliverables**

- Add instrumentation and counters for safepoint-map hits, misses, and
  fallback cases.
- Add dispatch statistics that distinguish mono hits, poly hits, and
  megamorphic slow-path traffic.
- Expand regression coverage across JIT, AOT, callbacks, IDE shell, and
  lexer/rope workloads.
- Update `docs/GC.md`, `docs/DEFERRED.md`, and `docs/SPRINTS.md` to
  reflect the landed design.

**Acceptance criteria**

- `cargo test` slices for `nod-runtime`, `nod-llvm`, and the focused
  Dylan fixtures pass.
- IDE shell callback and nested-message-loop cases remain stable.
- The docs describe the new precise-root and PIC stories accurately.

**Dependencies**

- Sprints 45d-45f

**Demo**

- a short performance-and-correctness report run against the same set of
  known-problem fixtures

## 6. Work That Should Wait

### 6.1 Code heap compaction

Factor's compaction and callstack-rewrite work is worth keeping in mind,
but it should not be promoted into the immediate sprint plan.

Reasons:

- NewOpenDylan does not yet own a moving executable-code heap in the
  Factor sense.
- the current JIT story is built on LLVM/MCJIT and external unwind
  registration, not on a custom moving code manager
- safepoint maps and PICs each have clear payoff now; code-heap
  compaction does not

The right time to revisit this is after Windows support is fully solid
and only if code ownership moves toward a relocatable runtime-managed
code heap.

### 6.2 Non-Windows ports

Mac and Linux remain strategic, but not in the first landing plan. The
porting work should happen once Windows has:

- stable precise roots for JIT and AOT
- stable callback/reentry behavior
- stable polymorphic dispatch caches
- stable IDE shell runtime behavior under GC pressure

At that point the port task is primarily about unwinding, frame walking,
and OS integration, not about still-changing GC/compiler architecture.

## 7. Recommended Order

If only one track can proceed immediately, do **precise safepoint maps
first**.

Why:

- they retire correctness gaps structurally
- they simplify the mutator hot path
- they reduce future GC-related debugging surface
- they make callback-heavy Windows code less fragile by construction

PICs are still important, but they are primarily a performance sprint.
Safepoint maps are both a hardening sprint and an architectural cleanup
sprint.

## 8. Immediate Next Actions

1. Approve the Windows-first sprint sequence in this document.
2. Pick Sprint 45c as the next hardening sprint.
3. Add a safepoint-map dump facility before touching GC behavior.
4. Keep the Factor code as design reference only; do not copy code.
5. Treat Mac and Linux as follow-on ports after the Windows story is
   complete.