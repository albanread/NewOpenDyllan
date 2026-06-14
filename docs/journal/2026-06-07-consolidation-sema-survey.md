# 2026-06-07 — Consolidation: full sema-corpus survey after the 53.5 gaps closed

*Not feature work — an honest inventory. With the three rope-family
divergences closed (53.5b anon-methods, 53.5d implicit method generics,
53.5e class-returns-by-name), re-run the whole-corpus byte-match survey to
see exactly what the Dylan sema walk does and does not reproduce, lock in
anything newly matching, and pin down what's left. Companion to the
[2026-06-05 consolidation](2026-06-05-consolidation-and-plan.md).*

## The survey

Built `dylan-sema.exe`, ran it against `--parse-with-rust dump-sema` over
every top-level fixture except the four self-host bundle sources
(`dylan-lexer` / `dylan-parser` / `dylan-c3` / `dylan-sema`) and the shim.
Normalised both sides identically (CRLF→LF, per-line + whole-block trim) and
byte-compared. Outcome over the ~46 surveyed fixtures:

- **All 38 gated fixtures MATCH.** No regressions from the 53.5 work.
- **Every *ungated* MATCH is a fixture the 53.5(1) survey deliberately
  skipped** — `_tmp_until_loop` / `_tmp_while_loop` (scratch), `gap-007-repro-ir`
  / `gc_precise_two_makes_ir` (IR-dump variants of already-gated bases),
  `gc_loop_accum` (stress variant). Gating them adds maintenance surface
  (more EXE runs) for ~zero new shape coverage, so the curation stands. **The
  gate is already comprehensive over the distinct real shapes.**
- **Three fixtures DIFFER**: `dylan-c3-smoke`, `dylan-macro-smoke`, and the
  scratch `_tmp_when_macro`.

## The three divergences — both are standalone-EXE artifacts, not walk bugs

**`dylan-c3-smoke` + `dylan-macro-smoke` — the stdlib-class-return gap, and
*only* that.** Every diff line is a function returning `<stretchy-vector>`:
the oracle dumps `return=Class(<stretchy-vector>)`, the Dylan walk dumps
`return=Top`. `<stretchy-vector>` is a *stdlib* class (not `define class`d in
the module), so 53.5e's user-class logic — which keys on the module's own
class names — doesn't fire.

The instinct is "teach the walk the stdlib class names too" (a runtime
`%class-exists?` primitive). That's the **wrong layer**, for the reason 53.1
chose names-not-ids in the first place: a pure AST walk shouldn't depend on
runtime class-registration *state* to decide whether `<stretchy-vector>` is a
class. In the Sprint 54 load-bearing path the walk emits the type **name**
(a span) and the **host** — which holds the class registry — resolves it to
`Class`/`Top` via `resolve_class_id_by_name`. So this gap **dissolves at the
wire**; closing it now in the standalone EXE would be throwaway work that
re-introduces the exact state-coupling the design avoids. Left for 54.

**`_tmp_when_macro` (scratch) — the macro-expansion-before-sema gap.** The
walk is missing `fn t-false` that the oracle records: the oracle expands
macros before recording, so a macro that expands to a definition shows up;
`sema-main` runs lex→parse→collect with no expand step (the same gap noted in
53.5c). Also dissolves once sema runs after the real Dylan expander in the
integrated front-end. It's a `_tmp_` scratch fixture anyway, not a gate
target.

So **both residual gaps are structural to the standalone verify EXE** (no
host registry, no expand step) and resolve when sema becomes load-bearing —
not new walk defects.

## Bonus: the `dump-dfm` panic is gone

The 52.6-era note that `dump-dfm <file>` panics at `aot.rs:1037` (the
class-id-drift assertion) — and is therefore an unreliable health signal — is
**stale**. The Sprint-54 on-ramp class-id-drift fix
([2026-06-06](2026-06-06-shim-class-id-drift-fix.md)) killed it. Re-verified
today: `dump-dfm` exits 0 with real DFM output on `point` / `rope` /
`richards-shape` / `gc_precise_two_makes` / `dylan-macro-smoke`, on both the
default shim path and `--parse-with-rust`, zero panics. `dump-dfm` is a
trustworthy health check again. (Session memory corrected.)

## Where it leaves us

The Dylan sema recording walk is **as complete as the standalone-EXE verify
model allows**: it byte-matches the Rust oracle on every distinct real shape
in the corpus. The only two residual divergences are both wire/Sprint-54
concerns (host-side class resolution; expand-before-sema), correctly deferred
rather than papered over in the EXE. No compiler code changed this pass — the
deliverable is the verified inventory, the memory correction, and a clean
read on what 54 must pick up. Next frontier remains the load-bearing step:
the sema wire (`dylan_sema_emit` + host reconstruct + `--sema-with-dylan`
verify-mode) and the 53.1 `analyse_module` / `lower_with_model` split.
