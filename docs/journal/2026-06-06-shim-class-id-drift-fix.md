# 2026-06-06 — Killing the shim class-id drift (the year-3 on-ramp keystone)

*The first real step of the 54–56 endgame. Before the Dylan front-end can
own sema (54) and lowering (55), the static-linked shim must register its
classes into the host process without colliding with the host's. It
didn't — `nod-driver eval` on the default (shim) path crashed on a
class-id-drift assertion. This is the long-pending #311 (host/shim
registration-conflict) and the named pre-54 prerequisite from the
[roadmap](2026-06-06-roadmap-54-56.md).*

## Goal

Make `nod_aot_register_user_class`'s drift assert stop firing when the
front-end shim's resolver runs inside a host whose class registry is
already populated. The symptom: `expected 1079 for <stream>, but already
registered at id 1076` — the shim's baked user-class ids were 3 higher
than the host's actual ids.

## What we did

Trace-driven diagnosis, then a one-spot fix:

1. **Instrumented `nod_aot_register_user_class`** (temporary
   `NOD_TRACE_CLASS_REG`) to log every `(name, expected, existing)` and,
   on a drift, dump the live user-band id→name table — returning
   idempotently so *all* drifts surfaced in one run.

2. **The data:** exactly two classes drift — `<stream>` (host 1076 / shim
   1079) and `<string-stream>` (1077 / 1080) — both by **+3**. The host
   table showed the order `…<c-wide-string>(1075), <stream>(1076),
   <string-stream>(1077), <c-ffi-error>(1078), <c-float>(1079),
   <c-double>(1080)`. So the 3 classes the shim placed *before* `<stream>`
   but the host placed *after* are exactly **`<c-float>`, `<c-double>`,
   `<c-ffi-error>`**.

3. **Root cause — divergent seed-registration order.** `nod_runtime_init`
   (the AOT/shim path, `aot.rs`) registers float c-types + `<c-ffi-error>`
   *eagerly*, right after `ensure_c_types_registered`, before the stdlib
   loads. The host JIT/eval path (`lower_module_full`, `lower.rs`)
   registered the integer c-types eagerly but **deferred** the float types
   (to first use) and `<c-ffi-error>` (to the `define c-function`
   pre-pass) — so they landed *after* the stdlib's `<stream>` define-class
   instead of before. `nod-sema/src/lib.rs:924` already documented the
   float half of this; the c-ffi-error half completed the +3.

4. **The fix (4 lines).** Add `ensure_float_types_registered()` +
   `ensure_c_ffi_error_registered()` to `lower_module_full`'s seed block,
   right after `ensure_c_types_registered()` — the same position
   `nod_runtime_init` uses. Now both paths assign identical user-band ids;
   the host puts `<stream>` at 1079, matching the shim's baked id.

`nod-driver eval "1 + 2"` (default shim path) now returns `3` instead of
panicking.

## Discovered

- **The shim didn't need rebuilding.** The fix moved the *host* to meet
  the shim's already-baked ids, not the reverse. The right repair for a
  "two sequences must agree" bug is to make them share one canonical
  order — here, the host adopting `nod_runtime_init`'s order — not to
  re-bake one side.
- **A trace that returns-instead-of-panicking is worth the extra few
  lines.** Surfacing *all* drifts in a single run (both `<stream>` and
  `<string-stream>`, with the full registry table) turned "panic on the
  first" into a complete picture immediately — the +3 pattern was obvious
  the moment both showed.
- **The real defect is duplicated seed-order knowledge.** Two places
  (`nod_runtime_init` and `lower_module_full`) independently list the seed
  registration order; they drifted. A future cleanup should give them one
  shared `register_all_seeds()` so they can't diverge again — noted, not
  done (minimal fix first).

## Where it leaves us

The keystone on-ramp task is done: the shim composes with the host
cleanly, so 54 (`lower_with_model`) and 55 (lowering in Dylan) can build
on the shim path without spurious class-id drift. Remaining on-ramp item:
finish the 53.x Dylan sema walk. Then Sprint 54 proper.
