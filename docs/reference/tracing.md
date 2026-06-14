# GC tracing and crash symbolication

This page describes the GC diagnostic infrastructure: a JSONL collection
tracer, address-watch/follow filtering, register-arg root-coverage probing, and
`.map`-file symbolication of AOT EXE backtraces. The intended use is
investigating "stale precise root" crashes — where the collector reclaims an
object that some live reference still points to — and more generally any
zeroed-wrapper or dangling-pointer crash (the headline symptom is
`stretchy_vector_push: not a <stretchy-vector>`, but the same probes work for
any crash of that shape).

The infrastructure is **inert by default**: nothing fires unless you set an env
var or read a `.map` file. Normal build/test gates are unaffected.

## `NOD_GC_TRACE` — JSONL collection tracer

- Files: `src/nod-runtime/src/gc_trace.rs` (sink) and
  `src/nod-runtime/src/heap.rs` (collection seams).
- Activation: set `NOD_GC_TRACE=<path>` in the env of the EXE that runs the
  collections (an AOT EXE or an integration-test binary — not the driver; the
  env propagates to spawned children).
- Output: one JSON object per line, flushed after every line, so an abort
  mid-cycle never loses a record.

### Event schema

| `ev` | Meaning | Fields |
| --- | --- | --- |
| `collect_begin` | start of a cycle | `seq`, `kind` (`minor`/`major`), `young_alloc` |
| `root` | one registered root slot at cycle begin | `seq`, `i`, `src` (`stack`/`jit`/`aot`/`values`), `slot`, `word` |
| `root_rewrite` | the evacuator's `visit` updated a root slot | `seq`, `slot`, `old`, `new`, `moved` (bool) |
| `rewrite` | *(if the evacuator field-hook is installed)* any pointer rewrite — roots and object payload fields | same as `root_rewrite` |
| `collect_end` | end of a cycle | `seq`, `kind`, `minor`, `major`, `young_live`, `old_live`, `promoted` |

Records sharing a `seq` belong to the same cycle.

The `root` event's `src` field distinguishes the thread root-stack, JIT
active-frame slabs, AOT active-frame slabs, and the multi-values buffer. This is
the provenance — useful for knowing which subsystem a slot belongs to.

## `NOD_GC_TRACE_WATCH` + `NOD_GC_TRACE_FOLLOW` — zoom-in filtering

Setting `NOD_GC_TRACE_WATCH=0xADDR[,0xADDR…]` restricts `root`, `root_rewrite`,
and `rewrite` emission to records that touch one of the watched addresses.
Matching is **untagged** (the low tag bit is masked), so both a tagged Word and
a bare pointer hit. Watching a *slot* address also works. `collect_begin` and
`collect_end` are always emitted as scaffolding.

Setting `NOD_GC_TRACE_FOLLOW=1` extends the watch set: any rewrite touching a
watched address adds its `old` and `new` addresses, so a move chain
(`A -> B -> C …`) stays tracked across passes and cycles without pre-listing
every relocation.

> **ASLR caveat.** Heap addresses differ per process launch. The follow seed
> must come from the **same** run. Within one run, the failure probe's stale
> print (below) is in the same process as the trace — seed from there.

## `nod-driver symbolicate` — AOT EXE address resolution

The AOT linker emits a `.map` file (the `/MAP` flag in `nod-driver build`)
alongside every EXE. AOT EXEs have no PDB, so the `.map` file is the only way to
name addresses, and `nod-driver symbolicate` automates the lookup.

A failure probe (see below) prints the EXE base address and the exact
`symbolicate` command to copy and paste. Typical usage:

```sh
# 1. The probe printed something like:
#    EXE base (GetModuleHandle NULL): 0x00007ff735b60000
#    hint: symbolicate with `nod-driver symbolicate \
#       --map <exe>.map --runtime-base 0x00007ff735b60000 < this-stderr`

# 2. Copy-paste, point --map at the EXE's adjacent .map, feed in the stderr.
PDIR=$(ls -td "$LOCALAPPDATA/Temp/nod-dylan-parser-"* | head -1)
./target/debug/nod-driver.exe symbolicate \
    --map "$PDIR/dylan-parser.exe.map" \
    --runtime-base 0x00007ff735b60000 \
    --in /tmp/crash.err
```

What it does: reads any text containing `0x` + 16 hex digits, looks each up
against the `.map`, and rewrites recognized IPs as `name+0xNN (0xIP)`. Anything
unrecognized stays raw. Output goes to stdout or `--out <file>`. A symbolicated
backtrace looks like:

```
push caller backtrace (15 frames):
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

- **Offsets greater than 4 MiB are suppressed.** Random hex values that happen
  to look like IPs (object addresses, tag bit patterns) commonly fall outside
  any symbol's range; the tool skips rewriting those rather than emit a
  meaningless `some_unrelated_sym+0x3a00000`. The threshold is hard-coded but
  generous.

- **`--runtime-base` is effectively mandatory.** EXEs built with
  `/HIGHENTROPYVA` almost never map at their preferred base. The probe prints the
  actual base; pass it through. Without it the slide is wrong and nothing
  resolves. The fallback heuristic is: take frame 0, find
  `stretchy_vector_push`'s preferred-base address in the `.map`, and subtract.

- **`--in -` defaults to stdin**, so pipes work:
  `cat crash.err | nod-driver symbolicate --map foo.map --runtime-base 0x…`.

- **Trace files work too.** `NOD_GC_TRACE` JSONL captures `slot` / `old` / `new`
  as 16-hex addresses. Symbolicate rewrites those if they fall in code (they
  generally do not — they are heap or slot addresses — but for any `slot` field
  in object headers it works).

## `NOD_DIAG_ARG_ROOT_COVERAGE` — register-arg coverage probe

`src/nod-dfm/src/liveness.rs::diagnose_arg_root_coverage` enumerates every call
site where a GC-typed *argument* is not in `safepoint_roots`. Set
`NOD_DIAG_ARG_ROOT_COVERAGE=summary` for one line per function with gaps, or
`=full` (or `=1`) for one line per gap with
`(site, callee, dst, arg, arg_position, arg_type)`.

This probe tests the hypothesis that arguments dead-after-call sail into callees
as stale register values. It is useful even when "no gaps" is the answer,
because it rules out a whole class of bugs in one command.

## `stretchy_vector_push` failure probe

`src/nod-runtime/src/collections.rs` — when push's entry-check fails, the
failure path prints:

```
stretchy_vector_push: not a <stretchy-vector>: sv=0xXXXX ptr=0xYYYY
EXE base (GetModuleHandle NULL): 0x...
push caller backtrace (N frames):
  frame  0: 0x...
  frame  1: 0x...
  ...
hint: symbolicate with `nod-driver symbolicate --map <exe>.map
                        --runtime-base 0x... < this-stderr`
```

The `sv` hex is what to seed `NOD_GC_TRACE_WATCH` with. The backtrace IPs are
captured via `RtlCaptureStackBackTrace` and symbolicate via the printed command.
The first non-runtime frame names the immediate AOT caller. This pattern is
reusable for any other stale-precise-root crash: change the panic site, leave the
structure.

## Worked example

From a clean state:

```sh
# 1. clear the JIT cache so codegen reruns with the current driver
rm -rf target/nod-jit-cache

# 2. run with full GC trace
NOD_GC_TRACE=F:/scratch/gc.jsonl \
  nod-driver dump-ast F:/scratch/example.dylan 2> /tmp/err

# 3. grab the bad sv from the panic
grep "sv=" /tmp/err
# → sv=0x000001cf568f09e9 ptr=0x000001cf568f09e8
```

To zoom in on the stale vector's lifecycle (do not re-run — addresses change per
launch; grep the same file):

```sh
grep -E '(old|new|word)":"0x[0-9a-f]+09e9' F:/scratch/gc.jsonl
# → shows every slot that ever held the vector family, in every cycle,
#   with provenance. Stack slots = registered roots; heap slots = object fields.
```

In a representative investigation this revealed that every slot that held the
vector was in the native-stack region (the AOT safepoint slabs) and none was in
the heap region (object fields) — so the residual was a missing stack-slot root,
not a slot-map or object-field issue.

To name the missing-root frame, symbolicate the panic's backtrace against the
`.map`:

```sh
PDIR=$(ls -td "$LOCALAPPDATA/Temp/nod-dylan-parser-"* | head -1)
EXE_BASE=$(grep "EXE base" /tmp/err | head -1 | grep -oE '0x[0-9a-f]+')
./target/debug/nod-driver.exe symbolicate \
    --map "$PDIR/dylan-parser.exe.map" \
    --runtime-base "$EXE_BASE" \
    --in /tmp/err
```

That gives the call chain (top to bottom of stack):

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

So the buggy frame is `dump-node` — it holds the stretchy-vector accumulator it
passes to `acc-string`, and that local is not kept registered across the
`acc-string` call that triggers a moving collection. From there the fix is a
`dump-dfm` of the fixture, finding the `acc-string` call in `dump-node`'s IR, and
inspecting its `safepoint_roots` set.

## Limitations

- **No symbols in the AOT EXE.** Backtrace IPs only symbolicate via the `.map`
  file. Sub-function granularity (line numbers, inline frames) needs `/DEBUG` +
  PDB, which is not generated yet.
- **Map symbols are sparse.** Some functions do not appear; lookups for those IPs
  land on the previous symbol with a huge `+0x…` offset. `nod-driver
  symbolicate` suppresses rewrites for offsets greater than 4 MiB to keep noise
  down, but if a real function spans more than that the lookup is skipped.
- **ASLR caveat for `NOD_GC_TRACE_WATCH`.** The follow seed must come from the
  same process. The push-failure `sv` print is in the same process as the trace —
  that is the canonical seed.
- **Adding code to `nod-runtime` is fragile.** Any new `.rs` file (or enough new
  body in an existing one) shifts Cargo's CGU partition enough to break the AOT
  link via the `aot_user_main_stub.rs` archive-extraction trick (`LNK2005:
  nod_user_main already defined`). Post-mortem helpers like the symbolicator
  therefore live in `nod-driver` instead. The constraint is documented in
  `aot_user_main_stub.rs`.
- **Object-field tracing is not committed.** The `rewrite` event's full
  object-field hook was used as a one-shot experiment and trips the same CGU edge
  case above; if a future investigation needs per-object field rewrites, the
  recipe lives with the related code.

## Related files

- `src/nod-runtime/src/gc_trace.rs` — JSONL sink + watch/follow.
- `src/nod-runtime/src/heap.rs` — collection seams (begin/end cycle, root emit,
  root-rewrite hook in `visit_roots`).
- `src/nod-runtime/src/collections.rs` — `stretchy_vector_push` failure probe
  (sv hex + EXE base + `RtlCaptureStackBackTrace`).
- `src/nod-driver/src/main.rs` — the `/MAP` linker flag and the `symbolicate`
  subcommand.
- `src/nod-dfm/src/liveness.rs` — `diagnose_arg_root_coverage`
  (`NOD_DIAG_ARG_ROOT_COVERAGE=summary|full`).

---

Reference: [Platforms](platforms.md) | [Performance](performance.md) | [Known limitations](known-limitations.md) | [Architecture](../architecture.md) | [GC](../compiler/gc.md) | [Runtime](../compiler/runtime.md) | [Glossary](../glossary.md)
