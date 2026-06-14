# NewOpenDylan — Engineering Journal

A running lab notebook. Where `SPRINTS.md` records *what* shipped per
sprint and the commit log records *what changed per file*, this
journal records the part that otherwise evaporates: **what we were
trying to do, how we approached it, why we chose what we chose, and
what we discovered along the way** — including the wrong turns, the
"oh, it's actually simpler" moments, and the lessons that should
outlive the session they were learned in.

The audience is us, six months from now, trying to remember why the
architecture is shaped the way it is.

## Convention

- One file per session or coherent work-arc:
  `YYYY-MM-DD-short-slug.md`.
- Index below, newest first.
- Each entry, loosely:
  1. **Goal** — what we set out to do this session.
  2. **What we did** — the arc, with commit refs.
  3. **Why** — the decisions, especially the ones we reversed.
  4. **Discovered** — the lessons. This is the part that matters
     most; be honest about surprises and dead ends.
  5. **Where it leaves us** — state + the obvious next move.
- Keep prose over bullet-spam where the reasoning is the point. This
  is a notebook, not a changelog.

## Entries

- [2026-06-10 — Sprint 56c-T: the methods table becomes a checked input](2026-06-10-sprint-56c-methods-table-verify.md)
  — The Dylan lowering emits a `=== methods ===` section (slot accessors + user methods,
  walk order, specialisers by name), verified against the Rust `methods` table at the
  dfm-dump seam (`split_once` peels it off before `parse_dfm_module`). Finding: user-method
  `body_fn_name` is id-encoded in Rust (`run-task$1082_1`) vs by-name in Dylan — verify
  canonicalises to by-name. 11 unit tests; point/richards-shape + 5 more byte-match.
  Precondition for consuming the methods table (retiring Rust's method build).
- [2026-06-10 — Roadmap: retiring the Rust front-end (agent-planned)](2026-06-10-roadmap-retire-rust-frontend.md)
  — The authoritative forward plan, produced by a 4-agent planning workflow (3 parallel
  code-grounded investigations + synthesis). Ordered sprints to the cutover: 56b-EXPAND
  (expander into lowering) → 56b-T/56c-T (verify the function-side tables) → 56a-WIRE/
  CONSUME (retire register_module_classes) → 56d-56f (combined flag → skip Phase-3/4 →
  default + delete Rust). Includes a cutover checklist + open questions.
- [2026-06-10 — Sprint 56b: the expander goes into the lowering path](2026-06-10-sprint-56b-expander-into-lowering.md)
  — `dylan_expand_then_lower_emit` expands Dylan-side (via the gated Sprint-52 expander)
  before the Dylan lowering, so macro fixtures reach the lowering as kernel AST instead of
  bailing. Host-seam change, no shim rebuild. Found expansion is necessary-but-not-sufficient
  (expanded forms hit `~`/`begin`/`block`); `macro-when-only` unlocks immediately. Survey 0
  mismatches. Also capped `[build] jobs = 6` to stop the workspace-test linker OOM.
- [2026-06-10 — Sprint 56 (axis-1): `if / elseif / else` in Dylan lowering](2026-06-10-sprint-56-elseif-nested-if.md)
  — `elseif` (ubiquitous) desugars to NESTED ifs, reusing the byte-matched single-if
  machinery: the elseif clause becomes a synthetic nested `if` for the else-arm.
  Gotcha caught by the whole-corpus survey: `make(<ast-statement>)` in the lowering
  broke `dylan-lower`'s own standalone dump-dfm (class out of scope) — fixed with a
  `make-if-statement` factory in the parser (a function call is tolerated, a class
  ref isn't). Unlocks `gc_loop_accum`; 0 mismatches; lowered 31→32. The survey's
  first real catch.
- [2026-06-10 — Sprint 56 (axis-1): `#(…)` list literals in Dylan lowering](2026-06-10-sprint-56-list-literals.md)
  — `#(a,b,c)` lowers to the `%nil`/`%pair-alloc` cons chain (elements source-order,
  then nil, then pairs reverse — `<class>` dsts). Unlocks `stdlib-size-call` (PHASE0).
  Trap: `dump-ast` renders it as `Call(#list,…)` but the parser builds a distinct
  `<ast-list-lit>` node — handle it at the `lower-expr` dispatch level, not in the
  call chain. Survey 0 mismatches; lowered 30→31/62.
- [2026-06-10 — Sprint 56 (axis-1): `%`-primitive lowering in Dylan](2026-06-10-sprint-56-percent-prim-lowering.md)
  — The Dylan AST→DFM lowering stops bailing on `%`-prim calls: `prim-callee` /
  `prim-arity` / `prim-result-label` mirror the Rust `LOWER_PRIMITIVE_TABLE` (127
  rows, generated from the Rust source to avoid transcription error) and emit the
  `nod_…` DirectCall. Unknown `%`-prims still bail (soundness). Closes
  `gap-007-repro` + 2 more through the flip; standalone lowered 27→30/62. Found
  `[no_alloc]` is never emitted on the live path (no `<dfm-comp>` field needed).
  Whole-corpus survey 0 mismatches; full gate suite 6/6.
- [2026-06-10 — Sprint 56a (step 1): the Dylan class derivation becomes a checked input on the live path](2026-06-10-sprint-56a-class-derivation-live-verify.md)
  — First increment of the "make classes load-bearing" centerpiece. `parse_sema_classes`
  recovers the dump's `=== classes ===` section (parents / CPL / slot layout, all by
  name); `verify_dylan_classes` asserts it matches the host's `register_module_classes`
  inside `analyse_module_from_dump`, so a Dylan-vs-Rust class-derivation divergence now
  fails the `--sema-with-dylan` compile loudly instead of being silently masked by Rust.
  Promotes the offline 53.3/53.4 byte-match oracle to a live invariant. 38/38 load-bearing
  gate MATCH; 5 pure-Rust parser unit tests. Found the dump is lossy for a *full* consume
  (no slot type-kinds / init-keywords) — retiring Rust (56b+) needs the wire to grow first.
- [2026-06-07 — Sprint 55b (B-i): class ids in the DFM dump → sealed dispatch flips](2026-06-07-sprint-55b-class-id-in-dump-Bi.md)
  — The decided crux fix: params/returns/block-params now dump `<class:N>` (via
  `type_label`, was id-dropping `<class>`); Dylan emits `<class:<name>>`, parser
  resolves at the seam. Unblocks SEALED dispatch — `richards-shape` (sealed)
  flip-matches. Chose the format change over a host re-stamp (which would mask
  Dylan param-typing bugs). Cache key bumped 2→3; translate-class/lower-slot-assign/
  richards-shape → FLIP_ONLY. 27 lowered, 0 mismatches.
- [2026-06-07 — Sprint 55b: `define method` / `define generic` (open scope)](2026-06-07-sprint-55b-define-method-open.md)
  — Method machinery: `define generic`→no-op, `define method`→body fn named
  `g$<spec>_…` (by class name; parser resolves to numeric at the seam).
  build-generic-names extended (load-bearing). Unlocks `richards-shape-open`;
  also needed list builtins (head/tail/…→%pair-*) and zero-inherited-slot
  superclasses. Sealed stays bailing (the `Class(0)` param crux → next: B-i).
  26 lowered, 0 mismatches.
- [2026-06-07 — Sprint 55b: slot-`:=` + the design/challenge roadmap](2026-06-07-sprint-55b-slot-assign-and-roadmap.md)
  — `slot(obj) := v` → `Dispatch <slot>-setter(obj,val)` (no class-id; never a
  StoreSlot at user sites). Also records the design→challenge→decide pass for the
  remaining 55 lowering: the verified sealed-param `Class(0)` crux, the
  build-generic-names gap, method-header-by-name resolution, and the decision to
  fix the crux via the `<class:N>` format change (not a host re-stamp, which
  would mask Dylan param-typing bugs). Order: slot-:= ✓ → define-method(open) →
  %-prim map → B-i crux (architect-vetoable). 24 lowered, 0 mismatches.
- [2026-06-07 — Sprint 55b: classifier primitives (`%is-generic?` / `%is-class?`)](2026-06-07-sprint-55b-classifier-primitives.md)
  — Two tiny shim-callable runtime primitives let the Dylan lowering query the
  registry: a call → Dispatch (generic) / DirectCall (function) / bail (%-prim),
  and a param → `<class>` (user/builtin class) / `<top>`. Restores hello +
  translate-loop, unlocks gap011 + builtin-class params. Also corrects the void
  rule (last-statement value, not `=> ()`). 0 corpus mismatches, 23 lowered.
- [2026-06-07 — Sprint 55b: call-path soundness (the unknown→DirectCall trap)](2026-06-07-sprint-55b-call-path-soundness.md)
  — A whole-corpus survey caught 4 NEW mismatches after make/dispatch: enabling
  `make` let the gap repros lower into a latent unsoundness — Phase-0 emitted
  `DirectCall` for ANY callee, wrong for stdlib generics (`size`→Dispatch) and
  `%`-prims (`%make-…`→`nod_…`). Fix: only DirectCall KNOWN top-level functions;
  bail other non-local callees. `hello`/`translate-loop` bail until a
  generic-name primitive lands. Invariant (0 corpus mismatches) restored. Lesson:
  the survey, not the curated gate, is the safety net.
- [2026-06-07 — Sprint 55b: make + dispatch (first class programs in Dylan)](2026-06-07-sprint-55b-make-dispatch.md)
  — `point`/`gc_precise_two_makes`/`translate-class` now lower in Dylan and
  byte-match through the flip. `make` emits `ClassMetadataPtr` BY NAME (parser
  resolves it post-registration); generic calls → `Dispatch` (host populates
  safepoints + resolves); class-typed params print bare `<class>` (the id isn't
  needed there). Only a few lines of Rust (`parse_const`); the rest is Dylan.
  Flip-only fixtures. richards still bails (needs `define method`).
- [2026-06-07 — Sprint 55a-tail: unary `-`, `define constant`, void functions](2026-06-07-sprint-55a-tail-unary-constant-void.md)
  — Three small forms: unary `-x`→NegInt/NegFloat, `define constant`→0-arg init
  fn, void (`=> ()`) functions→`<unit>`+bare Return. `kernel-arith` joins the
  text gate; `translate-loop` (void fns with loop safepoints) is the first
  FLIP-ONLY fixture — verifiable only through `--lower-with-dylan` because the
  host populates its safepoints. Establishes the text-gateable vs flip-only split.
- [2026-06-07 — Sprint 55: the lowering flip (Dylan AST→DFM is load-bearing)](2026-06-07-sprint-55-lowering-flip-load-bearing.md)
  — The keystone: `--lower-with-dylan` replaces the Rust Phase-3/4 functions with
  the Dylan `dylan-lower-emit` DFM, reconstructed host-side (`parse_dfm_module`),
  and runs the SAME back-end passes on it. Byte-identical `dump-dfm` across all 13
  lowering fixtures (new gate `dump_dfm_lower_with_dylan_byte_match`); bails fall
  back to Rust. Mirrors 54c's sema flip one stage later; dissolves the text-gate
  ceiling for the coming make/dispatch forms (same passes + same host class-ids).
- [2026-06-07 — Sprint 55b: `instance?` → TypeCheck](2026-06-07-sprint-55b-instance-typecheck.md)
  — Second id-free/pass-free 55b piece: `instance?(v, <class>)` → `TypeCheck v
  <label>` (dst <boolean>). The label is `ClassCheck::name()`, not verbatim —
  `<string>`/`<byte-string>` both print `<byte-string>`, `<vector>`/`<simple-
  object-vector>` both print `<simple-object-vector>`; everything else (incl.
  user classes) is the source name. Gate → 13. Closes the text-gateable surface
  of 55b; the rest needs the DFM wire (class-ids + post-passes).
- [2026-06-07 — Sprint 55b: slot-accessor emission (LoadSlot / StoreSlot)](2026-06-07-sprint-55b-slot-accessors.md)
  — First 55b increment: `define class` lowers its synthesized getter/setter
  accessors in Dylan, byte-identical to Rust. The id-free, pass-free island of
  55b (offsets `8+8i`, `SlotTypeKind` from the declared type). Two-pass emit
  (accessors before functions); bails on non-`<object>` super, `constant` slots,
  and generic/intrinsic calls (so `point` stays on Rust). The make/dispatch rest
  of 55b carries class-ids + post-passes — scoped to the DFM wire. Gate → 12.
- [2026-06-07 — Sprint 55a: generalize the short-circuit env-merge](2026-06-07-sprint-55a-short-circuit-env-merge.md)
  — The `|`/`&` twin of the if env-merge fix: merge set (RHS-assigned ∪ GC-typed,
  sorted, value-first), sc_edge carries pre-RHS temps + sc_rhs post-RHS, join
  created after the RHS. All three control-flow forms (if, short-circuit, loops)
  now carry the full env-merge. Verified `a|(x:=5)` + `a|(b&c)`; gate holds at 11.
- [2026-06-07 — Sprint 55a: generalize the `if` env-merge](2026-06-07-sprint-55a-if-env-merge.md)
  — Adding `:=` exposed two latent `if` bugs (confirmed vs Rust): assigned vars
  weren't threaded through the join, and the join was created before the arms
  (wrong block order for nested-control-flow arms). Fixed by giving `if` the
  loops' full env-merge: merge set = assigned-in-arms ∪ GC-typed env (sorted,
  value-param first), env snapshot/restore around arms, join created AFTER both
  arms. Gate → 11 fixtures (+ `lower-if-merge`). Short-circuit has the same
  latent shape — next.
- [2026-06-07 — Sprint 55a: `while`/`until` loops + `:=` (the env-merge)](2026-06-07-sprint-55a-loops-and-assignment.md)
  — The hardest 55a step: loop-carried vars threaded through a `loop_header`
  block-param (carried set = assigned ∪ used ∪ GC-typed env, sorted — drives
  header params + entry/back-edge args), until/while differ only in `If`
  polarity, `:=` is a pure SSA env-rebind (no computation), loops carry a `#t`
  void marker. Subagent-implemented from a recipe + exact target dumps, then
  reviewed function-by-function + independently byte-verified. Gate → 10
  fixtures (+ committed `lower-loop`). Next: generalize the env-merge to
  `if`/short-circuit (so arms can assign).
- [2026-06-07 — Sprint 55a: short-circuit `|` / `&`](2026-06-07-sprint-55a-short-circuit.md)
  — `|`/`&` aren't PrimOps; they lower to an `sc_edge`/`sc_rhs`/`sc_join`
  diamond (`|`: `If lhs edge rhs`; `&` swaps the targets), edge carries the LHS
  value, join-param is the result. Same shape/guard as `if`. Gate → 9 fixtures
  (+ committed `lower-shortcircuit`).
- [2026-06-07 — Sprint 55a: `if` — the block-parameter SSA core](2026-06-07-sprint-55a-if-control-flow.md)
  — The brutal core: `if` lowers to a then/else/join block-param-SSA diamond,
  byte-exact (block ids/labels, temp order, join merge-param, missing-else →
  `Const Bool(false)`, lattice-join param type, continuation in the join block).
  Bails to Rust on GC-typed env / elseif / `:=` (full env-merge lands with
  loops). Two shim-compile bugs caught: `cond` is a reserved keyword (→ `cnd`),
  and `make` caps at 8 keyword pairs (factored the terminator into `<dfm-term>`).
  Gate → 8 fixtures incl. unlocked `factorial` (recursion + if) and
  `jit_cache_sample_items`.
- [2026-06-07 — Sprint 55a (first forms): `let` bindings + string-debug escaping](2026-06-07-sprint-55a-let-and-strings.md)
  — Grows the Dylan lowering: multi-statement bodies + `let` (a non-captured
  let is just a name→value-temp binding, no extra computation), and Rust-`{:?}`
  string escaping in `format-dfm` (the only diff on `hello`). Lowering gate to 5
  fixtures byte-identical: + `hello`, `gap011-jcs-min-crash` (40 fns), and a new
  `lower-let` (chained lets). Next 55a form: `if` (block-param SSA core).
- [2026-06-07 — Sprint 55 Phase 0: the Dylan AST→DFM lowering scaffold](2026-06-07-sprint-55-phase0-lowering-scaffold.md)
  — Stands up `dylan-lower.dylan` (DFM structs + FunctionBuilder + lower-expr +
  a byte-exact `format-dfm`) and proves the byte-match end-to-end: `dump-dylan-dfm`
  (in-process via the `dylan-lower-emit` shim) == `dump-dfm` on the straight-line
  subset (`sprint09-add`, `mutual`). Two bugs the gate caught: no LocalEnv for
  param reads, and the `Module:` preamble being parsed as items (skip
  non-definitions like `collect-top-names`, never emit a wrong dump). Text
  transport (no DFM wire yet). New `dylan_lower_phase0_dump_dfm_byte_match` gate,
  grows form-by-form through 55a/b/c.
- [2026-06-07 — Sprint 55 plan: porting AST→DFM lowering to Dylan](2026-06-07-sprint-55-lowering-plan.md)
  — The plan for the last and densest front-end stage. Maps the target (a
  block-parameter SSA CFG; lowering emits ~8 of 10 `Computation` variants —
  never `SealedDirectCall`/`is_no_alloc`/`safepoint_roots`, the resolver +
  liveness stay Rust), the `dump-dfm` byte-match oracle (frozen formatter =
  JIT-cache key), the `--lower-with-dylan` flag + DFM wire, and the
  sub-phasing: Phase 0 scaffold (FunctionBuilder + wire + gate, trivial fns) →
  55a statements/exprs (the ~1.4k-LOC block-SSA + sorted env-merge core) → 55b
  classes/dispatch → 55c closures/blocks. ~5k LOC, multi-session; FFI/winapi
  deferred. Honest: the byte-exact temp/block-id + merge-param order is the
  brutal part — sub-gate form by form.
- [2026-06-07 — Sprint 54c: the Dylan sema goes load-bearing (`--sema-with-dylan`)](2026-06-07-sprint-54c-sema-load-bearing.md)
  — Closes Sprint 54. The back-end now consumes the Dylan-produced sema model
  instead of the Rust recompute, gated `dump-dfm` byte-identical (38/38, incl.
  unified_ide's 5277 DFM lines). A hybrid by design: the host still registers
  classes from the AST (ids are a runtime mechanism), while the Dylan walk owns
  the name-keyed recording (top-names/generics/sealing), parsed back via
  `parse_sema_dump` + `analyse_module_from_dump` and fed to
  `lower_module_full_with_model`. Opt-in (`--sema-with-dylan`), so default paths
  are untouched. Lexer + parser + macros + sema are now all Dylan, sema
  load-bearing — next is lowering itself (Sprint 55).
- [2026-06-07 — Sprint 54b: the Dylan sema walk goes in-process (shim-bundled)](2026-06-07-sprint-54b-sema-in-process-shim.md)
  — Bundle `dylan-c3.dylan` + `dylan-sema.dylan` into the statically-linked
  shim and export `dylan-sema-emit` (text model dump), so the host can run the
  Dylan sema walk IN-PROCESS — the channel 54c flips load-bearing. Zero new
  export infrastructure (AOT StaticLibrary keeps the symbol name + the shared
  resolver covers it); text transport reuses `collect-top-names` and round-trips
  losslessly (the dump is our own format). New `dump-dylan-sema` subcommand +
  `dylan_sema_in_process_byte_match` gate: **38/38 MATCH** vs the oracle,
  in-process. No regressions (parser/lexer/codegen shim paths green; `.obj`
  rebuilt with 0 redefinition lines).
- [2026-06-07 — Sprint 54a: the analyse | lower split (`analyse_module` → `SemaModel`)](2026-06-07-sprint-54a-analyse-module-split.md)
  — Sprint 54 begins: make the Dylan sema model load-bearing. 54a is the
  structural prerequisite — extract the recording phase out of the ~1000-line
  fused `lower_module_full` into `analyse_module(m) -> SemaModel` (top-names,
  generics, classes, sealing), then have lowering rebuild the same locals from
  the model so DFM construction is byte-for-byte unchanged. Two behavior-
  preserving subtleties: the shared `errors` accumulator (split into per-phase
  vecs with their early-returns intact) and sealing (compute in `analyse`,
  install at its historical point in `lower`). Verified: full sweep green bar
  the known `lexer_oracle` parallel-build flake; `sema_topnames` 38/38 doubles
  as the recording-output regression check. Sets up 54b (the wire) and 54c
  (the flip).
- [2026-06-07 — Consolidation: full sema-corpus survey after the 53.5 gaps closed](2026-06-07-consolidation-sema-survey.md)
  — Honest inventory, no feature work. Re-ran the whole-corpus byte-match
  survey: all 38 gated fixtures match, every ungated MATCH is a deliberately
  skipped transient/variant (gate is comprehensive over distinct real shapes),
  and the only DIFFERs are `dylan-c3-smoke` / `dylan-macro-smoke` (the
  stdlib-class-return gap — `<stretchy-vector>` returns) plus a scratch
  `_tmp_when_macro` (expand-before-sema). Both residual gaps are structural to
  the standalone verify EXE (no host class-registry, no expand step) and
  dissolve at the Sprint 54 wire — not walk bugs, so deferred not patched.
  Also: the old `dump-dfm` `aot.rs:1037` panic is fixed (class-id-drift fix);
  re-verified it's a reliable health signal again.
- [2026-06-07 — Sprint 53.5e: user-class return estimates, dumped by name](2026-06-07-sema-53-5e-class-return-by-name.md)
  — Closes the last rope-family divergence. `format_sema_model` rendered a
  function's class return as `Class(<raw-id>)` — a process-global id that
  leaked into the otherwise by-name dump (non-deterministic across builds, and
  unreproducible by the Dylan walk). Now it renders by name
  (`return=Class(<rope-leaf>)`) and the Dylan walk maps user-class returns to
  the same. `rope` / `ide_rope` / `unified_ide` join the gate (38 total); every
  fixture the 53.5(1) survey flagged is byte-matched. Stdlib-class returns +
  cross-process id stability remain Sprint 54.
- [2026-06-07 — Sprint 53.5d: implicit generics from bare `define method`](2026-06-07-sema-53-5d-implicit-method-generics.md)
  — Closes the second of the three rope-family divergences 53.5b uncovered.
  The oracle's `collect_generic_names` records a `generic <name>` per
  `DefineMethod` name; the Dylan walk now does too (deduped against explicit
  + slot generics). The rope family's `=== generics ===` section matches; the
  three are now **one line** from gating, blocked only by `empty-rope`'s
  `return=Class(<id>)` — a raw process-global class-id that is itself a
  portability leak in `format_sema_model` (everything else refers to classes
  by name). Fix options surfaced: render the return class by name, or wait
  for the Sprint 54 class-id work.
- [2026-06-07 — Sprint 53.5b: anonymous-method lifting (`__anon-method-N`)](2026-06-07-sema-53-5b-anon-method-lifting.md)
  — The Dylan sema walk now lifts `method (…) … end` literals in expression
  position to synthetic `__anon-method-N` top-level functions, mirroring the
  Rust `lift_anonymous_methods` pre-pass (pre-order, source-order numbering;
  arity = param count, return = `Top`). `nod-ide` byte-matches end-to-end and
  joins the gate (now 35). Ground truth corrected the 53.5(1) survey: `rope` /
  `ide_rope` / `unified_ide` carry two further, independent gaps (implicit
  generics from bare `define method`; user-class return = `Class(id)`, a
  Sprint-54 class-id-determinism concern) — so they stay ungated, scoped out
  of the anon-method work.
- [2026-06-06 — GAP-011 #2: it is a codegen WRONG-VALUE bug, not GC (diagnosis + design)](2026-06-06-gap011-2-codegen-wrong-value-diagnosis.md)
  — Diagnose-and-design pass on the second sema-walk crash. The headline is
  a correction: the `%byte-string-size` crash in `collect-top-names` is a
  **deterministic codegen miscompilation** (a temp at a control-flow merge
  bound to the wrong SSA value because lowering didn't thread it through a
  block param), **not** GC staleness — zero collections fire (tiny input),
  and the faulting address varies by ASLR. The `NOD_DIAG_MERGE_DIVERGENCE`
  detector over-reports (flags benign cases like a 40k-iteration repro that
  runs clean), so it's a screen not an oracle. Fix design: a `nod-dfm`
  `legalize_block_params` post-pass over the existing global liveness.
- [2026-06-06 — GAP-011: stretchy-vector mid-grow root staleness](2026-06-06-gap011-stretchy-vector-mid-grow-root-staleness.md)
  — Resuming the SEMA phase surfaced a real GC bug that blocked the build:
  the self-hosted Dylan lexer aborted mid-build on large files
  (`sv evacuated mid-grow`). `nod_stretchy_vector_push` rooted its vector
  via `RootGuard` and re-read it after the grow alloc, but `RootGuard` takes
  a *shared* `&Word`, so `-O2`/`-O3` reused the pre-GC register copy. Fix:
  `RootGuard::reload()` (volatile read of the registered slot). Verified —
  release parse step over all four self-host files clean, full sweep 0
  failed. (Then `dylan-sema.exe` built + ran, exposing #2 above.)
- [2026-06-06 — Killing the shim class-id drift (the year-3 on-ramp keystone)](2026-06-06-shim-class-id-drift-fix.md)
  — First step of the 54–56 endgame (the named pre-54 prerequisite, #311).
  `nod-driver eval` crashed on the default shim path: the shim's baked
  user-class ids ran 3 above the host's. Trace-driven diagnosis pinned it
  to divergent seed-registration order — the host deferred `<c-float>` /
  `<c-double>` / `<c-ffi-error>` past the stdlib's `<stream>`, while
  `nod_runtime_init` registers them eagerly. 4-line fix in
  `lower_module_full`; the shim didn't even need rebuilding.
- [2026-06-06 — Roadmap: Sprints 54–56, the last three rungs of front-end self-hosting](2026-06-06-roadmap-54-56.md)
  — Forward plan making the ratified roadmap concrete. On-ramp (finish the
  53.x Dylan sema walk + fix the shim class-id drift — the keystone risk),
  then **54** `lower_with_model` (sema goes load-bearing), **55** AST→DFM
  lowering in Dylan (last front-end stage; sub-phased 55a/b/c), **56**
  consolidation to one DFM handoff (flags retire, the deferred ~50× perf
  returns, front-end fully self-hosted).
- [2026-06-05 — Consolidation after the hacking week: state, loose ends, and the plan](2026-06-05-consolidation-and-plan.md)
  — A deliberate stabilise-and-plan pause after the fast Sprint 52–53 run.
  Inventories the loose ends (GAP-011 liveness, the `short_circuit_ops`
  hang, the unfinished 53.2 sema walk, shim class-id drift) triaged P0–P2,
  and lays out the path back to an unattended all-green sweep before
  resuming the sema port. Plus process notes for the calmer week.
- [2026-06-03 — Sema in Dylan (Sprint 53)](2026-06-03-sema-in-dylan-53.md)
  — Pipeline correction (lex→parse→expand→sema→**DFM**→lowering; DFM is the
  oddly-named CFG IR, its own stage). 53.1: captured the sema recording
  model (top-names + generics) onto LoweredModule alongside the existing
  classes + sealing; `format_sema_model` + `dump-sema` oracle + gate;
  `DYLAN_SEMA_WIRE.md`. Class refs by name not id (ids are process-global);
  sema has global side effects (single-call per process). Next: port the
  recording walk to Dylan, tractable-subset-first.
- [2026-06-02 — Porting the macro engine to Dylan (Sprint 52.1–52.5, 52.6 prep)](2026-06-02-macro-engine-to-dylan-52.md)
  — The macro expander joins the lexer and parser in being Dylan-written.
  Locus (B): expand Dylan-side before the wire, no new wire. Engine ported
  sub-task by sub-task (data model + collector, 7-kind matching,
  substitution + hygiene, multi-rule selection, fragment-level module walk
  to fixpoint), each behind a Rust-parity or hand-verified gate; five macro
  gates green. Discovered the whole-file text round-trip is fidelity-limited
  (preamble + keyword-name colon loss), so the 52.6 front-end integration
  must emit expanded tokens with synthesized spans, not text.
- [2026-06-02 — The Dylan parser is the default front-end (51e.6)](2026-06-02-parser-is-the-default.md)
  — Sprint 51e complete. With the class-id drift fixed, the Dylan parser
  flips from opt-in to the default real-pipeline front-end (`--parse-with-rust`
  opts out; Rust = fall-back + verify oracle), gated on shim availability.
  Default full sweep green (35 binaries; only environmental `ide_shell_infra`
  fails). Lexer + parser are now both Dylan; the 8 remaining corpus
  fall-backs are macro-phase work that closes in Sprint 52.
- [2026-06-02 — Shim-AOT class-id drift: a great diagnosis, a rejected fix](2026-06-02-class-id-drift-attempt-rejected.md)
  — Task #7. A delegated fix produced an excellent dual-manifestation
  diagnosis (GAP-001's stdlib `<stream>` classes made the "no stdlib
  define class" premise stale) but an implementation rejected on review:
  it masked a *self-introduced* `LNK2005` with `/FORCE:MULTIPLE`, which
  poisoned its own green-sweep. Independently verified the clean baseline
  was healthy (c3_oracle + bench_richards pass, no LNK2005) and reverted.
  Lesson: a green gate obtained by silencing an error class is not green.
- [2026-06-02 — The Dylan parser enters the real pipeline; the shim-AOT class-id drift surfaces](2026-06-02-parser-in-the-pipeline-and-the-class-id-drift.md)
  — Sprint 51e.5. `--parse-with-dylan` wired into compile/eval/build via
  a `set_parse_override` hook (mirroring the lexer), with Rust fall-back
  + verify-mode; `eval "1 + 2 * 3"` → 9 through the Dylan parser. Surfaced
  the cross-cutting blocker: firing any front-end shim registers its
  `define class`es through the shared user-class-id counter, drifting the
  AOT-baked class ids. Gates 51e.6 default + all of 52/53/54; diagnosed
  with three fix directions, deferred as a back-end fix.
- [2026-06-02 — Parser parity push: 14 → 28/36, and two traps](2026-06-02-parser-parity-push-14-to-28.md)
  — Sprint 51e. Authored the 51e–54 migration specs, then drove the
  translation gate 14→28/36 (Precedence:c ladder, comment-aware
  operator extraction, HashLit/DefineBinding kinds, definition modifiers
  + DefineGeneric). Two traps documented: fall-back reasons are
  first-reported-reason artifacts, and the gate's self-build can measure
  a stale binary. Remaining 8 fall-backs are macro-phase (Sprint 52) work.
- [2026-06-01 — The translator payoff: Paren-transparent dump, `:=` precedence, 9→14/36](2026-06-01-translator-payoff-paren-and-assign.md)
  — Sprint 51e. Cashing in the flat-precedence migration by removing the
  translator's nested-binop guard. Took two more fixes: a `Paren`-transparent
  dump formatter (grouping is in the tree shape, not the marker) and a real
  `:=`-precedence bug in the Dylan-in-Dylan parser that the byte-identical
  gate caught (`i := i + 1` was parsing as `(i := i) + 1`). Plus two stale
  C-precedence unit tests the migration had missed.
- [2026-05-31 — DRM flat precedence by default, `Precedence: c` migration bridge](2026-05-31-flat-precedence-pragma.md)
  — Sprint 51e. The translate gate exposed the Rust parser's C-style
  precedence as a real bug (Dylan is flat per the DRM). Fixing it broke
  the whole C-precedence-assuming corpus (incl. the stdlib's char
  predicates); resolved with a `Precedence:` header pragma — default
  flat, legacy files opt into `c`.
- [2026-05-31 — DylanAst → ast::Module: the parser starts replacing parse_module](2026-05-31-dylan-to-ast-translator.md)
  — Sprint 51e, fork #2. Wire enrichment for function signatures
  (kinds 25–30), the `dylan_to_ast` translator, the `--parse-with-dylan`
  flag with fall-back, and a byte-identical translation gate. `hello.dylan`
  translates byte-identically (1/34); the gate immediately caught a
  too-empty-Module bug from unspanned `Error` nodes.
- [2026-05-31 — Parser kind-coverage: the extend-and-test grind begins](2026-05-31-parser-kind-coverage.md)
  — Sprint 51e. The coverage harness drives kind-by-kind extension:
  span backfill (and the finding that unspanned Errors are leaves, not
  containers), then DefineClass/Method/Generic. 77% → 79%; `slot`
  surfaces as the next target.
- [2026-05-31 — Front-end self-hosting: the breakthrough session](2026-05-31-front-end-self-hosting.md)
  — Sprints 51b–51e. The Dylan lexer and parser go live inside the
  driver; the architecture is reframed to a Dylan front-end on a
  permanent Rust+LLVM back-end; the parser coverage harness measures
  77% baseline and produces the extend-list.
