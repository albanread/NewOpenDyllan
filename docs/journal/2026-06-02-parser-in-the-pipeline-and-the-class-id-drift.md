# 2026-06-02 — The Dylan parser enters the real pipeline; the shim-AOT class-id drift surfaces

*Sprint 51e.5. The Dylan parser stops being a `dump-ast` curiosity and
becomes a real front-end the compile/eval/build pipeline routes through —
with Rust fall-back and verify-mode. Doing so surfaced a pre-existing,
cross-cutting blocker that will gate every later phase: the shim-AOT
class-id drift. Follows [the parity push](2026-06-02-parser-parity-push-14-to-28.md).*

## Goal

Wire `--parse-with-dylan` into the *real* pipeline (not just the
`dump-ast` diagnostic), so the Dylan parser actually produces the
`ast::Module` that `compile`/`eval`/`build` lower — the milestone that
makes "the Dylan parser is the front-end" true (for the 28/36 it handles;
the rest fall back to Rust).

## What we did

**The parser override hook** (`9b990bc`), mirroring the lexer's
`set_lex_override` exactly. `nod_reader::parse_module_with_macros` became
a *dispatcher* (override-or-canonical); the canonical body is now
`parse_module_with_macros_rust` (byte-identical). `nod-driver` installs
`dylan_parse_module` (parse_to_tree → to_ast_module, whole-file fall-back
to `…_rust` on anything it can't translate) under `--parse-with-dylan` /
`--verify-parse`. Verify-mode runs both on the *pipeline* parse, diffs the
`format_ast_module` dumps, and proceeds with the Rust result.

Two correct-but-unobvious calls fell out:
- **The stdlib must bypass the override** (`nod-sema::stdlib` now calls
  `…_rust` directly). The stdlib is compiler infrastructure that must
  parse in every process; routing it through the partial Dylan parser
  crashed the build. The Dylan parser feeds the *user* pipeline, not the
  stdlib load.
- **`--parse-with-dylan` must NOT imply `--lex-with-dylan`.** The Dylan
  parse path lexes internally in the shim, and the whole-file Rust
  fall-back must keep the Rust lexer — the two lexers diverge on
  signed-number-as-arg (`-1`: Rust one signed `Integer`; Dylan `Minus` +
  `Integer` → a `UnOp` after the Rust parser folds it), which would
  silently diverge the byte-identical gate. (My brief said to imply it;
  the implementation correctly refused and documented why.)

Verified: default path green (codegen/sema/classes/dispatch + the
28/36 dump-ast gate, 0 divergences); `NOD_PARSE_WITH_DYLAN=1 eval
"1 + 2 * 3"` → `9` *through the Dylan parser*; multi-file AST-compose
(`compile_files_for_aot`) unaffected. Also unbroke the full-sweep build:
`gc_precise.rs` was missing the Sprint-48 `is_no_alloc` field and `gc.rs`
still asserted C-precedence `1+2*3==7` (both pre-existing staleness).

## Discovered — the shim-AOT class-id drift (the blocker)

Turning the shim on for an AOT build crashes class registration:
`nod_aot_register_user_class: class id drift — compiler expected 1079 but
runtime allocated 1081 for class <stream>` (`aot.rs:1016`).

**Root cause, precisely.** User class-ids are monotonic from
`ClassId::FIRST_USER` via `allocate_user_class_id()` (`classes.rs:699`).
The AOT contract (aot.rs:780-788) bakes each class's `expected_class_id`
at compile time and *asserts* the runtime allocates the same id — which
holds **only because "stdlib carries no `define class` today,"** so both
processes start from the same seed and register in the same order. The
Dylan **shim breaks that premise**: it *is* Dylan code with `define class`
(the parser's `<ast-*>`/`<token>` classes). Firing the shim's resolver
(`dylan_lex_jit::init` → `nod_aot_resolve_relocs`) registers those
classes through the *same* `next_user_id` counter, bumping it by the
shim's class count (+2 here) — so a subsequently-registered user class
lands at id+2, diverging from the compiler-baked id.

**Why it matters far beyond 51e.** Every front-end phase we migrate
(macros 52, sema 53, lowering 54) ships as a statically-linked Dylan shim
that, when active, registers its own classes. So this *one* bug gates:
(a) 51e.6 — defaulting the parser needs the full sweep green under the
shim flag; (b) all of 52/53/54's AOT integration. It is the critical-path
blocker for the rest of the migration, and it is **not** front-end work —
it's a back-end AOT-bootstrap fix.

**Fix directions (for the next session — NOT attempted here, per the
"don't destabilise the runtime" rule).** The mechanism points at three
candidates, in rough order of safety:
1. **Reset/seed the user-class counter deterministically around the AOT
   batch.** There's already `classes.rs:752` that retains only builtins
   and resets `next_user_id = FIRST_USER`. If the user-EXE resolver path
   establishes a known seed before registering the merged-LM classes
   (independent of whatever the shim registered), the baked ids match.
   Needs care: confirm the shim's classes aren't needed at user-EXE
   runtime (they shouldn't be — an AOT'd user program doesn't run the
   parser), and that the reset point is sound.
2. **Give front-end/shim classes their own id range** (e.g. a `FIRST_SHIM`
   band below `FIRST_USER`, or a separate counter) so they never consume
   user-class ids. Cleaner conceptually, more invasive.
3. **Register AOT user classes by name + look up, instead of asserting a
   baked monotonic id.** Changes the AOT contract; most robust, most work.

The assert is doing its job — it caught a real codegen-contract violation
loudly rather than letting dispatch silently corrupt. Keep it; fix the
premise underneath it.

## Where it leaves us

Parser phase substantially done: **28/36 byte-identical, wired into the
real pipeline, default path green, all pushed** (master == origin/master,
clean). `eval` provably runs through the Dylan parser.

**Next, in order:** (1) fix the shim-AOT class-id drift — the critical-path
unblocker for everything below; (2) 51e.6 — flip the parser default once
the full sweep is green under the shim flag; (3) the 8 remaining parser
fall-backs are macro-phase (`define macro`/`when`/`cond`) — they close
naturally in **Sprint 52**, where the Dylan front-end gains macro
parse+expand (locus B: expand Dylan-side before the wire emit); plus
`ide_win_calls` (`define c-function`) and `unified_ide`. Then 52 → 53 → 54
per `specs/README-frontend-migration.md`.
