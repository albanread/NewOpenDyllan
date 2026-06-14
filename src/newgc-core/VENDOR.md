# Vendored `newgc-core`

This directory is a **verbatim, in-tree copy** of the `newgc-core` crate from
the sibling NewGC project, vendored so NewOpenDylan can treat the GC as part of
one unified project — add collection tracing, and (eventually) fix GAP-011
directly rather than against a frozen git pin.

## Provenance

- **Source:** `git file:///E:/NewGC`, crate `crates/newgc-core`
- **Rev:** `15b50c6986057f8e03ee4c8d1fa3598e85ca4821`
  ("Fix conservatively-pinned objects' children dropped in the bare collect
  path") — current NewGC HEAD as of vendoring.
- **Method:** `git -C E:/NewGC archive 15b50c6 crates/newgc-core | tar -x --strip-components=2`
- **Vendored:** 2026-05-29

### Why HEAD rather than the old pin (22ec0e7)

We were previously pinned at `22ec0e7` ("Fix GAP-010: dirty-card scan corrupted
byte-string opaque payloads"). The refresh to HEAD picks up three further GC
fixes from the Lisp team:

- `9b960c3` — coordinator mark card scan reading byte-string payloads as pointers
- `6d1a799` — card pinned objects promoted in place by the Phase-3 flip (cons-elision)
- `15b50c6` — conservatively-pinned objects' children dropped in the bare collect path

The headline fix (`15b50c6`) is conservative-pin-specific; NewOpenDylan builds
`default-features = false`, which compiles the `conservative-pin` scanner out
entirely, so it likely doesn't affect us. But these are proven fixes, and
`9b960c3`/`6d1a799` touch the evacuation / card-scan paths that GAP-011's
stranded-root crash lives near — so the refresh is also a data point for that
investigation.

## Workspace integration

- Added as a member of the NewOpenDylan workspace (`src/newgc-core`).
- The crate's `Cargo.toml` inherits `edition` / `license` / `lints` from the
  workspace. NewOpenDylan's `[workspace.package]` (edition 2024,
  `MIT OR Apache-2.0`) and `[workspace.lints.rust]`
  (`unsafe_op_in_unsafe_fn = "deny"`) match the original NewGC workspace
  exactly, so inheritance resolves to identical settings.

## Re-syncing with upstream NewGC

To adopt a newer NewGC release, re-run the `git archive` over this directory
with the new rev, update the rev recorded above, and re-run the full gate suite.
Local edits made here (e.g. the `gc-trace` feature) must be re-applied or
upstreamed.
