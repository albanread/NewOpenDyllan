# NewGC bug report — `<environment>` reclaimed during minor GC under long-lived callback closure

**Filed by:** NewOpenDylan project (E:\NewOpenDylan\NewOpenDylan).
**Date:** 2026-05-24.
**Affected component:** NewGC page-heap, generational minor-collection scan (suspected: missing remembered-set / old-to-young write-barrier coverage at promote time).
**Reproducer:** AOT-built `nod-ide.exe` (Dylan IDE), interactive typing.
**Severity:** Hard crash under realistic workload; blocks the NewOpenDylan IDE from accepting keyboard input.

---

## 1. Symptom

A long-lived Dylan closure registered as a Win32 `WNDPROC` callback receives
keyboard input. Each keypress dispatches into Dylan code that mutates several
captured `<cell>` variables (cursor offset, rope edits, cached flat-string,
…). After a small number of keypresses (single-digit), the runtime panics:

```
[NOD_GC_DIAG] Word::as_ptr alignment guard: raw=0x756f636665722073 \
   addr=0x756f636665722072 ascii="s refcou"

thread '<unnamed>' (25168) panicked at src\nod-runtime\src\closures.rs:269:9:
nod_env_cell: env's cells slot is not a <simple-object-vector> (env raw = 0x27054af0569)
```

The `[NOD_GC_DIAG]` line is a defensive eprintln NewOpenDylan added to its
`Word::as_ptr` when it rejects a tagged-pointer-shaped Word whose post-mask
address isn't 8-aligned (see §6). The panic immediately afterwards is
`closures.rs::nod_env_cell` discovering that the `<environment>`'s `cells`
slot doesn't pass the `try_simple_object_vector` class check.

## 2. Memory forensics

The bogus Word `0x756f636665722073` decodes as ASCII "`s refcou`" (LE) —
clearly 8 bytes of byte-string payload from the IDE's loaded source file.

The env Word `0x27054af0569` is pointer-tagged; masked address `0x27054af0568`
is 8-aligned. The crash reads the `cells` slot at `env_addr + 8 = 0x27054af0570`.
The 8 bytes at that address are the bogus payload bytes.

`<byte-string>` layout in NewOpenDylan:

```text
[Wrapper 8B] [len: u32, _pad: u32 — 8B] [payload bytes, padded to 8B]
```

The payload starts at offset 16. Reading payload-ASCII at `env_addr + 8`
means `env_addr` is **16 bytes inside a recycled byte-string** — i.e. the
`env` pointer is stale. The memory that used to be an `<environment>` is
now a `<byte-string>`'s payload region.

**Conclusion: the env was reclaimed by GC, the page was reused for a
byte-string, and the stale env pointer survived in some surviving object's
slot.**

## 3. Reachability chain (what GC should have followed)

In NewOpenDylan, a Win32 callback closure is kept alive by:

1. **Callback registry root.** `nod-runtime/src/callbacks.rs` allocates a
   `Box<[UnsafeCell<Word>; 32]>` per signature, leaks it (process lifetime),
   and registers each cell's `*const Word` as a GC root via
   `crate::heap::register_root(cell_ptr)`. The closure Word lives in one
   of these cells once `register_callback` is called.

2. **Closure → environment edge.** The closure is a `<function>` instance
   (registered via `register_simple_user_class` with 6 slots: `name`,
   `arity`, `code-ptr`, `kind-tag`, `env-ptr`, `return-type`). The
   `env-ptr` slot holds a pointer-tagged `<environment>` Word. All six
   slots are scanned by `user_class_layout`, which reports
   `pointer_cells_start = 1, pointer_cells_end = total_cells` — every
   non-wrapper cell is treated as a pointer cell. (NewOpenDylan: see
   `src/nod-runtime/src/classes.rs::user_class_layout`.)

3. **Environment → cells SOV.** `<environment>` has one slot `cells:
   SlotType::Vector` holding a pointer-tagged `<simple-object-vector>`
   Word. Pointer-scanned.

4. **SOV → cells.** `<simple-object-vector>` uses
   `nod-runtime/src/classes.rs::vector_scan` which visits every element
   slot as a Word.

5. **Cells → values.** Each `<cell>` has a `value: SlotType::Object` slot,
   pointer-scanned.

Every link in this chain is, by static reading, a pointer slot the GC
should follow. The IDE's WNDPROC closure has ~13 captured cells in its
env, all reachable through chain steps 1-5.

The panic indicates that **the env at step 2's target was reclaimed** —
either step 2's link wasn't followed, or step 1's root cell wasn't
visited, or wp's `<function>` evacuation didn't rewrite env-ptr.

## 4. Hypothesis — generational soundness gap

NewGC's `cycle.rs` shows a G0 → G1 → Tenured promotion model with
`G0_PROMOTION_THRESHOLD` minor cycles before G0 objects migrate to G1.
The WNDPROC closure is created **once** at IDE startup, then survives
every collection (registry-rooted). After `G0_PROMOTION_THRESHOLD` minor
cycles it gets promoted to G1.

The `<environment>` was allocated once at the same time (one alloc per
`make_closure`). It either gets promoted along with the closure, or it
doesn't — depends on whether the promotion path walks `env-ptr` and
recursively promotes the target.

**Two failure shapes are consistent with the observed crash:**

### Hypothesis A: missing recursive promote
When wp's `<function>` is promoted to G1, only its bytes are copied —
the env at `env-ptr` stays in G0. wp's env-ptr slot in G1 still
contains the G0 address of env. Subsequent minor GCs scan G0 only;
they need to find wp.env-ptr (which is now in G1) to mark env. If
NewGC's minor scan doesn't walk G1 objects looking for G0 pointers
(no remembered set / card-table populated at promote time), env
becomes unreachable from the minor scan's POV and is reclaimed on
the next G0 cycle.

### Hypothesis B: missing write barrier on slot rewrite
Even at make_closure time, the env-ptr slot is written. If the
`alloc_object` + slot-store path doesn't go through a write barrier
that records the slot in a remembered set, the same failure mode
applies as soon as either side promotes.

Either way, the root cause looks like the **classical generational
remembered-set bug**: an old-gen object holds a young-gen pointer, the
minor collector doesn't know about it, the target is reclaimed.

## 5. What was verified vs not verified

**Verified (NewOpenDylan side):**

| Statement | Evidence |
|---|---|
| `<function>` slot 5 (env-ptr) is reported as a pointer cell by `user_class_layout` | `src/nod-runtime/src/classes.rs:415-437` — reports `(total_cells, 1, total_cells)` for any user class with >1 cell, no slot-type filtering |
| `<environment>` slot 1 (cells) is reported as a pointer cell | same |
| Callback registry cells are registered as GC roots on the dispatching thread | `src/nod-runtime/src/callbacks.rs::install_gc_roots_for_this_thread` |
| `visit_roots` rewrites registry slots on every cycle | `src/nod-runtime/src/heap.rs:1542-1563` — calls `evac.visit(&mut *ngc_slot)` for each registered slot |
| The closure's env-ptr slot is initialised with a pointer-tagged Word (bit 0 = 1, target 8-aligned) at make_closure time | `src/nod-runtime/src/functions.rs::make_function` — stores `Word::from_raw(env_word.raw())` |

**NOT verified (NewGC side — out of scope for this report):**

| Question | Where to check |
|---|---|
| Does `PageHeap::collect_minor` (or the cycle driver in `cycle.rs`) walk G1+ object slots looking for G0 pointers? | `crates/newgc-core/src/page_heap/cycle.rs::collect_minor`, `mark.rs` |
| Is a remembered set / card-table populated when a slot store crosses old→young? Is one populated when an object is promoted from G0 → G1 with G0 pointers in its slots? | `crates/newgc-core/src/page_heap/page_desc.rs::card_table`, search `remembered`/`write_barrier` |
| Does `alloc_object` + slot-init go through any cross-generation barrier? | NewOpenDylan side: `heap.rs::alloc_object`; NewGC side: how it instantiates pages |
| What `G0_PROMOTION_THRESHOLD` is, and after how many minor cycles wp is expected to promote | `crates/newgc-core/src/page_heap/cycle.rs` |

## 6. NewOpenDylan-side mitigations already in place (don't help here)

For context, NewOpenDylan has independently added defensive layers that
don't fix this bug but limit its blast radius:

- **`Word::as_ptr` alignment guard.** When a tagged-pointer-shaped Word
  has a non-8-aligned post-mask address, return `None` instead of
  dereferencing. This is what fired the `[NOD_GC_DIAG]` line above and
  caused `try_simple_object_vector` to fall through to the panic path
  cleanly rather than SEGV inside `*(p as *const Wrapper)`.
  (`src/nod-runtime/src/word.rs::as_ptr`.)

- **`NOD_GC_DIAG=1` envvar.** Enables the alignment-guard eprintln +
  backtrace capture for diagnostic runs.

- **Sprint 11b precise GC roots in JIT code.** Live Words across every
  Dylan call site are spilled to alloca slots and registered via
  `nod_register_root` / `nod_unregister_root`. This is the
  in-JIT-frame side of the contract and is working correctly here —
  the env is *not* reclaimed because of a missing in-frame root; the
  env is reclaimed because a *cross-generation* edge isn't being
  tracked.

- **WNDPROC discipline.** The Win32 callback closure is a one-line
  shell that calls a regular Dylan helper function. The shell itself
  doesn't allocate. (See `tests/nod-tests/fixtures/nod-ide.dylan`
  around line 808.) This doesn't help because the bug is in
  long-lived-closure-survives-promotion, not in immediate-allocation.

## 7. Reproducer

```powershell
# In the NewOpenDylan workspace (E:\NewOpenDylan\NewOpenDylan):
cargo run -q -p nod-driver -- build tests/nod-tests/fixtures/nod-ide.dylan -o F:/scratch/nod-ide.exe

# Copy any small Dylan/text file to F:\scratch\foo.dylan
# Run with diagnostics on:
$env:NOD_GC_DIAG = "1"
$env:RUST_BACKTRACE = "1"
F:\scratch\nod-ide.exe F:\scratch\foo.dylan

# In the IDE window: type a few characters. Crashes within ~5 keypresses.
```

The IDE allocates aggressively per keystroke (a rope-edit produces several
`<rope-node>` / `<rope-leaf>` / `<byte-string>` instances), so minor GCs
fire frequently. After 3-4 minor cycles, the WNDPROC closure has promoted
and the next minor GC reclaims its env.

NewOpenDylan side-config of interest: `DEFAULT_YOUNG_BYTES = 64 MB`,
`DEFAULT_OLD_BYTES = 192 MB` (see `src/nod-runtime/src/heap.rs`). Raising
these doesn't fix the crash — only the cycle count to first occurrence.

## 8. Suggested investigation order for the NewGC team

1. **Print or trace G0→G1 promotion events.** Confirm wp's `<function>`
   promotes within the observed keypress window. Look for
   `minors_since_g0_promote >= G0_PROMOTION_THRESHOLD` firing.

2. **At promote time, dump the promoted object's pointer slots.**
   Identify which slots point back into G0 after promotion. If any do
   (env-ptr will), check whether they're being added to any remembered
   set / card-table.

3. **At the next minor cycle, dump which old-gen objects' slots are
   re-scanned for G0 pointers.** If wp's promoted `<function>` is NOT
   in that set, the remembered-set/card-table is the gap.

4. **Inspect the `card_table` references in `page_desc.rs` and
   `space.rs`.** Is the card-table only used for the static-area? Does
   it cover heap-page slot writes? Does it cover promotion-time slot
   transfers?

5. **Cross-reference with NCL's GC.** NewCormanLisp (sibling project,
   E:\CL\NewCormanLisp) was the design source for many NewGC pieces.
   NCL's `docs/GC_LESSONS.md` Pattern 5 ("TLAB-composition bug
   invisible in unit tests because they use the direct allocator, not
   the mutator TLAB") describes a similar shape: a bug invisible in
   unit tests because the test path skips the contract that fails in
   production. The NewOpenDylan workspace tests (`gc_precise.rs`,
   `gc_stress.rs`) cover allocation+collection but don't exercise a
   long-lived closure with a long-lived registered callback root
   across many minor cycles.

## 9. Minimal regression test (suggested)

A workspace test that:

1. Creates a closure with N captured cells.
2. Stores the closure Word in a static slot registered as a GC root.
3. Runs `G0_PROMOTION_THRESHOLD + 1` minor cycles in a loop, allocating
   ~1 MB of throwaway byte-strings per cycle to force destination
   pressure.
4. After each cycle, reads the closure's env via `nod_env_cell` for
   each captured cell index and asserts the cell value round-trips.

This is the smallest standalone test that reproduces the failure mode
without dragging in Win32 / WNDPROC. If this fails, the bug is
confirmed at the runtime/GC layer alone.

## 10. Workarounds NewOpenDylan can take while NewGC is being fixed

In priority order:

1. **Force callback closures to allocate directly in Tenured.** Walk
   the closure + env + cells SOV + every cell and request tenured
   allocation. Avoids the generational interaction entirely. Long-term
   correct because callback closures genuinely are long-lived.

2. **Disable minor collection — only run major.** One-line patch in
   `src/nod-runtime/src/heap.rs::collect_minor` to call `collect_major`
   instead. Slow but correct.

3. **Pin callback closures' env + reachable graph as permanent roots.**
   Walk reachable graph at `register_callback` time and call
   `register_root` on every cell's pointer slot. Verbose but localised.

NewOpenDylan will choose one of these after this report is reviewed.

---

## Appendix A — exact NewOpenDylan files referenced

| File | Relevant code |
|---|---|
| `src/nod-runtime/src/closures.rs` | `<environment>` / `<cell>` class registration; `nod_env_cell` (panic site); `env_cells_vector` |
| `src/nod-runtime/src/functions.rs` | `<function>` class registration; `make_function`; slot layout |
| `src/nod-runtime/src/callbacks.rs` | Callback registry; `install_gc_roots_for_this_thread`; `wndproc_dispatch` |
| `src/nod-runtime/src/classes.rs` | `user_class_layout`; `SlotType` enum; class metadata |
| `src/nod-runtime/src/dylan_layout.rs` | `HeapLayout` impl bridging to NewGC |
| `src/nod-runtime/src/heap.rs` | `register_root`; `visit_roots`; `collect_minor` wrapping |
| `src/nod-runtime/src/word.rs` | Tag scheme; `as_ptr` alignment guard (NOD_GC_DIAG eprintln) |
| `tests/nod-tests/fixtures/nod-ide.dylan` | The IDE source; WNDPROC closure (line ~808) |

## Appendix B — exact crash output

```
[NOD_GC_DIAG] Word::as_ptr alignment guard: raw=0x756f636665722073 addr=0x756f636665722072 ascii="s refcou"
   0: <unknown>
   1: <unknown>
   ...
  12: CallWindowProcW
  13: IsWindowUnicode
  ...
  20: BaseThreadInitThunk
  21: RtlUserThreadStart

thread '<unnamed>' (25168) panicked at src\nod-runtime\src\closures.rs:269:9:
nod_env_cell: env's cells slot is not a <simple-object-vector> (env raw = 0x27054af0569)
stack backtrace:
note: Some details are omitted, run with `RUST_BACKTRACE=full` for a verbose backtrace.

thread '<unnamed>' (25168) panicked at /rustc/.../core/src/panicking.rs:225:5:
panic in a function that cannot unwind
```

The "panic in a function that cannot unwind" abort is because the panic
originates inside the WNDPROC `extern "system"` trampoline, which is
declared `extern "system"` (not `C-unwind`) per Win32 ABI requirements.
That's NewOpenDylan's design — the unwind-prevention is a symptom of
where the panic fires, not part of the bug.
