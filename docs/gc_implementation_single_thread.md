# GC Implementation — Single-Threaded Correctness

*Status as of Sprint 45f (metrics + safepoint setters). All components described here are committed and green.*

---

## Overview

NewOpenDylan uses a **precise, generational copying collector**.  The collector
is triggered synchronously at allocation sites — no concurrent marking, no
background threads.  For single-threaded code the mutator and collector run
in strict alternation: the mutator allocates until the young generation fills,
the collector runs to completion, then the mutator resumes with updated
pointers.

Two codegen surfaces share the same collector but use different root-reporting
mechanisms:

| Surface | How roots are reported |
|---|---|
| **JIT** (in-memory code text) | Slot-slab alloca + runtime registry keyed on `(namespace, site_id)` |
| **AOT / Image** (compiled bitcode) | Slot-slab alloca + inline `root_count` passed at each call site |

---

## 1. The Allocation Trigger

Every Dylan `make` expression, closure allocation, pair allocation, etc.
compiles down to a call to a runtime allocator (e.g. `nod_make`,
`nod_make_closure`, `nod_pair_alloc`).  All of these ultimately hit the heap
fast-path:

```
nod_make(class, kw_count, ...) {
    if young.top + size > young.end {
        heap.collect_minor()   // ← GC runs here
    }
    bump-allocate and return
}
```

Because the GC runs *inside* the allocator call, the Dylan function that
called `nod_make` is still on the stack.  Its live pointer-shaped locals must
be visible to the collector — this is the safepoint problem.

---

## 2. The Safepoint Model

### 2a. The Slot-Slab

Each compiled Dylan function owns a **slot slab**: a single stack-allocated
array of `Word` values, created in the function's entry block.  The slab is
sized to `max_safepoint_slots(func)` — the maximum number of pointer-shaped
temps live at any single call site in the function.

```
┌─────────────────────────────────────────────────────────────┐
│  Dylan function stack frame                                  │
│  ┌──────────────────────────────────────────────┐           │
│  │  slot_slab: [Word; max_safepoint_slots]       │  ← alloca │
│  │   [0]  [1]  [2]  ...                         │           │
│  └──────────────────────────────────────────────┘           │
│  slot_base = &slot_slab[0]                                   │
└─────────────────────────────────────────────────────────────┘
```

Before each allocating call, codegen **spills** every live pointer-shaped temp
into the slab:

```llvm
store t0, slot_base+0
store t1, slot_base+1
call nod_jit_begin_safepoint(namespace, site_id, slot_base)
call nod_make(...)
call nod_jit_end_safepoint(slot_base)
t0' = load slot_base+0      ; ← may be a new address after GC
t1' = load slot_base+1
```

After the call, `end_safepoint` pops the frame and the reloaded values replace
the SSA temps.  Any subsequent use of `t0` in the function actually uses
`t0'` — the post-relocation address.

### 2b. JIT safepoint registration

The runtime needs to know *which slots* in the slab hold live roots for a
given call site.  This mapping is registered once when the module is installed:

```
add_module(output) {
    register_jit_safepoints([
        JitSafepointEntry { namespace, site_id, slots: [0, 1] },
        JitSafepointEntry { namespace, site_id, slots: [0]    },
        ...
    ])
}
```

`slots` is the list of slab indices live at that particular call.  The data
comes from the liveness analysis (§3) and is baked into the module IR as
named globals (`nod_jit_safepoint__<ns>__<site>__<slots>`) for the
bitcode-replay path.

### 2c. AOT safepoint registration

AOT code calls `nod_aot_begin_safepoint(site_id, root_count, slot_base)`
directly.  The `root_count` is a compile-time constant — no registry lookup
needed.  `snapshot_active_aot_roots()` reads the first `root_count` slots
from `slot_base`.

---

## 3. Liveness Analysis

The `populate_safepoint_roots` pass (in `nod-dfm/src/liveness.rs`) runs after
DFM lowering and fills in the `safepoint_roots: Vec<TempId>` field on every
call-shaped `Computation`.

Algorithm (per block):

```
for each block B:
    def_index[t]       = index of the computation that defines t
                         (-1 for function/block params)
    last_use_index[t]  = last index in B at which t appears as an operand
                         len     if t appears in the terminator
                         len+1   if t is used in any other block (escapes)

    for each call at index c:
        safepoint_roots[c] = {
            t  :  def_index[t] < c  ≤  last_use_index[t]
               AND  t is pointer-shaped
        }
```

The "escapes block" approximation is conservative: a temp defined in block A
and used anywhere in block B is treated as live until the end of A.  This
over-protects but never under-protects.

---

## 4. Data Flow at GC Time

The following diagram traces exactly what happens when a Dylan function calls
`nod_make` and the young generation is full.

```
Dylan function (JIT)
│
├─ spill t0 → slot_base[0]
├─ spill t1 → slot_base[1]
├─ nod_jit_begin_safepoint(ns, site_id, slot_base)
│     └─ pushes ActiveJitSafepoint { ns, site_id, slot_base }
│           onto ACTIVE_JIT_SAFEPOINTS (thread-local)
│
├─ call nod_make(...)
│     └─ young generation full → collect_minor()
│           │
│           ├─ snapshot_roots()
│           │     ├─ ROOT_STACK    (Rust-side RootGuard slots)
│           │     ├─ snapshot_active_jit_roots()
│           │     │     └─ for each frame in ACTIVE_JIT_SAFEPOINTS:
│           │     │           lookup (ns, site_id) → slots: [0, 1]
│           │     │           yield &slot_base[0], &slot_base[1]
│           │     └─ snapshot_active_aot_roots()
│           │           └─ (empty in pure-JIT program)
│           │
│           └─ run_minor(roots)
│                 for each root ptr → *ptr = forward(*ptr)
│                 ┌─────────────────────────────────────────┐
│                 │  slot_base[0]: 0xOLD_A  →  0xNEW_A      │
│                 │  slot_base[1]: 0xOLD_B  →  0xNEW_B      │
│                 └─────────────────────────────────────────┘
│
├─ nod_jit_end_safepoint(slot_base)
│     └─ pops frame from ACTIVE_JIT_SAFEPOINTS
│
├─ t0' = load slot_base[0]   →  0xNEW_A  ✓
└─ t1' = load slot_base[1]   →  0xNEW_B  ✓
```

---

## 5. Call-Stack Nesting

Multiple levels of Dylan function calls each push their own frame onto
`ACTIVE_JIT_SAFEPOINTS`.  Every frame has its own `slot_base` (a different
stack frame's alloca), so they don't interfere.

```
call stack                  ACTIVE_JIT_SAFEPOINTS (thread-local stack)
──────────────────          ────────────────────────────────────────────
  foo (outer)               [ { ns, site=1, slot_base=&foo_slab } ]
    bar (inner)             [ { ns, site=1, slot_base=&foo_slab },
                               { ns, site=3, slot_base=&bar_slab } ]
      nod_make              snapshot_roots sees BOTH frames
        collect_minor       roots from foo_slab AND bar_slab
```

When `collect_minor` returns, both slabs have been updated in-place.
`bar` reloads from its slab, returns to `foo`, which reloads from its slab.

---

## 6. Root Sources Summary

```
snapshot_roots()
│
├── ROOT_STACK  (thread-local Vec<*const Word>)
│     Rust-side allocations protected by RootGuard.
│     Used by: nod-runtime internals, C-callback trampolines.
│
├── snapshot_active_jit_roots()
│     Reads ACTIVE_JIT_SAFEPOINTS thread-local.
│     For each active frame: looks up slot indices in jit_safepoint_registry,
│     yields pointers into the frame's slot slab.
│
└── snapshot_active_aot_roots()
      Reads ACTIVE_AOT_SAFEPOINTS thread-local.
      For each active frame: yields slot_base[0..root_count].
```

---

## 7. Post-GC SSA Reload

After `end_safepoint`, the old SSA values (`t0`, `t1`) are dead.  Codegen
replaces them in the `temps` map with the reloaded values:

```rust
// in end_safepoint:
for slot_info in rented.iter() {
    let reloaded = builder.build_load(i64ty, slot_info.slot, "gc.reload");
    temps.insert(slot_info.temp, reloaded);
}
```

Any IR that textually follows the call automatically picks up the reloaded
value because SSA resolution happens through the `temps` map.

---

## 8. Safepoint Polls (Loop Safety)

Sprint 45e added safepoint polls at function entry and loop-header blocks:

```rust
if b.id == func.entry || loop_headers.contains(&b.id) {
    emit_safepoint_poll();
}
```

`nod_safepoint_poll()` checks a process-global `SAFEPOINT_PARK_REQUESTED`
flag (relaxed load, branch-predicted not-taken).  For single-threaded code
this is currently always a no-op — the GC runs synchronously inside
`nod_make`, not via external park request.  The polls are the infrastructure
for future stop-the-world multi-threaded collection.

### 8b. Lessons Learned and Key Points

Sprint 45e's `gc-rope-file-load` verifier failures exposed three practical
rules for the current single-threaded safepoint implementation:

1. **Lowering block creation order matters because codegen still seeds
  `block_entry_temps` in linear `func.blocks` order.**  If an outer join or
  loop-exit block is appended before the nested block that actually feeds it,
  codegen can snapshot stale SSA state and later reuse a non-dominating
  reload.

2. **Outer merge blocks must be created after nested control flow is lowered.**
  This now applies to both `lower_if` and `lower_short_circuit`: the outer
  `join` / `sc_join` block has to come after any arm-local / rhs-local nested
  joins, otherwise the merge point can be emitted before its real
  predecessors have populated entry temps.

3. **Loop headers must carry all GC-managed env bindings, not just syntactically
  assigned or used names.**  A body safepoint may relocate a root that the
  source loop never mentions directly but post-loop code still reads.  If that
  binding is not threaded through the loop-header phi set, loop-exit code can
  inherit a body-local join temp instead of a dominating header value.

4. **The deciding regression surface was Dylan, not a Rust-only harness.**
  The concrete failing shape was `rope-line-to-offset` inside
  `gc-rope-file-load.dylan`, where an inner `if` in a loop body plus a
  safepoint-emitting call left a post-loop `if` reading a non-dominating temp.
  The fix is therefore only considered validated when the Dylan workload goes
  green end-to-end.

---

## 9. What Is NOT Yet Done

| Item | Notes |
|---|---|
| ~~`RtlAddFunctionTable`~~ | **DONE** — fully implemented in `jit_mm.rs`. The custom MCJIT memory manager calls `VirtualAlloc`, sorts pdata, and calls `RtlAddFunctionTable` on module finalize. Env var `NOD_TRACE_SEH` enables tracing. |
| Multi-threaded stop-the-world | Polls are wired; `SAFEPOINT_PARK_REQUESTED` now has `safepoint_request_stop()` / `safepoint_resume()` Rust API and `nod_safepoint_request_stop` / `nod_safepoint_resume` C-ABI wrappers. The full park/unpark protocol for multiple mutator threads is not yet exercised. |
| Inline allocation fast-path | `nod_make` always takes the runtime call path. No TLAB bump inline in JIT code yet. |
| Pinned/large-object roots | Large objects are handled by the `newgc-backend`; pinning protocol not yet exercised by Dylan code. |

---

## 10. GC Tracing and Metrics

### 10a. Metrics Collected

Every GC cycle updates the following counters in `HeapStats` (inside `heap.rs`):

| Field | Description |
|---|---|
| `minor_collections` | Total minor GC cycles fired |
| `major_collections` | Total major/full GC cycles fired |
| `young_bytes_allocated` | Cumulative bytes bump-allocated in young gen |
| `last_minor_pause_ns` | Wall-clock duration of the most recent minor GC (ns) |
| `last_major_pause_ns` | Wall-clock duration of the most recent major GC (ns) |
| `total_minor_pause_ns` | Cumulative minor GC wall-clock time (ns) |
| `total_major_pause_ns` | Cumulative major GC wall-clock time (ns) |
| `roots_at_last_minor` | Root-slot count snapshotted at the last minor GC |
| `roots_at_last_major` | Root-slot count snapshotted at the last major GC |
| `bytes_promoted` | Cumulative bytes promoted from young to old |
| `last_pinned_objects` | Conservative-pin count (always 0 in newgc-backend) |

These fields flow through `HeapStatsSnapshot` → `GcStats` → `gc_stats_report()`.

### 10b. Trace Output

When the `GC_TRACE_ENABLED` flag is set (driver `--gc-trace` flag calls
`set_gc_trace(true)`), both backends emit a line to stderr at each collection:

```
[GC minor #3] roots=12 promoted=8192B pause=47µs (total 134µs)
[GC major #1] roots=9 pause=210µs (total 210µs)
```

The trace output is produced inside `collect_minor`/`collect_full` by a
guarded `if crate::gc_trace_enabled() { eprintln!(...) }` block.

### 10c. `gc_stats_report()` Format

`gc_stats_report()` returns a multi-line string with all counters.  The shape
is stable and safe for `contains()` assertions in tests:

```
GC stats (backend = page-mark-evacuate)
  minor collections : 3
  major collections : 1
  young allocated   : 32768 bytes
  young live        : 0 bytes
  old live          : 16384 bytes
  last minor pause  : 47000 ns
  last major pause  : 210000 ns
  total minor pause : 134000 ns
  total major pause : 210000 ns
  roots last minor  : 12
  roots last major  : 9
  bytes promoted    : 24576 bytes
  last pinned objs  : 0
```

---

## 11. Callbacks and the GC

### 11a. Callback Architecture

Windows-ABI callbacks from the OS (e.g. `SetWindowsHookEx`, `EnumWindows`,
timer callbacks) cannot block at safepoints.  The runtime maintains a
**32-slot pool** per Dylan callback signature:

```
Registry {
    closures: Box<[UnsafeCell<Word>; 32]>,   // slab pinned on the heap
    occupied: [bool; 32],
}
```

Each `UnsafeCell<Word>` slot holds a tagged Dylan closure value.  The slab
itself is a `Box`-allocated heap object; its base pointer never moves.

### 11b. Root Registration

Per-thread, at the first dispatch through a given signature, the runtime calls
`install_gc_roots_for_this_thread(sig, registry)` which iterates all 32 slot
addresses and calls `crate::heap::register_root(slot)` for each.  The slot
addresses join the thread-local `ROOT_STACK`, so `snapshot_roots()` will
yield them at every subsequent GC.

Because the slots are registered *by address* (not by value), the GC traces
and updates whichever closure currently occupies a slot — empty slots hold the
nil immediate (`0 | TAG_IMM`), which the GC ignores (non-pointer tag).

### 11c. Sprint 11d Tenure Workaround

**Problem:** A closure dispatched via a callback lives in the JIT body's
register file during the dispatch, not in a GC-rooted slot slab.  If a minor
GC fires *between* loading the closure from the registry slot and entering the
JIT dispatch trampoline, the environment pointer held in a JIT register could
be forwarded without the JIT code seeing the new address.

**Workaround (Sprint 11d):** When `CALLBACK_TENURE_ENABLED` is set (AOT mode
only — set by `nod_aot_main_wrapper`), `register_callback` calls
`heap.collect_full()` immediately after storing the closure.  A full GC
promotes all objects reachable from the closure (the `<closure>` header, its
`<environment>`, and all captured `<cell>` objects) into the Tenured
generation.  Minor GC never moves Tenured objects, so the closure remains
at a stable address for the lifetime of the process.

```
register_callback(closure_val) {
    slot = find_free_slot();
    slot.write(closure_val);
    if CALLBACK_TENURE_ENABLED {
        heap.collect_full();   // tenure entire closure graph
    }
}
```

**Structural fix (future work):** Verify that the JIT dispatch trampoline
correctly brackets `env_ptr` as a root in a slot slab before invoking the
JIT body.  Once confirmed, the `collect_full` workaround can be removed.

### 11d. Per-Thread Lazy Root Installation

The `install_gc_roots_for_this_thread` call is guarded by a `thread_local!`
`HashSet<Sig>` so it fires at most once per (signature, thread) pair.  This
means callbacks that are only ever dispatched from one OS thread incur only
one registration overhead, regardless of how many callbacks are registered.

---

## 12. Closures and the GC

### 12a. Closure Object Layout

A Dylan closure compiles to a three-object graph on the heap:

```
<closure> [ fn_ptr | env_ptr | arity ]
              │
              └── <environment> [ cell[0] | cell[1] | … | cell[N-1] ]
                                      │
                                      └── <cell> [ current_value ]
```

All three object types are allocated in the young generation.  All three are
traced by the collector via their headers (class IDs identify field counts).

### 12b. Root Paths

- **JIT closures created in a function body:** The `closure_val` is stored
  into the calling function's slot slab before any subsequent safepoint, so
  it is a live root for any GC that fires after creation.
- **Closures captured by outer closures:** The environment's cell slots hold
  tagged `Word` values; the collector traces through them transitively.
- **Closures stored in callback registry slots:** Registered via
  `register_root`; see §11b.

### 12c. Cell Mutation Write Barriers

When Dylan code mutates a captured variable (`cell.value := new`), codegen
emits a write to `cell.value` via a `nod_cell_set` runtime call.  In the
current semispace backend, no write barrier is needed for young→young writes
because minor GC copies the entire young generation.

In the `newgc-backend` (generational), writes from old-gen objects into
young-gen objects would require a **card-mark write barrier** to ensure the
old→young pointer is recorded.  As of Sprint 45e, Dylan code creates closures
and cells in young gen and they tenure together at the next major GC.
Old-generation cells mutated to point at freshly-allocated young objects is
a future concern — the newgc-backend has card infrastructure but Dylan codegen
does not yet emit card-mark stores.

---
---

## 13. Component Map

```
nod-sema/lower.rs
  └─ populate_safepoint_roots(f)          ← liveness analysis fills safepoint_roots fields

nod-llvm/codegen.rs
  ├─ begin_emitted_safepoint()            ← spills roots into slot slab, calls nod_jit_begin_safepoint
  ├─ end_emitted_safepoint()             ← calls nod_jit_end_safepoint, reloads SSA temps
  ├─ find_loop_headers()                  ← detects back-edges by block-index comparison
  └─ emit_safepoint_poll()                ← emits nod_safepoint_poll call at entry + loop headers

nod-llvm/jit_mm.rs
  └─ JitMemoryManager::finalize_memory()  ← VirtualAlloc sections, sort pdata, RtlAddFunctionTable
                                           (NOD_TRACE_SEH env var enables tracing)

nod-llvm/jit.rs
  └─ add_module()
       └─ register_jit_safepoints()       ← installs (namespace, site_id) → slots mapping

nod-runtime/stack_map.rs
  ├─ ACTIVE_JIT_SAFEPOINTS               ← thread-local push/pop stack
  ├─ jit_safepoint_registry()            ← global BTreeMap<(u64,u64), JitSafepointEntry>
  ├─ nod_jit_begin_safepoint()           ← C-ABI: push frame
  ├─ nod_jit_end_safepoint()             ← C-ABI: pop frame
  └─ snapshot_active_jit_roots()         ← called by heap.rs at GC time

nod-runtime/aot.rs
  ├─ ACTIVE_AOT_SAFEPOINTS               ← thread-local push/pop stack
  ├─ nod_aot_begin_safepoint()           ← C-ABI: push frame with root_count
  ├─ nod_aot_end_safepoint()             ← C-ABI: pop frame
  └─ snapshot_active_aot_roots()         ← called by heap.rs at GC time

nod-runtime/heap.rs
  ├─ snapshot_roots()                    ← aggregates all three root sources
  ├─ collect_minor()                     ← young-gen evacuation + trace output + metrics
  └─ collect_full()                      ← full heap evacuation + trace output + metrics

nod-runtime/safepoint_poll.rs
  ├─ nod_safepoint_poll()                ← C-ABI: check flag, spin-park if set
  ├─ safepoint_request_stop()            ← Rust API: set SAFEPOINT_PARK_REQUESTED
  ├─ safepoint_resume()                  ← Rust API: clear SAFEPOINT_PARK_REQUESTED
  ├─ nod_safepoint_request_stop()        ← C-ABI wrapper
  └─ nod_safepoint_resume()             ← C-ABI wrapper

nod-runtime/callbacks.rs
  ├─ Registry { closures, occupied }     ← 32-slot slab pinned on heap
  ├─ install_gc_roots_for_this_thread()  ← per-thread lazy root registration
  └─ register_callback()                 ← stores closure, optionally tenures it

nod-runtime/lib.rs
  ├─ GcStats                             ← public snapshot of all counters
  ├─ gc_stats()                          ← snapshot from the live heap
  ├─ gc_stats_report()                   ← human-readable multi-line string
  ├─ set_gc_trace(bool) / gc_trace_enabled() ← toggle trace output
  └─ GC_TRACE_ENABLED                    ← AtomicBool (driver: --gc-trace)

nod-runtime/safepoint_poll.rs
  ├─ SAFEPOINT_PARK_REQUESTED            ← AtomicU8, fast-path load
  └─ nod_safepoint_poll()                ← C-ABI: check flag, park if set

nod-runtime/make.rs
  └─ RootGuard / nod_register_root       ← protects Rust-side allocations
```
