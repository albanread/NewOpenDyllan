# 2026-06-02 — Shim-AOT class-id drift: a great diagnosis, a rejected fix

*Sprint 51e/back-end. A delegated attempt at the shim-AOT class-id-drift
fix (task #7) produced an excellent root-cause diagnosis but an
implementation I rejected on review — it masked a self-introduced linker
collision with `/FORCE:MULTIPLE`, which poisoned its own validation.
Reverted to the clean checkpoint. Follows
[the drift diagnosis](2026-06-02-parser-in-the-pipeline-and-the-class-id-drift.md).*

## The diagnosis (keep this — it's correct and hard-won)

The drift has **two manifestations**, both from a premise that was
**already stale before the shim**: `aot.rs:780-788` says "stdlib carries
no `define class`," but GAP-001 added `<stream>` / `<string-stream>` as
stdlib `define class`es (`src/nod-dylan/dylan-sources/stdlib.dylan:990,993`).

- **Manifestation A — compiler-side duplicate registration.** Under
  `NOD_PARSE_WITH_DYLAN=1`, `compile_files_for_aot` runs
  `nod_runtime_init()` + `stdlib::ensure_loaded()` (`nod-sema/src/lib.rs:967-968`),
  registering `<stream>`=1079, `<string-stream>`=1080, BEFORE the first
  `parse_user_module` fires `dylan_lex_jit::init()` → the shim's
  `nod_aot_resolve_relocs`. That resolver carries baked registrations for
  its *merged* module, whose `user_classes` is prefixed by the stdlib's
  classes (`merge_modules`, lib.rs:1459-1465), so it re-registers
  `<stream>` (expected=1079) — but the registry already has it →
  `register_user_class_metadata` mints a duplicate at 1081 → the assert
  at `aot.rs:1016` fires.
- **Manifestation B — EXE-side user-id shift.** The shim's own parser
  classes (`<span>`=1081 … `<ast-generic-definition>`=1143) register
  fresh in the compiler, bumping `next_user_id`, so the user's
  `<c3-result>` *bakes* at 1144. The user EXE doesn't link the parser
  shim, so at runtime `<c3-result>` allocates at 1081 → "expected 1144,
  allocated 1081." The parser's presence in the compiler polluted the
  user program's class-id space.
- **Fix direction #1 (reset the counter) is UNSAFE**: it collides the
  user's `<c3-result>` with the shim's `<span>` at 1081 in the compiler
  registry, and `class_metadata_ptr` (linear scan, `classes.rs:662`)
  returns the first match → silent corruption. So the fix needs a
  *disjoint namespace* for shim classes (band / separate range), plus
  *idempotent* re-registration for A (if the name is already registered,
  assert the id matches and return — don't mint a duplicate), plus the
  ~11 `lower.rs` class-name-resolution sites made band-aware.

## Why the implementation was rejected

The attempt implemented the band (#2) — plausibly the right shape — but
added **`/FORCE:MULTIPLE` to the AOT EXE link** to get past an `LNK2005
nod_user_main` collision it claimed was a "pre-existing, release-only
regression." On review I verified that claim **independently** and it is
false: at the clean checkpoint `c88d322`, `c3_oracle` (the very test it
cited) **passes**, and `bench_richards` (a full AOT-compiled,
dispatch-heavy program) **passes in 26 s** — no `LNK2005`. So the linker
collision did **not** pre-exist; the band/registration change *introduced*
it (it shifted symbol emission / CGU partitioning), and `/FORCE:MULTIPLE`
*masked* it.

That is disqualifying for two reasons:
1. **`/FORCE:MULTIPLE` is a hammer** — it silences *every* duplicate-symbol
   error, not just `nod_user_main`. Shipping it would mask future real
   "two definitions" bugs anywhere in the AOT link.
2. **It poisons the validation.** The attempt's "202/0 green sweep" was
   obtained *with* `/FORCE:MULTIPLE` active, so the green light can't be
   trusted — the flag silenced the exact class of error its own change
   provoked.

## The lesson

- **Verify "pre-existing" claims independently.** A delegated fix that
  also "fixes a pre-existing issue" is a smell; stash it, rebuild clean,
  and check. Here the clean baseline was healthy, which flipped the whole
  assessment.
- **A green gate obtained by silencing an error class is not a green
  gate.** `/FORCE:MULTIPLE`, `-Wno-error`, demoted asserts, broadened
  `catch` — any of these can turn a real regression green. Reject fixes
  whose validation depends on them.
- The diagnosis was excellent and the band approach is probably right —
  the failure was the linker hammer, not the concept.

## Where it leaves us

Reverted to `c88d322` (clean, pushed; parser 28/36 + in-pipeline; AOT
baseline verified healthy). The attempt's code is archived in a labeled
git stash for reference. **Task #7 stands**, now with a sharper spec:
implement A (idempotent re-registration) + B (disjoint shim-class
namespace), make the ~11 `lower.rs` resolution sites band-aware, and
**investigate — never hammer — any `LNK2005` the change provokes**
(it points at a CGU/symbol-emission interaction worth understanding), and
**validate WITHOUT `/FORCE:MULTIPLE`.** The drift gates 51e.6 + all of
52/53/54 AOT integration, so it remains the critical-path next step.

## Correction (adversarial review, same day)

An adversarial agent was tasked to demolish this rejection. It found a real
error in the reasoning above, which I verified independently and accept:

- **"The linker collision did not pre-exist" is FALSE.** The `LNK2005
  nod_user_main` collision is a **pre-existing, documented release-mode**
  issue: `docs/manual/compiler/jit-and-aot.md:309-313`,
  `docs/manual/language/overview.md:258`,
  `docs/manual/getting-started.md:93` all record "release-mode AOT hits
  LNK2005 nod_user_main; debug works." I tested **debug** (c3_oracle,
  bench_richards pass) and wrongly rebutted the stash author's **release**
  claim — a debug-vs-release category error. (`bench_richards` is also not
  an AOT test; it's the in-process JIT path — another mischaracterisation.)
- **The proper fix for the release LNK2005 is the `codegen-units = 1`
  pin** that `jit-and-aot.md:313` *claims* nod-runtime already has — but
  `git grep codegen-units -- '*.toml'` returns **nothing**. The pin was
  never applied; the doc is aspirational. That's the real, separate bug.

**What still holds — the decision.** The stash adds `/FORCE:MULTIPLE`
*unconditionally* (no release/library guard), so it's on for the debug AOT
path too (where it's inert), and a force-linked **release** binary links
but is **non-functional** (prints nothing) — it converts a loud link error
into a silent runtime failure. So `/FORCE:MULTIPLE` is still the wrong
instrument and the change is right to not ship as-is.

**The better path (the adversary's salvage, which I adopt).** The
band + idempotency + 11-site `resolve_class_id_by_name` work is **sound and
is the hard-won part**. The minimal correct change is: take the stashed
work, **delete only the `/FORCE:MULTIPLE` line**, and re-validate in
**debug** (where the baseline is green and the release LNK2005 is
out of scope). Rejecting the *entire* change rather than that one line was
a proportionality miss. The release-AOT LNK2005 stays a separate,
pre-existing issue with its own fix (add the missing `codegen-units = 1`).

**Meta-lesson.** The original rejection reached the right *outcome* by a
*partly-wrong* argument. The adversarial pass corrected the argument and
recovered real work — exactly why challenging a consensus is worth the
tokens even when the consensus "wins."

## Resolved (committed `028f8ac`)

Adopted the salvage: applied the stashed band/idempotency/lower.rs work,
**deleted only the `/FORCE:MULTIPLE` line** (replaced with a comment
pointing at the real release fix, task #8), rebuilt the shim `.obj` so its
classes bake into the `FIRST_SHIM` band, and validated in DEBUG:

- The previously-crashing `c3_oracle`/`lexer_oracle`/`macro_engine` now
  **pass** under `NOD_PARSE_WITH_DYLAN=1` — no drift, no `LNK2005`, no
  `/FORCE:MULTIPLE`.
- Full sweep green BOTH ways (baseline and under the shim flag,
  `--test-threads=1`); every class-id-heavy suite (classes, dispatch,
  collections, conditions, gc, gc_precise, heap_objects, closures,
  first_class_functions, **bench_richards** full-AOT) passes — dispatch is
  uncorrupted with the band active. `dylan_parse_translate` 28/36
  unregressed. Only `ide_shell_infra` fails — a pre-existing environmental
  Win32/D2D access-violation (the band is off in baseline, so it cannot be
  this change), excluded as before.

The reverted attempt's stash was dropped (the work now lives in `028f8ac`).
This **unblocks 51e.6** (the parser can default once the full sweep is
green under the shim flag — it now is) and removes the shim-AOT blocker for
all of 52/53/54. The release-mode `LNK2005` remains open as task #8 (the
`codegen-units = 1` pin), independent of the front-end migration.
