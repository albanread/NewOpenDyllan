# GC tracing & AOT-crash symbolication guide

This guide describes the diagnostic infrastructure built for **GAP-011** —
specifically, how to investigate a "stale precise root" crash where the
collector reclaims an object that some live reference still points to. It is
generic to any future bug with the same shape (`stretchy_vector_push: not a
<stretchy-vector>` is the headline, but the same probes work for any
zeroed-wrapper / dangling-pointer crash).

The infrastructure is **inert by default** — nothing fires unless you set an
env var or read a `.map` file. Normal build/test gates are unaffected.

## What's built

### 1. `NOD_GC_TRACE` — JSONL collection tracer

- File: `src/nod-runtime/src/gc_trace.rs` (sink) and `src/nod-runtime/src/heap.rs` (collection seams).
- Activation: set `NOD_GC_TRACE=<path>` in the env of the EXE that runs the
  collections (the AOT parser EXE, an integration test binary — *not* the
  driver; the env propagates to spawned children).
- Output: one JSON object per line, flushed after every line (so an
  abort/`exit 9` mid-cycle never loses a record).

Events:

| `ev`              | Meaning                                                       | Fields                                                  |
| ----------------- | ------------------------------------------------------------- | ------------------------------------------------------- |
| `collect_begin`   | start of a cycle                                              | `seq`, `kind` (`minor`/`major`), `young_alloc`         |
| `root`            | one registered root slot at cycle begin                       | `seq`, `i`, `src` (`stack`/`jit`/`aot`/`values`), `slot`, `word` |
| `root_rewrite`    | the evacuator's `visit` updated a root slot                   | `seq`, `slot`, `old`, `new`, `moved` (bool)            |
| `rewrite`         | *(if the evacuator field-hook is installed)* any pointer rewrite — roots AND object payload fields | same as `root_rewrite`                                  |
| `collect_end`     | end of a cycle                                                | `seq`, `kind`, `minor`, `major`, `young_live`, `old_live`, `promoted` |

Records sharing a `seq` belong to the same cycle.

The `root` event's `src` field distinguishes thread root-stack vs JIT
active-frame slabs vs AOT active-frame slabs vs the multi-values buffer.
This is the provenance — useful for knowing which subsystem the slot belongs
to.

### 2. `NOD_GC_TRACE_WATCH` + `NOD_GC_TRACE_FOLLOW` — zoom-in filtering

Setting `NOD_GC_TRACE_WATCH=0xADDR[,0xADDR…]` restricts `root` and
`root_rewrite` (and `rewrite`) emission to records that touch one of the
watched addresses. Matching is **untagged** (the low tag bit is masked), so
both a tagged Word and a bare pointer hit. Watching a *slot* address also
works. `collect_begin`/`collect_end` are always emitted as scaffolding.

Setting `NOD_GC_TRACE_FOLLOW=1` extends the watch set: any rewrite touching a
watched address adds its `old` and `new` addresses, so a move chain
(`A→B→C…`) stays tracked across passes and cycles without pre-listing every
relocation.

> **ASLR caveat.** Heap addresses differ per process launch. The follow seed
> must come from the **same** run. Within one run, the panic's stale-`sv`
> print *is* in the same process as the trace — seed from there.

### 3. `nod-driver symbolicate` — AOT EXE address resolution

The AOT linker emits a `.map` file (`/MAP` flag in `nod-driver build`)
alongside every EXE. AOT EXEs have no PDB, so this is the only way to name
addresses — but you don't have to do it by hand any more.

The `stretchy_vector_push` failure probe (next section) prints the EXE base
address and the exact `symbolicate` command to copy-paste. Typical usage:

```sh
# 1. The probe printed something like:
#    [GAP-011] EXE base (GetModuleHandle NULL): 0x00007ff735b60000
#    [GAP-011] hint: symbolicate with `nod-driver symbolicate \
#       --map <exe>.map --runtime-base 0x00007ff735b60000 < this-stderr`

# 2. Copy-paste, point --map at the EXE's adjacent .map, feed in the stderr.
PDIR=$(ls -td "$LOCALAPPDATA/Temp/nod-dylan-parser-"* | head -1)
./target/debug/nod-driver.exe symbolicate \
    --map "$PDIR/dylan-parser.exe.map" \
    --runtime-base 0x00007ff735b60000 \
    --in /tmp/crash.err
```

What it does: reads any text containing `0x` + 16 hex digits, looks each up
against the `.map`, rewrites recognized IPs as `name+0xNN (0xIP)`. Anything
unrecognized stays raw. Output to stdout or `--out <file>`. The result for
the GAP-011 crash:

```
[GAP-011] push caller backtrace (15 frames):
  frame  0: _ZN11nod_runtime11collections20stretchy_vector_push…+0x2af (0x…)
  frame  1: nod_stretchy_vector_push+0x57 (0x…)
  frame  2: acc-string+0x144 (0x…)
  frame  3: dump-node+0x780f (0x…)
  frame  4: dump-node+0x308a (0x…)
  frame  5: dump-node+0x55d  ← recursion
  ...
  frame  9: nod_user_main+0x252
  frame 10: nod_aot_main_wrapper+0x18
  frame 11: main+0xe
```

Notes:

- **Offset > 4 MiB suppressed.** Random hex values that happen to look like
  IPs (object addresses, tag bit patterns) commonly fall outside any
  symbol's range; we skip rewriting those rather than emit a meaningless
  `some_unrelated_sym+0x3a00000`. The threshold is hard-coded but generous.

- **`--runtime-base` is mandatory IRL.** EXEs built with `/HIGHENTROPYVA`
  almost never map at their preferred base. The probe prints the actual
  base; pass it through. Without it the slide is wrong and *nothing*
  resolves — the heuristic-of-last-resort is: take frame 0, find
  `stretchy_vector_push`'s preferred-base address in the `.map`, subtract.

- **`--in -` defaults to stdin**, so pipes work: `cat crash.err | nod-driver
  symbolicate --map foo.map --runtime-base 0x…`.

- **Trace files work too.** `NOD_GC_TRACE` JSONL captures `slot` /
  `old` / `new` as 16-hex addresses. Symbolicate is happy to rewrite those
  too if they fall in code (they generally don't — they're heap or slot
  addresses — but for any `slot` field in object headers, it works).

### 4. `NOD_DIAG_ARG_ROOT_COVERAGE` — register-arg coverage probe

`src/nod-dfm/src/liveness.rs::diagnose_arg_root_coverage` enumerates every
call site where a GC-typed *argument* is NOT in `safepoint_roots`. Set
`NOD_DIAG_ARG_ROOT_COVERAGE=summary` for one line per function with
gaps, or `=full` (or `=1`) for one line per gap with
`(site, callee, dst, arg, arg_position, arg_type)`.

Why it exists: a 2026-05-30 agent-review hypothesised that args dead-after-
call were sailing into callees as stale register values. The probe was
built to test that hypothesis — and **refuted** it (1378 gaps in the
parser source; closing them all left the GAP-011 crash identical). The
probe stays as a permanent diagnostic for the next hypothesis with this
shape. Useful even when "no gaps" is the answer — it rules out a whole
class of bugs in one command.

### 5. `stretchy_vector_push` failure probe

`src/nod-runtime/src/collections.rs` — when push's entry-check fails (the
GAP-011 crash), the failure path prints:

```
[GAP-011] stretchy_vector_push: not a <stretchy-vector>: sv=0xXXXX ptr=0xYYYY
[GAP-011] EXE base (GetModuleHandle NULL): 0x...
[GAP-011] push caller backtrace (N frames):
  frame  0: 0x...
  frame  1: 0x...
  ...
[GAP-011] hint: symbolicate with `nod-driver symbolicate --map <exe>.map
                                  --runtime-base 0x... < this-stderr`
```

The `sv` hex is what to seed `NOD_GC_TRACE_WATCH` with. The backtrace IPs
(captured via `RtlCaptureStackBackTrace`) symbolicate via the printed
command. The first non-runtime frame names the immediate AOT caller.

This is reusable for any other stale-precise-root crash: change the panic
site, leave the structure.

## Worked example: GAP-011 on `jcs-40.dylan`

The headline. From a clean state:

```sh
# 1. clear caches so the parser EXE rebuilds with the current driver
rm -rf "$TEMP"/nod-dylan-parser-* target/nod-jit-cache

# 2. run with full GC trace
NOD_GC_TRACE=F:/scratch/gc.jsonl \
  nod-driver parse-dylan F:/scratch/jcs-40.dylan 2> /tmp/err

# 3. grab the bad sv from the panic
grep "GAP-011.*sv=" /tmp/err
# → sv=0x000001cf568f09e9 ptr=0x000001cf568f09e8
```

The JSONL log has ~400 records over 4 cycles. To zoom in on the stale
vector's lifecycle:

```sh
# (NOT a re-run — addresses change per launch. Grep the SAME file.)
grep -E '(old|new|word)":"0x[0-9a-f]+09e9' F:/scratch/gc.jsonl
# → shows every slot that ever held the vector family, in every cycle,
#   with provenance. Stack slots = registered roots; heap slots = object fields.
```

For the GAP-011 case this revealed: **every** slot that held the vector was
in the native-stack region (`0x71…`, the AOT safepoint slabs), **zero** in
the heap region (`0x19…`, object fields). So the residual was a missing
stack-slot root, not a slot-map / object-field issue.

To name the missing-root frame, the panic's backtrace gets symbolicated
against the `.map` via `nod-driver symbolicate`:

```sh
PDIR=$(ls -td "$LOCALAPPDATA/Temp/nod-dylan-parser-"* | head -1)
EXE_BASE=$(grep "EXE base" /tmp/err | head -1 | grep -oE '0x[0-9a-f]+')
./target/debug/nod-driver.exe symbolicate \
    --map "$PDIR/dylan-parser.exe.map" \
    --runtime-base "$EXE_BASE" \
    --in /tmp/err
```

That gives the chain (top to bottom of stack):

```
stretchy_vector_push          ← panic
nod_stretchy_vector_push      ← C-ABI shim
acc-string                    ← Dylan caller of push
dump-node (recursive)
dump-ast
nod_user_main
nod_aot_main_wrapper
main
```

So the buggy frame is `dump-node` — it holds the stretchy-vector accumulator
it passes to `acc-string`, and that local isn't kept registered across the
`acc-string` call that triggers a moving collection. From here the fix is a
`dump-dfm` of the parser fixture, finding the `acc-string` call in
`dump-node`'s IR, and inspecting its `safepoint_roots` set.

## Limitations / future work

- **No symbols in the AOT EXE.** Backtrace IPs only symbolicate via the
  `.map` file. Sub-function granularity (line numbers, inline frames) needs
  `/DEBUG` + PDB, which we don't generate yet.
- **Map symbols are sparse.** Some functions don't appear; lookups for those
  IPs land on the previous symbol with a huge `+0x…` offset. `nod-driver
  symbolicate` suppresses rewrites for offsets > 4 MiB to keep noise down,
  but if a real function spans more than that the lookup will be skipped.
- **ASLR caveat for `NOD_GC_TRACE_WATCH`.** The follow seed must come from
  the same process. The push failure's `sv` print is in the same process as
  the trace — that's the canonical seed.
- **Adding code to `nod-runtime` is fragile.** Any new `.rs` file (or
  enough body inside an existing one) shifts Cargo's CGU partition enough
  to break the AOT link via the `aot_user_main_stub.rs` archive-extraction
  trick (`LNK2005: nod_user_main already defined`). Post-mortem helpers
  like the symbolicator therefore live in `nod-driver` instead. The
  constraint is documented in `aot_user_main_stub.rs`.
- **Object-field tracing (Step 3) is documented in the GAP-011 writeup but
  the code is not currently committed.** It was used as a one-shot
  experiment to refute the "wrong slot-map" hypothesis. The hook trips the
  same CGU edge case above; if a future investigation needs per-object
  field rewrites, the recipe is in the writeup.

## Related files

- `src/nod-runtime/src/gc_trace.rs` — JSONL sink + watch/follow.
- `src/nod-runtime/src/heap.rs` — collection seams (begin/end cycle, root
  emit, root-rewrite hook in `visit_roots`).
- `src/nod-runtime/src/collections.rs` — `stretchy_vector_push` failure
  probe (sv hex + EXE base + `RtlCaptureStackBackTrace`).
- `src/nod-driver/src/main.rs` — `/MAP` linker flag *and* the
  `symbolicate` subcommand.
- `src/nod-dfm/src/liveness.rs` — `diagnose_arg_root_coverage`
  (`NOD_DIAG_ARG_ROOT_COVERAGE=summary|full`).
- `GAP-011_GC_team_writeup.md` — the investigation narrative, findings,
  refuted hypotheses.
