# 2026-06-05 — Consolidation after the hacking week: state, loose ends, and the plan

*A deliberate pause to stabilise and plan, after a run of late-night
sessions (Sprints 52–53) that moved fast. Nothing here is new feature
work — it's an honest inventory and a prioritised path back to a calm,
all-green baseline before the next push.*

## Where we actually are

The whole week's work is **committed and pushed** (origin/master in sync
through `31d3218`). The driver builds. The front-end self-hosting ladder:

| Layer    | Status | Evidence |
|----------|--------|----------|
| Lexer    | ✅ self-hosted | oracle gate green |
| Parser   | ✅ self-hosted | translate gate **34/44** byte-identical, zero divergences |
| Macros   | ✅ self-hosted (opt-in) | `NOD_EXPAND_WITH_DYLAN`, fidelity proven vs the 6k-line compiler source |
| Sema     | ◐ oracle done, Dylan port **WIP** | `dump-sema` + `DYLAN_SEMA_WIRE.md`; Dylan walk checkpointed, not wired |
| Lowering | ⬜ still Rust | (next after sema) |

The shape is exactly the ratified architecture: a Dylan front-end being
grown layer-by-layer on the permanent Rust+LLVM back-end, each layer
validated by a byte-identical "two compilers, one truth" gate. **The
trajectory is healthy; this is progress, not drift.**

## Loose ends the fast week left (triaged)

**P0 — correctness, blocks a clean sweep**
1. **GAP-011 liveness (#300, in progress).** Global live-in/out fixpoint
   landed in 48b, but the symptom (`stretchy_vector_push` stale-root
   panic) still fires on heavy parse loops. This is the one real
   correctness hole — precise GC roots going stale.
2. ~~**`short_circuit_ops` JIT tests hang.**~~ **RESOLVED (commit
   `5f43507`).** The shared-root-with-GAP-011 hypothesis was WRONG. The
   real cause: a **flat-precedence regression from the Sprint 51e DRM
   migration**, which this very plan's verification also missed (it never
   ran `--test short_circuit_ops`, exactly the gap that left the stale
   `parser.rs` precedence tests). The two loop conditions
   (`until (i = n | element(s,i) = 98)` etc.) were authored under the old
   C-precedence parser; under flat precedence they regroup as
   `((i = n | element(s,i)) = 98)`, so once the index check fails the
   `|`/`&` yields `#f` and the trailing `= 98` keeps the loop alive
   forever. Dumping the JIT IR proved codegen + the short-circuit
   lowering are correct — only the grouping was wrong. Fixed by
   parenthesising the comparands. All 10 pass in 0.32s; a repo-wide scan
   found no other inline-Dylan test with the same shape. **Lesson
   (again): a global precedence change must be validated by running every
   JIT/value suite, not just the structural gates — structural gates
   compare two parsers that now agree on the *wrong* grouping.**
   Aside found en route: `nod-driver eval` on the *default* (shim/AOT)
   path crashes on the class-id-drift assertion (`aot.rs:1037`) for
   class-registering snippets — i.e. P1-4 below is broader than
   "dump-sema only"; the `--parse-with-rust` JIT path is unaffected.

**P1 — completeness, no green/red impact yet**
3. **Sprint 53.2 sema walk is unfinished** (`dylan-sema.dylan`, just
   checkpointed). Still has `DBG` `format-out` diagnostics that pollute
   stdout; the gate it names (`sema_topnames.rs`) isn't written, so
   nothing builds or runs it. To finish: strip the DBG lines, write the
   gate to byte-match `--parse-with-rust dump-sema`'s top-names section on
   the class-free fixtures (`hello`, `factorial`), iterate.
4. **53.1 shim class-id drift.** `dump-sema` panics through the shim
   (`aot.rs:1037`) but works with `--parse-with-rust`. The in-process
   gate path is clean, so it's latent — but it's the same class-id-drift
   smell flagged back in the 52.6 rollout notes. Worth root-causing once,
   since it'll bite again as more layers route through the shim.

**P2 — cosmetic / deferred-by-design (leave alone for now)**
5. **`newgc-core` build warnings** (unused imports in `evac.rs`/`mark.rs`/
   `pin.rs`/`lisp_layout.rs`, one never-read `dest_gen` field).
   Deliberately NOT touched: the collector is mid-evolution and the
   standing rule is to leave GC code alone unless a sprint requires it.
   Clear these only as part of a GC-touching sprint.
6. **52.x expander is ~50× slower** under its opt-in flag. By design —
   per the locked decision, front-end perf waits for full consolidation
   (one DFM handoff when lex→parse→expand→sema→lower are all Dylan), not
   hybrid tuning. Not a regression; the default path is unaffected.
7. **Stale harness task list** (~300 entries, almost all completed
   sprints). Noise, not risk.

## The plan (recommended order)

The theme: **get back to an unattended all-green sweep before adding new
surface.** Correctness first, then finish what's half-built, then resume.

1. ~~**Make the test sweep completable again (P0-2).**~~ **DONE
   (`5f43507`)** — was a precedence regression, not a deadlock; see above.
   Remaining: run the full `cargo test -p nod-tests` once to confirm
   nothing *else* hangs or fails now that the keystone is cleared, and
   that the whole-crate build is clean (watch for any lingering
   `is_no_alloc` test-build breakage).
2. **Land GAP-011 (P0-1).** Likely shares a root with (1). Get the
   `stretchy_vector_push` stale-root panic to stop firing on heavy parse
   loops; close #300.
3. **Finish or formally park Sprint 53.2 (P1-3).** Either strip the DBG
   and write `sema_topnames.rs` to green, or, if sema is paused, leave the
   checkpoint as-is (it's inert) and note it parked. Don't leave it
   half-visible.
4. **Root-cause the shim class-id drift once (P1-4).** Before more layers
   depend on the shim path.
5. **Then, and only then, resume the sema Dylan port** (53.3+: classes,
   slot accessors) on a calm baseline.

## Process notes (the "professional way", for the calmer week)

- **Push at the end of every session.** A week of work sat 19 commits
  deep with no remote copy — one disk failure from gone. Cheap insurance.
- **Keep the two-compilers gate sacred.** It's caught precedence bugs,
  `:=` mis-parsing, and representation mismatches that neither parser
  reported alone. It's the safety net that makes fast weeks survivable.
- **Don't tidy the collector for cosmetics.** GC churn has wrecked
  progress before; warnings in `newgc-core` wait for a GC sprint.
- **Perf stays deferred until consolidation.** Resist hybrid-perf
  rabbit holes; the win falls out of the single DFM handoff.

## Sweep outcome (same day, after the keystone fix)

With `short_circuit_ops` fixed the full `cargo test -p nod-tests` sweep
became completable for the first time. Ran it `--no-fail-fast` to get the
whole picture. The striking result: **of ~24 distinct failures, exactly
ZERO were product bugs.** Every one was a test-harness artefact:

| Failure | Count | Root cause | Resolution |
|---|---|---|---|
| `short_circuit_ops` loops | 2 | flat-precedence regression (51e) in the test source | parens (`5f43507`) |
| `c_function_call` dedup | 1 | test isolation — `STUB_ENTRY_SLOTS` had no `_reset_*_for_tests` | add reset (`eef9cc4`) |
| `codegen`/`gc`/`heap_objects`/`runtime` | 17 | parallel-test race: stdlib load registered `<stream>` twice | thread-safe `ensure_loaded` (`a11c760`) |
| `ide_shell_infra` | 1 | same stdlib race | fell out with `a11c760` |
| `winffi_structs::set_rect` | 1 | flat-precedence regression in the packing expr (`33040000` = the flat eval) | parens (`2442878`) |
| `lexer_oracle::oracle_hello` | 1 | intermittent: concurrent `nod-driver build`→`link.exe` collide (`LNK1104`); passes in isolation | **open** (test-infra) |

Two lessons, both reinforcing the plan's process notes:

- **A completable sweep is a precondition for trusting "green".** Three of
  these (the stdlib race, the link flake, ide_shell_infra) were *only*
  ever observable once the keystone hang was cleared and the sweep could
  run end-to-end. The fail-fast default had been hiding everything past
  the first failure.
- **The 51e precedence migration's blast radius was wider than its
  verification.** It updated the codegen/runtime value tests but missed
  `parser.rs`, `short_circuit_ops`, and `winffi_structs` — because the
  structural gates (translate/coverage) compare two parsers that now
  *agree on the wrong grouping*, so only value-asserting JIT tests catch
  it, and those weren't all run. Every value/JIT suite must run after a
  precedence change.

**Remaining (one item):** the `lexer_oracle` parallel-build flake. The
build-shelling tests (`oracle_hello`, the AOT EXE tests) each invoke
`nod-driver build` → `link.exe`; run concurrently they contend on shared
output files. Fix options for the weekend: serialise the build-invoking
tests (`#[serial]` + a shared build lock), or give each a unique output
dir. Not a product bug — but it's the last thing between here and an
unattended all-green `cargo test`.
