# `nod-dylan/dylan-sources/`

The **Dylan-side standard library** for NewOpenDylan. Auto-loaded by
`nod_sema::stdlib::ensure_loaded()` before user code lowers; merged
into every user module's namespace via the AST-level merge pipeline
(Sprint 44).

## Files

- **`stdlib.dylan`** — the main hand-written stdlib. Collection ops,
  FIP wrappers, byte-string methods, `for-each` / `unless` / `when`
  macros, condition class hierarchy, dispatch helpers. ~867 lines as
  of the policy adoption.
- **`win32-constants.dylan`** — generator-emitted Win32 constants
  (extracted from `windows_api.db` by the `nod-winapi/build.rs` Sprint
  29 generator). Do not hand-edit; rebuild via the generator.

## Policy

**New stdlib API lands HERE by default**, not in `src/nod-runtime/`
(Rust). The boundary is defined by
[`docs/STDLIB_BOUNDARY.md`](../../../docs/STDLIB_BOUNDARY.md) — five
rules with one bottom line: write the Dylan version first, only fall
back to Rust when a missing primitive maps to a legitimate Rule-2
category (GC, safepoints, FFI/OS, tag/layout, atomics on shared
state, bootstrap primitives).

When lifting code from Open Dylan rather than writing from scratch,
see
[`docs/UPSTREAM_OPENDYLAN.md`](../../../docs/UPSTREAM_OPENDYLAN.md) for
the attribution workflow.

## Growth pattern

The directory is set up to grow file-by-file as the stdlib expands.
The stdlib loader picks up every `.dylan` file in this directory
automatically — no manifest needed. New stdlib chunks (e.g., a
ported `range.dylan`, a `priority-queue.dylan`, a separate
`condition-classes.dylan`) drop in as new files alongside
`stdlib.dylan`.

## License

Each file retains its own license header. Files lifted from Open
Dylan preserve the upstream notice (Functional Objects MIT-style or
Gwydion CMU license, both permissive); files written from scratch
follow the NewOpenDylan project license.
