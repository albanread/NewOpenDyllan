# Standard library boundary — Rust vs Dylan policy

> **Where stdlib code lives in NewOpenDylan, and the rules that keep it
> from drifting back to Rust by default.**

## Goal

NewOpenDylan today carries a *fat runtime, thin stdlib* — most
collection, condition, dispatch, and hashing behaviour lives in
`nod-runtime` as Rust. The pragmatic trade has paid off: fast sprint
cycles, small AOT binaries, clean Rust safety story. But every
language-shaped thing added to Rust narrows the floor of what Dylan
itself can express, and pushes the long-term self-hosting + Track 3
(Factor VM backend) story further out.

**This document sets the boundary where it is today and freezes the
direction of growth.** New stdlib goes in Dylan. The Rust runtime
stays focused on what only it can do.

## Five rules

### 1. Default: new stdlib API lands in Dylan

Any new user-visible function/method/generic class goes in
`src/nod-dylan/dylan-sources/stdlib.dylan` (or a sibling `.dylan` file
as the directory grows). Rust additions are the exception, Dylan is
the default.

Baseline: `stdlib.dylan` is ~867 lines as of the policy adoption. The
target is for that number to grow over time while the Rust-side
stdlib LOC stays flat.

### 2. Legitimate reasons to add Rust code (the gate)

A new function in `src/nod-runtime/` is justified **only** if it
touches one of these categories:

| Category | What it covers |
|---|---|
| **GC integration** | Heap allocation paths, write barriers, layout decisions, safepoint coordination |
| **Safepoint / roots** | Anything coordinating with NewGC's safepoint machinery or root registry |
| **FFI / OS** | Win32 calls, COM, callbacks, file I/O, process/environment, threads |
| **Tag / layout** | Pointer-tag manipulation, class-ID lookup, slot-offset computation, immediates |
| **Atomics on shared state** | Cache slots, dispatch state, anything cross-thread |
| **Bootstrap primitives** | Things Dylan can't express because the language doesn't define them (raw memory ops, intrinsics) |

Anything outside these categories should be Dylan. **No "convenience"
Rust additions.** No "while I'm here, I'll add `format-with-separator`
to nod-runtime." If it isn't shaped like one of the categories above,
it's Dylan or it doesn't ship.

### 3. Primitives, not APIs

When Rule 2 is satisfied, expose the Rust addition as a single
`%`-prefixed primitive doing the **minimum** necessary work. The
user-visible Dylan API composes on top.

```
❌ Bad:   nod_runtime_concat_three_strings_with_sep(a, b, c, sep)
✅ Good:  %byte-string-alloc, %byte-string-copy!, %byte-string-size
          + concat3-with-sep(a, b, c, sep) in stdlib.dylan
```

Primitives are intentionally awkward to use directly. That's the
feature — it pushes ergonomic API design up to Dylan where it belongs.

### 4. Pre-flight: write the Dylan attempt first

Before adding a Rust function, write the Dylan version. If it
compiles and runs — even if slower than ideal — ship it. Fall back to
Rust only when:

- The Dylan version won't compile (a primitive is genuinely missing),
  AND
- The missing primitive maps to a Rule-2 category.

This is the most important rule operationally. It catches the most
common drift mode: "I'll just add this in Rust because it's easier."

### 5. Watch the trend

Track these line counts informally at sprint boundaries:

```
src/nod-dylan/dylan-sources/*.dylan          ← should grow
src/nod-runtime/src/collections.rs           ← flat or shrinking
src/nod-runtime/src/tables.rs                ← flat or shrinking
src/nod-runtime/src/conditions.rs            ← flat or shrinking
src/nod-runtime/src/strings.rs               ← flat or shrinking
src/nod-runtime/src/lists.rs                 ← flat or shrinking
src/nod-runtime/src/format_out.rs            ← flat or shrinking
```

Not a CI gate; just a quarterly look. If a right-column number grows
across multiple sprints, the policy isn't being followed and the
trend is the signal to investigate.

## Frozen exceptions

A small explicit list of "things that stay Rust-side even though they
could theoretically move." These are *frozen exceptions*, not
categories that grow:

- **Hash function inner loop** — performance, eventually wants SIMD,
  needs tight GC integration.
- **Dispatch cache slot atomic ops** — cross-thread coordination via
  Rust atomics.
- **Allocator hot path** — NewGC integration, allocation-and-write-
  barrier fusion.
- **`<table>` bucket-array operations** — bucket storage primitives
  need tight GC root coordination; Dylan wrappers acceptable but the
  underlying ops stay Rust.
- **`<condition>` runtime mechanics** — `nod_signal`, `nod_run_block`,
  CleanupGuard, NLX unwind path. These coordinate with Rust panic
  machinery and can't move. Condition *classes* could move; the
  signal/unwind mechanism cannot.

New entries to this list require explicit conversation, not silent
addition.

## Worked examples

**Example A — "I want `find-element` on collections."**
- Rule 4 check: writable in Dylan? Yes — `for (x in c) if (pred?(x)) return x; end; end;`.
- Rule 1: Dylan. Goes in `stdlib.dylan`.

**Example B — "I want bulk-copy between byte-strings."**
- Rule 4 check: writable in Dylan? Yes in principle (`%byte-set!`
  loop) but too slow for stdlib quality.
- Rule 2 check: bulk-byte-copy is a layout/perf primitive — qualifies.
- Rule 3: expose `%byte-string-copy!(dst, dst-start, src, src-start, count)`.
  Write `replace-subsequence!` and friends in Dylan over it.

**Example C — "I want a `<priority-queue>` class."**
- Rule 4 check: writable in Dylan? Yes — heap operations over
  `<stretchy-vector>`.
- Rule 1: Dylan. New file `src/nod-dylan/dylan-sources/priority-queue.dylan`.
- The fact that nod-runtime *could* have an efficient binary-heap
  doesn't matter — the Dylan version is fine. If benchmarks later
  prove a hot path needs Rust, that's a deliberate Rule-2 escalation,
  not a default.

**Example D — "I want `current-thread-id`."**
- Rule 2 check: OS-interface category, qualifies.
- Rule 3: `%current-thread-id` primitive, single OS call. Dylan-side
  wrapper if any API polish wanted.

## Migration mode (optional, later)

The five rules above stop new growth on the Rust side. They do **not**
require migrating existing Rust-side stdlib to Dylan.

Once the *direction* is set, an opt-in second pass can start migrating
piece by piece. Each migration is its own focused sprint with explicit
gates:

- "Move `concatenate` from `nod-runtime/src/collections.rs` to `stdlib.dylan`"
- "Move `<table>` element accessor Dylan-side, keep bucket-array
  primops in Rust"
- "Move `condition` class hierarchy printers from Rust to Dylan"

Each candidate gets a full test sweep gate and moves the boundary in
one direction. Not urgent; the rules above are sufficient to prevent
regression even if no migration ever happens.

## How this links to other policies

- **`docs/UPSTREAM_OPENDYLAN.md`** — when a Rule-1 Dylan addition can
  be lifted from Open Dylan instead of written from scratch, do that.
  Each lift moves behaviour from "implicitly in Rust" to "explicitly
  in Dylan stdlib," with attribution.
- **`docs/TRACKS.md`** (when written) — Track 2 (Dylan self-hosting)
  and Track 3 (Factor VM backend) both benefit directly when more
  behaviour lives in Dylan. The migration mode above feeds both
  tracks.

## Enforcement

Light-touch. No CI gate. The rules apply at:

1. **Sprint planning** — when scoping a new feature, identify which
   side it lands on. Rule 4 (pre-flight) is the gate.
2. **Code review** — Rust PRs in `nod-runtime/src/{collections,tables,
   conditions,strings,lists,format_out}.rs` get an explicit Rule-2
   check. "Does this match one of the legitimate categories?"
3. **Sprint retros / quarterly** — Rule 5 trend check on LOC counts.

If a Rust addition lands that violates the rules, the remediation is
**migration in a follow-up sprint**, not revert. The migration mode
above is the natural home for it.

## File pointers (where this policy is referenced)

- `src/nod-dylan/dylan-sources/stdlib.dylan` — header comment
- `src/nod-dylan/dylan-sources/README.md` — directory README
- `src/nod-runtime/src/lib.rs` — crate-root comment
- `src/nod-runtime/src/collections.rs` — file header
- `src/nod-runtime/src/tables.rs` — file header
- `src/nod-runtime/src/strings.rs` — file header
- `src/nod-runtime/src/conditions.rs` — file header
- `src/nod-runtime/src/lists.rs` — file header
- `src/nod-runtime/src/format_out.rs` — file header

If a new stdlib-shape Rust file is added, add a header reference too.
