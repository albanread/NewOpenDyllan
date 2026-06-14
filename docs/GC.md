# NewOpenDylan GC — design stub

*The generational mark-evacuate collector (NewGC) shipped (Sprint 11+) and became the default backend in Sprint 23, with precise roots from the JIT/AOT. Maintained reference: [`manual/compiler/gc.md`](manual/compiler/gc.md).*

The garbage collector for NewOpenDylan is a **precise, generational copying
collector** written in pure Rust, inheriting from
[`E:\CL\NewCormanLisp\docs\GC.md`](../../CL/NewCormanLisp/docs/GC.md). Headlines
(per [MANIFESTO.md](../MANIFESTO.md) §The garbage collector):

## Validation policy

When the goal is to validate the NewOpenDylan compiler, JIT, or GC, the
workload under test must execute in Dylan. Rust test code may launch the Dylan
program, gather counters, and print reports, but it must not replace the Dylan
allocation or transformation workload with a Rust-side surrogate. Otherwise we
would only be testing the Rust harness, not the Dylan compiler/runtime path the
project exists to build.

- Precise root finding via LLVM `gc.statepoint` (no conservative scanning past
  bring-up).
- Generational copying: young + old + pinned static area for compiled code,
  sealed-class metadata, and the loaded image.
- Multi-threaded mutator, stop-the-world collector. Per-thread TLABs for a
  lock-free allocation fast path.
- Software card-marking write barrier.
- Class metadata is pinned; sealed classes live forever.
- Multimethod dispatch caches participate in collection.

Open questions for the full doc:
- Cons-equivalent headerless layout — what proves "monomorphic at the call site"?
- Interaction with Dylan's `<class>` versus instance object headers.
- Sealing-driven inline allocation optimisation — how aggressively?

See SPRINTS.md Sprints 09–11 for the bring-up sequence.
