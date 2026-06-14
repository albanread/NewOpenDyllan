# 2026-06-07 — Sprint 55b: `instance?` → TypeCheck

*The second id-free, pass-free piece of 55b. `instance?(value, <class>)` now
lowers in Dylan to a `TypeCheck`, byte-identical to Rust.*

## What

In the call path, `instance?` is intercepted before the generic/intrinsic bail:
lower the value, then emit `t = TypeCheck value <label>` with `t: <boolean>`
(dst minted last, mirroring `lower_instance_check`). The class argument must be a
bare class name; a complex type expression bails.

A `TypeCheck` is neither a call (no safepoint) nor a `Dispatch` (no resolver),
and — crucially — its class label carries **no class-id**, so it byte-matches
the plain `dump-dfm` gate directly.

## The one non-obvious bit: the label is `ClassCheck::name()`, not verbatim

Most class names print verbatim (`<integer>`, `<boolean>`, `<character>`,
`<symbol>`, `<object>`, and user classes by source name). But two builtins
**normalize** to their canonical class, because Rust maps several source names to
one `ClassCheck` variant and prints the variant's `name()`:

- `<string>` and `<byte-string>` → both print **`<byte-string>`** (variant String)
- `<vector>` and `<simple-object-vector>` → both print **`<simple-object-vector>`** (variant Vector)

Confirmed by dumping each against the Rust oracle (not assumed from the variant
list — `String::name()` could equally have been `<string>`; it isn't). Everything
else, including user classes and `<object>`, is the source name as written.

## Verification

`lower-instance.dylan` (committed): a user class (also exercising accessor
emission) plus `instance?` on `<integer>` (verbatim), `<string>` and `<vector>`
(both normalizations), `<character>` (verbatim), and the user class (by name) —
all byte-identical. Gate → **13 fixtures**; `point.dylan` still bails (its `make`
remains unported). No regressions.

## Where it leaves us

The id-free, pass-free surface of 55b — slot accessors and `instance?` — is now
complete. Everything left in 55b (`make` → `ClassMetadataPtr`, generic
`Dispatch`, slot-`:=` → setter Dispatch, the sealed resolver, safepoint roots)
carries a process-global class-id and/or a post-lowering pass, so it can't
byte-match the standalone text gate. That's the cue to build the structured DFM
wire (Dylan lowering → host reconstructs ids by name → host runs the passes) —
the 55 analogue of 54c — rather than stretch the text gate further.
