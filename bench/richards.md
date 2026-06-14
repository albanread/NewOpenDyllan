# Richards-shape benchmark — sealing perf trajectory

*Hand-curated; refresh after each measurement run. The bench prints to stdout but does not auto-overwrite this file. Append new measurements to the History table below.*

## What this measures

A Dylan program with a sealed task-class hierarchy + sealed multimethod dispatch, JIT-compiled twice (sealed and open variants) and timed. Both variants must return the same value (semantic equivalence is asserted in the test). The ratio is **open elapsed / sealed elapsed** — the speedup sealing buys at the dispatch level.

The bench is for **trajectory observation**, not for gating sprints. Performance targets are aspirational (the manifesto's "Dylan stops being a slow toy" framing); the current project stage prioritises correctness. The test assertion is `ratio >= 0.95` — a regression guard against accidentally re-introducing dispatch overhead, not a target.

## Setup

- Fixture: `tests/nod-tests/fixtures/richards-shape.dylan` (sealed) and `…-shape-open.dylan` (open)
- Outer-loop iterations: 500
- Inner-loop iterations: 2000
- Dispatches per `step` (unrolled 16-call chain): 16
- Total `run-task` dispatches in the bulk loop: **16 000 000**
- Total slot-accessor dispatches (inside method bodies): **63 999 988**
- Both fixtures return `6 240 000 021` (asserted equal in the test)

## History

Append a row per measurement. State the build mode; debug-vs-release matters because Rust runtime functions absorb most of the per-call cost in debug.

| Date       | Sprint  | Build   | Sealed     | Open       | Ratio   | Notes                                                                |
| ---------- | ------- | ------- | ---------- | ---------- | ------- | -------------------------------------------------------------------- |
| 2026-05-18 | 16      | release | 14 446 ms  | 15 369 ms  | 1.06×   | Sprint 11b mutex baseline                                            |
| 2026-05-18 | 16      | release | 14 933 ms  | 15 348 ms  | 1.03×   | OptLevel=2; mutex still dominant — opt level doesn't help            |
| 2026-05-18 | 11c     | release |    660 ms  |    915 ms  | 1.39×   | Lock-free roots; 17–22× faster overall; dispatch differential visible |
| 2026-05-18 | 11c     | debug   |  7 100 ms  |  7 700 ms  | 1.09×   | Debug Rust runtime cost dominates; ratio compresses                  |

## Interpretation

The sealed variant resolves every `run-task(t :: <task>, …)` call site at compile time via Sprint 15's dispatch-resolution pass: each of `step`'s 16 calls receives a `Class(<idler>)` / `Class(<worker>)` / … specifier from the static parameter types, and the resolver emits a direct `call @run-task$<class>` with no cache check and no `nod_dispatch` indirection. The slot-accessor dispatches inside each method body resolve the same way.

The open variant goes through Sprint 13's monomorphic inline cache: each call site loads the receiver's class id from its wrapper, compares against the cached id and generation, branches to the cached method or `nod_dispatch`. Because each call site sees the same receiver class across iterations, the cache is fully monomorphic — 64 million cache hits demonstrate the IC is working as designed.

The Sprint 11c lock-free roots removed the dominant per-call mutex cost. Both variants are dramatically faster (~17–22× end-to-end); the dispatch differential, previously masked, is now ~1.4× in release.

## Future expected progressions (aspirational, not gates)

These should each move the ratio up, measurable by appending to the History table when they land:

- **Sprint 11d / 19 — `gc.statepoint` precise roots.** Eliminates per-call `nod_register_root` / `nod_unregister_root` extern calls; LLVM keeps live values in registers across calls. Should both lower the absolute time and widen the sealed-vs-open gap.
- **Sprint 18 — LLVM optimisation passes.** Cross-function inlining within the JIT module; sealed-direct calls become inlinable (currently they jump to a function-pointer-resolved symbol that LLVM can't see through). Probably the biggest single perf unlock when it lands.
- **Sprint 28 — Multi-threaded mutator.** Affects throughput on concurrent workloads, not the single-thread ratio measured here, but worth tracking separately.

## Reproducing

```text
cargo test -p nod-tests --test bench_richards -- --ignored \
    bench_richards_speedup --nocapture                       # debug

cargo test -p nod-tests --release --test bench_richards -- --ignored \
    bench_richards_speedup --nocapture                       # release
```

The test is `#[ignore]` by default so `cargo test --workspace` doesn't pay the bench cost on every run. The assertion is `ratio >= 0.95` — a mode-agnostic regression guard.
