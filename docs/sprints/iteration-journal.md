# Corpus iteration journal

Autonomous, goal-driven iteration over the OpenDylan test corpus
(`opendylan-tests/`). Loop: pick a test → run it → diagnose → fix the compiler
bug *or* add the reasonable missing stdlib feature (no massive subsystems like
DUIM) → re-run → on a pass, record it here and keep going. Verify no regression
(in-tree fixtures stay green) and commit each win.

## Status

| Metric | Value | Notes |
|--------|-------|-------|
| In-tree fixtures (`dump-ast`/`dump-dfm`) | 55 / 55 | regression guard — must stay green |
| OpenDylan corpus parse (`dump-ast`) | 150 / 161 | language + stdlib suites (DUIM/etc. excluded); 101 at session start |
| OpenDylan corpus compile (`dump-dfm`, `--parse-with-rust`) | 71 / 161 | … → 63 (DRM classes) → 71 (DRM classes batch-2) |
| OpenDylan corpus build/run | self-contained programs build + run | `tak`/`benchmark`/`define test` → `.exe`, correct results |
| Macro engine | definition macros ✅ | first one (`benchmark`) builds+runs; was: only body/call macros |
| Evidence | `tak`/`benchmark` build to `.exe` and run | pure benchmark computation compiles + runs correctly (=7) |

## Iterations

*(newest first)*

### 2026-06-15 — Iteration 22: `for (var in collection)` lowering; reverted a hollow testworks fix

- **for-in-collection** (`lower.rs`): `for (x in coll) … end` now lowers via FIP,
  composing with numeric/step/`until:`/`while:`/`finally` + parallel/nested clauses
  (one `%fip-init` state per in-clause; parallel stops at the shortest). Verified
  build+run: `sum #(10,20,30)`=60, parallel→3, `finally`=600, nested=220, empty→99,
  and numeric `sum 1..100`=5050 still works. nod-sema 48/0 (+4 tests). in-tree 55/55,
  smoke-aot 6/6. Corpus 71→71 (the 17 `for…in` files are blocked UPSTREAM — the
  testworks `test`-macro re-parse, parse diagnostics, undefined idents — not by the
  in-clause; the feature is correct + ready for when those clear).
- **Reverted a HOLLOW testworks fix.** An agent's `?options:parameter-list` rule for
  `define test/suite (keyword: …)` heads reported corpus 71→78, but build+run review
  exposed it as hollow: `define test foo (description: "x") … end` with a caller
  fails `unknown callee foo` and dump-ast shows the head left RAW/unexpanded — the
  rule does NOT match keyword property-lists (the macro-engine rewrite didn't fix that
  iteration-11 gap), so the test function is SILENTLY DROPPED and dump-dfm "passes" by
  losing the function. Reverted (corpus back to a real 71). The keyword-property-list
  `define test/suite` head remains an OPEN deep parser problem (failed 2 agents);
  needs hands-on parser work, not another fire-and-forget rule. The
  `interface-specification-suite` no-op (legit, +4) was reverted with it; re-do
  cleanly later. (This is exactly why every agent change is build+run-reviewed.)

### 2026-06-15 — Iteration 21: `#rest` parameter collection + variadic stdlib

- **`#rest` collection** (`lower.rs`): a `#rest var` param now binds `var` to a fresh
  sequence of the trailing actual args. Caller-side design — the CALLER builds the rest
  `<simple-object-vector>` and passes it as one slot, so the callee keeps a fixed-arity
  LLVM signature and NO codegen/runtime change was needed. `TopNames.rest_fns` tracks
  each `#rest` function's fixed-param count (round-trips through sema-dump).
- **Un-deferred variadic stdlib** (call-site shortcuts, any arity): `list(…)` →
  cons-chain, `vector(…)` → SOV, N-ary `max`/`min` → fold over the binary generic.
- **Verified build+run:** `f(a,#rest m)=a+size(m)` → f(10,1,2,3)=13, f(5)=5;
  `g(#rest xs)=reduce(\+,0,xs)` → g(1,2,3,4)=10; `list(10,20,30)` size 3;
  `vector(1,2,3,4)` size 4; `max(1,9,4,7)=9`, `min(5,2,8,1)=1`. in-tree 55/55 both
  paths, nod-runtime 150/0, smoke-aot 6/6, corpus 71/161 (capability; corpus bodies
  gated on the testworks harness).
- Remaining: N-ary `compose`/multi-arg `curry` need a spread-`apply` on a `<function>`
  value (distinct from rest-collection) — queued.

### 2026-06-15 — Iteration 20: DRM class markers batch-2 — corpus 63 → 71

Extended `system-classes.dylan` with 27 more pure-Dylan class-name markers (AOT-safe,
self-rebuilt): reflection/types (`<type>`/`<class>`/`<singleton>`), the number tower
(`<number>`/`<complex>`/`<real>`/`<rational>`/`<float>`/`<byte>`/`<bit>`), conditions
(`<restart>`/`<arithmetic-error>`/`<stream-error>` + EOF/read/write variants),
collections (`<list>`/`<deque>`/`<set>`/`<object-table>`/`<string-table>`/
`<stretchy-sequence>`/float-vectors). `instance?` CPL verified; 8 corpus files newly
lower (classes/core/numbers/byte-vector/streams/collections-suite/…). in-tree 55/55
both paths, smoke-aot 6/6, corpus 63 → 71.
Limitation: seed classes (`<integer>`/floats/`<pair>`) not re-parented under the new
abstract bases, so `instance?(1.5,<float>)` etc. are #f (clears the ident, not full
subtype semantics). **Now-dominant blockers:** testworks `test`-macro re-parse (40
files), `define interface-specification-suite` (24), `for`-`in`-collection lowering (17).

### 2026-06-15 — Iteration 19: deeper macros + DRM system classes + funcall fix (3 agents)

- **Macros (+5):** `iterate` (named-let loop, variadic via `*` repetition),
  `when-let`/`if-let`, `collecting`/`collect` (ordered accumulation via a pinned
  `%collect-acc`), `assert`/`debug-assert`. Build+run verified (sum-via-iterate=15,
  fib(10)=55, when-let, collecting evens). Also made `parse_local_expr_compat` always
  emit `Statement::Local` so `iterate`'s `begin`-wrapped local method lowers.
- **DRM system classes (+10), corpus 60 → 63:** `<synchronization>`/`<lock>`/
  `<simple-lock>`/`<recursive-lock>`/`<semaphore>`/`<notification>`/`<thread>`/
  `<generic-function>`/`<float-vector>`/`<type-error>` as pure-Dylan `define class`
  (AOT-safe; new `system-classes.dylan` + self-rebuild). `instance?` CPL verified;
  `notifications-spec`/`threads-spec`/`test-assignment` now lower. Next sibling
  blockers: `<byte>`/`<restart>`/`<class>`/`<singleton>`/`<type>`/`<stretchy-sequence>`.
- **Funcall fix (real root cause):** a user `define function f` whose name collides
  with a stdlib generic (the loader rewrites every stdlib `define function` to a
  method-on-`<object>` generic) resolved to the STDLIB body — `make_function_ref`
  short-circuited on `is_generic_defined` before checking for a DIRECT registration.
  Fix: direct `(name,arity)` registration now wins; genuine multi-method generics
  still use the trampoline. Un-deferred `compose`/`curry`/`rcurry`/`always` (fixed
  arity). Verified: `compose(inc,double)(5)=11`, `curry(\+,10)(5)=15`, `rcurry`,
  `always`, `let f = method(a,b) a+b end; f(3,4)=7`. (N-ary forms await `#rest`
  collection — next.)
- **Verified:** in-tree 55/55 both paths, nod-runtime 150/0, nod-sema 44/0,
  smoke-aot 6/6, eval=2, corpus 63/161.

### 2026-06-15 — Iteration 18: `<character>` code-convertible + char ops (autonomous wave)

- Added `%char-code`/`%code-char` primitives (nod-runtime + lower.rs + codegen/jit) so
  a `<character>` converts to/from its integer code; `<character>` carved out of the
  generic object-equal path so char `=`/`~=`/`<`/`>` use inline integer primops; and
  i32↔i64 coercion at every runtime-call boundary (chars are raw i32, integers tagged
  fixnums — kept the i32 repr because a tagged-fixnum char would be bit-identical to
  `<integer>` and corrupt `instance?`/dispatch).
- `strings.dylan`: `<character>` predicates (`whitespace?`/`alphabetic?`/`digit?`/
  `alphanumeric?`/`uppercase?`/`lowercase?`), `as-uppercase`/`as-lowercase` on a char,
  and `as(<integer>, ch)` / `as(<character>, code)` (compile-time intrinsic).
- Verified build+run: `'A'='A'`=#t, `'A'='B'`=#f, `as(<integer>,'A')`=65,
  `as-uppercase('a')`='A', `digit?('5')`=#t/`digit?('x')`=#f, `whitespace?(' ')`=#t.
  nod-runtime 150/150 (+3), in-tree 55/55 both paths, smoke-aot 6/6, corpus 60/161
  (byte-identical pass set). Unblocks the char API the strings agent had to skip.
- Limitation (documented): runtime dispatch on a raw `<character>` resolves by STATIC
  type (an odd code can't be runtime-classified vs a pointer) — correct for these ops.

### 2026-06-15 — Iteration 17: macro `*` repetition + functional stdlib (autonomous wave)

- **Macro engine: zero-or-more repetition.** `{ unit } ...` (optionally `{ unit } ; ...`
  / `, ...`) in a macro pattern now binds a repeated variable to a sequence and the
  template splices it back; empty match is valid. Used it to collapse `cond` from 4
  hard-capped arity rules to ONE uncapped rule. Verified build+run: `cond` with 1, 5,
  7, and zero clauses all fire the right arm (the old cap couldn't do 5+). nod-macro
  6/0. (`nod-macro/src/lib.rs` +517, `macros.dylan` cond rewrite.) Also fixed the
  resulting non-exhaustive-match compile breaks in `tests/nod-tests/tests/
  macro_match.rs` + `macro_expand.rs` (new `PatternElem::Repetition` /
  `MatchedFragment::Repeated` arms) so the test crate builds.
- **stdlib `functional.dylan`** (NEW): `identity`, `complement`, `choose-by`
  (pure-Dylan, build+run verified: 42 / small / 3).
- **Real codegen bug found (high-value next target):** the variadic combinators
  (`compose`/`curry`/`rcurry`/`always`) are BLOCKED because **calling a captured/
  parameter `<function>` value with 2+ args computes garbage** — only the 1-arg
  `%funcall1` path is correct; `%funcall2` works for builtin operator refs (`\+`) but
  NOT user functions. Fixing multi-arg funcall on user `<function>` values unblocks
  curry/compose + a chunk of higher-order code. Queued.
- **Verified:** in-tree 55/55 both paths, smoke-aot 6/6, eval=2, nod-macro 6/0,
  nod-sema 44/0, corpus 60/161 (engine/availability additions, not compile-count
  movers).

### 2026-06-15 — Iteration 16: stdlib extension (workflow: sequences + numbers + collections)

A 3-agent workflow (one worktree per category), each build+run-verified by me on main.

- **sequences.dylan** (+10): `copy-sequence`, `fill!`, `position`, `count`,
  `find-element`, `remove-duplicates`/`!`, `intersection`, `difference`, `union`.
- **numbers.dylan** (NEW, +25): `abs`, `negative`, `quotient`/`remainder`/`modulo`,
  the `truncate`/`floor`/`ceiling`/`round` families (+ two-value `…/` forms), `gcd`,
  `lcm`, `expt`/`power`, `logand`/`logior`/`logxor`/`lognot`/`ash`/`logbit?`/
  `integer-length` (over the bit-vector word primitives). Float-only rounding +
  variadic `#rest` forms deferred (no float path / `#rest` binding).
- **collections.dylan** (+5 kept): `key-sequence` (+ `<table>`), `key-test`,
  `element-or-default` (+ `<table>`), `copy`, `map-into`.
- **Dedup:** the sequences and collections agents independently added the same five
  (`copy-sequence`/`fill!`/`find-element`/`remove-duplicates`/`!`); removed the
  collections copies (kept in sequences) so the loader gets one `<object>` method each.
- **Verified:** `gcd(12,18)=6`, `abs(-5)=5`, `expt(2,10)=1024`, `logand(12,10)=8`,
  `union` size 5, `copy-sequence`/`key-sequence` correct (build+run); in-tree 55/55
  both paths (shim re-baked for the new file), smoke-aot 6/6, corpus 60/161 unchanged
  (stdlib additions add availability; corpus files fail on other downstream gaps).
- **Found + FIXED a separate pre-existing regression:** the CLI `eval` command
  returned `<eval-entry> missing after lowering`. Root cause: the Dylan-parser shim
  (default path, rebuilt during the collection-classes work) mis-translates eval's
  synthetic `define function <eval-entry> () … end` wrapper, dropping the entry; the
  Rust reference parser handles it (`--parse-with-rust eval` always worked, as did
  the in-process library `eval` used by unit tests — the shim override is a
  driver-only feature). Fix: CLI `eval` now implies the Rust parser
  (`main.rs` `want_parse_with_rust`). `eval '1 + 1'` = 2 again; the shim default path
  is unchanged for `dump-dfm`/`build` (in-tree 55/55 both paths). The underlying
  shim mis-translation of `<…>`-named function definitions is logged as a follow-up.

### 2026-06-15 — Iteration 15: stdlib strings + select/case + collection classes (3 parallel agents)

Three agents in parallel worktrees (disjoint files), each build+run-verified by me on
main before merge.

- **strings** (`stdlib/strings.dylan`, +18 ops): case-mapping, predicates, trim,
  `string-position`/`count-substrings`/`replace-substrings`, `repeat-string`,
  `string-reverse`, `string-to-integer`, `split`/`join`, `string-equal?` — pure-Dylan
  over `%byte-string-*`. Verified by running. **Gap surfaced:** `<character>` lowers
  to a raw i32 (not a tagged fixnum), so char↔code ops are blocked on a future
  `%char-code`/`%code-char` primitive — char-typed predicate overloads deferred.
- **select / case** (`nod-reader/src/parser.rs`): these were **dead parser surface** —
  `Expr::Case` was parsed but never lowered ("expression form `case` not lowered").
  Now desugared to the kernel `if`-tree IN THE PARSER (AOT-safe): full arity (no cap),
  comma-separated multi-value arms (a dropped-value bug fixed), `by` test, `otherwise`,
  empty consequents → `#f`, `select` key evaluated exactly once. Verified by running.
  **Corpus 59 → 60** (`gabriel/boyer` via `case`).
- **collection classes** (`stdlib/arrays.dylan` + nod-runtime/lower/codegen):
  `<array>`/`<vector>`/`<simple-vector>`/`<byte-vector>`/`<bit-vector>` registered as
  **pure-Dylan `define class`** (the AOT-safe `<stream>` route — no class-id drift);
  **`instance?` CPL-walk fix** (new `ClassCheck::VectorOrUserClass`: SOV fast path OR
  CPL walk, so `instance?(bitvector, <vector>)` is correct); and a **working
  `<bit-vector>`** (`bitvectors.rs`: packed 60-bit-word store, `make` redirect in
  `lower_make`, `element`/`size`/`set-bits!`/`unset-bits!`/`bit-count` + word bitwise
  primitives `logand/logior/logxor/lognot/ash`). Verified by running: instance? cases
  + `make(<bit-vector>, size: 10)` set/count = 3, multi-word `size: 130` = 130.
  **Deferred (Phase B):** `bit-vector-and/or/xor/andc2` (multi-value, pad keywords),
  variadic `vector(...)`, `limited(...)`, packed `<byte-vector>`.
- **Regression fixed:** the iter-14 block-return work deleted
  `nod_runtime::allocate_block_id`, but `tests/nod-tests/tests/conditions.rs` still
  imported it → the nod-tests crate didn't compile (I'd missed it: I ran
  `-p nod-runtime`, not `-p nod-tests`). Replaced with a test-local counter.
- **Recheck:** in-tree 55/55 on BOTH the shim and `--parse-with-rust` paths (shim
  re-baked via self-rebuild for the new classes), smoke-aot 6/6, nod-runtime 147/0
  (+3 bit-vector tests), nod-sema 44/0, nod-tests conditions 9/0, corpus 60/161.

### 2026-06-14 — Iteration 14: AOT levers — LNK2005 gate + block-return + func-refs

Process: a design→adversarial-challenge→synthesis workflow produced vetted go/no-go
plans ([deep-levers-plan.md](deep-levers-plan.md)); the AOT levers were all gated on
one infra bug, so I fixed that first, then agents (in worktrees off the fixed main,
so they could build+run-verify for the first time) implemented the two GO plans, and
I **re-verified each by building+running exes on main** before merging.

- **GATE — cold-build `LNK2005 nod_user_main` fixed** (`nod-aot-stub` crate). The
  default stub now lives in its own crate → its own object → MSVC on-demand archive
  extraction reliably drops it; nod-runtime's CGU partition can no longer merge it
  with an always-pulled module. (`codegen-units=1` was the WRONG fix.) This also
  fixes AOT linking for fresh clones / CI / self-rebuild. Guard: `smoke-aot.sh`.
- **block-return** — `block (return) … return(x) … end` crashed in built exes. Three
  compounding bugs: AOT safepoint shadow-stack never truncated on NLX (now symmetric
  with the JIT path), `extern "C"`→`"C-unwind"` on the safepoint shims (unmasked the
  diagnostic), and block_id ORed bit 62 → overflowed the fixnum domain (now bit 61).
  Verified build+run: `trial(5)=99`, `trial(1)=7`, cleanup/nested correct, exit 0;
  negative control crashes (load-bearing); `error("boom")` still crash-dumps.
  **0 corpus-compile gain** (runtime fix — dump-dfm never ran the code) but real:
  `block(return)` programs now RUN instead of crashing.
- **func-refs** — `\==`/`\~=`/`instance?` as VALUES crashed built exes
  (`no registered function ==`; shims were JIT-only). Registered the shims in
  `ensure_operator_shims_registered` (runs in BOTH JIT and AOT startup) + added
  `operator_arity` entries. `~=` is value-semantics (via `nod_object_equal_p`), not
  identity. Verified build+run: `1,1,1,0` and `\~=` value-semantics proof.
  **Corpus 58 → 59** (`apple-dylan-test-suite/test-collection3`).
- Guards: in-tree 55/55, nod-runtime 144/0, nod-sema 44/0, smoke-aot now 6/6
  (added `blockreturn` + `funcref` cases). See [[aot-build-run-verification]].
- Note: one self-inflicted scare — a hasty `git merge --ff-only` aborted on a dirty
  `Cargo.lock` and I force-deleted the (unmerged) block-return branch; recovered the
  dangling commit via reflog and merged cleanly. Lesson: don't `branch -D` before
  confirming the merge landed.

### 2026-06-14 — Iteration 13: fix `head(#())`/`tail(#())` returning garbage (runtime)

- **Bug** (surfaced by iter-12's benchmark run): `nod_pair_tail(#())` /
  `nod_pair_head(#())` returned raw `0` (not a valid Word) for the empty list, so
  `empty?(tail(#()))` was *false* and list iterations like `l := tail(tail(l))` /
  `l = l then tail(l)` never terminated on odd-length lists.
- **Fix** (`src/nod-runtime/src/lists.rs`): guard the `nil` immediate —
  `tail(#()) = #()` and `head(#()) = #f` instead of garbage. One runtime function
  body each, so it's identical on the JIT and AOT paths (no class-id/registration
  risk). Verified by build+run: `empty?(tail(#())) = 1`; `tail(#(1))` still empty;
  in-tree 55/55, nod-runtime 144/0, smoke-aot OK, `for` battery still 15/15/10/60,
  corpus unchanged at 58 (correctness fix, not a compile-count mover).
- **Build note:** `cargo build -p nod-driver` did NOT reliably rebuild `nod-runtime`
  for the AOT-EXE link path — an explicit `cargo build -p nod-runtime` was needed for
  the fix to reach a built EXE. Relevant to why agent worktree build+run checks can
  diverge from a clean-`main` rebuild. See [[aot-build-run-verification]].

### 2026-06-14 — Iteration 12: complete the Dylan `for` loop (agent, worktree) — compile 55 → 58

- **What:** generalised `for` lowering (`src/nod-sema/src/lower.rs`) from single-numeric
  to the full clause vocabulary — **step** clauses (`var = init then next`),
  **multiple** comma-separated clauses, **`until:`/`while:`** keyword clauses, and the
  **`finally`** result. Implements Dylan's **simultaneous-step** semantics (each
  clause's next-value is computed into a temp, then all assigned — so a later clause's
  step reads the *old* bindings) and makes numeric `to` **direction-aware** (`by -1`
  ⇒ `>=`, fixing a latent bug in the old single-numeric path).
- **Verified by me on clean main (build → exe → run):** `sumfin(5)=15`
  (numeric+step+finally), descending `by -1` `=15`, `while:` `=10`, and the
  parallel-step proof `sumlist(#(10,20,30))=60` (a sequential step would give the wrong
  sum). Regression: in-tree 55/55 (ast+dfm), nod-sema **44/44** (7 new for-desugar
  tests), smoke-aot 4/4, sum 1..100=5050 still works.
- **Corpus 55 → 58:** gabriel `div2`, `browse`, `destru` newly lower (were blocked on
  `for…finally`/step/multi-clause). The benchmark EXEs still hit *other* pre-existing
  runtime gaps beyond `for` (`tail(#())` non-empty, `block(return)` exit path,
  `<vector>`/`limited` classes) — out of this task's scope.
- **Pattern confirmed:** this is the 4th agent (all *lowering*) to hold up under my
  build+run review (iters 9, 11-B, 12), vs 3 *AOT/runtime/parser* agents reverted.
  Agents + a hard build+run gate work well for lowering; delicate AOT/parser work I keep
  in hand. See [[aot-build-run-verification]].

### 2026-06-14 — Iteration 11: `#key` params + computed-callee calls (agent B, kept); agents A & C reverted

Spun up three worktree agents (user: "use agents to assist"); reviewed each by
building + RUNNING executables on clean `main` (NOT trusting self-reports). Only
one held up.

- **KEPT — agent B (`src/nod-sema/src/lower.rs`):** lowers (1) `#key`/`#rest`/
  `#next` params (previously never bound — `define function f (a, #key b) a + b
  end` failed to lower on baseline); (2) computed/curried callees (`adder(10)(5)`);
  (3) keyword args threaded through the funcall path. **Verified on main:**
  `f(3, b: 4)`→7, `f(a: 10, b: 20)`→30, `adder(10)(5)`→15, positional add→6.
  Guards: in-tree 55/55, nod-sema 37/37, smoke-aot 4/4, eval=2, corpus 55→55 (no
  regression). Remaining clean-failing gaps: `#rest` arg *collection* (LLVM arity
  error — binding works, varargs collection unimplemented) and an immediately-
  called method literal with a keyword arg (`(method (#key x) x end)(x: 7)` →
  "unknown callee"). **0 corpus gain** (the pattern isn't in the corpus) but a real
  foundational compiler improvement (keyword params are core Dylan). Dropped B's
  `Cargo.toml` link change (see LNK2005 note below).
- **REVERTED — agent A (`test` macro):** used `?opts:parameter-list` to match
  testworks option heads, but `(description: "x")` is a keyword *property list*, not
  a parameter list, so the parser never hands it to the macro — a no-op for the real
  cases. Its "prints 42 / 24→13" claims did not reproduce on clean main. The real
  fix needs PARSER support for property-list heads in definition-macro calls.
- **REVERTED — agent C (func-refs/classes):** honestly reverted its class part, but
  the Part-1 func-ref program (`\==`/`instance?` as values) crashes on clean main
  with the same iteration-10 error (`no registered ==`) — the shims still aren't
  live in the AOT EXE. Its `/FORCE:MULTIPLE` link flag addressed a cold-build
  LNK2005 (real — see below) but is the wrong instrument. "1,1,1,0 verified" did not
  reproduce.

**Lesson reinforced:** agents are reliable for pure-Dylan stdlib and (re-verifiable)
lowering, but NOT for AOT/runtime/parser internals — their worktree build+run checks
don't reproduce on a clean main build. Re-verify every agent change by building +
running on main; do the delicate AOT/parser fixes directly. (Two independent agents
hit a cold-build LNK2005 their warm-`main` reviewer does not — logged in
known-limitations.)

### 2026-06-14 — Iteration 10: REVERTED — agent's runtime change was AOT-broken; new AOT guard

- **Attempt (agent, worktree):** resolve `instance?`/`==`/`~=` as first-class
  function references + register `<vector>`/`<array>`/`<lock>`/`<thread>` class
  names. The agent verified via `dump-dfm` + `eval` and reported in-tree 55/55.
- **Why it was reverted:** on review I built **executables** and ran them — two
  AOT-only crashes the agent's verification never exercised:
  1. **Class-id drift** (`aot.rs` drift assert): registering 9 new classes in
     `collections.rs::ensure_registered` shifted the class-id sequence so the AOT
     EXE baked one id for `<stream>` but the runtime allocated another — *every*
     built EXE crashed on startup.
  2. **Missing AOT shim** (`functions.rs` `nod_make_function_ref: no registered
     function ==`): the `==`/`~=`/`instance?` func-ref shims were registered in
     the JIT path but not the AOT runtime path, so `\==` etc. crashed when called
     from a built EXE.
  `dump-dfm` only checks *lowering* and `eval` ran against a stale shim, so both
  bugs were invisible to them. The +1–2 corpus "compile" gain was hollow (files
  lowered but crashed when run). **Fully reverted to the prior good commit.**
- **Permanent fix to process:** added `tools/smoke-aot.sh` — builds + RUNS a
  handful of programs through the AOT pipeline and asserts stdout. Catches exactly
  this class of bug (the `dump-dfm`/in-tree guards never build an EXE). Run it
  after any nod-runtime class/shim or nod-sema lowering / AOT codegen change.
  Green on the current state (arith / for / local method / 0-param method).
- **Targeting rule learned:** pure-Dylan stdlib additions (iter 8) are AOT-safe by
  construction — the loader runs identically in JIT and AOT. Rust runtime class /
  shim registration is delicate (compile-time vs EXE-runtime id/registration
  consistency) and MUST be build+run verified, registering in the AOT path too.
- **Bonus finding (pre-existing, deferred):** a function whose body is written on
  ONE line and contains a `for` loop fails to build (`codegen: unknown callee`),
  while the identical multi-line form works — a newline-sensitivity gap in the
  Rust-parser/for-desugar path. Edge case (real corpus code is multi-line); logged
  in known-limitations.

### 2026-06-14 — Iteration 9: back-end lowering features (agent, worktree) — compile 52 → 55

- **Approach:** ran a dedicated agent in an isolated git worktree; reviewed +
  verified + merged its work onto main (all in `src/nod-sema/src/lower.rs`).
- **Landed:** (1) numeric-range `for` → `let`+`while` desugar (`<=`/`<`/`>` per
  `to`/`below`/`above`, default step 1); (2) 0-parameter `define method` → a
  direct-call function; (3) **`local method`** via the existing closure/cell
  machinery — one `<cell>` per local method up front (mutual recursion),
  `%make-closure` capturing those cells, calls through `%cell-get` +
  `nod_funcall_N` (fixed a real source-vs-mangled name-keying bug along the way);
  (4) statement forms (`block`/`for`/`local method`) in expression position via a
  `pending_sink` on the builder. `in`/explicit-step/multi-clause/`finally` `for`
  forms still bail cleanly.
- **Verified by me (not just the agent's claims):** in-tree 55/55 (ast+dfm);
  nod-sema unit tests 37/37; corpus compile 52 → 55. **Correctness runs** (build
  → exe → run): `for` sum 1..100 = 5050, `for above` countdown = 15, recursive
  `local method` `fact(5)` = 120 (capture + self-recursion), 0-param method = 42.
  `deriv`/`tak`/`ctak` newly compile.

### 2026-06-14 — Iteration 8: stdlib library bindings (agent, worktree) — compile 47 → 52

- **Approach:** dedicated agent in an isolated worktree; I read the full diff,
  verified, and merged. New `stdlib/sequences.dylan` (+ registration in
  `stdlib.rs`), pure Dylan over existing primitives (FIP, pair, SOV, `%funcall*`).
- **Added:** number predicates (`even?`/`odd?`/`zero?`/`positive?`/`negative?`);
  sequence accessors/searches/folds (`first`/`second`/`third`/`last`/`member?`/
  `any?`/`every?`/`find-key`/`reduce1`/`empty?`/`aref`); builders
  (`reverse`/`choose`/`remove`/`add`/`list`/`pair`/`head`/`tail`); `max`/`min`;
  setters; `$maximum-integer`/`$minimum-integer`/`$machine-word-size`.
- **Verified:** corpus compile 47 → 52; in-tree 55/55; `eval '1 + 1'` = 2.
- **Caveat (documented inline):** a few are thin stand-ins (non-destructive
  `add!`/`reverse!`/`sort!`, 1-arg `list`, ignored `test:`/`count:`) — labeled
  stubs in the same spirit as the testworks helpers, to be made faithful when the
  class machinery / variadic `#rest` / in-place primitives land. Harmless now
  (suites don't *run* yet — no runner).

### 2026-06-14 — Iteration 7: minimal testworks (`define test`/`define suite` + `check-*`) — corpus compile 34 → 47

- **Demand:** ~84 corpus files use the testworks harness (`define test`/`define
  suite`, `check-equal`/`check-true`); `` `define test/suite` not lowered ``
  blocked them.
- **Added (via the new definition-macro engine):** `define test NAME () body
  end` → `define function`; `define suite … end` → a no-op function (the
  `test`/`suite` listing is dropped — running suites needs a real runner);
  `check-equal`/`check-true`/`check-false` + `assert-true` functions (leading
  `description` accepted and ignored). Minimal stand-ins — testworks is a
  separate package, not vendored.
- **Result:** a synthetic `define test`/`define suite` builds + runs (→1);
  corpus **compile (`dump-dfm`, `--parse-with-rust`) 34 → 47 / 161**. In-tree
  fixtures 55/55. This exercises the definition-macro engine on real,
  high-frequency forms. Files that additionally need `common-dylan`/`io`/
  `collections` bindings stay blocked on the (unported) library stack.

### 2026-06-14 — Iteration 6: definition-macro recursive span rewrite — `tak`'s macros fully expand

- **Blocker:** the definition-macro shortcut (lexing the expansion with the
  call-site file id) broke RECURSIVE expansion of a body-macro *inside* the
  produced item — a `benchmark-repeat` call inside the `define benchmark`-produced
  function re-lexed the wrong tokens (its span pointed into the expansion buffer).
- **Fix (`nod-macro`):** `expand_definition_macro` now uses a scratch `SourceMap`
  and an origins-based `rewrite_spans_item` (mirroring `expand_one`), so the
  produced item's spans map to the real source and recursive body-macro expansion
  re-lexes correctly. Added `walk_item_spans` (reusing the existing
  `walk_stmt_spans`/`walk_expr_spans`).
- **Result:** `tak.dylan`'s macro layer **fully expands** — `define benchmark` →
  function, `benchmark-repeat` → its body, `assert-equal` resolves. The error
  moves to a **back-end lowering gap**: `` `local method` not lowered `` (the
  `trtak` method uses a `local method`) — no longer a macro/parse issue. The
  synthetic `define benchmark` still builds + runs; in-tree fixtures 55/55.
- **Next:** lower `local method` (back-end) — blocks `tak`/`trtak` and several
  other gabriel files; then a real benchmark file should compile end-to-end.

### 2026-06-14 — Iteration 5: testworks-compat stdlib helpers + matcher nested-`end` fix

- **Demand:** real gabriel benchmarks (`tak.dylan`) call `benchmark-repeat
  (iterations: N) … end` and `assert-equal` — from `testworks`, which is a
  separate package not vendored in the reference tree.
- **Added (minimal, faithful):** `assert-equal(expected, actual)` (equality
  check) to `stdlib/collections.dylan`; `benchmark-repeat ?opts:expression
  ?body:body end` (runs the body, yields its value — drops only the repeat-count
  timing) to `stdlib/macros.dylan`.
- **Found + fixed a *third* nested-`end`-balancing bug:** the macro **matcher's**
  `?body` termination (`nod-macro`) had its own hardcoded block-opener list,
  missing the library body-macros (`benchmark-repeat`, `with-lock`, …). Extended
  it to match the parser's set, so a matched macro's body can contain nested
  library-macro `end`s.
- **Verified:** `assert-equal(7,7)`→`#t`, `(7,8)`→`#f`; `benchmark-repeat` in a
  normal function body builds + runs (=7); the `benchmark` definition macro now
  matches real `tak`'s body. In-tree fixtures unchanged (55/55).
- **Remaining blocker for `tak.dylan`:** definition-macro-produced items use a
  span shortcut (lexed with the call-site file id), so RECURSIVE expansion of a
  body-macro *inside* the produced item (`benchmark-repeat` inside the
  `benchmark`-produced function) re-lexes the wrong tokens. Needs an origins-based
  span rewrite of the produced item (scratch SourceMap + `rewrite_spans_item`) —
  next.

### 2026-06-14 — Iteration 4: definition macros (engine feature) — `define benchmark`

- **Demand:** the gabriel benchmark files wrap pure functions in
  `define benchmark NAME () body end` — a *definition* macro. Our engine had **no
  definition-macro support** (the marquee missing macro feature). A hand-extracted
  `tak` already builds to an exe and runs correctly, so the wrapper was the blocker.
- **Research (agent):** the collect→expand two-pass and the rule parser already
  handle this shape; the gap is recognising a top-level `define <word> … end`
  (an `Item::DefineOther` whose keyword is a registered macro) and expanding it by
  re-parsing the substituted expansion as a top-level **item**, not an expression.
- **Fix (`src/nod-macro/src/lib.rs`):** added `expand_definition_macro` (mirrors
  `expand_one` but re-parses via `parse_module_with_macros_rust` → `Item`) and a
  span-based `call_site_fragments_span`; `expand_module` now rewrites
  `DefineOther{keyword ∈ table}` through it. Added the first definition macro,
  `benchmark`, to `stdlib/macros.dylan`.
- **Result:** `define benchmark foo () 3 + 4 end` → `define function foo` →
  **builds to an `.exe` and runs (prints 7)**. The first definition macro in
  NewOpenDylan. In-tree fixtures unchanged (55/55).
- **Caveats / follow-ups:** verified via `--parse-with-rust`; the DEFAULT pipeline
  routes through the Dylan parser, which doesn't yet recognise definition-macro
  calls (panics "define: expected a define-body word") — needs Dylan-parser
  awareness or a Rust fallback in the lowering path. Nested body-macro expansion
  inside a definition-macro body + fine-grained diagnostics need an origins-based
  span rewrite. Real gabriel files additionally need `benchmark-repeat` +
  `assert-equal`.

### 2026-06-14 — Iteration 3: route library body-macros as macro calls (parser bug)

- **Target:** the residual `KwEnd` (context-(b) body-macros in *parsed* function
  bodies) — gabriel `stak`/`traverse`/`triang` (`dynamic-bind`), plus
  `with-lock`/`with-open-file`/etc. used inside `define function`/`method`.
- **Diagnosis:** these are block-openers (`is_block_opener_kw`) but were not in
  the parser's `known_macros` set, so the statement dispatch parsed them as plain
  calls (no body) and their `end` dangled.
- **Fix:** route any `is_block_opener_kw` word (not just `known_macros`) to
  `parse_body_shaped_macro_call` when the call shape matches — one-line guard
  change at the expression/statement dispatch.
- **Result:** corpus parse **139 → 150**; `KwEnd` failures 12 → 1. In-tree
  fixtures unchanged (55/55). The remaining 11 failures are long-tail singletons
  (keyword-symbol atoms, no-`end` define-forms, adjacent strings, `==`/operator
  names, a couple of param-list `,` cases).

### 2026-06-14 — Iteration 2: nested body-macro `end` balancing (parser bug)

- **Target:** the dominant `KwEnd` cluster (28 files) — gabriel benchmarks
  (`define benchmark` wrapping `benchmark-repeat (…) … end`), and threads/io/
  system tests using `with-lock` / `with-open-file` / `printing-object` /
  `collecting` / `timing` / `profiling`.
- **Diagnosis (agent):** *two* causes. (1) A structural bug —
  `parse_body_shaped_macro_call` (the parsed-body counterpart to
  `skip_body_to_matching_end`) tracked only grouping depth, not block depth, so
  **any** body-macro whose body contained a nested `end`-block closed early
  (even already-known macros: `unless (t) if (t) … end; end;` failed). (2) Several
  library/test body-macros weren't in the block-opener set.
- **Fix:** gave `parse_body_shaped_macro_call` the same block-depth tracking as
  `skip_body_to_matching_end`; extended `is_block_opener_kw` with the
  library/test body-macros (`benchmark-repeat`, `with-lock`, `with-open-file`,
  `printing-object`, `collecting`, `timing`, `profiling`, …).
- **Result:** corpus parse **123 → 139**; `KwEnd` failures 28 → 12; 11 gabriel
  benchmarks (boyer, ctak, dderiv, deriv, div2, fft, puzzle, tak, takl, browse,
  destru) now parse. In-tree fixtures unchanged (55/55 ast + dfm).
- **Residual:** a few context-(b) body-macros (e.g. `dynamic-bind` in
  stak/traverse/triang) still parse as plain calls because they aren't *routed*
  as macro calls (not in the parser's known-macro set) — next.

### 2026-06-14 — Iteration 1: `for` iteration-clause forms (parser bug)

- **Target:** gabriel benchmarks `div2`, `browse`, `triang`, `cl-stubs`, `cn2`,
  `destru`, `traverse` — parse-fail `expected ) after for-clauses, got Equal` /
  `got KeywordColon`.
- **Diagnosis:** compiler bug. `parse_for_clause` only handled `var in …`,
  `var from … [to/below/above/by]`, and bareword `until`/`while`. It rejected
  three real Dylan for-clause forms: `var = init then next` (explicit step),
  `until:` / `while:` keyword clauses, and `var keyed-by key in coll`.
- **Fix:** added `ForClause::Step` + `ForClause::Keyed` AST variants
  (`ast.rs`, `format_dylan.rs`) and extended `parse_for_clause` to accept all
  three forms (`parser.rs`).
- **Result:** every "for-clauses" parse error eliminated; corpus parse
  **121 → 123**. `cl-stubs` now fully parses; the others advance past the
  for-clause and surface the next blocker (the `KwEnd` body-macro cluster,
  next iteration). In-tree fixtures unchanged (55/55 ast + dfm).
