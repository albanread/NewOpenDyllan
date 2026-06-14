# Compiler gaps log

Living record of Dylan-language features or compiler bugs surfaced by
**dogfooding** — writing real Dylan to drive real Dylan tooling. The
mission of every dogfooding sprint (the IDE, the in-Dylan lexer, the
eventual in-Dylan parser/sema) is exactly to flush these out.

Each gap stays here until it ships a fix. Workarounds are recorded
verbatim so we can audit "still hacking around it?" at any time and
remove them once the underlying issue is closed.

## Format

```
## GAP-NNN — short title

* **Discovered**: sprint + commit SHA + file:line of the workaround.
* **Symptom**: minimal code that fails / unexpected behaviour.
* **Workaround**: what the dogfooder did instead (still in tree).
* **Planned fix**: what the compiler should ultimately do.
* **Scope**: rough size estimate (small / medium / large).
* **Status**: open | in-progress | fixed in SHA.
```

Sort by ID. New gaps append. Don't renumber.

---

## GAP-001 — No `<stream>` class in the runtime

* **Discovered**: Sprint 45a, commit `29e1040`,
  `tests/nod-tests/fixtures/dylan-lexer.dylan` around line 130
  (the `print-token` generic).
* **Symptom**: wanted to write
  `define generic print-token (t :: <token>, source :: <byte-string>, stream :: <stream>) => ()`
  so each token class can print itself to a stream (the canonical
  Dylan I/O abstraction). No stream classes existed in stdlib.
* **Workaround**: `print-token-to-string` shape. Tokens knew how to
  render themselves to a byte-string; `dump-tokens` concatenated the
  results. Retired with the fix.
* **Fix**: added a minimum-viable stream surface to
  `src/nod-dylan/dylan-sources/stdlib.dylan`:
  - `<stream>` abstract base class
  - `<string-stream> (<stream>)` concrete subclass with a single
    `stream-bytes :: <stretchy-vector>` slot
  - `make-string-stream() => <string-stream>` constructor
  - `write-byte(stream, b)` / `write-string(stream, s)` /
    `as-byte-string(stream)` generics + methods specialised on
    `<string-stream>`
  
  The write-side methods append bytes into the stretchy-vector;
  `as-byte-string` materialises a fresh `<byte-string>` of the
  right size and copies the accumulated bytes in. Future
  subclasses (`<file-stream>`, `<console-stream>`, `<input-stream>`)
  slot in via the same generics.

  End-to-end smoke test confirms `make-string-stream() →
  write-string + write-byte → as-byte-string` round-trips byte-exact
  through the AOT pipeline.

  This is also the **first time stdlib defines a class user code
  uses** — earlier classes like `<rope>` were always in the user's
  own AST. The class-resolution path
  (`find_class_id_by_name(name)` at `lower.rs:4317`) already
  worked for this — the stdlib lowering registers classes via
  `register_simple_user_class` and the user-side lookup falls
  through to the runtime registry. No compiler change needed,
  just the stdlib addition.
* **Regression test**: `tests/nod-tests/tests/sema.rs::
  gap_001_string_stream_round_trips`.
* **Scope**: small. ~70 lines of Dylan in stdlib.dylan.
* **Status**: **fixed in SHA `a689fcd`** (this commit). The full stream
  hierarchy (`<file-stream>`, `<input-stream>` for the parser, etc.)
  is its own future sprint when the IDE / Sprint 46+ parser need
  them.

## GAP-002 — `define constant` names don't resolve from function bodies

* **Discovered**: Sprint 45a, commit `29e1040`,
  `tests/nod-tests/fixtures/dylan-lexer.dylan` (the literal
  `1000000` appeared at three sites with comments).
* **Symptom**: a module-level `define constant $line-multiplier =
  1000000;` declaration is correctly parsed and lowered (as a
  zero-arg function returning the value), but `collect_top_level_names`
  in `nod-sema/src/lower.rs` only looked at `Item::DefineFunction`
  entries — never registered the constant in the name-resolution
  table. So bareword references from inside a `define function`
  body raised `LoweringError::UndefinedIdent` even though the
  constant was right there in scope.
* **Workaround**: the literal `1000000` was repeated at three sites
  in `offset-to-line-col-packed` / `unpack-line` / `unpack-col`.
  Retired with the fix.
* **Fix**: two changes in `nod-sema/src/lower.rs`:
  1. `collect_top_level_names` now also walks `Item::DefineConstant`
     and `Item::DefineVariable`, registering them with arity 0 and
     adding them to a new `TopNames::constants_and_variables` set.
  2. The `Expr::Ident` arm of `lower_expr` checks
     `is_constant_or_variable(name)` BEFORE the existing
     make-function-ref paths. When true, it emits a zero-arg
     `Computation::DirectCall` that evaluates the constant's body
     and returns its value — the right Dylan semantics (constants
     are values, not callable refs).
* **Regression test**: `tests/nod-tests/tests/sema.rs::
  gap_002_define_constant_resolves_from_function_body`.
* **Scope**: small. ~30 lines of sema.
* **Status**: **fixed in SHA `59e6f9f`**. `define variable` is a
  separate, deeper gap — see GAP-004.

## GAP-004 — `define variable` not lowered

* **Discovered**: Sprint 45a follow-up while fixing GAP-002 (this
  commit). The repro
  ```
  define variable *counter* = 41;
  define function main () => () *counter* := *counter* + 1; ... end;
  ```
  surfaces `unsupported [Span ...]: define variable not lowered in
  Sprint 06`. The `Item::DefineVariable` arm of the per-item
  lowering loop emits an `Unsupported` lowering error rather than
  generating a function body, so the variable's name is never bound
  to anything callable.
* **Symptom**: `define variable foo = expr;` fails to lower at all
  — fails BEFORE the GAP-002 name-resolution path is even reached.
* **Workaround**: avoid `define variable`. The lexer fixture used
  `define constant` exclusively. Retired with the fix.
* **Fix**: full `<cell>`-backed read/write/init pipeline in 7 steps:
  1. **Runtime storage** — `variable_cell_slot_addr(name) ->
     &'static AtomicU64` slot-allocator pattern (Sprint 38c shape,
     mutable variant) in `nod-runtime/src/lib.rs`. Slots hold the
     cell-pointer Word, registered as GC roots on first allocation
     so the cell itself stays reachable across GC cycles.
  2. **Runtime API** — `nod_aot_register_variable(name, name_len,
     init_fn_ptr)` (in `aot.rs`) calls the init function to compute
     the initial value, allocates a fresh `<cell>` via `nod_make_cell`,
     stores the cell pointer in the slot. `nod_var_get_by_name` /
     `nod_var_set_by_name` (in `closures.rs`) read/write through the
     slot lookup + cell deref.
  3. **Lower `Item::DefineVariable`** — emits THREE bodies: a
     `__init-<name>()` zero-arg function with the init expression,
     a getter `<name>()` that calls `nod_var_get_by_name`, and a
     setter `<name>-setter(v)` that calls `nod_var_set_by_name`.
  4. **Setter wiring** — `lower_assign` (lower.rs:4798) gained a
     module-variable branch: when the LHS resolves to a `define
     variable`, emit a DirectCall to `nod_var_set_by_name` with the
     interned variable name + RHS. `TopNames` split into separate
     `constants` and `variables` sets so assignment to a `define
     constant` correctly errors out.
  5. **AOT registration** — `LoweredModule` gained a `variables:
     Vec<VariableRegistration>` field; codegen emits one
     `nod_aot_register_variable(name, len, &__init-name)` call per
     variable inside `nod_aot_resolve_relocs` AFTER class/method/
     block registration (variables can call any registered function
     during init).
  6. **JIT path** — the JIT-side initialisation mirror runs after
     the engine materialises; calls each `__init-*` function and
     stores the result via `nod_var_set_by_name`. Symmetric with
     the AOT resolver.
  7. **GC discipline** — the cell pointer in the slot is reachable
     because the slot is registered as a heap root; the cell's
     `value` slot is `SlotType::Object` so the contained Word is
     traced via the existing Sprint 24 machinery.
* **Regression tests**:
  - `tests/nod-tests/tests/sema.rs::gap_004_define_variable_lowers_to_getter_and_init`
    — lowering-side check.
  - End-to-end smoke (manual): build `define variable *counter* = 41;`
    program, run, observe `initial = 41` → `*counter* := *counter* + 1`
    → `after-bump = 42` → `*counter* := 99` → `after-set = 99`.
    Verified byte-exact through the AOT EXE pipeline.
* **Scope (actual)**: medium-large. ~600 lines across nod-runtime,
  nod-sema, nod-llvm. 7 commits worth of independently-verifiable
  steps merged here into one for atomicity.
* **Status**: **fixed in SHA `74e6221`** (this commit). GAP-002's regression
  test still passes — constants stay immutable, variables are the
  only writable kind.

## GAP-005 — `if` without `else` arm refuses to lower

* **Discovered**: Sprint 45a rework (commit `1d32575`),
  `tests/nod-tests/fixtures/dylan-lexer.dylan` print-token method.
* **Symptom**: writing `if (cond) write-string(stream, "  ") end;`
  raised `unsupported [Span ...]: Sprint 06 lowers only
  if-expressions with an else arm`. Dylan supports the else-less
  form; the compiler rejected it.
* **Workaround**: explicit `else #f` arm. Retired with the fix.
* **Fix**: in `Expr::If` lowering, when `else_` is `None` synthesise
  an `Expr::Bool(span, false)` and pass it to `lower_if`. Semantically
  correct (Dylan: missing else returns `#f`). Same 3-block CFG, no
  special-case in lower_if.
* **Regression test**: `tests/nod-tests/tests/sema.rs::
  gap_005_if_without_else_lowers`.
* **Scope**: small. ~10 lines of sema.
* **Status**: **fixed in SHA `8e153b2`** (this commit). Note GAP-006 still
  applies if the synthesised else's `#f` doesn't shape-match the
  then-arm's last-expression type — see below.

## GAP-006 — void-returning calls in if-arms panic codegen

* **Discovered**: Sprint 45a rework (commit `1d32575`), print-token
  method using `if (cond) write-string(stream, "  ") end`.
* **Symptom**: codegen panics with `phi incoming temp defined` at
  `src/nod-llvm/src/codegen.rs:1233` when an if-arm's last
  expression is a void-returning generic call (return type `()`)
  AND the if's value flows into a join-block phi.
* **Root cause**: the `Computation::DirectCall` / `Dispatch` /
  `SealedDirectCall` codegen arms in `nod-llvm/src/codegen.rs`
  guarded `self.temps.insert(*dst, v)` behind `if let Some(v) = v`.
  When the called function returned void (`v == None`), the dst
  TempId was NEVER inserted into `state.temps`. But the lowering
  pass allocates a dst TempId regardless of the call's return
  arity. When that orphan TempId then appeared as a Jump arg into
  a join block, the phi-incoming wiring step at codegen.rs:1233
  panicked because `state.temps.get(arg_temp)` returned None.
  Not a type-system issue — a missing-binding issue.
* **Workaround**: ensure both arms produce a same-shape value,
  e.g. add a trailing `#f` sentinel after void calls. Retired
  with the fix.
* **Fix**: all three call-flavour Computation arms now insert
  `load_imm_nil()?.into()` for the dst TempId when the underlying
  emit returns None. Phi joins get a real i64 LLVM value (Dylan's
  canonical "no meaningful value" — `nil`). Consumers that ever
  use the value see `nil`, which is the right semantics for a void
  call's "result".
* **Regression test**: `tests/nod-tests/tests/sema.rs::
  gap_006_void_call_in_if_arm_does_not_panic`, plus the end-to-end
  smoke that the Sprint 45a `print-token` method now uses the bare
  `if (~instance?(...)) ... end` shape without any sentinel `#f`.
* **Scope**: small. ~15 lines of codegen.
* **Status**: **fixed in SHA `8e153b2`** (this commit).

## GAP-007 — Function-local heap references go stale across heavy allocation loops

* **Discovered**: Sprint 45b,
  `tests/nod-tests/fixtures/dylan-lexer.dylan` — the lex+dump path
  for the Dylan-in-Dylan lexer.
* **Symptom**: a function holds a heap-object reference in a `let`
  local (a `<stretchy-vector>`, a `<string-stream>`, a `<byte-string>`)
  and threads it through a loop that calls into other functions that
  allocate. After ~92–650 iterations (depending on the per-iteration
  allocation pressure) the local's word turns into garbage and the
  next use trips one of:
  - `stretchy_vector_push: not a <stretchy-vector>` in
    `src/nod-runtime/src/collections.rs:989`
  - `<no-applicable-methods-error>: no applicable method for
    \`write-byte\` on (<unknown:NNN>, <integer>)` raised by sema's
    method dispatch
  Class id `NNN` in the second form is a different small integer on
  every run — classic stale-pointer behaviour. Function parameters
  show the same failure as `let` locals; passing the vector/stream
  through a helper function's parameter slot does NOT save it.
  Module-level `define variable` cells DO survive because they live
  in cell-backed slots registered as GC roots (the Sprint 24 / GAP-004
  machinery).
* **Minimal reproducer**:
  ```dylan
  define class <tok> (<object>) end class;
  define function dump (vec :: <stretchy-vector>) => ()
    let stream = make-string-stream();
    let n = %stretchy-vector-size(vec);
    let i = 0;
    until (i = n)
      let t = %stretchy-vector-element(vec, i);
      write-string(stream, "abcdef");
      write-byte(stream, 10);
      i := i + 1;
    end;
  end function;
  ```
  Run with `n > ~92`. `vec` and `stream` both become garbage between
  iterations once `write-string` triggers enough allocations to grow
  the stream's backing storage. The `lex_count2.dylan` variant on
  this shape FAILS AT BUILD with a verifier error
  `Instruction does not dominate all uses!` involving a `gc.reload`
  PHI — same root cause surfacing as ill-formed LLVM IR instead of
  runtime corruption.
* **Workaround in tree**: the lexer fixture stashes its three
  heaviest-trafficked heap roots as module variables:
  - `*tokens* :: <object>` — the `<stretchy-vector>` accumulator
  - `*dump-stream* :: <object>` — the dump-tokens output stream
  - `print-token` writes through `*dump-stream*` directly (with
    helpers `write-line-col-to-dump-stream` and
    `write-escaped-source-text-to-dump-stream`)
  This pushes the failure envelope from ~92 lines to ~650 lines of
  the lexer's own source — enough for sprint 45b's working corpus
  (hello.dylan, the Sprint 45-era tests) but NOT enough to dump the
  lexer fixture on itself (~1265 lines, 38 KB). The workaround
  surface is documented in-source where it lives.
* **Root cause (verified by reading codegen + liveness)**: the bug is
  NOT in the GC liveness pass and NOT in the safepoint runtime — both
  are correct. The bug is in **phi-incoming wiring in
  `src/nod-llvm/src/codegen.rs`**:

  - `pending_incoming` (line 1140) is typed as
    `Vec<(BlockId, BasicBlock, Vec<TempId>)>` — it records the
    symbolic TempIds of jump args, not the resolved SSA values.
  - `emit_terminator` for `Terminator::Jump` (line 3092-3107) pushes
    the TempIds onto `pending_incoming` and moves on.
  - At end-of-function (line 1226-1236), the phi-wiring loop calls
    `state.temps.get(arg_temp)` to resolve each TempId → SSA value.
  - **But `end_safepoint` (line 3175) MUTATES `state.temps` every
    time it runs**:
    ```rust
    self.temps.insert(slot_info.temp, reloaded);
    ```
    Every safepoint reload overwrites `temps[t]` with a fresh
    `%gc.reload.tN` SSA value defined IN the current block.

  By the time phi-wiring runs at the end, `state.temps[t]` holds the
  **last** reload SSA value across the entire function — typically
  defined deep inside the loop body. The phi for `t` at the loop
  header ends up taking that same body-block SSA value on BOTH
  incoming edges. The entry-edge then can't possibly dominate it.

  This matches the GAP-007 IR-verifier error pattern exactly: the
  phi name `phi.t{}` (line 1206) and the gc.reload name `gc.reload.t{}`
  (line 3173) appear together in the failure message as the same
  TempId. Both incomings use the same value.

* **Symptom matrix explained by the root cause**:
  - **Build-time `Instruction does not dominate all uses!`** — both
    phi incomings reference `%gc.reload.tN` defined inside the body
    block. Entry-edge dominance violated.
  - **Runtime stale-pointer "after N iterations"** — when LLVM block
    layout happens to satisfy dominance, the IR is valid but
    semantically wrong: entry edge reads from a slot that wasn't
    initialised this call. Different `<unknown:NNN>` per run because
    the alloca slot pool is per-function-instance and the residual
    bits drift across runs.
  - **Function parameters fail identically to `let`-locals** —
    params skip entry-block phi creation but, once threaded into a
    downstream phi, go through the same `temps[p]` lookup that
    `end_safepoint` clobbered.
  - **Module-level `define variable` cells survive** — they bypass
    phi-wiring entirely. Each read calls `nod_var_get_by_name`
    against a registered cell slot.
  - **Workaround "envelope" of ~650 lines** — only because the most
    heavily allocating temp was hoisted into a module slot; the
    bug still bites every other `let`-local that's loop-carried.

* **The fix (small, surgical, three locations in `codegen.rs`)**:
  Snapshot SSA values at jump-emit time instead of resolving at
  phi-wiring time.

  1. Change `pending_incoming` type (line 1140) from
     `Vec<(BlockId, BasicBlock, Vec<TempId>)>` to
     `Vec<(BlockId, BasicBlock, Vec<BasicValueEnum<'ctx>>)>`.
  2. In `Terminator::Jump` (line 3092-3107), resolve `args` to SSA
     values BEFORE the branch, then push them:
     ```rust
     let arg_vals: Vec<BasicValueEnum<'ctx>> =
         args.iter().map(|t| self.temp_val(*t)).collect();
     ```
  3. In the wiring loop (line 1226-1236), iterate over the
     pre-resolved values directly — drop the `state.temps.get`
     lookup.

  Net ~10 lines. Snapshotting at emit-time captures the SSA value
  as it flowed out of the actual predecessor — which is exactly
  what a phi-incoming wants.

* **Related-bug bonus**: Sprint 11d's WNDPROC callback hang chase
  (Tasks #239 / #243 / #244 / §10.1 closure-graph tenuring) is the
  same root cause in a different shape — the callback frame's
  closure cells were threading through dispatch loops with
  loop-carried phis. The §10.1 tenuring hack worked around the
  symptom by pinning the cells in old-gen so reloads stopped
  mattering. If this fix lands cleanly, Sprint 11d Step F (#245)
  should be retire-able without the tenuring hack.

* **Regression test**: minimal reproducer above lands as
  `tests/nod-tests/tests/gap_007_stale_locals.rs` with fixture
  `tests/nod-tests/fixtures/gap-007-repro.dylan`. Add a focused
  unit test in `src/nod-llvm/src/codegen.rs::tests` that builds
  a 2-block function with a Jump-args phi-incoming, runs a fake
  safepoint between them, and asserts the resulting LLVM IR's
  phi incomings reference the pre-safepoint SSA value (NOT the
  reload).

* **Scope** (revised): SMALL — ~10 lines of code change + 2-3
  regression tests. Hot path though: this code runs for every
  Dylan function with a Jump terminator carrying args, so the
  full `cargo test` sweep IS required (one of the exceptions to
  the "Dylan-only changes skip the sweep" rule). One-day sprint.

* **Workaround retirement**: once the fix is in, revert the
  `*tokens*` / `*dump-stream*` module-var stash in
  `tests/nod-tests/fixtures/dylan-lexer.dylan` (Sprint 45b
  workaround) back to natural `let`-locals; add a sanity test
  that `dump-dylan-tokens` on the lexer fixture itself produces
  no errors (currently impossible — see §"Workaround in tree"
  above).

* **Status**: **fixed in SHA `f1d71c4`** (this commit). Regression
  tests landed alongside in
  `tests/nod-tests/tests/gap_007_stale_locals.rs` with fixtures
  `gap-007-repro.dylan` (JIT + AOT runtime) and
  `gap-007-repro-ir.dylan` (LLVM-IR shape assertion). The Sprint 45b
  workaround in `tests/nod-tests/fixtures/dylan-lexer.dylan`
  (`*tokens*` / `*dump-stream*` module-variable stash) remains in
  tree pending its own retirement commit — the natural acceptance
  gate for that retirement is the self-dump
  (`nod-driver dump-dylan-tokens tests/nod-tests/fixtures/dylan-lexer.dylan`)
  succeeding end-to-end on the lexer's own source.

  **Cascade complete**: the broader class of GC phi-wiring and
  env-scope-leak bugs that GAP-007 represented was fully closed by
  Sprint 45c–45h through GAP-008–013. Empirical confirmation: the
  five-file IDE (`nod-ide.exe`) compiles, opens a window, and runs
  the Win32 message loop without crash. The lexer fixture workaround
  retirement is the one open action remaining in this gap family.

## GAP-008 — Short-circuit (`&`/`|`) `sc_join` missing conservative GC-binding phi params

* **Discovered**: Sprint 45e GC integration test (`gc-rope-file-load`)
  during T16 mirror-writing workload.
* **Symptom**: LLVM verifier "Instruction does not dominate all uses"
  for GC-managed temps in if-arm blocks following a compound Boolean
  condition that contains a safepoint-emitting call in the right-hand
  side. Minimal repro shape:
  ```dylan
  let s :: <byte-string> = ...;
  if (element(s, i) >= 97 & element(s, i) <= 122)
    // ... uses s ...
  end if;
  ```
  `lower_short_circuit` created an `sc_join` block with params only
  for names *assigned* inside the rhs expression.  When the rhs
  evaluation (`element` dispatch) fired a GC safepoint that reloaded
  `s` → `%gc.sN.reload.s`, that reload was propagated via
  `note_successor_entry_temps` into `sc_join`'s entry-temp snapshot.
  From there it was copied into `block_entry_temps[then]` and
  `block_entry_temps[else]` when the outer if's `Terminator::If`
  fired. Because `sc_rhs` has `sc_edge` as a sibling predecessor of
  `sc_join`, the reload defined in `sc_rhs` does **not** dominate the
  `then`/`else` arm blocks. Both arms' Jump args resolved to the same
  non-dominating reload, producing an invalid phi.
* **Workaround**: none (breaks stdlib.dylan compilation).
* **Planned fix**: apply the same conservative GC-binding merge to
  `lower_short_circuit` as is already applied to `lower_if`: add all
  GC-managed env bindings (not just rhs-assigned names) to the
  `sc_join` block's params. This makes `sc_join` the proper merge
  point for every GC-managed pointer, so its phi value dominates all
  successors.
* **Scope**: small — 5 lines in `nod-sema/src/lower.rs`
  (`lower_short_circuit`).
* **Status**: **fixed** (same commit that adds this gap entry).

## GAP-009 — Loop body/exit blocks created before short-circuit condition blocks, causing stale `block_entry_temps`

* **Discovered**: Sprint 45e GC integration test (`gc-rope-file-load`),
  surfaced after GAP-008 fix.
* **Symptom**: LLVM verifier "Instruction does not dominate all uses" for
  GC-managed temps in blocks AFTER a while/until loop whose condition is
  a short-circuit `&`/`|` expression. Minimal shape:
  ```dylan
  while (some-call() & other-call())
    ...inner if using gc-managed bindings...
  end;
  if (some-comparison)   // ← phi uses loop-body value in violation of SSA
    ...
  end
  ```
  `lower_while_like` created `loop_body` and `loop_exit` blocks
  **before** calling `lower_expr(cond)`. `lower_expr(cond)` for a
  short-circuit condition creates `sc_edge`, `sc_rhs`, `sc_join` which
  therefore appear **after** `loop_exit` in `func.blocks`. Codegen
  iterates `func.blocks` in creation order, so it processes `loop_exit`
  before `sc_join` (its only CFG predecessor). `block_entry_temps[loop_exit]`
  is empty at that point, and `loop_exit` processes with stale state from
  whatever block preceded it — picking up a GC-managed phi value from inside
  the loop body (e.g. `phi.t41` from the loop-body inner-if join). That
  stale value propagates via `note_successor_entry_temps` to the if-arm
  blocks after the loop, which don't dominate the loop-body block where the
  phi was defined.
* **Workaround**: none (breaks functions with `while (a & b)` + post-loop if).
* **Planned fix**: create `loop_body` and `loop_exit` blocks *after*
  `lower_expr(cond)` returns. The condition blocks (`sc_edge`, `sc_rhs`,
  `sc_join`) are then earlier in `func.blocks`, so codegen processes
  `sc_join` before `loop_exit` and `block_entry_temps[loop_exit]` is
  correctly seeded.
* **Scope**: small — 6-line reorder in `nod-sema/src/lower.rs`
  (`lower_while_like`).
* **Status**: **fixed** (same commit that adds this gap entry).

## GAP-003 — No multi-value return / no multi-binder `let`

* **Discovered**: Sprint 45a, commit `29e1040`,
  `tests/nod-tests/fixtures/dylan-lexer.dylan`
  (the `offset-to-line-col-packed` function shape).
* **Symptom**: wanted to write
  ```dylan
  define function offset-to-line-col (off, source)
   => (line :: <integer>, col :: <integer>)
    ...
    values(line, col)
  end function;
  ...
  let (line, col) = offset-to-line-col(off, source);
  ```
  Neither the multiple-value return nor the multi-binder `let`
  form was implemented. Per nod-sema's "Out of scope" doc-comment,
  multi-value was a recognised future feature.
* **Workaround (retired)**: packed `line * 1_000_000 + col` into one
  integer return. Paired `unpack-line` / `unpack-col` accessors
  decoded it at call sites. Worked because both line and col are
  bounded small integers, but was ugly and would be wrong for
  anything else. Retired with the fix.
* **Fix (Sprint 47)**: SBCL-style secondary-values buffer. No ABI
  changes. Five phases:
  1. **Runtime** (`src/nod-runtime/src/values.rs`) — thread-local
     `[Word; 8]` buffer + `usize` count, `extern "C"` shims
     `nod_values_clear` / `nod_values_set` / `nod_values_get` /
     `nod_values_count`, and `snapshot_active_values_roots` wired
     into `heap::snapshot_roots` so the GC scans the buffer when a
     multi-binder `let` is mid-destructure across a safepoint.
  2. **Primitives** — four entries in
     `LOWER_PRIMITIVE_TABLE` (`%values-clear`, `%values-set!`,
     `%values-get`, `%values-count`) routing to the runtime shims.
  3. **Parser/AST** — `Statement::Let { binders: Vec<Binder>, … }`
     already accepted multi-binder `let (a, b, c) = …` from earlier
     work; Sprint 47 only flips its lowering from "Unsupported" to
     the SBCL destructure. The multi-binder shape is a frozen
     kernel binding form (can't desugar to nested single-binder
     `let` — the RHS must be evaluated exactly once).
  4. **Sema** (`src/nod-sema/src/lower.rs`) —
     `values(a, b, c)` (recognised on the call's callee name)
     lowers to `nod_values_set(0, b); nod_values_set(1, c); return a`;
     multi-binder `Statement::Let` lowers to
     `nod_values_clear(); let a = expr; let b = nod_values_get(0); …`.
     A new `lower_let_multi_binders` helper is called from every
     site that processes `Statement::Let` (function body, lift
     thunk, loop body, expression-stmt context).
  5. **Workaround retirement** — lexer fixture's
     `offset-to-line-col-packed` → `offset-to-line-col` returning
     `values(line, col)`; call sites use `let (l, c) = …`. The
     `$line-col-shift` constant and `unpack-line` / `unpack-col`
     helpers are deleted.

  Single-value receivers (`let x = call()`) don't touch the buffer
  at all — zero overhead for the common case. The polluted-buffer
  trap (call A returns multi, call B is single-valued, then `let
  (b1, b2) = B`) is solved by the receiver-side `nod_values_clear`
  before the call: if B doesn't write to the buffer, B2 reads past
  count and gets `#f`.
* **Regression tests** (this commit):
  - `tests/nod-tests/tests/sema.rs::gap_003_phase_b_values_primops_lower`
    — the four `%values-*` primops resolve.
  - `tests/nod-tests/tests/sema.rs::gap_003_values_*` + `…multi_binder_let_*`
    + `…polluted_buffer_does_not_leak_across_calls` — six lowering
    shape checks covering single/multi `values()`, two/three binder
    `let`, single-value RHS fallback, and clear-before-each-call.
  - `tests/nod-tests/tests/aot_dylan.rs::aot_gap_003_divmod_multi_value`
    + `…polluted_buffer_does_not_leak` — end-to-end through the AOT
    pipeline.
  - Runtime unit tests in `src/nod-runtime/src/values.rs`
    (clear/set/get round-trip, count growth, snapshot returns
    pointers, set-at-cap panics, polluted-buffer test).
* **Scope (actual)**: medium. ~250 lines runtime, ~250 lines sema,
  ~400 lines tests. Five atomic commits, one per phase.
* **Status**: **fixed in Sprint 47** — runtime mechanism (`values.rs`)
  in `abc61e3`; sema lowering for `values(...)` and multi-binder `let`
  in `5181da5`; AST policy + regression tests in `a2a448b`; lexer
  fixture retirement + this doc update in this commit.

## GAP-010 — AOT zero-root safepoints panic in codegen when no slot slab exists

* **Discovered**: Sprint 46 rope-backed lexer experiment, when trying to
  route `tests/nod-tests/fixtures/dylan-lexer.dylan` through a Dylan-side
  rope load path before lexing.
* **Symptom**: AOT codegen panicked at
  `src/nod-llvm/src/codegen.rs:4328` with
  `safepoint slot slab missing` while compiling a function that emitted an
  image safepoint with **zero** GC roots. `Emit::init_safepoint_slot_slab`
  intentionally skipped slab allocation when `max_safepoint_slots(func) = 0`,
  but `begin_safepoint` still unconditionally called
  `safepoint_slot_base_ptr()` on the AOT path before `nod_aot_begin_safepoint`.
  The runtime contract in `src/nod-runtime/src/aot.rs` only dereferences
  `slot_base` when `expected_root_count > 0`, so zero-root frames do not need
  a real slab pointer.
* **Workaround**: none inside Dylan source; any attempt to introduce an
  extra zero-root safepoint in the AOT-built lexer fixture crashed the build.
* **Planned fix**: for image safepoints, pass `ptr null` as `slot_base` when
  `roots.is_empty()` instead of demanding a safepoint slab. Keep the existing
  slab path for non-empty root sets.
* **Scope**: small — one conditional in
  `src/nod-llvm/src/codegen.rs::begin_safepoint` plus a focused codegen test.
* **Status**: **fixed** (same commit that adds this gap entry).

## GAP-011 — AOT safepoint verification counted only legacy registered roots

* **Discovered**: Sprint 46 rope-backed lexer retry, after GAP-010 was fixed
  and `tests/nod-tests/fixtures/dylan-lexer.dylan` could again build through
  the AOT pipeline with Dylan-side rope loading enabled.
* **Symptom**: runtime execution panicked at `src/nod-runtime/src/aot.rs:440`
  with errors like
  `AOT safepoint 464 registered 0 roots; expected 1 (baseline 4, patchpoint gc.s464)`.
  The codegen metadata and slot slab agreed that one precise root was live, but
  `verify_aot_safepoint` asked `crate::heap::root_count()`, and that helper only
  counted the legacy thread-local `register_root` stack. Active AOT safepoint
  roots lived in `snapshot_active_aot_roots()` and were therefore invisible to
  the verification and end-of-safepoint checks.
* **Workaround**: temporarily fall back to plain `%read-file(path)` in the lexer
  fixture, or manually rebuild the cached lexer EXE after runtime changes while
  debugging. Neither was a real fix.
* **Planned fix**: keep `root_count()` as the legacy registered-root count for
  its existing callers, add a total-root helper that includes active JIT and AOT
  precise safepoint roots, and use that helper in AOT begin/verify/end protocol
  checks. Update the runtime unit test to validate the precise slot-slab path
  directly instead of manually mirroring roots through `register_root`.
* **Scope**: small-medium — runtime accounting in `src/nod-runtime/src/heap.rs`
  and `src/nod-runtime/src/aot.rs`, plus focused AOT protocol test updates.
* **Status**: **fixed** (same commit that adds this gap entry).

## GAP-012 — Loop-body `let` bindings leak into post-loop GC root merges

* **Discovered**: Sprint 46/47 five-file IDE build
  (`tests/nod-tests/fixtures/ide_helpers.dylan` — `nod-basename`
  function) surfaced after GAP-007/008/009/010/011 were all fixed.
* **Symptom**: AOT build fails at the LLVM verifier with four
  `Instruction does not dominate all uses!` errors, all pointing at
  `%phi.t24` / `%phi.t23` (the phi parameter for a loop-body `let`
  binding `b`) being stored into GC root slots in `else15` (a
  post-loop if-arm block). `b` is defined at `join13` — an if-join
  block inside the loop body — which does NOT dominate `else15`.
  Minimal Dylan shape that triggers it:
  ```dylan
  let sep-pos = -1;
  let i = 0;
  until (i = n)
    let b = element(path, i);   // ← `b` is a loop-body let
    if (b = 92 | b = 47)
      sep-pos := i;
    end;
    i := i + 1;
  end;
  if (sep-pos < 0)              // ← post-loop if
    path
  else
    copy-sequence(path, sep-pos + 1, n)  // safepoint sees `b`!
  end
  ```
* **Root cause**: `lower_while_like` restored only *loop-carrier*
  variables (names in `loop_var_order`) in `env` after the loop
  exits. Names introduced by `let` inside the loop body — such as
  `b` — remained in `env` pointing at loop-body-local SSA temps
  (in this case the `join13` phi parameter for `b` after the inner
  short-circuit/if lowering). The conservative GC merge in the
  subsequent `lower_if` for the post-loop `if (sep-pos < 0)` saw
  `env["b"]` as a live GC-managed binding and included `phi.t24`
  in the join's phi params and safepoint roots, producing an LLVM
  SSA value used outside its defining block.

  The same scope leak is present in `lower_if` (arm-local `let`
  names survive in `env` after the join) and `lower_short_circuit`
  (rhs-local names survive after the `sc_join`). In practice, the
  `lower_if` arm leak is low-risk because:
  - The then-arm env is reset to `pre_env` before the else-arm,
    so then-arm `let` names don't propagate.
  - Else-arm `let` names do stay, but only surface as a problem
    if a NESTED outer `if` (or loop) conservatively GC-merges them
    when they resolve to non-dominating temps. `lower_while_like`
    is the only currently-confirmed site where this mis-merge
    actually violates LLVM dominance.

* **Fix**: three one-liner `env.retain(...)` additions:
  1. **`lower_while_like`** — before body lowering, snapshot
     `pre_body_env_names: HashSet<String> = env.keys().cloned()
     .collect()`. After the loop-var restoration and before
     `self.switch_to(exit_b)`, call
     `env.retain(|name, _| pre_body_env_names.contains(name))`.
     This evicts all loop-body `let` names on loop exit.
  2. **`lower_if`** — after inserting join params into `env`, call
     `env.retain(|name, _| pre_env.contains_key(name))`. Evicts
     arm-local `let` names (those not in the pre-if env) after the
     join.
  3. **`lower_short_circuit`** — after inserting sc_join params, call
     `env.retain(|name, _| pre_rhs_env.contains_key(name))`.
     Evicts rhs-local `let` names after the sc_join.

  All three `pre_*_env` / `pre_*_env_names` snapshots were already
  computed by these functions for other purposes; no new clones needed.

* **Scope**: small — 3 lines of `env.retain(...)` in
  `src/nod-sema/src/lower.rs`, plus a snapshot declaration in
  `lower_while_like`.
* **Status**: **fixed** (this commit). Five-file IDE AOT build now
  succeeds (`compiled: target/nod-ide.exe`) with no LLVM verifier
  errors.

## GAP-013 — AOT end-safepoint assertion too strict for permanent GC roots

* **Discovered**: Sprint 46/47 five-file IDE runtime (after GAP-012
  fixed the LLVM dominance errors and the IDE binary could be
  generated and executed).
* **Symptom**: `target/nod-ide.exe` panicked at
  `src/nod-runtime/src/aot.rs:470` with
  `AOT safepoint 1458 lost active roots before end: current 39 baseline 7 expected 0 (patchpoint gc.s1458)`.
  `current (39) ≠ baseline + expected (7 + 0 = 7)`.
* **Root cause**: the Win32 callback layer
  (`install_gc_roots_for_this_thread`) registers 32 permanent
  `ROOT_STACK` entries on the first call from a thread (one per
  registered window-procedure callback cell). This first-touch
  registration happens INSIDE the body of a call that is bracketed
  by `nod_aot_begin_safepoint` / `nod_aot_end_safepoint`. After the
  call returns, `total_root_count()` is `baseline + 32` rather than
  `baseline + 0`. The `end_safepoint` assertion used `assert_eq!`,
  which treats any INCREASE as an error. The same issue exists in the
  post-pop assertion ("leaked roots after end").
* **Why `assert_eq!` is wrong here**: the contract the assertion
  should enforce is "none of OUR safepoint slots were removed before
  end" (i.e., `current >= baseline + expected`). Permanent roots
  being ADDED during the call are legitimate — they are intended to
  outlive the call and must not be treated as a leak.
* **Fix**: change both `assert_eq!` calls in `end_aot_safepoint` to
  `assert!(current_root_count >= ...)` and
  `assert!(post_pop_root_count >= ...)`. The "too few roots" check
  (a callee removing roots it shouldn't) is preserved; the "too many
  roots" rejection is removed.
* **Scope**: small — 2-line change in
  `src/nod-runtime/src/aot.rs::end_aot_safepoint`.
* **Status**: **fixed** (this commit). IDE binary runs without
  immediate safepoint assertion failure; GUI window opens and enters
  the Win32 message loop normally.
* **Leak-detection trade-off**: the `>=` relaxation removes the
  ability to catch "caller registered MORE roots than expected and
  never cleaned them up" bugs. The `assert_eq!` form provided that
  check; `assert!(>=)` does not. This is a permanent loss until
  `install_gc_roots_for_this_thread`'s permanent-root injection is
  moved to before the first safepoint (e.g. via a `#[ctor]`-style
  first-call-init at the AOT main wrapper start), at which point the
  permanent roots will already be in `baseline_root_count` and the
  strict equality can be restored.
* **Test resolved in `3986171`**: the
  `aot::tests::aot_exec_safepoint_hooks_detect_missing_root`
  `#[should_panic]` test was failing because (a) the test called
  `begin_aot_safepoint(7, 2, ...)` matching the registered
  `root_count=2`, so verify's count check trivially passed, and (b)
  `verify_safepoints_enabled()`'s `OnceLock` could cache `false` from
  other tests that ran first. The fix passes `expected_root_count=1`
  so `begin`'s `assert_eq!` catches the mismatch (registered 2 ≠
  codegen-emitted 1) and panics with the expected message, and adds a
  `#[cfg(test)] thread_local` so `verify_safepoints_enabled()` returns
  true in the test binary without needing the env var. Leak-detection
  coverage at end-of-safepoint remains permanently weaker (the
  trade-off documented above) until permanent-root injection moves
  before the first safepoint.

---

## Notes

* The IDE (Sprint 41+) and the in-Dylan lexer (Sprint 45+) are
  collectively the **highest-pressure correctness tests** the
  compiler has — every gap they surface is a gap that real users
  will hit. Time spent fixing these gaps is time well spent.
* When a gap is **fixed**, leave its entry in this file but flip
  `Status` to `fixed in SHA xxxxxxx`, and remove the workaround
  marker comments from the source. The entry stays as historical
  context (and as a regression-test reminder).
