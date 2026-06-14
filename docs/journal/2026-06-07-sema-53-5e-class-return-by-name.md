# 2026-06-07 — Sprint 53.5e: user-class return estimates, dumped by name

*53.5d left the rope family one line from byte-identical:
`empty-rope () => (r :: <rope-leaf>)` dumped `return=Class(<id>)` in the
oracle vs `return=Top` in the Dylan walk. This entry closes that line — and
fixes a latent non-determinism in the oracle while doing so — so `rope`,
`ide_rope`, and `unified_ide` finally gate.*

## The bug behind the gap

`format_sema_model` rendered a function's return estimate with
`TypeEstimate`'s `Debug`, so a class return printed as `Class(1082)` — the
**raw, process-global class-id**. That is a portability leak: every other
class reference in the dump goes through `sema_class_name` (parents, cpl,
slot origin, sealing) *precisely because* ids are process-global (the 53.1
design note). The one `return=Class(<id>)` line made the oracle's own output
non-deterministic across builds that register classes in a different order —
and handed the Dylan walk an id it has no way to reproduce (it never
registers user classes in the runtime; it only walks the AST).

So the fix isn't to teach the Dylan walk the id (that's the Sprint 54
class-id-determinism problem). It's to stop emitting the id at all.

## What

**Rust (`format_sema_model`, `nod-sema/src/lower.rs`).** Render a `Class`
return by name, like everything else in the dump; other estimates keep their
`Debug` name:

```rust
let ret = match est {
    TypeEstimate::Class(id) => format!("Class({})", sema_class_name(ClassId(*id))),
    other => format!("{other:?}"),
};
```

`empty-rope` now dumps `return=Class(<rope-leaf>)`.

**Dylan (`defn-return-estimate`, `dylan-sema.dylan`).** Match it: a return
type that names a user class estimates as `Class(<name>)`. The walk
pre-collects every `define class` name (so resolution is order-independent,
mirroring the oracle's "register all classes before lowering any body"),
then promotes a `map-type-estimate` "Top" to `Class(<name>)` when the type
name is a known user class and not `<object>` / `<top>` (which the oracle
pins to `Top`). Builtin scalars (`<integer>` → `Integer`, `<byte-string>` →
`String`, …) and unknown types are unchanged.

This deliberately covers only **user** classes — the names the Dylan walk
actually knows from the module. A return type that is a non-scalar *stdlib*
class (`<stream>`, `<vector>`, …) still estimates as `Top` here while the
oracle would say `Class(<that-class>)`; no current fixture exercises it, and
closing it needs the full runtime class registry, i.e. the Sprint 54 work.

## Why it's safe for the 35 already-gated fixtures

Both edits are no-ops for them:

- **Rust side:** a fixture with a `Class(id)` line would have diverged under
  the *old* format (oracle `Class(N)` vs the Dylan walk, which has no way to
  emit `Class(N)`), so it could not have been gated. None of the 35 have such
  a line → the rendering change touches zero of their lines.
- **Dylan side:** likewise, a fixture with a user-class *return* would have
  diverged before this change → none of the 35 has one → the new
  `Class(<name>)` branch fires for none of them.

The change only ever fires on the rope family, which is exactly where the gap
was. Slot-getter return estimates are untouched (they go through
`slot_type_to_estimate` / `slot-est`, not this path, and were already
matching).

## Verification (re-run, not trusted)

- Rebuilt `nod-driver` (the `lower.rs` change) + `dylan-sema.exe`; diffed all
  four rope-family fixtures against `--parse-with-rust dump-sema`: **all four
  FULL MATCH**. `empty-rope` reads `return=Class(<rope-leaf>)` on both sides.
- `sema_topnames` (`--ignored`): **38/38 MATCH** (added `rope`, `ide_rope`,
  `unified_ide`); 1 passed, 0 failed.
- `sema_self_host` (`--ignored`): 1 passed. `nod-sema` units: 23 passed.
  `sema_dump`: 1 passed (its `fn distance-squared arity=1` prefix assertion
  is unaffected — it never checked `return=`).

Rust change, but confined to `format_sema_model` (the `dump-sema` path only —
not codegen/lowering), so the targeted sema suite is the right guard; the
full sweep is not implicated.

## Where it leaves us

All five fixtures the 53.5(1) survey flagged are now byte-matched and gated
(38 total). The Dylan sema recording model reproduces the Rust oracle across
the whole curated corpus — functions, constants, variables, slot accessors,
generics (explicit, slot, and implicit-from-method), class hierarchies +
CPLs, sealing, anonymous-method lifting, and user-class return estimates.
The remaining sema work is the load-bearing `--sema-with-dylan` + verify-mode
step (retire the oracle crutch), then Sprint 54 (`lower_with_model`) — where
the broader class-id-by-name / determinism story continues for the cases this
corpus doesn't reach (stdlib-class returns, cross-process id stability).
