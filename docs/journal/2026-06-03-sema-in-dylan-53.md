# 2026-06-03 — Sema in Dylan (Sprint 53)

*Porting the semantic model — the authoritative record of what a module
declares — from Rust to Dylan. Follows [the macro engine](2026-06-02-macro-engine-to-dylan-52.md).
Spec: `specs/53-sema-dylan.md`.*

## Pipeline correction up front

I'd been saying "sema (53) then lowering (54)" and skipping a stage. The
front-end is **lex → parse → expand → sema → DFM → lowering**, and **DFM
(the Dylan Flow Model) is the oddly-named control-flow-graph IR** — its
own stage (`nod-dfm`: blocks + terminators). So:

- **sema** (53) — the recording model only (names, classes/CPL, generics,
  sealing). No IR.
- **DFM** (54; the spec's "AST→DFM lowering") — building the CFG IR.
- **lowering** — DFM→LLVM codegen (`nod-llvm`), permanent Rust back-end;
  the Dylan→Rust boundary becomes the DFM wire once the front-end is
  whole.

In `lower_module_full` (~7.7k lines) the sema record and the DFM/CFG
construction are **fused**. Sprint 53's `analyse_module → SemaModel` /
`lower_with_model` split *is* drawing the sema | DFM line. Recorded in
memory so I don't collapse the stages again.

## 53.1 — SemaModel + dump-sema oracle (landed)

The verification prerequisite: a serialisable model + a deterministic
dump to byte-compare the two implementations.

`LoweredModule` already carried the **classes** (`user_classes` — name,
parents, CPL, slots+offsets) and **sealing**. The two recording outputs
that were computed and *discarded* — `top_names` (fns→arity+return-est,
constants, variables) and `generics` — I captured onto `LoweredModule`
too (additive; lowering behaviour unchanged). `format_sema_model`
serialises the four sections deterministically; `dump_sema_for_file` +
the `dump-sema` driver subcommand expose it. Gate `sema_dump.rs` checks
`point.dylan`'s model is complete (CPL via C3, slots `x@8`/`y@16`, slot
generics, fn arities). Wire doc `DYLAN_SEMA_WIRE.md` committed.

### Decisions / discoveries

- **Names, not ids.** The dump references classes by name. Class ids come
  from a process-global counter and won't match the future Dylan impl;
  names + slot offsets + CPL order are the portable invariants. The wire
  doc bakes this in (deviating from the token/AST "spans not values" rule
  for the *id* specifically).
- **Sema has global side effects.** `dump_sema_for_file` registers the
  module's classes in the process-global registry, so it's **single-call
  per process** — a second call hits `ClassRedefinitionNotSupported`. The
  determinism comes from sorted tables, not re-running; verify-mode (53.5)
  must compute the two models in separate processes.
- **dump-sema CLI hits the pre-existing shim drift.** Through the shim,
  `dump-sema` (like `dump-dfm`) panics at `aot.rs:1037` (class-id drift,
  `<stream>` 1076 vs 1079). Run it `--parse-with-rust` for the oracle; the
  in-process test path has no shim and is clean. This drift is a separate
  pre-existing bug.
- **codegen tests need `--test-threads=1`** (pre-existing global-registry
  race) — not a regression from the capture.

I did the **additive capture** rather than the full structural
`lower_with_model` split first: it delivers the oracle (the thing that
unblocks the Dylan port) at near-zero risk on the 7.7k-line file. The
structural split (lowering reads *only* `SemaModel`) is a later 53.1 step.

## Where it leaves us

The sema model is captured + dumpable + gated; the wire contract is
written. Next is the Dylan port of the recording walk, tractable-subset
first (spec Move 3): **top-level name table → classes+slots (reusing the
already-ported C3) → generics+sealing**, each byte-matched via `dump-sema`
on a growing fixture subset. Then integrate `--sema-with-dylan` +
verify-mode (53.5). Per the perf strategy, the *default* flip waits for
the whole front-end; this sprint is about correctness + the model
crossing to Dylan.

## 53.2 implementation plan (scoped, turnkey)

Write a Dylan `collect-top-names(ast-body, source) -> <byte-string>`
(new `dylan-sema.dylan`, bundled with `dylan-parser.dylan` so it sees the
`<ast-*>` tree) that emits the `=== top-names ===` section byte-matching
the Rust `format_sema_model` for **class-free** fixtures first
(`factorial`, `hello`, `mutual`, `kernel-arith`).

- **Oracle target** (factorial): `fn factorial arity=1 return=Integer` /
  `fn main arity=0 return=Integer`. Sorted by name; then `constant <n>` /
  `variable <n>` lines (sorted).
- **AST accessors** (from `dylan-parser.dylan`):
  - root `<ast-body>` → constituents vector.
  - `<ast-body-definition>`: `defn-word` (token; "function"/"method"),
    `defn-method-name` (token | #f), `defn-params` (`<ast-param-list>` |
    #f), `defn-return` (`<ast-return-spec>` | #f).
  - `<ast-param-list>`: `params-required` (vector) → **arity** = its size.
  - `<ast-return-spec>`: `ret-values` (vector of `<ast-typed-name>`); the
    first value's type expr → the **return estimate**.
  - `<ast-list-definition>`: `defn-word` ("constant"/"variable") →
    constants/variables; the bound name is in `defn-list`.
  - token text via `token-source-text(tok, source)` (already used by the
    lexer adapter) or `identifier-token-name`.
- **Return-estimate mapping** (must match `TypeEstimate` Debug names):
  `<integer>`→`Integer`, `<single-float>`→`SingleFloat`,
  `<double-float>`→`DoubleFloat`, `<character>`→`Character`,
  `<boolean>`→`Boolean`, `<byte-string>`/`<string>`→`String`, no return
  spec / unknown type → `Top`. **TODO:** confirm against
  `collect_top_level_names` (now ~line 4250+ in `lower.rs` after the 53.1
  insertions) — it may infer from the body when no `=>` is present
  (factorial has an explicit `=> (<integer>)`, so the explicit-type path
  covers the first fixtures; body-inference is a follow-up).
- **Gate**: a `dump-sema`-style driver (`.prj` bundling lexer+parser+sema)
  that prints the top-names section for a fixture; a Rust gate runs the
  Dylan driver and `--parse-with-rust dump-sema`, slices both
  `=== top-names ===` sections, and asserts byte-equality. Start with
  `factorial`; grow the fixture set.

Auto-generated slot-accessor names (`<C>-getter-x`, …) in top-names come
from **class** processing, so they arrive with 53.3 (classes); 53.2
covers user `define function`/`constant`/`variable` only — which is why
class-free fixtures are the right first gate.
