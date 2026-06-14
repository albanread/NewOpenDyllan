# 2026-05-31 — Front-end self-hosting: the breakthrough session

*Sprints 51b–51e. Commits `9f22f86` … `db0b082`.*

## Goal

Continue Sprint 51b: get the Dylan-written lexer (`dylan-lexer.dylan`,
from Sprint 45) actually *running inside `nod-driver`* and driving the
real compile pipeline — not as a corpus exercise, but as the front-end
the driver uses. The standing year-3 ambition was self-hosting; the
near-term task was "make the Dylan lexer redirect the Rust lexer."

By the end of the session that goal had not only landed but pulled the
parser, an architecture reframe, and a coverage harness in behind it.

## What we did

The arc, in order:

1. **Wire format + shim (`9f22f86`).** Wrote `docs/DYLAN_TOKEN_WIRE.md`
   — the 16-byte-record contract for tokens crossing the Dylan↔Rust
   boundary — *before* either side's code. Built `dylan-lex-shim.dylan`:
   a classifier mapping the Dylan `<token>` hierarchy onto Rust
   `TokenKind` ordinals, trivia + preamble filtering, a text-stdout
   emitter. Verified byte-identical to `dump-tokens` on hello +
   factorial.

2. **`set_lex_override` hook (`fc386bc`).** Made `nod_reader::lex` an
   install-once dispatcher: `lex_rust` is the canonical path,
   `set_lex_override(LexFn)` lets a host swap in an alternate. All 8
   existing `nod_reader::lex` call sites pick up the swap transparently.

3. **JIT-strap take 1 (`827bf5d`, `3925e55`) — and the dead end.** Tried
   to JIT the shim into an isolated MCJIT engine at driver runtime,
   look up `dylan-lex-collect`, install its fn-pointer as the override.
   Got all the way through codegen + `register_methods` (241) +
   `register_top_level_functions` (310) and then **crashed in
   `register_variables`** on the first `define variable` init thunk
   (`*line-col-cache-pos* := 0`). Diagnosed but not fixed — the JIT-link
   path wasn't binding something the init thunk referenced.

4. **Static-link take 2 (`3917796`) — the right answer.** The user
   pointed out I was overcomplicating it: *"is it not just a case of
   registering 'main' and letting LLVM find it — see how NewCP lets
   Rust be replaced with M2 modules."* That reframed the whole thing.
   Added `AotShape::StaticLibrary` to the AOT pipeline (keep
   source-language symbol names, skip the synthetic `main`, promote the
   reloc resolver to external linkage), a `nod-driver build --library`
   flag, and a `build.rs` that finds `dylan-lex-shim.lib.obj`, links it
   into the driver, and sets a cfg. The lexer's externs now resolve to
   real Dylan-compiled code already in the process. **`--lex-with-dylan`
   went live, byte-identical to the Rust lexer on 4/5 fixtures** (the
   5th is the known signed-number lex divergence).

5. **Parser verify mode (`696727d`).** Same static-link pattern, second
   entry: `dylan-parse-collect` lexes+parses internally and returns the
   top-level error count. `--verify-parse` runs *both* parsers and
   compares the accept/reject verdict. 6/7 fixtures agreed; the 7th,
   `cond_smoke.dylan`, **diverged — and the Dylan parser was right.**

6. **Fixed the Rust parser (`496e0c1`).** The divergence was a *Rust*
   gap: `dump-ast` wasn't seeding the parser's body-macro name set, so
   `cond … otherwise … end` errored with "unexpected token KwOtherwise"
   four times. Seeded the names; 7/7 agree.

7. **AST wire format + `dump-dylan-ast` (`cb4e8db`).** `docs/DYLAN_AST_WIRE.md`
   — a tree-shaped wire format: flat stretchy-vector of 4-int records
   `(kind, span_lo, span_hi, subtree_size)`, pre-order packed. The
   Dylan `dylan-parse-emit` walks its AST emitting these; the Rust
   `dylan_parse_wire.rs` decodes them into a tree and prints it.
   **The Dylan parser is now a real AST producer for Rust consumers** —
   `dump-dylan-ast hello.dylan` prints a tree with the string literal
   sliced out of the source by the span the Dylan side emitted.

8. **Architecture ratification (`727aa13`).** Wrote `docs/ARCHITECTURE.md`
   and reframed MANIFESTO / PLAN / SPRINTS / README around the durable
   shape the session revealed (see "Discovered" below).

9. **Coverage harness (`db0b082`).** `dylan_parse_coverage.rs`: sweep the
   fixtures through `dump-dylan-ast`, aggregate the `Error` nodes into a
   frequency-ranked punch-list. **Baseline: 77% of corpus AST nodes
   already structured.**

## Why

**Why static-link beat JIT.** The JIT path tried to recreate the whole
runtime registration sequence (methods, blocks, functions, variables) at
driver runtime against a freshly-JIT'd module, and stumbled on a
JIT-link binding gap in the variable-init path. The static-link path
sidesteps *all* of that: the shim is AOT-compiled once, its relocations
resolved at link time by the existing AOT resolver, and the driver
just calls its `extern "C"` symbols. There is no runtime registration
replay to get wrong. The shim is *already part of the process*. This is
the NewCP "M2 modules replace Rust" pattern: compiled units talk through
linker-resolved symbols, not through a JIT engine. The lesson generalises
— **prefer "compile it and link it" over "JIT it at runtime" whenever the
phase is stable enough to ship as an `.obj`.** (We kept the JIT path in
the tree for a future hot-swap use case, but static link is the default.)

**Why the wire format is committed before the code.** See the lesson
below — this was the single most important process decision and it's
why both sides matched on first try.

## Discovered

The lessons, in rough order of how much they'll matter later:

1. **"Code gen is code gen."** The realisation that unlocked the
   architecture: if the Dylan front-end produces *the same DFM* the
   Rust front-end produces, LLVM emits *the same machine code*. The
   back-end can't tell which front-end fed it and doesn't care. That
   means the back-end (codegen, GC, JIT, linker) **never needs to move
   to Dylan** — DFM is the permanent cut line. The project is a Dylan
   front-end on a Rust+LLVM back-end, the same split rustc and GHC draw.
   This turned "someday self-hosting" into "the front-end self-hosts;
   the back-end is permanent, by design." (Ratified in `ARCHITECTURE.md`.)

2. **Across a language seam you pass bytes, not data structures.** The
   instinct "just hand them the `Vec<Token>` / the AST" is correct
   *inside one process in one language* — the type is the interface.
   The instant the boundary is a compiled-language seam (different
   allocator, type system, notion of "string"), the data structure on
   one side **does not exist** on the other. The only shared thing is a
   byte layout, and that layout *is* the interface. So:
   - **Lock the wire-format spec before either side's code.** Doing
     this is *why* the Dylan emitter and Rust reader matched on first
     try, for both tokens and AST.
   - **Cheapest shape that carries the data, not the most natural.**
     Rich `<ast-*>` classes flatten to 4-int records; each side
     reconstructs its own local shape. Type richness is private.
   - **Spans, not values, when both sides hold the source.** Emit
     `(VariableRef 527..537)`; the reader does `&src[527..537]`. Don't
     ship data you can address. This kept the AST wire format tiny.

   The user named the meta-lesson exactly: *"I was too used to just
   passing a data structure around."* That instinct is great until
   there's a seam, and there's no warning siren when you cross it.

3. **Two compilers are better than one — and verify-mode proves it.**
   Running both parsers and comparing isn't just a regression check;
   it's a *correctness amplifier*. On its very first run `--verify-parse`
   found a real Rust parser bug (the `cond` gap) that the Dylan parser
   had already got right. When the two agree you have evidence both are
   right; when they disagree the diff is the work order. We're keeping
   both implementations deliberately — the Rust front-end stays as the
   verify-mode oracle until each Dylan phase is the proven default.

4. **Measure before you grind.** Before the coverage harness, "how much
   of the front-end is done in Dylan?" was a vibe. After: 77% of AST
   nodes, with a ranked punch-list. The harness is simultaneously the
   *test* (shows divergence from the Rust reference) and the *plan*
   (ranks what to build next by frequency). Build the dashboard before
   the grind; let it tell you the order.

5. **The migration has a repeatable five-step shape.** Write the phase
   in Dylan → AOT-compile `--library` → static-link into the driver →
   bridge across a committed wire format → gate behind a `--…-with-dylan`
   flag, verify, default. Documented in `ARCHITECTURE.md`. Each future
   front-end phase (macros, sema, lowering, eventually the project
   manager) follows it.

## Where it leaves us

**Live in the driver today:** Dylan lexer (`--lex-with-dylan`,
byte-identical), Dylan parser verify (`--verify-parse`, 7/7 agree),
Dylan parser AST emit (`dump-dylan-ast`, real trees).

**The vision, sharpened (user's framing):** eventually the *entire*
front-end is Dylan — including a Dylan project manager that drives the
front-end phases to compile a project. The Rust `nod-driver` stays as
the **host/platform layer** (reads `.prj`, caches `.obj`s, drives the
linker and JIT engine) — and that's a *feature*, not debt; it's the OS
the Dylan front-end runs on. The project manager is the capstone
front-end piece because it orchestrates all the others.

**Immediate next moves** (the harness's punch-list, ranked):

| Count | Construct | Note |
|------:|-----------|------|
| 923 | `<unspanned 0..0>` | **span backfill** — ONE fix (compute spans from child extents), not a missing kind. Highest leverage: declassifies the bucket and reveals the `if`/`let`/etc. hiding in it. |
| 192 | `if` | `<ast-statement>` |
| 104 | `define-class` | gateway to Dylan-side sema (class graph) |
| 86 | `until` / `while` | loops |
| ~12 | `define-method`/`generic`, `cond`, `unless` | long tail |

Each is one Dylan `emit-node` method + one Rust `Kind` variant + one
`DYLAN_AST_WIRE.md` row; rerun the harness to watch coverage climb.
After kind coverage: a `DylanAst → nod_reader::ast::Module` translator
so `--parse-with-dylan` *replaces* `parse_module` outright.

**Open tickets filed this session:** Sprint 51c-2 (host/shim
registration-conflict audit — the workaround was hardcoding the
body-macro name list in `dump-ast`); Sprint 51e (the kind-coverage
grind above).

**Meta:** this was, by a distance, the most productive session of the
project — year-3 milestones (lexer + parser self-hosting) landed in a
day because the Sprint 39–44 AOT/static-link machinery was already
there to build on. The bottleneck now is genuinely just "write more
Dylan emit-methods," which is exactly where we want the bottleneck to be.
