# Performance and hardening

Windows-first scope. This page covers two focused compiler/runtime tracks:
precise JIT safepoint maps on Windows x64, and bounded polymorphic inline
caches (PICs). Both replace working-but-fragile mechanisms with the standard
production-VM designs.

## Scope

1. **Precise JIT safepoint maps on Windows x64.** Replace the current
   spill-and-reload-via-runtime-slots scheme with a callsite-indexed map
   emitted by codegen and consumed by the GC.
2. **Polymorphic inline caches (PICs).** Extend the monomorphic dispatch cache
   into a bounded cold -> mono -> poly -> megamorphic-shared state machine.

The scope is intentionally Windows-only for the first landing. Mac and Linux
remain important ports, but they are follow-on work after the Windows runtime,
JIT, AOT pipeline, and IDE path are structurally correct and fully supported.

Out of scope for the first landing:

- cross-platform unwinder work
- executable-code compaction
- callstack return-address rewriting for moved code
- full register-root precision on day one
- deoptimization or OSR machinery

## Why these two tracks

NewOpenDylan already has the beginnings of both stories, but in forms that stop
short of the structural fix.

### Precise roots

The DFM and LLVM layers model safepoint protection as a list of live temps
attached to each call, then emit runtime bracketing around the call:

- spill temp to a stack slot
- call `nod_register_root(slot)`
- make the allocating call
- call `nod_unregister_root(slot)`
- reload the slot and rebind the temp

That design is visible in:

- `src/nod-dfm/src/ir.rs`
- `src/nod-dfm/src/liveness.rs`
- `src/nod-llvm/src/codegen.rs`

It works, but it leaves GC correctness tied to mutable temp state and post-call
rewrites. The GAP-007 family (see [Known limitations](known-limitations.md))
shows why that is brittle even when individual local bugs are fixed.

The structural alternative is the standard VM design:

- every safepoint has a machine-PC identity
- codegen records which roots are live there
- each root is described by a location, not by a temp name
- the collector walks frames, looks up the PC, and scans only those precise
  locations

That is the model used by HotSpot, V8, and other production JITs.

### PICs

Dispatch already has a real per-site cache slot with generation checks and a
fast path, but it is still monomorphic:

- one receiver class
- one cached method
- a miss falls back to `nod_dispatch`

That design is visible in:

- `src/nod-runtime/src/dispatch.rs`
- `src/nod-llvm/src/codegen.rs`

This is good enough for cold -> mono, but not for real Dylan callsites that see
a small stable set of receiver classes. Those sites miss on every alternation
and give up the main performance win that PICs are supposed to provide.

The structural fix is also standard:

- cold -> mono
- mono -> poly with a tiny bounded entry set
- poly -> megamorphic shared fallback once the bound is exceeded

## What the repository already has

### A dormant stack-map model

`src/nod-runtime/src/stack_map.rs` is already shaped around the right runtime
abstraction:

- `StackMapEntry { pc, slots }`
- `LiveSlot::FpOffset(i32)`
- `LiveSlot::SavedRegister(u8)`
- `walk_parked_frame()`

The work is therefore not to invent a root model. The work is to:

- make codegen produce the records
- register them with installed JIT/AOT code
- make the GC consult them on Windows

### Windows unwind registration in the JIT

`src/nod-llvm/src/jit_mm.rs` already registers Windows SEH unwind tables with
`RtlAddFunctionTable`. That means a Windows-first precise-frame walk is a
bounded engineering task, not an unbounded portability project. The repository
therefore already contains a dormant frame-map data model in the runtime and a
Windows-specific unwind substrate in the JIT memory manager — enough to justify
a Windows-only safepoint-map landing.

### The current root path is still temp-oriented

The active implementation in `src/nod-llvm/src/codegen.rs` still uses
`begin_safepoint()`, `end_safepoint()`, `safepoint_slot_pool`, and calls to
`nod_register_root` / `nod_unregister_root`. That is the machinery the
safepoint-map work retires.

### The current dispatch cache is monomorphic

`src/nod-runtime/src/dispatch.rs` defines a single `CacheSlot` with `class`,
`method`, `generation`, `hits`, `misses`, and `site_id`. That is a solid
cold/mono substrate and the natural place to grow a bounded PIC without
changing the high-level dispatch contract.

## Design decisions

### Windows-first means Windows-first

The first implementation targets:

- Windows x64 JIT
- Windows x64 AOT-generated EXEs
- current-thread and callback-heavy paths that already matter for the IDE shell

It does not block on DWARF or libunwind work, macOS compact unwind, Linux frame
walking, or an all-thread park protocol beyond what Windows support needs first.

### The first safepoint-map landing is stack-slot based

The first version uses frame slots as root homes, even though the eventual
end-state also tracks saved registers. This keeps the codegen contract simple,
lets the runtime and GC plumbing land first, and still removes the
temp-mutation correctness class that the current root shim suffers from.
Register-root precision is a follow-up once the map format and GC integration
are working.

### `nod_dispatch` stays the correctness oracle

The PIC work does not fork the dispatch semantics. Fast paths stay a cache in
front of the existing semantic authority. That means:

- a PIC miss still delegates to runtime dispatch logic
- generation invalidation remains authoritative
- sealed-direct and sealed-chain lowering continue to coexist with the fallback
  dispatch path

## Windows Safepoint Map Contract

**Goal.** Replace the temp-based safepoint contract with a Windows-first
PC-and-location contract without changing GC behavior yet.

### Compiler IR and analysis

- `src/nod-dfm/src/ir.rs` is the first contract to change. The current
  call-shaped nodes carry `safepoint_roots: Vec<TempId>`, which bakes the
  spill/reload story into the IR surface. This is replaced with a
  safepoint-description payload that can name live values abstractly enough for
  codegen to assign stable homes.
- `src/nod-dfm/src/liveness.rs` keeps answering the same semantic question, but
  the output of `populate_safepoint_roots()` evolves from "these temp ids are
  live across the call" to "these root-bearing values are live at this
  safepoint and must receive frame locations". The first change is additive:
  compute the same live set, but hand it to a new lowering shape rather than the
  old spill shim.

### LLVM codegen

`src/nod-llvm/src/codegen.rs` is the owning implementation surface. It adds:

1. a per-function safepoint-record accumulator keyed by post-call PC identity,
2. a frame-layout helper for GC-visible stack slots,
3. a debug dump path so tests can assert emitted safepoint metadata before the
   runtime consumes it.

The existing `begin_safepoint()`, `end_safepoint()`, and `safepoint_slot_pool`
machinery remains intact at this stage; the first slice is metadata emission and
introspection only. The concrete call emitters that eventually switch over are
the same ones that currently take `safepoint_roots`: direct call paths, builtin
helper calls, and `emit_dispatch()`.

### JIT and AOT installation seams

- `src/nod-llvm/src/jit.rs` needs a follow-on install hook for per-module
  safepoint maps; at the contract stage, the only required work is to define the
  data that will be passed later.
- `src/nod-llvm/src/aot.rs` does not need full runtime consumption yet, but the
  map shape is chosen with both JIT and AOT serialization in mind so the
  contract does not become JIT-only by accident.

### Runtime seam

`src/nod-runtime/src/stack_map.rs` already provides the target runtime
abstraction. The preferred move is to conform codegen to this shape rather than
redesign it.

### Acceptance

- A focused `nod-llvm` test compiles a function with two allocating callsites
  and produces two distinct safepoint entries keyed by PC.
- The entries list frame-slot locations, not temps.
- The metadata is stable under later SSA rewrites such as phi wiring.
- No GC behavior changes at this stage.

The smallest credible first slice is: define a location-based safepoint payload
in `nod-dfm`, thread it through `nod-llvm` without changing runtime GC, emit a
stable test-visible record of safepoint maps, and add one focused codegen test
proving the metadata shape. That slice is narrow enough to validate quickly and
strong enough to lock the architecture before touching GC behavior.

## GC Consumes Safepoint Maps

**Goal.** Make the Windows runtime and collector consult safepoint maps for JIT
frames at allocating callsites.

- Add JIT-side safepoint-map registration in `src/nod-llvm/src/jit.rs`.
- Add AOT metadata install/load support in `src/nod-llvm/src/aot.rs`.
- Extend `src/nod-runtime/src/heap.rs` to scan registered frame maps in addition
  to existing static roots.
- Wire the Windows-first parked-frame and frame-walk glue needed to use
  `StackMap::lookup(pc)` and `walk_parked_frame()`.
- Keep the existing spill/`register_root` path alive as a fallback until the new
  path is verified.

### Acceptance

- A Windows-only integration test forces GC inside nested JIT calls and proves
  the collector consults the correct safepoint entry by PC.
- AOT and JIT both pass the same root-preservation regression fixture.
- Callback-heavy shapes still run correctly.

This depends on the safepoint-map contract above and on the existing SEH
registration in `src/nod-llvm/src/jit_mm.rs`.

## Retiring Spill/Reload Safepoints; Loop Polls

**Goal.** Remove the temp-slot root shim from the hot path and close the
remaining correctness gap for non-allocating loops.

- Delete `begin_safepoint()`, `end_safepoint()`, and the `safepoint_slot_pool`
  hot-path use from `src/nod-llvm/src/codegen.rs`.
- Remove JIT reliance on `nod_register_root` / `nod_unregister_root` for
  ordinary callsite protection.
- Add loop/back-edge safepoint poll emission in `src/nod-llvm/src/codegen.rs`.

### Acceptance

- GAP-007-class loop-carried local fixtures still pass.
- A callback/reentry stress case continues to pass under repeated GC.
- No ordinary JIT callsite emits register/unregister root bracketing.
- The Dylan lexer self-dump runs cleanly under GC pressure without a
  module-variable workaround driven by temp-root fragility.

> The root-cause class behind the temp-root fragility is fixed (the GAP-007
> through GAP-013 family is closed). A residual module-variable workaround in
> the lexer fixture (`*tokens*` / `*dump-stream*`) is the remaining cleanup; the
> self-dump on the lexer's own source is the acceptance gate.

## Bounded PICs on Dispatch Sites

**Goal.** Extend dispatch caches from monomorphic to bounded polymorphic.

- Replace the single-entry `CacheSlot` payload in
  `src/nod-runtime/src/dispatch.rs` with a bounded small-array PIC payload while
  preserving generation-invalidation semantics.
- Update `src/nod-llvm/src/codegen.rs` fast-path emission from one class compare
  to a bounded chain of compares and direct calls.
- Add a shared megamorphic fallback path once the cap is exceeded.
- Preserve the existing per-site symbol and relocation scheme in
  `src/nod-llvm/src/symbols.rs`.

### The PIC state machine

| State | Behavior |
|---|---|
| **cold** | site compiles its first cache on the first call |
| **mono** | one receiver class, one cached method; a fast class compare guards a direct call |
| **poly** | misses add entries while below the PIC cap; the fast path is a bounded chain of compares and direct calls |
| **megamorphic** | once the cap is hit, the site stops growing and falls through to a shared megamorphic dispatch path |

### Acceptance

- Tests cover cold -> mono, mono -> poly, poly hit, generation miss, and
  poly -> megamorphic transitions.
- Alternating 2-3 receiver-class dispatch sites stop taking the slow path every
  time. A dispatch micro-benchmark shows a clear drop in misses for a stable
  2-class or 3-class callsite.

## Stabilization

After the structural changes land, the remaining correctness and measurement
work is:

- instrumentation and counters for safepoint-map hits, misses, and fallback
  cases.
- dispatch statistics that distinguish mono hits, poly hits, and megamorphic
  slow-path traffic.
- expanded regression coverage across JIT, AOT, callbacks, IDE shell, and
  lexer/rope workloads.

## Work that should wait

### Code heap compaction

A moving executable-code heap with callstack-rewrite support is worth keeping in
mind but is not in the immediate plan, because:

- NewOpenDylan does not yet own a moving executable-code heap.
- the current JIT story is built on LLVM/MCJIT and external unwind registration,
  not on a custom moving code manager.
- safepoint maps and PICs each have clear payoff now; code-heap compaction does
  not.

The right time to revisit this is after Windows support is fully solid, and only
if code ownership moves toward a relocatable runtime-managed code heap. The
general lesson held in reserve: code addresses, inline caches, and frame
metadata must be wired through relocatable indirection if the runtime ever owns
moving executable code.

### Non-Windows ports

Mac and Linux remain strategic, but not in the first landing plan. The porting
work should happen once Windows has stable precise roots for JIT and AOT, stable
callback/reentry behavior, stable polymorphic dispatch caches, and stable IDE
shell runtime behavior under GC pressure. At that point the port task is
primarily about unwinding, frame walking, and OS integration.

## Recommended order

If only one track proceeds immediately, do precise safepoint maps first. They
retire correctness gaps structurally, simplify the mutator hot path, reduce
future GC-related debugging surface, and make callback-heavy Windows code less
fragile by construction. PICs are still important, but they are primarily a
performance track; safepoint maps are both a hardening track and an
architectural cleanup.

---

Reference: [Platforms](platforms.md) | [Known limitations](known-limitations.md) | [Tracing](tracing.md) | [Architecture](../architecture.md) | [Codegen](../compiler/codegen.md) | [GC](../compiler/gc.md) | [Glossary](../glossary.md)
