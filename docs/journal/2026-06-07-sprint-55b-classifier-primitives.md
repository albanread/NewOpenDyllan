# 2026-06-07 — Sprint 55b: classifier primitives (`%is-generic?` / `%is-class?`)

*The call-path soundness fix made the Dylan lowering bail on every non-local
callee (losing `hello`, `translate-loop`). The real fix is to let the Dylan side
QUERY the runtime: two tiny primitives that classify a name as a generic or a
class. With them the call path emits the right shape, the void rule is corrected,
and coverage jumps to 23 fixtures with zero corpus mismatches.*

## Two primitives, same pattern as the byte-string prims

- `nod_is_generic_defined(name) -> bool` (dispatch.rs) → `is_generic_defined`.
- `nod_is_class_defined(name) -> bool` (dispatch.rs) → `find_class_id_by_name`.

Each is wired the established way: a `#[no_mangle]` runtime export (decode the
`<byte-string>` Word, return `imm.true_`/`imm.false_`), a
`LOWER_PRIMITIVE_TABLE` row in lower.rs (`%is-generic?` / `%is-class?` →
the `nod_…` name, arity 1, `Boolean`), and a `RUNTIME_SYMBOLS` row + symbol const
in codegen.rs (so the shim's `DirectCall` resolves). No new codegen — the table
drives it. They're shim-only (no user code calls them), and the registry is live
by lowering time (`stdlib::ensure_loaded` precedes it), so stdlib generics
(`size`/`add!`) and builtin classes (`<stretchy-vector>`) resolve correctly.

## What they fix in the Dylan call path / typing

- **Generic vs function vs prim.** A non-`make`/`instance?` call now: known
  top-level function → `DirectCall`; else a generic (slot getter in `fb-generics`
  OR `%is-generic?`) and NOT a top-name → `Dispatch`; else a `%`-prim → bail
  (the `nod_…` name map is still deferred); else (non-generic stdlib function or
  unknown ident, e.g. `format-out` / `done`) → `DirectCall`. This restores
  `hello` + `translate-loop` AND unlocks the stdlib-generic fixtures.
- **Builtin class params.** `label-for-type-name` now types a non-scalar param
  as `<class>` when it's a user class (AST set) or a registered class
  (`%is-class?`), `<top>` for the universal `<object>` and genuinely-unknown
  types. So `v :: <stretchy-vector>` → `<class>` (was wrongly `<top>`).

## The void rule, corrected (again)

`hello`'s `main () => () format-out(...)` returns `<top>` + `Return t1` in Rust —
`=> ()` does NOT force void. The rule is: the function value is the **last
statement's** value; it's void only if that statement is void (a loop). Fixed by
resetting `last-temp` to `#f` on a void statement (so a trailing loop wins over an
earlier `let`), and dropping the wrong `defn-is-void?` check. `flood`
(`let i = 0; while … end`) → void ✓; `hello` (`format-out(...)`) → returns t1 ✓.

## Verification

Whole-corpus survey: **0 mismatches**, 23 fixtures Dylan-lowered through the flip
(was 19). Gates: `hello` + `translate-class` rejoin `PHASE0_LOWER_FIXTURES`;
`gap011-repro`/`-repro2` + `translate-loop` join `FLIP_ONLY_LOWER_FIXTURES`.
`sema_topnames` 6/6, `codegen` 8/8, `nod-dfm` 12/12, `dylan_parse_translate` +
`lexer_oracle` green, `nod-runtime` 144/144 (`--test-threads=1`; the parallel aot
safepoint-registry tests are a pre-existing global-state flake). Renamed
`lower-class-accessors`'s class `<pt>`→`<coord>` so the in-process round-trip
(which runs all PHASE0 fixtures in one process) doesn't hit a class redefinition
against `translate-class`'s `<pt>`.

## Where it leaves us

The call path is now both sound AND complete for non-method code. Remaining: the
`%`-prim → `nod_` name map (unlocks `gap-007`/`rope`/`ide` %-prim fixtures), and
`define method` / `define generic` (method bodies named `g$classid` — the
class-id-in-name problem — + the sealed resolver, for richards). Then 55c
closures/blocks.
