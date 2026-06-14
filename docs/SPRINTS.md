# NewOpenDylan — Sprint Plan

*Drafted 2026-05-15. Companion to [`PLAN.md`](PLAN.md) (the 12-phase
roadmap), [`MANIFESTO.md`](MANIFESTO.md) (the design commitments), and
[`ARCHITECTURE.md`](ARCHITECTURE.md) (the Dylan-front-end / Rust+LLVM-
back-end split, ratified at Sprint 51).*

> **Architecture pivot (Sprint 51, 2026-05-31):** the sprints below
> 45 were planned as a pure-Rust compiler that ships a Dylan stdlib.
> Sprints 45–51 proved the **front-end can be Dylan**: the lexer and
> parser, written in Dylan, now run inside `nod-driver` itself
> (`--lex-with-dylan`, `--verify-parse`, `dump-dylan-ast`). The
> project's permanent shape is now a **Dylan front-end on a Rust+LLVM
> back-end, split at DFM IR** — see [`ARCHITECTURE.md`](ARCHITECTURE.md).
> Sprints 52+ migrate the remaining front-end phases to Dylan; the
> back-end (codegen, GC, JIT, linker) stays Rust forever. **Status
> (2026-06-07):** macros (52, opt-in `NOD_EXPAND_WITH_DYLAN`) and sema
> (53 recording walk; 54 **load-bearing** via `--sema-with-dylan`) are
> done; AST→DFM lowering (55) is **load-bearing (opt-in)** via
> `--lower-with-dylan` with 55a/early-55b complete and 55b
> dispatch/55c closures remaining; 56 (consolidation, flip-to-default)
> is planned. The live, detailed status for the 53–56 self-hosting line
> lives in [`journal/README.md`](journal/README.md) and the
> `journal/2026-06-07-sprint-55-*` entries — **not** in the sprint
> bodies below, which predate it. [`ARCHITECTURE.md`](ARCHITECTURE.md)
> has the phase-by-phase status table.

## Preamble

The sprint cadence is **two weeks, one developer, one demo**. Each
sprint must end with something the user can run, not a milestone in
a tracker. The trajectory:

- The first sprint produces a `cargo run -p nod-driver -- --version`
  and a workspace skeleton — cheap, demonstrable, unblocks everything.
- Sprints 1–16 cover PLAN.md phases 0–6: workspace → reader → namespace
  → kernel JIT → GC bring-up → classes → sealed multimethod dispatch.
  Sprint 16 ends with a real Dylan example — a slice of
  `simple-richards` — running through sealed dispatch.
- Sprints 17–20 hand off into macros (phase 7) so that the rest of the
  Dylan stdlib can be ported as library code, not compiler code.
- Sprints 21+ (conditions, collections, FFI, stdlib, IDE polish, AOT)
  are sketched only — their detail depends on what falls out of the
  early sprints.

**Compiler first.** This is a manifesto commitment (core
decision 8): no IDE until the compiler can JIT and run non-trivial
Dylan code. Sprints 01–16 are headless — `nod-driver` subcommands
(`dump-tokens`, `dump-ast`, `dump-graph`, `dump-dfm`, `dump-llvm`,
`eval`) and `cargo test` are how we know we are alive. The first
IDE we ship is the first non-trivial Dylan program NewOpenDylan
compiles, calling Win32 directly through `c-ffi` (Sprint 23-ish:
after macros, FFI, and the Windows-FFI runtime stack borrowed
from NewCormanLisp). Open Dylan's `sources/environment/` tree is
the re-implementation target.

**Sibling-project leverage is the budget mechanism.** Where NewM2,
NewCP, NewCormanLisp (NCL), NewBCPL, or NewFB already solved a
problem, we lift the code with attribution rather than rewriting.
Each sprint flags what is lifted vs. what is fresh.

**Tests gate sprints.** The OpenDylan test corpus at
`E:\NewOpenDylan\opendylan-tests\` is the regression battery; sprints
declare which files they intend to make pass.

---

## Sprints 21 → 43c at a glance

This document was drafted up front and carries the detailed retros
for Sprints 01–38c. The sprint cadence has continued well past the
original 32-sprint plan — the front-end, runtime, and dispatch core
matured fast enough that the work shifted into FFI, AOT, and a real
Dylan-side IDE earlier than projected. Use the git log
(`git log --oneline --grep='^Sprint'`) for the precise sequence; the
per-sprint detail below is partially backfilled and partially still
to-write. Headline arc:

- **21 – 26**: closures (cell promotion), first-class function values,
  `<table>` + content-based hashing, NewGC page-heap backend swap-in,
  closure-based stdlib (`reduce` / `map` / `do` in Dylan), retire
  hardcoded `Expr::Unless` / `Expr::Case` in favour of stdlib macros,
  polish bundle (n-ary funcall, generic-dispatch trampoline).
- **27 – 36**: the FFI arc. Vendored Win32 metadata DB →
  `define c-function` syntax → API stub table + Win64 trampolines →
  Win32 constants generated into stdlib → `<c-string>` /
  `<c-wide-string>` marshaling → JIT-time bare-name materialization
  → callback trampoline pool (closure → C fn-ptr) → `<c-struct>`
  field accessors → COM via the `windows` crate (DXGI/D3D11/D2D/
  DirectWrite) → an IDE shell that pops a real Win32 window.
- **37 – 38g**: JIT object-code cache + cross-process bitcode replay.
  Five categories of baked runtime addresses (immediates, static-area
  pointers, stub entries, cache slots, generic-function pointers)
  converted to externally-resolved globals so cached bitcode survives
  a fresh process. Includes the on-disk replay wired into the
  eval pipeline (38f) and headline subprocess cache-speedup test
  (38g).
- **39a – c**: AOT mode. `cargo run --bin nod-driver -- build foo.dylan
  -o foo.exe` produces a standalone Win64 EXE. Dual-output
  `nod-runtime` (rlib + staticlib), codegen `main`-stub injection,
  object-file emission via `inkwell::TargetMachine`, stdlib pre-
  compilation merged into user modules at build time.
- **40a – d**: bring everything-Dylan-can-do into AOT. User-defined
  classes registered at EXE start-up, Win32 callbacks (WNDPROC /
  EnumWindows), the COM device chain, bare-name Win32 calls.
- **41a – g**: build the IDE shell itself in Dylan. Message pump,
  WM_SIZE handling, source viewer, vertical + horizontal scrolling,
  menu bar (File + Help), File → Open / Save / Save As / Recent.
  The `nod-ide.exe` binary at `F:\scratch\nod-ide.exe` is the
  headline of this arc.
- **42**: real string ops. Sprint 42-pre fixed a latent SSA
  dominance bug in `lower_if` (env-merge across arm bodies) that
  the rope sprint would have hit otherwise. Sprint 42a wired five
  byte-string primitives in Rust and 12 stdlib methods in Dylan
  (`size`, `element`, `concatenate`, `copy-sequence`, `subsequence`,
  `starts-with?`, `ends-with?`, `find-substring`, `as-uppercase`,
  `as-lowercase`), plus universal `=` dispatch for non-numeric
  operands. Phase E retired five IDE Rust shims to pure Dylan
  using the new methods — net −820 lines of glue across five layers
  of wiring.
- **43a – c**: the first non-trivial user data structure in Dylan.
  An immutable `<rope>` (read core, edits via split + concat, line
  indexing with cached newline counts per node), ~650 lines of
  Dylan, 24 self-tests pass under AOT including a 200-op random-
  edit walk that stresses NewGC with thousands of small
  allocations.
- **45a**: types + dump infrastructure landed (Dylan lexer in Dylan).
  `tests/nod-tests/fixtures/dylan-lexer.dylan` defines the full
  `<token>` class hierarchy (16 concrete subclasses), `<span>`,
  `colour-of` / `token-kind-name` / `print-token` generics per
  class, and a `dump-tokens` formatter producing the canonical
  text-diff oracle format. Stub `lex` returns `[<eof-token>]` only;
  real lexing lands in 45b. New `nod-driver dump-dylan-tokens
  <path>` subcommand AOT-compiles the embedded lexer source and
  prints the dump.
- **45c – e (planned)**: Windows-first GC hardening. 45c defines a
  PC-keyed safepoint-map contract for JIT/AOT code using the existing
  `nod-runtime/src/stack_map.rs` runtime shape, plus a debug dump of
  emitted safepoint records. 45d teaches the Windows runtime/GC to
  consult those maps when scanning JIT frames, reusing the existing
  SEH registration already wired in `nod-llvm/src/jit_mm.rs`. 45e
  removes the spill/register_root safepoint shim from ordinary JIT
  callsites and adds loop/back-edge safepoint polls.
- **45f – g (planned)**: dispatch-cache performance follow-through.
  45f grows the current monomorphic `CacheSlot` into a bounded PIC
  state machine (cold → mono → poly cap 3 → megamorphic shared),
  keeping `nod_dispatch` as the semantic oracle. 45g is stabilization:
  counters, regression expansion, IDE-shell/callback stress coverage,
  and doc rewrites across `DEFERRED.md`, `GC.md`, and this file.

Sprint 38c is the last entry with a long-form retro below. Sprints
38d through 43c are best understood from the commit log and the
running code; the long-form backfill is a TODO for the next
documentation pass.

---

## Sprint 01 — Workspace Skeleton
**Goal:** Compile an empty `nod-driver` binary and print a version banner.
**Length:** 2 weeks
**Phase (from PLAN.md):** 0 — Workspace skeleton

### Deliverables
- [ ] Root `Cargo.toml` with `resolver = "3"`, `edition = "2024"`, workspace lints (`unsafe_op_in_unsafe_fn = "deny"`), shared `[workspace.dependencies]`.
- [ ] Empty crates: `nod-driver`, `nod-reader`, `nod-macro`, `nod-namespace`, `nod-sema`, `nod-dfm`, `nod-opt`, `nod-llvm`, `nod-loader`, `nod-runtime`, `nod-dylan` (source-only crate, no Rust), plus `tests/nod-tests` and `tests/nod-od-suite`.
- [ ] `nod-driver` CLI: `--version`, `--help`, no-op `compile` / `repl` subcommands stubbed.
- [ ] LICENSE files (`MIT OR Apache-2.0`).
- [ ] `docs/` skeleton: `GC.md`, `DFM.md`, `SEALING.md`, `MACROS.md` with one-paragraph stubs.
- [ ] GitHub Actions CI: build + clippy + fmt on Windows x86_64.
- [ ] `README.md` linking PLAN, MANIFESTO, SPRINTS.

### Acceptance criteria
- `cargo build --workspace` clean on `x86_64-pc-windows-msvc`.
- `cargo clippy --workspace -- -D warnings` clean.
- `cargo run -p nod-driver -- --version` prints `nod-driver 0.0.1 (LLVM <version>)`.
- CI green.

### Dependencies
- LLVM version pinned (match NewM2 / NCL major).
- Rust MSRV pinned in workspace `Cargo.toml`.

### Risks
- Bikeshedding crate names. Lock the table from PLAN.md §2.1 verbatim.
- `inkwell` not yet on the latest LLVM major; if so, pin the LLVM
  version to whatever NewM2 currently uses to stay coordinated.

### Sibling-project leverage
- Workspace `Cargo.toml` structure and lints lifted verbatim from
  NewCormanLisp.
- CI workflow lifted from NewM2's `.github/workflows/`.

### Demo
`cargo run -p nod-driver -- --version` from a fresh checkout.

---

## Sprint 02 — Lexer
**Goal:** Tokenise Dylan source into a typed token stream, exposed through `nod-driver dump-tokens`.
**Length:** 2 weeks
**Phase (from PLAN.md):** 1 — Reader + AST

### Deliverables
- [ ] `nod-reader::lex` — state-machine lexer producing `Token { kind, span, text }`. Token kinds: identifier, `<class-name>`, `#"symbol"`, `#:keyword`, `keyword:`, integer literal (decimal, hex `#x`, binary `#b`, octal `#o`), float literal, string literal with escapes, character literal `'a'`, all operator/punctuator forms documented in `E:\opendylan\sources\dfmc\reader\lexer.dylan`.
- [ ] `Span { file_id: u32, lo: u32, hi: u32 }` with an interner for file ids.
- [ ] `nod-reader::format_tokens(src) -> String` debug dump.
- [ ] `nod-driver dump-tokens <file>` subcommand.
- [ ] `tests/nod-tests/reader/`: unit tests using fixtures lifted from `opendylan-tests/sources/dfmc/reader/tests/` (start with the literal-form tests — they're hermetic).

### Acceptance criteria
- Lexer round-trips all `opendylan-tests/sources/dfmc/reader/tests/` fixture inputs (token kinds and text agree with hand-checked expectations for at least 50 fixtures).
- `nod-driver dump-tokens opendylan-tests/sources/testing/cmu-test-suite/dylan-test.dylan` produces a non-empty, schema-stable dump.

### Dependencies
- Sprint 01.

### Risks
- Dylan's lexer has edge cases (numeric prefixes, operator-as-identifier
  with `\+`, hash-keyword distinction). Time-box and defer obscure
  forms with `TODO` tokens.

### Sibling-project leverage
- Span/interner pattern from `newm2-reader`.

### Demo
`nod-driver dump-tokens opendylan-tests/sources/testing/cmu-test-suite/dylan-test.dylan` — schema-stable token stream.

---

## Sprint 03 — Fragments + Infix Parser Core
**Goal:** Parse the Dylan expression grammar into AST nodes, anchored on fragments that carry source locations.
**Length:** 2 weeks
**Phase (from PLAN.md):** 1 — Reader + AST

### Deliverables
- [ ] `nod-reader::Fragment` — token-tree with parentheses/braces/brackets grouped; matches the role of `dfmc/reader/fragments.dylan`.
- [ ] Pratt-style infix parser for: literals, identifiers, function calls, binary/unary operators with Dylan precedence (`+ - * / mod rem`, comparison, `& |`, `:=`), `if`/`unless`/`case`/`select`, `begin … end`, `let`, `local method`, anonymous `method (…) … end`, parenthesised groups.
- [ ] AST as an `enum Expr` with `Span` on every node.
- [ ] `nod-reader::format_ast(expr) -> String` indented dump.
- [ ] `nod-driver dump-ast <file>` subcommand.

### Acceptance criteria
- Round-trips at least 80% of expressions in `opendylan-tests/sources/testing/cmu-test-suite/dylan-test.dylan` to a stable AST dump.
- Operator precedence verified against `opendylan-tests/sources/dfmc/reader/tests/expressions.dylan`.

### Dependencies
- Sprint 02.

### Risks
- Dylan's grammar has context-sensitive bits (statement vs. expression
  position for macros). Defer macro positions to Sprint 17 — for now,
  parse `define …` heads only.

### Sibling-project leverage
- Pratt-parser skeleton from `newcp-reader` (Component Pascal has a
  similar expression-grammar shape).

### Demo
`nod-driver dump-ast` on a hand-written Dylan expression file produces a tree dump matching expectations.

---

## Sprint 04 — Definition Forms + Body Parser
**Goal:** Parse all top-level `define` forms and the body grammar (statements, locals, returns).
**Length:** 2 weeks
**Phase (from PLAN.md):** 1 — Reader + AST

### Deliverables
- [ ] Parser for: `define constant`, `define variable`, `define function`, `define method`, `define generic`, `define class`, `define library`, `define module`. Slot definitions, init keywords, specialisers, return types.
- [ ] `define macro` is parsed *as a fragment* (not expanded) — the body is captured raw for Sprint 17.
- [ ] Statement-level parsing: `for`, `while`, `until`, `block`, `local`, `let`, sequence-of-statements, multiple-value bindings `let (a, b) = …`.
- [ ] `Module:` header comment parsed and attached to the AST root.
- [ ] `nod-driver dump-ast` updated to cover top-level forms.
- [ ] AST round-trip pretty-printer (`format_dylan(ast) -> String`) that produces parseable Dylan; verified on a fixture.

### Acceptance criteria
- Parses every `.dylan` file in `opendylan-tests/sources/testing/cmu-test-suite/` without error (results are AST dumps — semantics not checked yet).
- AST → pretty-print → re-parse is a fixed point for at least 20 hand-picked files.

### Dependencies
- Sprint 03.

### Risks
- `define macro` body syntax (`=> { … }` template) is non-trivial; in this sprint we only need to *skip* past it correctly, not understand it.

### Sibling-project leverage
- Pretty-printer pattern from `ncl-reader`.

### Demo
`nod-driver dump-ast opendylan-tests/sources/dylan/tests/constants.dylan` prints a complete AST.

---

## Sprint 05 — LID Files + Library / Module Graph
**Goal:** Parse `.lid` and `dylan-package.json` manifests; build the library/module DAG; resolve `use` / `import` / `export` / `rename`.
**Length:** 2 weeks
**Phase (from PLAN.md):** 2 — Module graph

### Deliverables
- [ ] `nod-namespace::Lid` parser for the `Library:`, `Files:`, `Library-Pack:`, `Platforms:`, `Synopsis:` keys; `.hdp` parsed for legacy interop.
- [ ] `nod-namespace::Package` parser for `dylan-package.json`.
- [ ] Library / module DAG construction from parsed `define library` / `define module` forms.
- [ ] `use` resolution with `import:` / `exclude:` / `rename:` / `prefix:` / `export:`.
- [ ] Cycle detection with a structured diagnostic.
- [ ] `Binding` resolution: every `Module: foo` source file resolves identifiers against module `foo`'s import set.
- [ ] `nod-driver dump-graph <lid>` subcommand — emits a Graphviz-shaped dump.

### Acceptance criteria
- Loads `opendylan-tests/sources/dylan/dylan.lid` (91 files) and produces a complete module graph without error.
- Loads the kernel library + `common-dylan` test library and resolves cross-library references.
- `dump-graph` output validates with `graphviz` (`dot -Tpng`).

### Dependencies
- Sprint 04.

### Risks
- Platform conditionalisation in LID files (`Platforms: x86-win32`) — handle in a follow-up if not in v0.1.
- LID is line-oriented but tolerant of weird whitespace; budget some bug-hunting.

### Sibling-project leverage
- DAG + cycle-detection from `newcp-loader` / `ncl-loader` (the graph
  shape is identical).

### Demo
`nod-driver dump-graph dylan.lid | dot -Tpng > dylan.png` renders the kernel library's 91 source files grouped by module with `use` arrows.

---

## Sprint 06 — DFM IR Skeleton + Format Dump
**Goal:** Define the SSA IR shape (the "DFM-equivalent") and lower a trivial AST (arithmetic + `let` + direct calls) to it.
**Length:** 2 weeks
**Phase (from PLAN.md):** 3 — Minimal kernel

### Deliverables
- [ ] `nod-dfm`: `Computation` enum with `Call`, `DirectCall`, `PrimOp`, `Const`, `If`, `Return`, `Values`, `BindExit`, `UnbindExit`, `Closure`, `MakeEnvironment`. (Generic dispatch nodes come in Sprint 13.)
- [ ] `Temporary { id, type_estimate }` with a `TypeEstimate` lattice (just `<top>`, `<bottom>`, `<integer>`, `<single-float>`, `<double-float>`, `<character>`, `<boolean>`, `<string>` for now).
- [ ] `Block`-structured IR with explicit entry/exit; SSA invariants checked by a verifier.
- [ ] `nod-sema` (first appearance): AST → DFM for the kernel subset — integer/float literals, arithmetic, comparison, `if`, `let`, top-level `define constant`, `define function` (non-generic), direct function calls within one library.
- [ ] `nod-dfm::format_dfm` indented dump with type-estimate annotations.
- [ ] `nod-driver dump-dfm <file>` subcommand.

### Acceptance criteria
- Lowers a hand-written `kernel-arith.dylan` (a few `define function` doing integer/float arithmetic) to DFM whose dump is reviewable.
- Verifier passes on every emitted function.

### Dependencies
- Sprint 05 (we need binding resolution to know what calls bind to).

### Risks
- Premature commitment to IR opcodes. Keep the enum private to `nod-dfm` and accept that Sprints 10–13 will add fields.

### Sibling-project leverage
- IR shape from `newm2-ir` and `ncl-ir`. The SSA-block + Computation +
  Temporary structure is portfolio-wide.

### Demo
`nod-driver dump-dfm kernel-arith.dylan` shows annotated SSA.

---

## Sprint 07 — LLVM Codegen Thin Slice
**Goal:** Emit LLVM IR from DFM for the kernel subset; JIT-compile and execute a function that returns an integer.
**Length:** 2 weeks
**Phase (from PLAN.md):** 3 — Minimal kernel

### Deliverables
- [ ] `nod-llvm` crate: `inkwell`-based context, module, builder; pinned LLVM major matching NewM2.
- [ ] DFM → LLVM IR lowering for: i64 arithmetic, f64 arithmetic, comparisons, branches, direct calls, returns.
- [ ] JIT execution engine (lifted from `newm2-llvm/src/jit_mm.rs`).
- [ ] `nod-driver eval <expr>` REPL prototype: parse → DFM → LLVM IR → JIT → run → print result. Single-shot, no live image yet.
- [ ] `nod-driver dump-llvm <file>` subcommand prints textual LLVM IR.

### Acceptance criteria
- `nod-driver eval '1 + 2 * 3'` prints `7`.
- `nod-driver eval 'let x = 41; x + 1 end'` prints `42`.
- A hand-written `factorial.dylan` defining `factorial(10)` returns `3628800` when called from `nod-driver eval`.
- LLVM IR dump available for every example.

### Dependencies
- Sprint 06.
- JIT-MM port from NewM2 (one-time cost).

### Risks
- LLVM version drift across the portfolio — coordinate the pin.
- `inkwell` API churn on the targeted LLVM major.

### Sibling-project leverage
- `jit_mm.rs` (Win64 SEH `RtlAddFunctionTable` registration) lifted from NewM2.
- `inkwell` wrapper patterns from NCL.

### Demo
Type expressions at `nod-driver eval` and see them evaluate. `nod-driver dump-llvm` prints the emitted textual IR.

---

## Sprint 08 — REPL Loop + Live Image (no GC yet)
**Goal:** A persistent REPL that keeps defined functions/constants in an arena and lets later expressions call them.
**Length:** 2 weeks
**Phase (from PLAN.md):** 3 — Minimal kernel

### Deliverables
- [ ] `nod-driver repl` mode: read line → parse → lower → JIT → install in module → call. Module is persistent within the REPL session.
- [ ] `nod-loader` (first appearance): per-definition installation, generation counter, dirty-tracking placeholder (full retirement comes later). Lift the shape from `ncl-loader`.
- [ ] FFI stub `format-out(fmt, args …)` lowering to a call into a Rust `extern "C"` shim that prints to stdout. The `c-ffi` proper comes later; this is just one well-known intrinsic.
- [ ] `define constant` and `define function` work across REPL lines.
- [ ] `:dump-dfm <name>` and `:dump-llvm <name>` REPL meta-commands reprint the IR for a definition installed earlier in the session.
- [ ] Arena allocator for any heap data (still no GC); intentionally leaks.

### Acceptance criteria
- Multi-turn REPL: `define function sq (x :: <integer>) x * x end` then `sq(7)` → `49`.
- `format-out("%d\n", sq(7))` prints `49` to stdout.
- `:dump-llvm sq` after the definition prints the JITed IR.
- A "hello, world" via `format-out("hello, world\n")` works end-to-end through the JIT.

### Dependencies
- Sprint 07.

### Risks
- Linking JIT-compiled functions across separate LLVM modules is fiddly
  (symbol resolution). Use a single growing module for now; split
  later.

### Sibling-project leverage
- Loader shape from `ncl-loader`.
- REPL line-reader from NCL's `ncl-driver`.

### Demo
In the REPL: type a function definition, call it on the next line, run `:dump-llvm <name>` to see the JITed IR.

---

## Sprint 09 — GC Phase 1: Tagged Pointers + Allocator + Boxed `<integer>`
**Goal:** Replace the arena with a real heap. Tagged pointers, `<wrapper>` headers, bump allocator, no collection yet.
**Length:** 2 weeks
**Phase (from PLAN.md):** 4 — GC + heap objects

### Deliverables
- [ ] `nod-runtime::heap`: a Rust-side heap with a `PageAllocator` shim (Windows `VirtualAlloc`). Bump-pointer allocation. No collection.
- [ ] Tagged pointer representation (lowest bit: 0 = fixnum, 1 = pointer-to-header). 63-bit fixnum range.
- [ ] `<wrapper>` header (8 bytes): pointer to class metadata, GC info bits.
- [ ] `Value` ABI for the JIT: every Dylan value is one `i64` register-sized word.
- [ ] `nod-llvm` codegen updated to emit tag-check / tag-strip primitives for integer arithmetic. Inline the fast path.
- [ ] Class metadata table (statically interned): `<integer>`, `<single-float>`, `<double-float>`, `<boolean>`, `<character>`, `<symbol>`, `<string>` (still a placeholder layout).
- [ ] `instance?(x, <integer>)` primitive working.
- [ ] `nod-driver dump-heap` REPL meta-command (`:dump-heap`) prints a flat list of allocated objects.

### Acceptance criteria
- `1 + 2` is a fixnum-on-fixnum add with no allocation (verified by `:dump-heap` showing zero new allocations across the call).
- Allocating an out-of-tag-range integer (Sprint 12 territory) is flagged with a clear "not yet supported" diagnostic — not silently wrong.
- `instance?(42, <integer>)` returns true; `instance?(42, <boolean>)` returns false.

### Dependencies
- Sprint 08.

### Risks
- Tag-bit choice (0 = fixnum vs. 1 = fixnum) needs to match what
  arithmetic codegen wants; pick early and document in `docs/GC.md`.

### Sibling-project leverage
- Tagged-pointer scheme and `<wrapper>` header pattern from NCL.
  Adopt Dylan's choice of tag layout (matches `dfmc/runtime`).

### Demo
`:dump-heap` shows zero objects when running pure fixnum code, and starts listing allocations as soon as a `<string>` literal is touched.

---

## Sprint 10 — GC Phase 2: Strings, Symbols, Vectors, Static Roots
**Goal:** Allocate real heap objects (`<byte-string>`, `<symbol>`, `<simple-object-vector>`) and trace them from a static root set. Still no collection — just precise root identification.
**Length:** 2 weeks
**Phase (from PLAN.md):** 4 — GC + heap objects

### Deliverables
- [ ] `<byte-string>`, `<symbol>`, `<simple-object-vector>` layouts in `nod-runtime`. UTF-8 encoded byte-strings.
- [ ] Constructors emit `make-string`, `make-vector` primitive calls.
- [ ] Symbol intern table (lifted from NCL).
- [ ] Static root set: REPL module's top-level bindings.
- [ ] Tracer that walks the static roots and prints the heap graph (no collection yet).
- [ ] `nod-driver dump-heap` subcommand.
- [ ] `:inspect <root-name>` REPL meta-command walks heap references from a named root and prints class + slots; one screen at a time, navigable by typing follow-up reference indices.

### Acceptance criteria
- `format-out("%s\n", "hello")` allocates a `<byte-string>` and prints `hello`.
- `dump-heap` shows the live string and its `<wrapper>`.
- Tracer reports the static root reachability of every allocated object correctly (verified against hand-counted fixtures).

### Dependencies
- Sprint 09.

### Risks
- Symbol interning under JIT — symbols must be value-equal across
  separately-JITed call sites. Use a global table behind a mutex.

### Sibling-project leverage
- Symbol-intern table directly from NCL.
- String layout from NCL (Dylan and CL agree on UTF-8 byte-string).

### Demo
Allocate strings and vectors at the REPL, walk them with `:inspect`.

---

## Sprint 11 — GC Phase 3: Generational Copying Collector + Safe Points
**Goal:** A working stop-the-world generational copying GC, driven by `gc.statepoint`-emitted stack maps and a cooperative safepoint poll.
**Length:** 2 weeks
**Phase (from PLAN.md):** 4 — GC + heap objects

### Deliverables
- [ ] LLVM `gc.statepoint` / `gc.relocate` emission in `nod-llvm` codegen at every call site. `gc.statepoint`-example strategy or the custom strategy already used by NewM2/NCL.
- [ ] Stack-map decoder in `nod-runtime` (lifted from NCL).
- [ ] Safepoint-poll lowering pass: at function entry, loop back-edges, and call returns, emit a load-and-branch against a thread-local "should park" flag.
- [ ] Young-generation copying collector. Old generation as a separately-tracked region (promotion happens on the second survival).
- [ ] Card-marking write barrier in `nod-llvm` (one byte per 512-byte card).
- [ ] GC stress test: a fibonacci-style allocator that triggers thousands of minor GCs.
- [ ] `:gc-stats` REPL meta-command and a `nod-driver --gc-trace` flag that dumps live/used/free per generation, GC count, last-pause time after each collection.

### Acceptance criteria
- A loop that allocates 1M `<byte-string>` objects completes without OOM.
- GC count > 100 over that run, no leaks reported by tracing test.
- `:gc-stats` reflects pulses of allocation and collection across the run.
- `dump-heap` correctness preserved across collections (fixture-based).

### Dependencies
- Sprint 10.

### Risks
- This is the highest-risk single sprint. The `gc.statepoint` lowering
  is the de-risked part (NCL has done it); what's new is wiring it to
  a Dylan-shaped object layout. Budget a buffer week, or split into
  Sprints 11a/11b if needed.
- Win64 SEH interaction with safepoint polls.

### Sibling-project leverage
- **Heavy lift from NCL.** The `gc.statepoint` lowering pass,
  cooperative-park protocol, TLAB design, card-marking barrier, and
  stack-map decoder are all lifted with attribution.

### Demo
The fibonacci-allocator runs through `nod-driver --gc-trace`; the trace stream shows pulses of allocation and collection in real time.

---

## Sprint 12 — Classes + Slots, Single Dispatch Placeholder
**Goal:** `define class` produces real classes with slot layout, getters, setters, `make`, `initialize`. Generic functions exist but dispatch is single-receiver.
**Length:** 2 weeks
**Phase (from PLAN.md):** 5 — Classes, slots, single dispatch

### Deliverables
- [ ] `define class <foo> (<bar>) slot a :: <integer>, init-keyword: a:; … end;` parsed and lowered into class metadata.
- [ ] C3 linearisation algorithm in `nod-sema` (port of `dispatch.dylan`'s C3 implementation).
- [ ] Fixed-offset slot layout for single inheritance.
- [ ] Auto-generated getter and setter generics: `a(p)`, `a(p) := v`.
- [ ] `make(<foo>, a: 1, b: 2)` working through a `make` generic.
- [ ] `initialize(obj, #key)` user-overridable.
- [ ] Single-dispatch generic functions: look up methods by receiver class; ignore other specialisers for now.
- [ ] `instance?(x, <foo>)` exact + subclass.
- [ ] `nod-driver dump-classes` subcommand and `:classes` REPL meta-command — list classes, dump slots + getter/setter for one.

### Acceptance criteria
- A hand-written `point.dylan` defining `<point>` with `x`, `y`, plus a `distance` method, computes `distance(make(<point>, x: 3, y: 4))` → `5.0`.
- C3 linearisation matches Python's `mro()` on the same class graph for 10 fixtures (sanity check — same algorithm).
- `:classes` lists the kernel classes installed so far; `:classes <point>` dumps its slot table.

### Dependencies
- Sprint 11.

### Risks
- Slot inheritance under MI is non-trivial; defer the *MI* case to
  Sprint 14 — this sprint only needs single inheritance to work.

### Sibling-project leverage
- C3 algorithm is small and standard; port from any reputable
  reference (Python or `dispatch.dylan`).

### Demo
Define a `<point>` class at the REPL, instantiate it, call `distance`. `:classes <point>` dumps the slot table.

---

## Sprint 13 — DFM Dispatch Node + Method-Lookup Runtime
**Goal:** Introduce the `<dispatch>` IR node, a runtime method-lookup function, and inline caches at call sites for unsealed generics.
**Length:** 2 weeks
**Phase (from PLAN.md):** 5–6 — single → multimethod

### Deliverables
- [ ] `nod-dfm`: `Computation::Dispatch { generic, args }` and `Computation::DirectCall { method, args }`. Lowering chooses `Dispatch` for generic calls, `DirectCall` for `define function`.
- [ ] Multimethod method-lookup algorithm in `nod-runtime`: given a generic and argument tuple of classes, return the most-specific applicable method or signal `<no-applicable-methods-error>` (signalling fully proper is a Sprint 19 deliverable; for now, panic with a diagnostic).
- [ ] Per-call-site monomorphic inline cache: one-entry cache keyed on the receiver's `<wrapper>`; cache miss falls through to the lookup.
- [ ] `nod-llvm` emits the inline-cache check inline at each call site.
- [ ] `add-method` / `remove-method` operations on a generic; method table is a sorted `Vec<Method>` with a generation counter.
- [ ] Cache invalidation: bump the generation counter on `add-method` / `remove-method`; inline caches compare generation on each call.
- [ ] `:dispatch-stats <generic>` REPL meta-command + `nod-driver dump-dispatch` listing each call site for a generic with its current cache state (cold / monomorphic / polymorphic) and the generation it was last validated against.

### Acceptance criteria
- A two-method generic (`area(<circle>)`, `area(<square>)`) dispatches correctly on both receiver classes; the inline cache reports monomorphic after several calls on the same class.
- Adding a third method invalidates the cache; next call goes through the lookup; cache repopulates.
- `:dispatch-stats` reflects each transition.

### Dependencies
- Sprint 12.

### Risks
- Inline-cache thread-safety. Use atomic generation + relaxed loads
  for the cache fields; document the memory model in `docs/SEALING.md`.

### Sibling-project leverage
- Inline-cache scheme is conceptually portable from NCL's generic
  function caches, but the data shape is Dylan-specific.

### Demo
Define two methods, call the generic from the REPL on each receiver type, run `:dispatch-stats area` and see the cache transition cold → monomorphic.

---

## Sprint 14 — Multiple Inheritance + Slot Layout
**Goal:** Classes with multiple superclasses, C3-driven slot layout, fixed-offset access where possible, indirect fallback otherwise.
**Length:** 2 weeks
**Phase (from PLAN.md):** 5 — Classes, slots, single dispatch

### Deliverables
- [ ] MI in `define class … (<a>, <b>) …`; C3 linearisation produces the class precedence list (CPL).
- [ ] Slot layout algorithm: walk the CPL, assign fixed offsets when all paths agree, fall back to a per-class indirection table when they don't (matches Dylan's `slots-have-fixed-offsets?-bit`).
- [ ] Slot accessor codegen consults the layout decision and emits either a direct load or a hash-lookup.
- [ ] `next-method` machinery — methods can call the next-most-specific method.
- [ ] `:classes <name>` slot listing distinguishes fixed-offset slots ("`@N`") from indirect ones ("`[indirect]`").

### Acceptance criteria
- A diamond hierarchy `<top>` → `<a>`, `<b>` → `<d>` with slots in `<a>` and `<b>` works; `<d>` instances can read/write both.
- A more pathological hierarchy that forces indirect layout is exercised by a fixture, and the indirect access works.
- `next-method` chain in a 4-deep inheritance walk produces the right sequence.

### Dependencies
- Sprint 13.

### Risks
- The fixed-vs-indirect-layout decision is the trickiest piece of
  Dylan's class system. Cross-reference `E:\opendylan\sources\dylan\
  class.dylan` lines 19-39 closely and put the algorithm in
  `docs/CLASSES.md`.

### Sibling-project leverage
- None — this is Dylan-specific. NCL has single dispatch only.

### Demo
Diamond-inheritance fixture from `opendylan-tests/sources/dylan/tests/classes.dylan` (subset) runs at the REPL.

---

## Sprint 15 — Sealing Analysis + Compile-Time Dispatch Resolution
**Goal:** Honour `sealed` declarations on classes and generics; resolve dispatch at compile time when sealing permits; emit direct calls.
**Length:** 2 weeks
**Phase (from PLAN.md):** 6 — Multimethod dispatch + sealing analysis

### Deliverables
- [ ] Parse `sealed` class modifier, `sealed` generic modifier, `define sealed domain g (<a>, <b>);` declarations.
- [ ] `nod-sema` records sealing facts on class and generic objects.
- [ ] `nod-opt`: dispatch-resolution pass that consults sealing facts and the type-estimate lattice. For each `<dispatch>` node, if the static type estimates plus sealing imply a single applicable method, rewrite to `<direct-call>`. This is the analogue of `dfmc/optimization/dispatch.dylan`'s `guaranteed-joint?`.
- [ ] Type-estimate propagation strengthened: receiver-class narrowing through `instance?` guards, slot-type-implied narrowing.
- [ ] Inline caching becomes the *fallback* path; the optimised case is a direct call with no cache.
- [ ] `:dispatch-stats` and `dump-dispatch` add a column marking sealed-direct vs. cached; `nod-driver dump-sealed` lists which generics are sealed over which classes.
- [ ] Live-incremental compilation: if a redefinition would invalidate a sealing assumption, surface a structured diagnostic rather than silently miscompiling (MANIFESTO commitment).

### Acceptance criteria
- A generic with two methods over sealed-domain classes is compiled into two direct calls in the LLVM IR for two specialised call sites (verified by `dump-llvm`).
- Adding a method to a sealed generic from inside the defining library works; from outside, errors at parse/sema with a clear message.
- A redefinition that would break a sealed-domain assumption surfaces a structured diagnostic on stderr and refuses the patch.

### Dependencies
- Sprint 13, Sprint 14.

### Risks
- Sealing analysis is subtle. Limit v0.1 to: single-library sealing,
  no library-merge — that's a v2 deliverable per PLAN.md §2.5(f).

### Sibling-project leverage
- None — Dylan-specific. This is the keystone language feature.

### Demo
A two-method `area` generic over sealed `<circle>` and `<square>` compiles to direct calls; `dump-llvm` shows no dispatch overhead. Add a method from another library; see the sealing diagnostic.

---

## Sprint 16 — `simple-richards` Subset Runs End-to-End
**Goal:** A real Dylan benchmark — a curated slice of `simple-richards` — JIT-compiles and runs against sealed multimethod dispatch.
**Length:** 2 weeks
**Phase (from PLAN.md):** 6 — Multimethod dispatch + sealing analysis

### Deliverables
- [ ] Port enough of `opendylan-tests/sources/testing/benchmarks/richards/simple-richards.dylan` to run under our compiler. Where the source uses macros / collections we haven't ported, hand-rewrite the affected parts and document the deltas (the macros land in Sprint 17–19).
- [ ] Whatever runtime primitives are needed: `<list>` minimum API (cons, car, cdr, null?), basic `<integer>` arithmetic at full speed, sealed dispatch on the task-record class hierarchy.
- [ ] Performance: a dated row in `bench/richards.md`'s History table comparing sealed vs all-`<dispatch>` runs on the same source. The 5× speedup target from the original brief is **dropped** — at this project stage we measure correctness and track perf as a trajectory rather than gating on a ratio. The bench's `ratio >= 0.95` assertion is a regression guard against re-introducing dispatch overhead, not a target. Future ratio improvements land naturally as Sprint 11d (`gc.statepoint`) and Sprint 18 (LLVM optimisation passes) come in.
- [ ] `nod-driver --profile compile-and-run` writes a per-method call-count + resolved-direct/cache-hit/cache-miss summary at end of run.

### Acceptance criteria
- `nod-driver compile-and-run simple-richards-subset.dylan` (or the hand-written `richards-shape` substitute, given upstream Richards' unimplemented forms) produces the expected result count.
- `--profile` output confirms sealed-direct dispatch dominates the tallies.
- A dated measurement row is added to `bench/richards.md`. (No ratio target; the test asserts `>= 0.95` as a regression guard.)

### Dependencies
- Sprint 15.

### Risks
- Richards uses several language features (records, generics, basic
  iteration) that may surface bugs at integration time. Plan for a
  bug-hunting buffer.
- Original Richards depends on macros (`for`, `with-…`); the
  hand-rewritten subset avoids them.

### Sibling-project leverage
- The Richards benchmark itself is open-source under Open Dylan's
  licence; we use it as a fixture per MANIFESTO §13 (open inputs only).

### Demo
**The headline demo.** `nod-driver --profile compile-and-run simple-richards-subset.dylan` produces the expected output and a profile dominated by sealed-direct dispatch. A Dylan programmer reading the source would recognise this as Dylan.

---

## Sprint 17 — Macro Expander: Pattern Matching Engine
**Goal:** Match `define macro` pattern clauses against call-site fragments; substitute templates with hygiene.
**Length:** 2 weeks
**Phase (from PLAN.md):** 7 — Macros

### Deliverables
- [ ] `nod-macro`: pattern parser for `define macro foo { foo ?x:expression } => { … ?x … }` rules.
- [ ] Fragment-level matching: `?x:expression`, `?x:name`, `?x:variable`, `?x:body`, literal tokens.
- [ ] Template substitution preserving source locations.
- [ ] Hygiene: introduced identifiers freshened per expansion.
- [ ] Integration into the compiler pipeline: after parsing top-level forms, before namespace resolution, expand macros that the parser captured as fragments in Sprint 04.
- [ ] `nod-driver dump-expanded <file>` shows post-macro AST.

### Acceptance criteria
- Hand-written `define macro unless { unless ?cond ?body end } => { if (~ ?cond) ?body end };` works at the REPL.
- Source-location preservation verified: a runtime error inside an `unless` body points at the original source span, not the expansion.

### Dependencies
- Sprint 16.

### Risks
- This is the second-highest-risk sprint after GC. The fragment-tree
  matching grammar is non-trivial.

### Sibling-project leverage
- None directly — Dylan macros are unique. But the fragment data
  structure was set up in Sprint 03 precisely to make this possible.

### Demo
Define `unless` as a macro at the REPL; use it in a function.

---

## Sprint 18 — Twelve Most-Common Macro Shapes
**Goal:** Implement enough macro features for the kernel-library macros in `sources/dylan/` to expand.
**Length:** 2 weeks
**Phase (from PLAN.md):** 7 — Macros

### Deliverables
- [ ] Coverage for: `unless`, `when`, `case`, `cond`, `for`, `while`, `until`, `with-open-file`, `block`/`exception`/`cleanup` (parsed; signalling lands in Sprint 19), `let`-extensions, definition macros (`define table`, `define inline function`), function macros.
- [ ] Multiple-rule macros (multiple `{ … } => { … }` clauses with backtracking).
- [ ] Auxiliary rules (`rule` inside `define macro`).
- [ ] Cross-file macro use (definition in module A, use in module B).
- [ ] `nod-driver dump-expanded --trace <file>` prints the full expansion chain for each macro call site, source-location-anchored.

### Acceptance criteria
- `opendylan-tests/sources/dylan/tests/macros.dylan` test fixtures expand correctly (target: 80% of cases).
- `for (i from 1 to 10) format-out("%d\n", i) end` runs at the REPL.

### Dependencies
- Sprint 17.

### Risks
- Backtracking pattern matcher edge cases. Lots of fixture-driven
  debugging.

### Sibling-project leverage
- None.

### Demo
Run `for (i from 1 to 10) … end` at the REPL; `:expand <last>` (or `nod-driver dump-expanded --trace`) shows the expansion chain.

---

## Sprint 19 — Conditions, NLX, Restart Stubs
**Goal:** `block`/`exception`/`cleanup` works; `<condition>` hierarchy exists; `signal` and basic handlers work; restarts present but minimal.
**Length:** 2 weeks
**Phase (from PLAN.md):** 8 — Conditions, NLX, restarts

### Deliverables
- [ ] `<condition>`, `<warning>`, `<error>`, `<serious-condition>` class hierarchy.
- [ ] Handler stack in `nod-runtime` (heap-allocated handler frames, thread-local chain per MANIFESTO §risks(d)).
- [ ] `block (return) … return(v) … exception (<error>) … cleanup … end` codegen — non-local exits use a parallel Dylan-handler chain; OS unwinder runs only `cleanup` clauses.
- [ ] `signal(<my-error>)` walks the handler chain.
- [ ] `<simple-restart>` class and `make-restart` plumbing; `invoke-restart` works for a single bound restart (full restart semantics in v1.x).
- [ ] Replace the panic-on-no-applicable-methods from Sprint 13 with a real `<no-applicable-methods-error>` signal.
- [ ] `:handlers` REPL meta-command prints the live Dylan handler chain at the prompt (and at a breakpoint, once a debugger lands).

### Acceptance criteria
- `block () signal(make(<error>, message: "x")) exception (c :: <error>) c.condition-message end` returns `"x"`.
- `cleanup` clauses run on both normal exit and unwound exit.
- `:handlers` shows the right chain inside an unfinished `block`/`exception` form entered via a paused signal.

### Dependencies
- Sprint 18 (we need macros to write `block`/`exception` body parsing cleanly).

### Risks
- Win64 SEH interaction with the parallel handler chain. Reference
  the M2NEW NLX work; do not invent a new approach here.

### Sibling-project leverage
- NLX scheme from NewM2 (the Win64-SEH-bridged design from M2NEW).

### Demo
Throw and handle an exception at the REPL; `:handlers` prints the chain.

---

## Sprint 20 — Forward Iteration Protocol + Core Collection Types
**Goal:** The collection protocol plus `<list>`, `<simple-object-vector>`, `<stretchy-vector>`, `<range>` working through `for`.
**Length:** 2 weeks
**Phase (from PLAN.md):** 9 — Collections, iteration protocol

### Deliverables
- [ ] Forward iteration protocol (the seven-values contract).
- [ ] `<collection>`, `<sequence>`, `<explicit-key-collection>`, `<mutable-collection>` hierarchy.
- [ ] Concrete: `<list>` (proper + improper), `<simple-object-vector>`, `<stretchy-vector>`, `<range>`. Defer `<table>` (hash), `<deque>`, `<string>` collection-ness, limited collections to v1.x.
- [ ] `map`, `do`, `reduce`, `concatenate`, `size`, `element`, `element-setter`.
- [ ] `for` macro consumes the iteration protocol; sealed dispatch on `<simple-object-vector>` should inline the iterator.
- [ ] `:inspect` REPL meta-command grows a truncated-preview rendering for collections (first N elements, total size); follow-up indices walk into elements.

### Acceptance criteria
- `reduce(\+, 0, range(from: 1, to: 100))` returns `5050`.
- `map(method (x) x * x end, #(1, 2, 3))` returns `#(1, 4, 9)`.
- `opendylan-tests/sources/collections/tests/bit-vector-tests.dylan` runs (subset; full coverage in a later sprint).

### Dependencies
- Sprint 19.

### Risks
- The protocol is intentionally branchy; sealing-driven inlining
  needs to actually fire for performance, otherwise iteration is slow.

### Sibling-project leverage
- None — Dylan-specific iteration protocol.

### Demo
Run `reduce` + `map` at the REPL; profile shows the iterator inlined via sealing.

---

## Sprints 21–35 — Sketches (phases 7+ continuations and 8–12)

> Detail level intentionally lower: each is 2-3 sentences. Concrete
> deliverables are decided after Sprint 20 retrospective. Sprints
> 21–24 have **landed** and carry retrospective notes in place of the
> original sketch; downstream sprints have slid forward to make room.

### Sprint 21 — First-class function values (landed)
Shipped: `<function>` heap class + `<wrong-number-of-arguments-error>`, anonymous-method lifting pass, `nod_funcall_N` / `nod_apply` trampolines, operator-shim registry (`\+`, `\-`, …), top-level / JIT / generic function-ref resolution. `\name` and `method (…) … end` in expression position work as first-class values. Free-variable capture (closures) explicitly deferred to Sprint 24.

### Sprint 22 — `<table>` + hashing (landed)
Shipped: `<table>` heap class with open-addressing buckets, FNV-1a hash + `==` equality machinery via `%object-hash` / `%object-equal?`, `%make-table` / `%table-element` / `%table-element-setter` / `%table-keys` / `%table-values` / `%table-remove-key` primitives wired through the lowerer, stdlib generics over `<table>`. `<not-hashable-error>` lands as a Sprint 19-shaped condition. `<string>` collection conformance slides to a later sprint.

### Sprint 23 — NewGC swap-out (landed)
Replaced the bespoke semispace `Heap` with the sibling-project `PageHeap<DylanLayout>` from `E:\NewGC`. Default feature `newgc-backend`; escape hatch `semispace-backend` keeps the old heap reachable for one sprint of cohabitation. `DylanLayout` binds Dylan's class-driven scan/size machinery to NewGC's `HeapLayout` trait via per-class `LayoutFn` pointers stored on `ClassMetadata`. Card-marking write barrier and root-set discipline unchanged.

### Sprint 24 — Closures (free-variable capture) — landed
Shipped: `<cell>` and `<environment>` heap classes (`nod-runtime/src/closures.rs`), a cell-conversion pass in `nod-sema/src/lower.rs` that promotes captured locals to heap cells and wires per-closure environments through the existing `<function>` `env-ptr` slot, and an env-ptr-conditional dispatch in `nod_funcall_N` / `nod_apply` (ABI choice 1 from the brief — closure bodies grow a synthetic env first parameter; top-level functions keep their Sprint 21 ABI unchanged). The canonical Dylan idiom `let m = 10; map(method (x) x * m end, #(1, 2, 3))` returns `"#(10, 20, 30)"`. By-reference capture: `:=` inside a closure body mutates the underlying cell, and the outer scope reads through the same cell — the textbook ML/Scheme semantics. Captured parameters are cell-promoted alongside captured `let` bindings (so curried `method (a) method (b) a + b end end` works). Test count moves from 410 / 0 / 5 to 421 / 0 / 5 under the `newgc-backend` default; the `semispace-backend` escape hatch stays green. Deferred to follow-up: closure-body arity-0 calls (Sprint 21's `anonymous_method_zero_args` limitation still bites — covered by writing dummy-arg variants in the meantime), env-sharing between sibling closures created in the same scope (each currently allocates its own env even if the capture sets overlap exactly), and deep nesting beyond two levels (works in practice but no explicit acceptance test).

### Sprint 26 — Polish bundle (landed)
Three small surface-level cleanups closed before the c-ffi greenfield, each from a Sprint 21/22/24 DEFERRED bin.

**A. Arity-0 and arity-3+ closure calls.** Sprint 21 wired the env-bound funcall dispatch at arities 1 and 2; arity 0 surfaced a "not supported" lowering error and arity 3+ was implicit. Sprint 26 extends the direct-funcall family to arities 0..=5 (`nod_funcall0`, `nod_funcall3`, `nod_funcall4`, `nod_funcall5` join `nod_funcall1` / `nod_funcall2`), each dispatching on the `<function>`'s `env-ptr` slot exactly like the existing pair. The Sprint 24 brief's `closure_writes_captured_variable` test now exists in its canonical `method ()` form (no dummy arg needed); the new `funcall_arity.rs` test file pins arities 0/3/4/5 with both env-less and closure-with-capture variants. Arities 6+ continue to route through `nod_apply` (8-cap unchanged).

**B. `make(<range>, from:, to:)` keyword-init.** Sprint 21 had to use the `%make-range(1, 100, 1)` primitive workaround because the canonical Dylan spec form left the `by:` slot at zero and the range iterator never advanced. The fix is a one-line default: `<range>`'s `range-by` slot now defaults to fixnum `1` via the new `slot_integer_default` helper. The Sprint 21 headline test `dylan_reduce_plus_zero_range_one_to_hundred_is_5050` now uses `reduce(\+, 0, make(<range>, from: 1, to: 100))` end-to-end, closing the deferral.

**C. Generic-dispatch trampoline for `\name`.** Sprint 22's `register_top_level_functions` had a "first-registration-wins" hack: when `\size` was used as a value, Sprint 21's function-ref machinery had to pick ONE method body's code-ptr to register against the source name. That made `\size(<table>)` call the wrong body. Sprint 26 introduces `FUNCTION_KIND_GENERIC_TRAMPOLINE` (a fourth `<function>` kind-tag value, alongside top-level/lifted-anon/closure) and `make_generic_trampoline_ref`: when `make_function_ref(name, arity)` is asked for a name that already has at least one registered method (`is_generic_defined`), it returns a trampoline `<function>` Word whose `env-ptr` slot stashes the `&'static GenericFunction` pointer. Every `nod_funcall_N` checks the kind-tag first; on a match it routes to `dispatch_via_generic_trampoline`, which walks the applicable-method chain via `nod_dispatch` and tail-calls the most-specific body. `\size(<table>)` now selects the `(t :: <table>)` method, `\size(<list>)` selects the generic-fallback body, and `\size(<range>)` likewise — confirmed by `generic_function_ref.rs`. The Sprint 22 shadow-registration in `register_top_level_functions` is removed.

Test count moves from 425 / 0 / 5 to 441 / 0 / 5 under `newgc-backend` default; semispace escape hatch tracks from 417 to 438. Clippy clean. 5x flake check clean.

### Sprint 25 — Retire `Expr::Unless` in favor of stdlib macros — landed
Shipped: body-shaped macro call parser (`Expr::MacroCall { name, span }` recognised at parse time when `<name>(head…) body… end` appears and `<name>` is in the parser's known-macro set, seeded from the stdlib by `nod-sema::parse_user_module`). `define macro unless` joins `define macro for-each` in `nod-dylan/dylan-sources/stdlib.dylan`; the parser-hardcoded `parse_unless` arm and the `Expr::Unless` AST variant are deleted. `unless (cond) body end` parses to `Expr::MacroCall("unless", ...)`, the stdlib's `unless` macro expands it to `if (~ cond) body else #f end`, and the kernel `Expr::If` lowering handles the rest. As a bonus, `for-each (x in #(1, 2, 3)) total := total + x end` now works as a body-shaped surface — the Sprint 20b deferred call site that the parser couldn't recognise. Test count moves from 421 / 0 / 5 to 425 / 0 / 5 under `newgc-backend` default; semispace escape hatch from 413 / 0 / 5 to 417 / 0 / 5. Deferred: `Expr::Case` retirement (case's multi-arm `=>` syntax doesn't fit the body-shaped recogniser — needs auxiliary `rule` clauses inside `define macro`; tracked for Sprint 26). The `feedback_dylan_lang_defined_by_macros.md` direction is validated: the compiler shrinks by ~70 deleted lines of hardcoded `unless` machinery and the language surface grows by ~10 lines of Dylan macro source.

### Sprint 27 — FFI Phase A: data fork + `nod-winapi` crate + Binding DLL provenance + `define c-function` parser — landed
Opens the 10-sprint FFI trajectory that ends at the Dylan-side IDE shell. **Data plumbing only — no API calls execute yet.** Three deliverables:

**A. Vendored Windows API metadata + `nod-winapi` crate.** Forked `E:\windows_api\windows_api.db` (29 MB SQLite, schema v5 — `kind ∈ {primitive, reference, pointer, enum, struct, union, interface, delegate, apis-container, type}`) into the workspace at `data/windows_api.db`. New crate `src/nod-winapi/` with `build.rs` projecting primitive-typed function signatures into a compact `WinApiIndex` struct, `postcard`-serialising, and `zstd`-19 compressing into `$OUT_DIR/winapi_data.bin.zst`. `lib.rs` includes the blob via `include_bytes!(env!("WINAPI_DATA_BIN"))`, decompresses + parses on first access through a `LazyLock`-wrapped `HashMap` index. Projected subset: **13,080 functions across 336 DLLs** (kernel32 contributes ~1165 — every primitive-typed function from `Beep` to `WaitForSingleObject`). Embedded blob: **205,118 bytes** — 6.7% of the Sprint 27 3 MB budget. The schema's `reference` kind (BOOL, HRESULT, HANDLE, DWORD, …) carries no `target_type_id`, so we resolve well-known Windows typedefs by NAME against a static table in `build.rs`. Constants table is hand-curated for now (`MB_OK`, `INVALID_HANDLE_VALUE`, …) — the upstream DB doesn't model constants yet.

**B. `Binding` struct + `BindingId` table in `nod-namespace::graph`.** The Sprint 04 `BindingId(u32)` was scaffolding; this sprint populates it for the first time. New `Binding { id, name, kind: BindingKind::CFunction, dll: Option<String> }` and `Graph::record_c_function_binding(module, name, dll) -> BindingId`. Dylan-to-Dylan bindings still live in the flat sema tables — they'll migrate in a future namespace-consolidation sprint. For Sprint 27 the `Binding` table is the single source of truth for c-function DLL provenance.

**C. `define c-function` parser surface + sema validation.** Parser arm dispatches `define c-function NAME (PARAMS) => (RET); library: "STR"; [c-name: "STR";] end;` at `nod-reader/src/parser.rs:1722` (sibling to `define function`). New AST variant `Item::DefineCFunction { name, params, return_, c_name, library, span }`. Sema (`nod-sema/src/lower.rs`) lowers each declaration into a `CFunctionBinding` carried on `LoweredModule::c_functions`, probes the embedded `nod-winapi` index for the (DLL, c-name) pair, sets `resolved_in_db: bool`, and surfaces a non-fatal `LoweringWarning::CFunctionNotInDb` for unresolved symbols (user might target a custom DLL). **Crucially: any call site that invokes a c-function name errors with `Sprint 28` deferral text** — the AST scan in `scan_module_for_c_function_calls` walks `Expr::Call { callee: Ident(name) }` against the c-function name set. This locks in the deferral. Headline test (`tests/nod-tests/tests/c_function_parse.rs::c_function_call_site_errors_in_sprint27`) parses + sema-lowers `define c-function Beep ... end; define function call-beep() Beep(440, 1000); end;` and asserts the diagnostic exists.

**Plus:** 14 new c-type seed classes (`<c-bool>`, `<c-dword>`, `<c-int>`, `<c-uint>`, `<c-short>`, `<c-ushort>`, `<c-long>`, `<c-ulong>`, `<c-word>`, `<c-byte>`, `<c-pointer>`, `<c-handle>`, `<c-string>`, `<c-wide-string>`) in `nod-runtime/src/c_types.rs` — registered via the same `ensure_*_registered` pattern as Sprint 19 conditions and Sprint 22 tables. No marshaling behavior yet; the classes exist so sema can resolve their names without erroring. Sprint 28 will give them real ABI behavior.

Test count moves from 441 / 0 / 5 to **455 / 0 / 5** under `newgc-backend` default (+7 from `tests/nod-winapi/tests/lookup.rs`, +7 from `tests/nod-tests/tests/c_function_parse.rs`); semispace escape hatch from 438 / 0 / 5 to 452 / 0 / 5. Clippy `--all-targets -- -D warnings` clean (only the deliberate build-script `cargo:warning=...` lines, which is intended diagnostic output). 5x flake check clean.

Deviation from the brief: the upstream `windows_api.db` schema doesn't carry a `constants` table; Sprint 27 ships with a hand-curated list of ~10 well-known constants (`MB_OK`, etc.) to keep the Phase A smoke test honest. Sprint 28 (or a separate DB-extension task) can widen this.

### Sprint 28 — FFI Phase B: per-module API stub table + first end-to-end `Beep(440, 50)` — landed
Headline acceptance: `Beep(440, 50)` runs through `eval_expr_with_items_to_string`, produces an audible 50ms beep (when an audio device is present — returns `#t` regardless), and the test passes.

**A. `<c-ffi-error>` condition + WinFFI types (Phase A).** New `nod-runtime/src/winffi.rs` (~900 lines including the per-arity trampolines) carries `ApiStubEntry { dll_name_ptr, dll_name_len, symbol_name_ptr, symbol_name_len, fn_ptr: AtomicPtr<u8>, signature: ApiCallSignature }`, `ApiStubTable { entries: &'static [ApiStubEntry] }`, `CArgKind` / `CReturnKind`, plus `<c-ffi-error>` as a subclass of `<error>` with `dll-name`, `symbol-name`, `os-error-code`, `message` slots.

**B. Win64 trampolines for arity 0..=8 (Phase B).** Nine `#[unsafe(no_mangle)] pub unsafe extern "C-unwind" fn nod_winffi_call_N(entry: u64, a0: u64, …) -> u64`. Each loads the resolved fn-ptr from the entry (Acquire), unboxes each arg per the recorded signature, transmutes to `extern "system" fn(…)` (Win64 ABI: RCX/RDX/R8/R9 + stack slots beyond shadow space), invokes, reboxes the return as a Dylan Word.

**C. Eager LoadLibrary + GetProcAddress (Phase C).** `resolve_symbol(dll, symbol)` caches HMODULEs in a process-wide `Mutex<HashMap<String, isize>>`. `resolve_into_entry(entry_ptr, dll, symbol)` populates one entry, bumps WinFFI stats. **Deviation from the brief**: we use `windows-sys`'s raw `LoadLibraryA` / `GetProcAddress` instead of `libloading`. `nod-runtime` already depends on `windows-sys` for `Win32_System_Memory`; adding `Win32_System_LibraryLoader` is a one-feature bump rather than a whole new dependency.

**D. Lowering + codegen (Phase D).** `nod-sema/src/lower.rs` gets a Phase 3b pre-pass that walks `Item::DefineCFunction` declarations, builds the marshaling signature from the `<c-…>` ident annotations, deduplicates `(dll, symbol)` pairs, and allocates a single per-module `ApiStubTable` in the static area. The per-call lowering (in `lower_call`) emits `Computation::DirectCall { callee: "nod_winffi_call_N" }` against the synthetic trampoline name; the first arg is a `ConstValue::WordBits` carrying the raw static-area pointer to the entry. Codegen (`nod-llvm/src/codegen.rs`) gets 9 new symbol constants + a 9-row entry in `SPRINT_20B_PRIMITIVES`; the JIT layer (`nod-llvm/src/jit.rs`) binds them to the runtime trampoline addresses via `LLVMAddGlobalMapping`.

Module init: `nod-sema::initialize_module_winffi` walks `LoweredModule::c_function_stub_table` after the JIT engine finalises and calls `resolve_into_entry` for each spec; failures surface as `EvalError::WinFfiInit { class_name: "<c-ffi-error>", dll, symbol }` so tests can pattern-match without parsing a rendered message.

**E. Acceptance tests (Phase E).** `tests/nod-tests/tests/c_function_call.rs` (Windows-only, all `#[serial]`):
- `headline_beep_call_returns_true` — `Beep(440, 50)` → `"#t"`.
- `get_tick_count_returns_increasing_value` — `GetTickCount` + `Sleep` + `GetTickCount`, asserts delta ≥ 0.
- `get_current_process_id_returns_integer` — PID > 0, fits in u32.
- `sleep_zero_returns_without_crashing` — void-return `Sleep(0)` surfaces as `"#()"`.
- `get_current_process_returns_handle` — pseudo-handle (-1) returned, asserts non-zero.
- `api_stub_table_deduplicates_call_sites` — two call sites of `GetTickCount` → `winffi_stats().entries == 1`.
- `unknown_dll_signals_c_ffi_error` — `block`-free expectation via `EvalError::WinFfiInit { class_name: "<c-ffi-error>", dll: "nosuchmodule_sprint28.dll", … }`.
- `unknown_symbol_signals_c_ffi_error` — same shape, `kernel32.dll` + bogus symbol name.

Plus a rewritten `c_function_call_site_lowers_in_sprint28` test in `c_function_parse.rs` (was the Sprint 27 deferral test), plus `c_function_with_unsupported_type_still_defers` for `<c-string>` (Sprint 30 territory).

Test count: **455 → 464 / 0 / 5** under `newgc-backend` default (+9 tests). Semispace escape hatch: **452 → 461 / 0 / 5**. Clippy `--all-targets -- -D warnings` clean. 5x flake-check clean.

Sprint 28 scope is integer/pointer args/returns up to arity 8. Strings (Sprint 30), structs (34), callbacks (33), COM (35), and variadics remain deferred. Per-call `GetLastError` is available manually; auto-raise on Win32 failure (the `set-last-error:` plumbing) waits for Sprint 30+.

Deviation from the brief's wrapper API: the `eval_expr_with_items_to_string` wrap requires a blank line between `Module:` and the first item because `scan_preamble` greedily consumes lines with continuation indents (an indented `(args)` line on a `define c-function` would otherwise get eaten). Documented inline.

### Sprint 29 — Win32 constants generator (`$MB-OK`, `$WM-PAINT`, …) — landed
Replaces magic-number FFI call sites with idiomatic named constants. `MessageBoxW(NULL, "hi", "title", $MB-OK)` and `PostMessageW(hwnd, $WM-CLOSE, 0, 0)` now resolve at lowering time without a single function-ref hop.

**A. Database investigation (Phase A).** Confirmed schema v5 of `windows_api.db` carries 7,773 `enum`-kind type rows (`MESSAGEBOX_STYLE`, `WIN32_ERROR`, `SHOW_WINDOW_CMD`, …) but **NOT** their member values — no `enum_members` table, no `is_const=1` rows in `types`, no rows that reference the enum type via `target_type_id`. The upstream WinMD importer didn't project member integers into the SQLite shape. Falling back to a hand-curated source of truth: `data/win32_constants.txt`, 300 entries covering the most-used Win32 constants (MessageBox flags, window messages, window styles, ShowWindow commands, GetWindowLong offsets, standard cursors/icons, system metrics, GDI ROP codes, process/file access rights, VirtualAlloc flags, standard handles, WaitFor* returns, HRESULT codes, Win32 error codes).

**B. Build-time extraction (Phase B).** `src/nod-winapi/build.rs::project_constants` now reads `data/win32_constants.txt` (parsed by a stdlib-only INI-style parser — no new dep) and emits 300 `ConstantInfo` rows into the embedded blob. Each entry carries name, i64 value (parsed from decimal or `0x…` hex, sign-extended), and optional source-DLL annotation. Duplicate names allowed only if values agree (e.g., `MB_ICONERROR == MB_ICONSTOP == 0x10` — three Win32 spellings for the same flag value). Build-time `cargo:warning` reports the actual count: `nod-winapi: 13080 functions, 300 constants, 336 dlls`.

**C. `nod_winapi::iter_constants` (Phase C).** New public API surface for walking the embedded constant set. `find_constant` (Sprint 27) stays as the random-access lookup; `iter_constants` covers the generator and the regression test that locks in the 50-constant floor.

**D. Generator binary (Phase D).** `src/nod-winapi/src/bin/generate_constants.rs` reads `data/win32_constants.txt` (preserving category headers so the generated Dylan file stays grouped) and emits `src/nod-dylan/dylan-sources/win32-constants.dylan` — 300 `define constant $NAME = value;` lines, with `_` → `-` transformation and `$` prefix per Dylan convention. Values < 256 emit as decimal, larger values as `#xHEX` (Dylan hex literal), negatives as signed decimal. Run via `cargo run --quiet -p nod-winapi --bin generate_constants`; idempotent against unchanged source.

**E. Stdlib loader picks up `win32-constants.dylan` (Phase E).** `nod-sema/src/stdlib.rs` refactored to a multi-file `STDLIB_FILES` list. The loader parses each file, merges items into a single module, then strips `Item::DefineConstant { value: Expr::Integer(_, n) }` entries into a process-global `STDLIB_CONSTANTS: HashMap<String, i128>`. User-code lowering (`Expr::Ident` resolution in `lower.rs`) consults this map BEFORE the function-ref fallback path so `$MB-OK` becomes `ConstValue::Integer(0)` — not a `<function>` Word. The 300 constants never become functions in the stdlib JIT engine; they're pure compile-time values.

**F. Sprint 28 headline test wired through a constant (Phase F).** `tests/nod-tests/tests/c_function_call.rs::flash_window_with_named_constants` evaluates `$WM-NULL + GetTickCount()` and asserts the sum is the (positive) tick count, proving `$WM-NULL` resolves to 0 in the same expression context as a real Win32 call.

**G. Acceptance tests (Phase G).** New `tests/nod-tests/tests/win32_constants.rs` with 9 tests:
- `mb_ok_resolves_to_zero` — small zero flag round-trips.
- `wm_paint_resolves_to_15` — `0x000F` hex source surfaces as decimal `"15"`.
- `mb_iconerror_resolves_to_16` — `0x10` round-trips.
- `ws_overlappedwindow_is_complex_mask` — `0x00CF0000 == 13565952`, the union of OVERLAPPED|CAPTION|SYSMENU|THICKFRAME|MINIMIZEBOX|MAXIMIZEBOX.
- `gwl_style_resolves_to_minus_16` — negative offset round-trips through the curated `-16` spelling.
- `unknown_constant_errors_at_lower` — `$NOT-A-REAL-CONSTANT` produces `EvalError::Lower` with an `undefined ident` diagnostic.
- `constant_usable_in_arithmetic` — `$MB-OK + $MB-ICONERROR == 16`, proving both names resolve as integers in the same expression.
- `stdlib_constants_count_at_least_50` — locks the lower bound on coverage by inspecting `nod_sema::stdlib::constants_table()`.
- `winapi_iter_constants_count_at_least_50` — same lower bound at the `nod-winapi` layer.

Test count: **464 → 475 / 0 / 5** under `newgc-backend` default (+10 tests, including the new `flash_window_with_named_constants` in `c_function_call.rs` and 9 acceptance tests in `win32_constants.rs`). Semispace escape hatch: **461 → 472 / 0 / 5**. Clippy `--all-targets -- -D warnings` clean. 5x flake check clean.

Deviation from the brief: the brief considered a TOML-formatted curated file as one option for the hand-curated set; we went with a simpler `key = value` line-based format (`data/win32_constants.txt`) so `build.rs` could parse it with no new dep. The generator binary (Rust, not Python — keeps the toolchain story to "just `cargo`") preserves category headers from the source file so the emitted Dylan stays grouped by feature area.

Closes the Sprint 27 deferred entry about the upstream constants table; opens a new deferred entry about reviving enum-member type-checking (Sprint 30+) and string constants (`IID_*`, `CLSID_*` — Sprint 30+ territory).

### Sprint 30 — FFI Phase C: `<c-string>` + `<c-wide-string>` + `$NULL` — landed
Empirical headline: `lstrlenW("héllo") → "5"`. 'é' (U+00E9) is two UTF-8 bytes (0xC3 0xA9) but one UTF-16 code unit; only correct UTF-8 → UTF-16 transcoding produces 5. A byte-copy implementation would return 6, and any test that just checks ASCII strings would never spot the bug. The non-ASCII assertion *is* the proof that string marshaling works.

**A. `TempBuf` infrastructure + per-call buffer lifetimes (Phase A).** New `enum TempBuf { Narrow(Vec<u8>), Wide(Vec<u16>) }` in `nod-runtime/src/winffi.rs`. Each arity-N trampoline (`nod_winffi_call_1` .. `nod_winffi_call_8`) now allocates `let mut temps: Vec<TempBuf> = Vec::new();` on its stack frame before the unbox phase; `unbox_arg(w, kind, &mut temps)` pushes one `TempBuf` per string arg, returns the buffer's `as_ptr() as u64`, and the `Vec` drops at end of scope — *after* the C call returns. No leaks; lifetime is exactly the call.

**B. `CArgKind::NarrowString` + `CArgKind::WideString` (Phase A continued).** Two new discriminants on `#[repr(u8)] enum CArgKind` (values 12 and 13), plus `CReturnKind::NarrowString` (8) and `CReturnKind::WideString` (9). `signature_from_names` recognises `"<c-string>"` and `"<c-wide-string>"` for both arg and return positions. The receive-side path (Win32 API returns an LPCSTR/LPCWSTR — e.g. `GetCommandLineW`) scans the returned pointer to its null terminator (capped at 1MiB) and copies into a fresh Dylan `<byte-string>` via `intern_string_literal`.

**C. Wide-string transcoding (Phase A continued).** Uses `s.encode_utf16().collect::<Vec<u16>>()` + push(0) — std-lib only, no new transitive deps. The narrow path is intentionally pass-through bytes (UTF-8 → LPSTR with terminator) — this matches CP_ACP on the ASCII subset and avoids pulling in `WideCharToMultiByte` for the headline test. CP_ACP conversion for non-ASCII narrow strings is a deferred polish item.

**D. `$NULL` constant + null-pointer marshaling (Phase B).** New "Pointer / handle sentinels" category in `data/win32_constants.txt`: `NULL = 0`. The Sprint 29 generator picks this up, so `src/nod-dylan/dylan-sources/win32-constants.dylan` now exposes `define constant $NULL = 0;`. The marshaling change is one branch in `marshal_narrow_string` / `marshal_wide_string` / `unbox_arg`'s `Pointer|Handle` arm: a Dylan fixnum 0 → C `null` pointer. Callers can write `MessageBoxW($NULL, "hi", "title", $MB-OK)` idiomatically.

**E. Stats accounting (Phase A continued).** `WinFfiStats` gains a `tempbufs_allocated_lifetime: usize` counter, bumped by `marshal_narrow_string` / `marshal_wide_string`. Useful for the per-call allocation regression test and for any future cost-watch. Reset alongside the other counters by `_reset_winffi_stats_for_tests`.

**F. Sema lift of the Sprint 28 deferral (Phase C).** The Sprint 28-era `c_function_with_unsupported_type_still_defers` test is replaced by `c_function_with_string_arg_lowers_in_sprint30` in `tests/nod-tests/tests/c_function_parse.rs`: lowering a `MessageBoxA(<c-handle>, <c-string>, <c-string>, <c-dword>) => (<c-int>)` declaration now succeeds and produces an `ApiCallSignature` with arg kinds `[Handle, NarrowString, NarrowString, UInt32]` and return kind `Int32`.

**G. Acceptance tests in a new file (Phase C+).** `tests/nod-tests/tests/winffi_strings.rs` — 9 value-asserting tests + 1 ignored-by-default interactive demo:
- `lstrlen_w_returns_correct_wide_length` — `lstrlenW("hello world") → "11"`.
- `lstrlen_w_handles_unicode_correctly` — **`lstrlenW("héllo") → "5"`** (the empirical UTF-16 proof).
- `lstrlen_a_returns_correct_narrow_length` — `lstrlenA("hello world") → "11"`.
- `lstrlen_a_handles_utf8_as_bytes` — `lstrlenA("café") → "5"` (5 UTF-8 bytes; proves narrow path doesn't transcode).
- `lstrlen_w_empty_string` — `lstrlenW("") → "0"`.
- `null_constant_evaluates_to_zero` — `$NULL → "0"`.
- `null_pointer_via_dollar_null` — `lstrlenW($NULL) → "0"` (NULL pointer reaches the API per MSDN's documented contract).
- `mixed_args_string_and_int` — `lstrcmpW("abc", "abc") → "0"` (two wide-string args, separate temp buffers).
- `tempbuf_allocation_count_tracks_string_args` — two `lstrlenW` calls bump `tempbufs_allocated_lifetime` from 0 to exactly 2.
- `message_box_w_pops_real_dialog` — **`#[ignore]`-gated** opt-in developer demo; run manually via `cargo test --test winffi_strings -- --ignored`. NOT invoked by routine `cargo test`.

The brief originally suggested `IsBadStringPtrW($NULL, 10)` for the NULL-marshaling test; we substituted `lstrlenW($NULL)` because IsBadStringPtrW is deprecated and behaves unreliably on modern Windows, while `lstrlenW`'s NULL contract is documented and stable. Same shape of proof — fixnum 0 must reach the API as a real null pointer or the API would crash / return garbage instead of 0.

Test count: **475 → 484 / 0 / 6** under `newgc-backend` default (+9 passing string tests; +1 ignored MessageBoxW). Clippy `--all-targets -- -D warnings` clean. 5x flake check clean. The semispace backend is no longer routinely exercised — newgc default is the only verification path now.

**Out of scope for Sprint 30 (deferred):**
- C → Dylan string returns at the headline level (basic LPCSTR/LPCWSTR scan-and-copy is wired up via `CReturnKind::NarrowString` / `WideString`, but the out-buffer pattern — caller-allocated buffer + length, e.g. `GetWindowTextW(hwnd, buf, len)` — needs a separate sprint).
- CP_ACP encoding conversion for `<c-string>` (currently pass-through UTF-8 bytes).
- True wide-character Dylan-side storage (`<unicode-string>` Dylan class for UTF-16 payloads — currently we transcode at the boundary).
- BSTR / OLEStr handling (Sprint 35 — COM territory).

Sprint 30's "Dylan-side IDE bring-up" slot from the prior plan shifts forward; the FFI Phase C string-marshaling work moves into the Sprint 30 slot, and the IDE work tracks behind the Sprint 31 (`common-dylan` port) entry as Sprint 32+. Renumbering of downstream slots is deferred to the next sprint-plan review.

### Sprint 31 — JIT-time Win32 API materialization (bare-name calls) — landed

**Goal:** `GetTickCount64()` returns the system uptime via `eval_expr_to_string` **without any `define c-function` declaration above it**. Sprint 28 wired the table; Sprint 31 makes the table populate itself from the embedded `nod-winapi` index when Dylan source references a bare Win32 name.

**A. Sema lookup hook (`nod-sema/src/lower.rs`).** After the existing Phase 3b walk that builds the per-module stub table from explicit `define c-function` declarations, a new pre-Phase-4 pass walks the AST for `Expr::Call { callee: Expr::Ident(name), ... }` and collects every name that (a) isn't user-declared, (b) isn't a Dylan top-level function, generic, or class, and (c) passes a shape filter for Win32 exports (`looks_like_win32_export`: at least one uppercase letter, all ASCII alphanumerics, ≥ 3 chars). For each such candidate `try_jit_materialize_winapi(name)` consults `nod_winapi::functions()`:

1. **A/W default to W.** Bare `MessageBox` is rewritten to `MessageBoxW` first; the literal name is the fallback. Bare `MessageBoxA` keeps the explicit A suffix.
2. **Cross-DLL priority.** When a name resolves in multiple DLLs, `WINAPI_DLL_PRIORITY` breaks the tie: `kernel32.dll` > `user32.dll` > `gdi32.dll` > `advapi32.dll` > `shell32.dll` > `comctl32.dll` > alphabetical fallback.
3. **Signature derivation.** `build_signature_from_function_info` walks `FunctionInfo::params` + `return_type` (`nod_winapi::TypeRef`) and maps each to `nod_runtime::CArgKind` / `CReturnKind`. Unmappable shapes (`Void` as a param, `Pointer { pointee: Function }`, struct-by-value — none of which actually reach the embedded blob because `build.rs` filters them — and arity > 8) return `Err(reason)`; the materialization declines and the caller surfaces a "Win32 function found but signature uses unsupported types" diagnostic.

**B. `BindingSource` enum (`nod-sema/src/lower.rs`).** New `BindingSource::{UserCFunction, JitMaterialized}` on `CFunctionBinding`. User declarations always carry `UserCFunction`; the bare-name fallback decline-on-collision rule guarantees JIT materialization never overwrites an explicit binding. Introspection (`introspect_bindings`) exposes the field directly.

**C. Stub-table integration.** Synthesized bindings feed into the existing `c_function_specs` / `spec_dedupe` machinery — two bare references to `GetTickCount64` in the same module share one slot, and the resolver / trampoline path is unchanged from Sprint 28. No new IR variants, no new runtime infrastructure, no marshaling changes. Sprint 31 is sema-only.

**D. Diagnostics.** Bare-name calls whose Win32 entry exists in the index BUT whose signature uses unsupported categories now emit `LoweringError::Unsupported { message: "Win32 function `X` was found in the embedded windows_api.db index, but its signature uses unsupported types (...); declare an explicit `define c-function X ... library: ...; end;` with a shim signature, or wait for Sprint 33 (callbacks) / Sprint 34 (structs)." }`. Bare names that aren't in the index at all fall through to the existing `Codegen(UnknownCallee)` path — same behavior as before Sprint 31.

**E. Stats.** `WinFfiStats::materialized_lifetime` (process-global counter, bumped from `nod_runtime::winffi_record_materialized()` once per synthesized binding) surfaces materialization activity for tests and the future `winffi-stats` diagnostic command.

**F. Tests (`tests/nod-tests/tests/winffi_materialize.rs`).** 10 acceptance tests + 1 ignored marker, all `#[serial]` (`#[cfg(windows)]`):
- `bare_GetTickCount64_resolves_to_kernel32` — **headline**: bare-name uptime call, asserts > 1000 ms.
- `bare_GetCurrentProcessId_resolves_correctly` — positive u32.
- `bare_Sleep_resolves_to_void_returning` — void-return materialization works.
- `bare_lstrlenW_resolves_with_string_marshaling` — bare `lstrlenW("héllo")` → "5" (UTF-16 transcoding through a synthesized binding).
- `bare_MessageBox_resolves_to_W_variant` — introspection: bare `MessageBox` materializes as `MessageBoxW` from `user32.dll`, no dialog popped.
- `bare_MessageBoxA_resolves_explicitly` — introspection: explicit A suffix kept.
- `user_define_c_function_overrides_materialization` — user-declared `GetTickCount` carries `BindingSource::UserCFunction`, exactly one binding (no duplicate JIT-materialized entry).
- `unsupported_signature_declines_materialization` — `CreateProcessW` (10 params, > 8 arity cap) either errors with "unsupported types" or falls through to `UnknownCallee`; never silently succeeds.
- `stats_show_materialization_count` — two distinct bare calls bump `materialized_lifetime` by 2.
- `duplicate_bare_calls_share_one_materialization` — two calls to the same bare name share one slot (one materialization counter bump).
- `ambiguous_name_picks_kernel32_first` — `#[ignore]` marker: no genuine cross-DLL collisions in the current embedded blob; priority order covered by the pure-function unit test in `nod-sema` below.

Plus 7 unit tests in `nod-sema::lower::sprint31_tests`: `winapi_dll_priority_orders_kernel_first`, `looks_like_win32_export_filters_correctly`, `jit_materialize_GetTickCount64_yields_kernel32_no_args`, `jit_materialize_bare_MessageBox_picks_W`, `jit_materialize_unknown_name_returns_not_found`, `jit_materialize_lstrlenW_succeeds`, `jit_materialize_EnumWindows_outcome`.

**Headline acceptance:** `eval_expr_to_string("GetTickCount64()")` returns a positive integer > 1000 with no `define c-function` declaration. The bare-name `lstrlenW("héllo")` returns "5" through the same path Sprint 30 proved out for explicit declarations.

**Gate results:** 501 / 0 / 7 under newgc default (484 → 501; +17 new tests, +1 ignored marker). 5x sequential flake clean. `cargo clippy --workspace --all-targets -- -D warnings` clean.

**Out of scope (deferred):**
- Process-global materialized-binding cache (each eval-module re-materializes; not yet measurably hot).
- Better ambiguity fix-it hints (currently the message just names the colliding DLLs; no auto-suggest).
- Materialize-by-pattern (`Get*`, `*A` / `*W` family expansions) for IDE auto-completion.
- A/W resolution that walks back to a single canonical Dylan-name (currently `MessageBox` materializes to `MessageBoxW` and the synthesized binding's `dylan_name` stays `MessageBox` — the user code sees the bare name they wrote).

### Sprint 32 — Callbacks: closure → C function pointer — landed

**Goal:** `EnumWindows(callback, $NULL)` enumerates every top-level window on the test machine, invoking a Dylan closure (`method (hwnd, lp) ... #t end`) once per window, with the closure incrementing a captured-variable counter that survives across calls. The keystone IDE-essential FFI capability — `WNDPROC` for window procedures, `WNDENUMPROC` for `EnumWindows`. Sprint 28 wired Win32 → Dylan calls; Sprint 32 closes the reverse direction, Dylan-as-callback → Win32.

**A. Pre-allocated trampoline pool per signature class (`nod-runtime/src/callbacks.rs`).** Each Win32 callback signature has a fixed pool of 32 slot trampolines, one `extern "system" fn` per slot. A macro generates `wndproc_slot_0` … `wndproc_slot_31` (and the `wndenumproc_slot_N` family); each slot trampoline knows its slot ID at compile time and forwards to a per-signature dispatcher that looks up the registered closure Word for that slot, marshals C args → Dylan `Word`s, calls the closure via Sprint 24's `nod_funcall_N`, and rebox the return.

Sprint 32 ships two signatures:
- `Wndproc`: `extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT` (the WNDPROC contract for `RegisterClass(W)`).
- `Wndenumproc`: `extern "system" fn(HWND, LPARAM) -> BOOL` (the WNDENUMPROC contract for `EnumWindows` and family).

The fixed pool of 32 slots per signature is the Sprint 32 cap; tunable later via build-time const. The slot trampolines occupy 64 `extern "system"` symbols in `nod-runtime` (32 × 2 signatures), each with `#[unsafe(no_mangle)]` so the linker pins their addresses.

**B. Registry per signature (`OnceLock<Mutex<Registry>>`).** Each `Registry` holds `Box<[UnsafeCell<Word>; 32]>` — one stable closure-cell address per slot — plus an `occupied: [bool; 32]` bitmap. The slab address is stable for the process lifetime; the cell pointers are valid GC root targets.

**C. GC root discipline — per-thread.** Sprint 11c's `register_root` is thread-local (each mutator's `ROOT_STACK` is its own `RefCell<Vec<*const Word>>`). The callback registry's cells must be in EVERY mutator thread's root stack — otherwise a collection on a thread that didn't install them misses the registered closures. `install_gc_roots_for_this_thread(sig, registry)` registers all 32 cells on first touch from each thread, idempotent-guarded via `thread_local! { static WNDPROC_ROOTS_INSTALLED: Cell<bool>; }`. Both `register_callback` and the dispatchers call it on entry — covering the mutator and the OS-callback thread.

**D. JIT-callable externs.** `nod_register_wndproc(closure_word) -> Word` and `nod_register_wndenumproc(closure_word) -> Word`. Each is `unsafe extern "C-unwind"` to match the rest of the runtime's JIT ABI; the return is the slot's trampoline address packed into a fixnum-tagged `<c-pointer>` Word (the Sprint 28+ ABI for raw addresses). On pool exhaustion, surfaces a `<c-ffi-error>` via `nod_signal` (diverges).

**E. Lowering wiring (`nod-sema/src/lower.rs::LOWER_PRIMITIVE_TABLE`).** Two new primitives:
- `%register-wndproc(closure)` → `nod_register_wndproc`, arity 1, returns `<top>` (the `<c-pointer>` Word).
- `%register-wndenumproc(closure)` → `nod_register_wndenumproc`, arity 1.

**F. Stdlib wrappers (`nod-dylan/dylan-sources/stdlib.dylan`).** Two thin functions:

```dylan
define function as-wndproc-callback (closure) => (ptr)
  %register-wndproc(closure)
end function;

define function as-wndenumproc-callback (closure) => (ptr)
  %register-wndenumproc(closure)
end function;
```

A unified `as-c-callback(closure, signature-symbol)` form is deferred until Dylan-side `select` lowers cleanly (the current macro layer doesn't reach `select` at stdlib-load time).

**G. Codegen + JIT symbol bindings (`nod-llvm/src/codegen.rs`, `jit.rs`).** Two new symbol constants (`NOD_REGISTER_WNDPROC_SYMBOL`, `NOD_REGISTER_WNDENUMPROC_SYMBOL`) added to the `SPRINT_20B_PRIMITIVES` table; matching `LLVMAddGlobalMapping` entries in `jit.rs::add_module`. No new IR variants — the call lowers as a plain `DirectCall` against a `%`-prefixed primitive name, the same shape as every other runtime extern.

**H. Tests.** Six integration tests in `tests/nod-tests/tests/winffi_callbacks.rs` (`#![cfg(windows)]`, all `#[serial]`):

- **`enum_windows_invokes_callback_for_each_top_level_window`** — **the Sprint 32 headline**. `EnumWindows(callback, $NULL)` invokes a Dylan closure that increments a captured `count := count + 1` once per top-level desktop window; counter ends up positive (asserted `> 0` and `< 100_000` for sanity).
- `register_wndenumproc_returns_non_null_pointer` — non-zero trampoline address.
- `register_wndproc_returns_non_null_pointer` — same for WNDPROC (arity 4 closure body).
- `two_callbacks_get_distinct_addresses` — two registrations land in distinct slots → distinct trampoline addresses → Dylan-side `a = b` is `#f`.
- `callback_pool_full_signals_error` — 32 registrations succeed via the Rust API, the 33rd returns `Err(PoolFull)`; pool reset clears state for subsequent tests.
- `closure_survives_gc_pressure` — register a closure, force two minor GCs, invoke the trampoline directly via an `extern "system"` fn-pointer transmute; the closure body still runs.

Plus five in-module unit tests in `nod-runtime/src/callbacks.rs::tests` (`#[serial]`): synthetic dispatch via direct trampoline call (one each for WNDPROC and WNDENUMPROC), distinct-slot-addresses Rust check, pool-full Rust check, rebox-helper truth-table coverage.

**Headline acceptance:** the EnumWindows test returned a count of top-level windows on the test machine — a positive integer reflecting the actual Windows shell state at test time.

**Gate results:** 512 / 0 / 7 under newgc default (501 → 512; +11 new tests). 5x sequential flake clean. `cargo clippy --workspace --all-targets -- -D warnings` clean.

**Out of scope (deferred — see DEFERRED.md):**

- **Callback unregistration.** Sprint 32 registrations are leak-by-design: once a closure is registered, its slot stays occupied for the process lifetime. A future sprint adds `release-c-callback(ptr)` semantics with safe-point coordination so the OS isn't holding a stale trampoline mid-callback.
- **Additional signatures.** `TIMERPROC` (`SetTimer`), `THREADPROC` (`CreateThread`), `DLGPROC` (`DialogBox*`), Win32 hook procs (`SetWindowsHookEx`), CRT `qsort`/`bsearch`, and the various `EnumXxx` family beyond windows.
- **JIT-emitted per-callback trampolines.** Alternative to the fixed pool — each registration emits a fresh trampoline via the JIT, eliminating the pool-size cap. Memory cost per callback grows; freed-trampoline reclamation interacts with MCJIT engine lifetime. Sprint 32 ships the simpler fixed-pool architecture; a JIT-emitted variant becomes valuable when 32-slot saturation actually bites.
- **Cross-thread callback semantics.** If the OS invokes our trampoline on a thread different from the mutator that registered the closure, Sprint 32's per-thread root-installation handles GC root reachability, but a future cross-thread Sprint will need to lock the closure's environment frames (for closures with captured state in the moveable heap touched concurrently with mutator allocations).
- **Unified `as-c-callback(closure, sig-symbol)` surface.** Pending Dylan-side `select` lowering for the symbol-dispatched form.
- **`extern "system"` panic-on-unwind discipline.** Sprint 32 has the same UB exposure as Sprint 28's `nod_winffi_call_N` — a Dylan signal that crosses the OS callback boundary aborts (on Windows MSVC, panic crossing `extern "system"` is structurally unwound with a `STATUS_STACK_BUFFER_OVERRUN` abort). The mitigation is the same as Sprint 28's: trust callers to handle conditions before the closure returns. Tightening this (catching unwinds inside the dispatcher and returning a default value with a side-channel error) is a Sprint 33+ task.

### Sprint 34 — Structs: `<c-struct>` family for IDE-essential Win32 shapes — landed

**Goal:** `let pt = make(<point>); GetCursorPos(pt); point-x(pt) + point-y(pt)` runs through `eval_expr_to_string` and returns a real screen-cursor coordinate sum — empirical proof that struct allocation, address-of marshaling, field setters (by the C function via pointer), and field getters (Dylan reading the buffer back) all work end-to-end. The keystone IDE-essential FFI capability: `GetMessageW(LPMSG, …)`, `BeginPaint`, `GetCursorPos`, `GetClientRect`, `SetRect`, `GetSystemTime`, `GetLocalTime`, and every other Win32 API that takes a pointer to a caller-allocated struct.

**A. `<c-struct>` infrastructure (`nod-runtime/src/structs.rs`).** A new module registers a `<c-struct>` parent class at process boot via `ensure_structs_registered`, then six concrete subclasses (`<point>`, `<rect>`, `<size>`, `<filetime>`, `<systemtime>`, `<msg>`). Each concrete class:

- has `instance_size = 8 (wrapper) + struct_byte_size` matching the Win64 `sizeof` (POINT=8, RECT=16, SIZE=8, FILETIME=8, SYSTEMTIME=16, MSG=48);
- carries `is_byte_payload: true` on the `ClassMetadata` so the GC's `DylanLayout` reports an opaque payload (same pattern as `<byte-string>`);
- has a per-field layout table (`StructFieldInfo { name, offset, kind }`) accessible via `struct_layout_for(class_id)` for diagnostics.

Concrete struct classes bypass `register_simple_user_class` (which fixes instance size at `8 + 8*slot_count` and forces `is_byte_payload = false`) and go through a Sprint 34–local `register_struct` helper that allocates a custom `ClassMetadata` directly in the static area.

**B. Field accessor primitives (`nod-runtime/src/structs.rs`).** Each `nod_struct_get_*` / `nod_struct_set_*` pair takes a struct Word and a fixnum-tagged byte offset; the get returns a tagged fixnum, the set returns the value Word (Dylan setter convention). Sprint 34 wires six widths: i32, i64, u16, u32, u64, pointer. Unaligned reads/writes throughout so packed fields (e.g. `WPARAM` at MSG offset 16) need no extra alignment care.

The primitives' `offset` arg is itself a fixnum-tagged Word (`n << 1`) — JIT-emitted code passes Dylan integer literals which lower to tagged Words; a `decode_offset` helper unpacks the tag. Sprint 34 caught this convention mismatch the hard way (initial implementation treated the raw u64 as the offset, which silently doubled every access; field roundtrips passed because set and get cancelled out, but Win32 calls — which write at the true offsets — surfaced the bug as wrong field positions when read back).

**C. Stdlib field accessors (`nod-dylan/dylan-sources/stdlib.dylan`).** One getter and one setter per field of every seed struct, ~60 functions total, hand-generated. The setter signature follows the Sprint 12 unary-setter calling convention (`slot-getter(obj) := v` → `slot-getter-setter(obj, v)`): `point-x-setter(p, v)` forwards to `%struct-set-i32(v, p, 0)`. Sprint 35+ adds a `define c-struct` Dylan-side parser surface that emits these automatically.

**D. Auto-coerce in marshaling (`nod-runtime/src/winffi.rs::unbox_arg`).** When a `<c-function>` parameter is declared `<c-pointer>` or `<c-handle>` AND the actual Dylan arg is a pointer-tagged `<c-struct>` subclass instance, the marshaler passes `wrapper_ptr + 8` (the byte-payload start) instead of the wrapper address itself. The recognition test is `is_c_struct_instance(w)`, which walks the wrapper's class through `is_subclass(class, <c-struct>)`. The walk is short (Sprint 34 seed structs have a 3-entry CPL: self → `<c-struct>` → `<object>`), and Sprint 34 measured no observable hot-path impact — the `OnceLock::get` for the c-struct class id is non-locking, and `is_subclass` is a linear scan of a 3-entry Vec.

**E. Tests (`tests/nod-tests/tests/winffi_structs.rs`, plus inline `#[cfg(test)]` in `structs.rs`).**

Pure-Dylan field roundtrips (no Win32):
- `point_alloc_zeroes_fields` — `make(<point>)` zero-fills the payload; reading both fields returns `"0"`.
- `point_field_setter_roundtrip` — `point-x(p) := 42; point-y(p) := 99; point-x(p) + point-y(p)` returns `"141"`.
- `rect_all_four_fields` — set all four `<rect>` fields, compute width + height, expect `"270"`.
- `systemtime_u16_field_roundtrip` — `<systemtime>` u16 fields roundtrip through `2026 + 5 + 22 = "2053"`.
- `msg_mixed_width_fields_roundtrip` — `<msg>` exercises pointer, u32, u64, i64, i32 widths in one expression; sum `"15377"`.
- `point_is_subclass_of_c_struct` — `instance?(make(<point>), <c-struct>)` returns `"#t"`.

Rust-side metadata + GC:
- `instance_sizes_match_win64_sizeof` — POINT=16, RECT=24, SIZE=16, FILETIME=16, SYSTEMTIME=24, MSG=56 (including 8-byte wrapper).
- `point_survives_minor_gc` — root-installed `<point>` survives a `collect_minor()` cycle with its field intact.

Win32 headlines:
- **`get_cursor_pos_returns_screen_coords`** — **the Sprint 34 headline**. `GetCursorPos(pt)` writes real cursor x/y; Dylan reads them and the sum lands in a sensible `[−100k, 100k)` range. Run output: `[Sprint34 headline] GetCursorPos x+y = 44`.
- **`get_system_time_returns_current_year`** — `GetSystemTime(st); systemtime-year(st)` returns the current UTC year. Run output: `[Sprint34 headline] GetSystemTime year = 2026`.
- **`set_rect_populates_all_four_fields`** — `SetRect(r, 10, 20, 30, 40)` writes left/top/right/bottom; Dylan packs them as `left + top*10 + right*100 + bottom*1000 = 43210`. Run output: `[Sprint34 headline] SetRect packed sum = 43210`.
- `get_local_time_returns_sensible_month_and_day` — month ∈ [1,12], day ∈ [1,31].

**F. Verification.** 532 / 0 / 7 under `newgc-backend` default (512 → 532; +20 new tests across `winffi_structs.rs` integration suite and `structs.rs` inline unit tests). 5x sequential flake clean. `cargo clippy --workspace --all-targets -- -D warnings` clean.

**Headline acceptance:**
- `GetCursorPos x+y = 44` — actual desktop cursor coordinates rendered through Dylan.
- `GetSystemTime year = 2026` — Win32 wrote `2026` as a u16 at SYSTEMTIME offset 0; Dylan read it back.
- `SetRect packed sum = 43210` — all four `<rect>` fields populated by the Win32 API and read back by Dylan in correct positions.

**One existing fixture rename.** `tests/nod-tests/fixtures/point.dylan` defined a user-class `<point>` for the Sprint 12 distance-squared regression. With `<point>` now a seed struct, the fixture's class collided at lowering time (the `<point>` name resolves to the seed class, and a fresh `define class <point>` triggers `ClassRedefinitionNotSupported`). Renamed the fixture's class to `<user-point>` — purely a name change; no behavioural impact on the regression test.

**Out of scope (deferred — see DEFERRED.md):**

- `define c-struct` Dylan-side parser surface. Sprint 34 seeds six structs in Rust; the user surface for declaring new structs from Dylan source is a Sprint 35+ task.
- **Struct-by-value marshaling.** Win64 ABI rules for ≤8-byte structs (passed in register) and >8-byte structs (passed via hidden pointer) are real but every IDE-essential Win32 API uses pointer parameters (`LPMSG`, `LPRECT`, `LPPOINT`). Defer until a real use case demands it.
- **Nested struct field syntax.** MSG.pt is a POINT; Sprint 34 surfaces `msg-pt-x(m)` / `msg-pt-y(m)` as flat-offset accessors. Dotted-notation `msg.pt.x` access lands with the `define c-struct` parser.
- **Variable-length structs.** `BITMAPINFO`'s `bmiColors[1]` header-trick layout (and similar APIs) requires a different allocation model. Sprint 35+.
- **C → Dylan struct view.** Sprint 34 auto-coerces Dylan-struct → C-pointer one way only. A Win32 API that returns `LPRECT` and the Dylan caller wants to read its fields requires explicit `wrap-as-rect(ptr)` in Sprint 34 (deferred — no IDE-track API in the seed set returns a struct pointer that Dylan needs to read).
- **Per-bucket "is-c-struct" Wrapper flag.** Sprint 34 uses `is_subclass(class, <c-struct>)` for the auto-coerce decision; the CPL walk is 3 entries deep so the cost is negligible. If a future profile shows the test as hot, switching to a dedicated bit on the Wrapper (parallel to Sprint 22's bucket-state byte) is a one-line change.

### Sprint 35 — COM via `windows` crate: DXGI / D3D11 / D2D / DirectWrite infrastructure — landed

**Goal:** an offscreen D2D + DirectWrite text-rendering chain reachable from Dylan source. The Sprint 35 brief originally sketched a hand-rolled C++ shim DLL wrapping COM as plain C; we instead use the official Microsoft [`windows` crate](https://docs.rs/windows) as the COM-aware layer. The shim lives in Rust at `nod-runtime::com_shim`, uses the `windows` crate's typed interfaces for refcount-correct COM, and exposes ~30 `%`-primitive entries through the Sprint 20b primitive-call path. Dylan source builds a 256×256 BGRA8 texture, renders "hello, dylan" with DirectWrite + a red brush, reads back the pixel buffer through a CPU-mapped staging texture, and asserts text glyphs produced non-zero red pixels.

**Architectural shape.**

A. **COM handle registry.** A process-global `Mutex<HashMap<u64, ComObject>>` in `com_shim.rs` owns one typed `windows`-crate interface per Dylan-held handle. Cloning a `windows` COM type bumps refcount (`AddRef`); dropping calls `Release`. The registry's `register(obj) → u64` hands out monotonic counter handles which Dylan treats as opaque `<c-handle>` tokens. `release(handle)` removes the entry, which drops the typed wrapper, which calls `Release`. No manual AddRef/Release in our shims.

B. **Typed accessors per variant.** A `typed_accessor!($name, $variant, $ty)` macro generates `get_dxgi_factory`, `get_d3d11_device`, etc. — each takes a u64 handle, untags the fixnum tag bit, and returns `Option<TypedInterface>` cloned out of the registry. Cloning gives the caller an owned reference that survives `Drop` independently of the registry's entry.

C. **`windows` crate feature flags.** Sprint 35 enables `Win32_Foundation`, `Win32_System_Com`, `Win32_Graphics_Dxgi`, `Win32_Graphics_Dxgi_Common`, `Win32_Graphics_Direct3D`, `Win32_Graphics_Direct3D11`, `Win32_Graphics_Direct2D`, `Win32_Graphics_Direct2D_Common`, `Win32_Graphics_DirectWrite`, and `Foundation_Numerics` (the last for `Matrix3x2`). Build cost: one-time ~3-minute clean compile of the windows crate; incremental builds are sub-second. The `windows` crate itself adds ~300MB to `target/`.

D. **Float-marshaling deviation.** The brief sketches `<c-float>` / `<c-double>` Dylan args feeding float-aware trampoline variants. Sprint 35 instead routes the whole COM surface through **integer-encoded scalars** — color channels are 0..=255 Dylan integers (the shim divides by 255 to get f32), pixel coordinates are integer Dylan values (the shim casts to f32 in stride). This eliminates the trampoline restructure entirely: every shim signature is `extern "C-unwind" fn(u64, u64, …) -> u64`, lowered through the existing Sprint 28 mechanism without change. The `<c-float>` / `<c-double>` Dylan classes are still registered (Phase A acceptance item), and `CArgKind::Float32` / `CArgKind::Float64` exist in the enum and `from_c_type_name` mapping — sema accepts `define c-function` declarations using these types — but the trampoline path for them panics with a deliberate "Sprint 36+" message. Sprint 36+ wires the trampoline shape that actually marshals native floats when a real use case demands it.

E. **`%`-primitive routing, not `define c-function`.** Sprint 28's `define c-function` path goes through `LoadLibrary`/`GetProcAddress` to look up Win32 DLL exports. The COM shim functions live in our own process, not in a DLL, so the Sprint 28 path doesn't apply. Sprint 35 wires every shim as a `%`-primitive in `LOWER_PRIMITIVE_TABLE` (the same mechanism as `%struct-get-i32`, `%nod_make_table`, etc.) — codegen emits a `DirectCall { callee: "nod_*", … }`, the JIT layer binds the runtime symbol via `LLVMAddGlobalMapping`, and the call returns straight through the standard primitive ABI. Dylan source uses `%dxgi-create-factory()` style invocation directly.

F. **Fixnum-tag discipline at the FFI boundary.** Sprint 28 primitives that return raw u64 values (handles, counts, HRESULTs) are passed back as Dylan Words through the primitive-call result temp. The Word tag bit must be 0 (fixnum) — a raw odd integer like 1 would parse as a pointer-tagged Word and trigger a null-pointer-dereference in the formatter. Sprint 35 introduces `tag()` and `untag()` helpers in `com_shim.rs`: every shim entry untags its u64 args before use and wraps every successful return in `tag()`. The macro-generated typed accessors do the untagging once at the lookup boundary.

G. **String marshaling.** DirectWrite expects UTF-16. Sprint 35 shims that take string args (font family, locale, text content) accept Dylan `<byte-string>` Words and convert UTF-8 → UTF-16 on the stack via `utf16_from_dylan_byte_string` helpers. This reuses `winffi::read_dylan_byte_string` (made `pub(crate)` for the dependency). No new trampoline path required.

**Shim surface (32 entries).** All `extern "C-unwind" fn(u64, …) -> u64`.

*Lifecycle / diagnostics:*

- `nod_com_release(handle)` → drop the registry entry, refcount goes to zero.
- `nod_com_registry_len()` → diagnostic count of live entries.
- `nod_com_last_hresult()` / `nod_com_clear_last_hresult()` → thread-local last-error.

*DXGI (3):* `nod_dxgi_create_factory`, `nod_dxgi_device_from_d3d_device`, `nod_dxgi_create_surface_from_texture`.

*D3D11 (3):* `nod_d3d11_create_device` (tries hardware, falls back to WARP), `nod_d3d11_get_immediate_context`, `nod_d3d11_create_texture_2d` (USAGE_DEFAULT + BIND_RENDER_TARGET + BIND_SHADER_RESOURCE).

*D2D (10):* `nod_d2d_create_factory` (`ID2D1Factory1` for device interop), `nod_d2d_create_device`, `nod_d2d_create_device_context`, `nod_d2d_create_bitmap_for_target` (wraps a DXGI surface as an `ID2D1Bitmap1`), `nod_d2d_set_target`, `nod_d2d_begin_draw`, `nod_d2d_end_draw` (returns HRESULT), `nod_d2d_clear`, `nod_d2d_set_transform_identity`, `nod_d2d_create_solid_color_brush`.

*Drawing primitives (3):* `nod_d2d_draw_text_layout`, `nod_d2d_draw_rectangle`, `nod_d2d_fill_rectangle`.

*DirectWrite (4):* `nod_dwrite_create_factory`, `nod_dwrite_create_text_format`, `nod_dwrite_create_text_layout`, `nod_dwrite_get_layout_metrics` (returns packed width+height).

*Pixel readback (4):* `nod_d3d11_copy_to_staging_and_map` (creates a CPU-readable staging texture, copies GPU→staging, calls `Flush`, then `Map`s for read), `nod_d3d11_last_staging_handle` / `nod_d3d11_last_mapped_row_pitch` (companions returning the staging handle + row pitch from the last copy), `nod_d3d11_unmap`, plus `nod_count_non_zero_red` (scans BGRA8 pixels at byte+2 of each 4-byte pixel and counts non-zero).

**ID2D1RenderTarget cast trick.** The `windows` crate doesn't auto-deref `ID2D1DeviceContext` to its parent `ID2D1RenderTarget`. Many drawing methods (`BeginDraw`, `EndDraw`, `Clear`, `SetTransform`, `CreateSolidColorBrush`, `DrawRectangle`, `FillRectangle`, `DrawTextLayout`) live on the parent. A `dc_as_render_target(dc_handle) -> Option<ID2D1RenderTarget>` helper does an `IUnknown::cast()` on the device-context interface (which is the same underlying COM object) to obtain the typed render-target view. The cast is a vtable lookup, essentially free.

**Headline acceptance — `d2d_offscreen_renders_text_glyphs`.** Dylan source builds the entire device chain, clears the texture to opaque black (so non-zero-red pixels can only be from the red brush), draws "hello, dylan" with DirectWrite at (10, 50) in 24-DIP Segoe UI, maps the staging texture, and counts red-channel pixels.

Run output: **`717`** red pixels rendered (out of 65 536 total) — proof that text glyphs, not background fill, produced the red. The chain exercises every layer: DXGI factory, D3D11 device, D3D11 texture allocation, DXGI surface cast, D2D factory + device + device context + bitmap, DirectWrite factory + text format + text layout, solid-color brush, BeginDraw/EndDraw bracketing, CPU readback through a staging texture, and pixel-level pointer reading. **Sprint 35's headline goal — text glyphs rendered into pixels we read back — is met.**

**Refcount discipline acceptance.** `ten_handles_released_clears_registry` creates 10 DXGI factories from Dylan source, walks `%com-registry-len()` to confirm the count grows to 10, releases each one, walks the count back to 0. Asserts `before - after == 10` — and gets exactly 10. The `windows` crate's `Drop` discipline propagates through our registry: removing a `HashMap` entry drops the typed wrapper which calls `Release`. No leaks observed across 5 sequential test runs.

**Refcount registry empty-after-reset acceptance.** `refcount_registry_starts_empty_after_reset` calls `%com-registry-len()` immediately after `_reset_com_registry_for_tests()` and asserts 0. Proves the test-side reset path zeros the registry cleanly.

**EndDraw success acceptance.** `d2d_clear_and_end_draw_succeed` builds the chain to the device-context level, calls `BeginDraw → Clear(128,64,200,255) → EndDraw`, and asserts the HRESULT return is 0 (S_OK). This is the closest Sprint 35 comes to "float-marshaling proof" — the clear's 4 color channels are Dylan integer args, the shim converts each to f32, the call succeeds.

**Tests added (14).** `winffi_d2d.rs` ships 11 `#[serial]` tests covering the headline, every factory creation, the refcount discipline, and EndDraw success, plus 2 non-serial `<c-float>` / `<c-double>` class-registration sanity checks. The com_shim module's `#[cfg(test)] mod tests` adds 3 unit tests proving the registry's COM-Drop discipline at the Rust level. Test counts: baseline 532 → 546 (+14). All `#[serial]` because the COM handle registry is process-global.

**Verification.**
- `cargo test --workspace --no-fail-fast`: 546 passed, 0 failed, 7 ignored.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean (only warnings are in the external `newgc-core` crate, same as baseline).
- 5x sequential flake check: 546/0/7 every run.

**Out of scope for Sprint 35 (deferred — see DEFERRED.md):**

- **Float-marshaling trampoline shape.** `<c-float>` / `<c-double>` are registered for sema-acceptance only; no Sprint 35 shim takes a native float arg. Sprint 36+ ships per-shape trampolines for Win64 floats-in-XMM marshaling when a use case demands it (e.g. Direct2D animation curves with real-valued time arguments).
- **HWND-bound swap chains.** Sprint 35 ships offscreen-only rendering — no `CreateSwapChainForHwnd`, no `IDXGISwapChain1`, no `Present()`. Lights up in Sprint 37 once the IDE window exists.
- **Linear/radial gradient brushes, geometries, paths.** Solid-color brush only.
- **WIC bitmap interop.** Image loading from PNG/JPEG via the Windows Imaging Component is a follow-on.
- **D2D effect graphs and animations.** Useful for IDE polish; not Sprint 35.
- **Device-loss recovery.** When the GPU is reset (driver crash, monitor change), every D3D11 / D2D resource is invalidated. Production polish.
- **Compositional swap chains.** `CreateSwapChainForComposition` enables IDE panes embedded in non-Win32 hosts (XAML islands, etc.). Later.
- **Hand-rolled C++ shim DLL (the original Sprint 35 brief).** Superseded by the `windows`-crate approach. The C++ shim is no longer on the roadmap.

### Sprint 36 — IDE shell: the FFI journey's headline payoff — landed

**Goal:** assemble the nine FFI sprints into a working Win32 IDE shell. `cargo test --test ide_shell -- --ignored` opens a real titled window, renders "hello, dylan" via D2D + DirectWrite into the window's client area through an HWND-bound swap chain, handles WM_PAINT/WM_SIZE/WM_DESTROY via a Dylan-source WNDPROC closure, and exits cleanly when the user closes the window. The infrastructure pieces (WNDCLASSEXW registration, HWND swap-chain creation, WNDPROC dispatch, message-loop primitives) are verified separately by six `ide_shell_infra.rs` tests that use message-only / hidden windows so routine `cargo test` doesn't pop UI.

**Architectural shape.**

A. **Two new C-struct types: `<wndclassexw>` (80 B) and `<paintstruct>` (72 B).** Both go through the Sprint 34 `<c-struct>` family — `is_byte_payload = true`, parent `<c-struct>`, field offsets verified at Sprint-36-time against the `windows`-crate's `std::mem::size_of::<…>()` (one of the new unit tests). The stdlib gets accessor pairs for the load-bearing fields: `wndclassexw-cbSize`, `lpfnWndProc`, `lpszClassName`, `hInstance`, `hIconSm`; `paintstruct-hdc`, `fErase`, and the nested `rcPaint` fields surfaced flat as `paintstruct-rc-{left,top,right,bottom}`.

B. **HWND-bound swap chain shims (5 new entries in `com_shim.rs`).** A new `IDXGISwapChain1` variant joins the `ComObject` enum. The Sprint 36 shims:
- `nod_dxgi_factory_from_d3d_device(d3d)` — walks `D3D11Device → IDXGIDevice → IDXGIAdapter → IDXGIFactory2`, since `CreateSwapChainForHwnd` requires the factory associated with the device's adapter (creating a fresh factory via `CreateDXGIFactory2` produces a swap chain that silently no-ops on Present).
- `nod_dxgi_create_swap_chain_for_hwnd(factory, device, hwnd, w, h)` — `DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`, two buffers, BGRA8, `DXGI_ALPHA_MODE_IGNORE`.
- `nod_d2d_create_bitmap_from_swap_chain(dc, sc)` — fetches the back buffer surface and wraps it as a D2D bitmap. Same property shape as Sprint 35's offscreen path but `DXGI_ALPHA_MODE_IGNORE` to match the swap chain.
- `nod_dxgi_swap_chain_present(sc)` — VSync (`SyncInterval = 1`).
- `nod_dxgi_swap_chain_resize_buffers(sc, w, h)` — for WM_SIZE; caller must release the previous bitmap first (DXGI fails `ResizeBuffers` otherwise).

C. **Window-class + window-creation helpers (also Rust-side).** Building a `WNDCLASSEXW` from pure Dylan would require pinning a wide-string buffer with process-lifetime semantics — Sprint 30's `<c-wide-string>` is heap-allocated. Instead `nod_register_window_class(wndproc-ptr, class-name)` does the work in Rust: leaks a fresh `Vec<u16>` into a process-global `HashMap` keyed by class name (idempotent on the name), transmutes the trampoline pointer to the `WNDPROC` newtype shape (`HWND, UINT, WPARAM, LPARAM → LRESULT` — all four newtypes are `repr(transparent)` over their integer types on Win64), and calls `RegisterClassExW`. Plus two test-shaped helpers: `nod_create_message_only_window(atom)` for the message-pump tests (passes `HWND_MESSAGE` as parent — never displays) and `nod_create_hidden_window(atom)` for the swap-chain test (creates an `WS_OVERLAPPEDWINDOW` but never calls `ShowWindow`). DXGI rejects message-only windows as swap-chain targets, hence the two variants.

D. **Message-pump primitives.** `nod_post_message`, `nod_pump_one_message` (PeekMessage / TranslateMessage / DispatchMessage loop, drains up to 32 messages, returns the count dispatched), `nod_destroy_window`, plus a critical `nod_def_window_proc` shim: a Dylan WNDPROC closure that returns 0 fails `WM_NCCREATE` and thereby fails `CreateWindowExW`. The infrastructure tests' WNDPROCs delegate to `%def-window-proc` for unhandled messages.

**The interactive IDE shell test (`tests/ide_shell.rs`).** A single `#[ignore]`-gated test. Dylan source builds the D2D device chain once, registers a window class with a WNDPROC closure that handles WM_PAINT (lazy-creates a D2D bitmap on first paint, clears white, draws "hello, dylan" in black at (50, 50), Present), WM_DESTROY (calls `PostQuitMessage(0)`), and delegates everything else to `DefWindowProcW`. Then it `CreateWindowExW`s a 800×600 `WS_OVERLAPPEDWINDOW`, calls `ShowWindow` + `UpdateWindow`, and runs the canonical Win32 `while (GetMessage > 0) { TranslateMessage; DispatchMessage }` pump. Exits when `GetMessage` returns 0 (`WM_QUIT`). The test asserts `exit-code = 0`.

This is the FFI journey's headline payoff: nine sprints (27 → 36) of incremental machinery compose into a working IDE shell whose source-level shape matches what a Win32 SDK sample would look like in C. The Dylan source body is ~50 lines including comments.

**The non-interactive infrastructure tests (`tests/ide_shell_infra.rs`).** Six `#[serial]` tests cover the pieces in isolation:

1. `wndclassexw_struct_has_correct_size` — 88 bytes (wrapper + 80-byte payload).
2. `paintstruct_struct_has_correct_size` — 80 bytes (wrapper + 72-byte payload).
3. `register_window_class_succeeds_with_dylan_wndproc` — register a class with a trivial Dylan WNDPROC; assert the atom is non-zero.
4. `hwnd_swap_chain_creation_with_hidden_window` — create a hidden `WS_OVERLAPPEDWINDOW`, build the full GPU device chain, bind a swap chain. Assert the swap chain handle is non-zero.
5. `message_pump_processes_posted_message` — PostMessage WM_USER three times; the WNDPROC closure increments a captured counter; the pump dispatches; we read the counter back.
6. `wndproc_closure_receives_correct_hwnd` — the WNDPROC captures its `hwnd` argument in a cell; we compare the captured value against the HWND `CreateWindow` returned. They match.

**Tests added (7).** 6 in `ide_shell_infra.rs` + 1 ignored in `ide_shell.rs`. Plus 4 new unit tests in `nod-runtime::structs` (the two struct-size checks, the two field-offset checks). Test counts: baseline 546 → 557 (+11). All `#[serial]` because Win32's class registry and the COM handle registry are process-global. The `ide_shell.rs` test is `#[ignore]`-gated; routine `cargo test` does NOT pop a window.

**Verification.**
- `cargo build --workspace`: clean.
- `cargo test --workspace --no-fail-fast`: 557 passed, 0 failed, 8 ignored.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- 5x sequential flake check: 557/0/8 every run after removing `_reset_callbacks_for_tests()` from `ide_shell_infra.rs`'s setup. Resetting that registry orphans the WNDPROC pointers Win32 holds in registered classes — `DispatchMessage` then hits a stale slot and `debug_assert!` aborts. The fix is to let the callback pool grow naturally; 32 slots is comfortably above what the test binary needs.

**Out of scope for Sprint 36 (deferred — see DEFERRED.md):**

- **Native float-marshaling trampolines.** Still on the Sprint 37+ list — the IDE shell uses integer-encoded float arguments (color channels 0..=255, pixel coordinates as ints). Sub-pixel positioning waits for the float marshaling sprint.
- **Multi-window support, menus, toolbars.** Sprint 36 is one window with no chrome; the COM handle registry is process-global so multi-window is mechanical but untested.
- **WIC bitmap interop for icons / images.** Same status as Sprint 35.
- **DPI awareness.** No `SetProcessDpiAwareness` call; the window inherits the process's DPI mode.
- **Window resizing polish.** The shell handles WM_SIZE wiring at the API level (`nod_dxgi_swap_chain_resize_buffers` exists) but the interactive demo doesn't subscribe to it — the back buffer stays at 800×600 even if the user resizes.
- **Compositional swap chains** (`CreateSwapChainForComposition`), **device-loss recovery**, **gradient brushes / geometries / paths**, **D2D effect graphs and animations** — same Sprint 35 carry-overs.

### Sprint 37 — JIT object-code cache — landed

**Goal:** the second `eval_expr_to_string("…")` of identical Dylan source returns at least 10× faster than the first by short-circuiting the codegen + MCJIT compile + binding-registrations pipeline.

**Headline measurement.** Sample fixture is ~160 helper functions invoked from an entry expression (`tests/nod-tests/fixtures/jit_cache_sample{,_items}.dylan`). Cold compile ≈ 73 ms; cached re-eval ≈ 140 µs. Observed ratio: **~500×** in isolation, comfortably ≥10× under heavy parallel `cargo test --workspace` load.

**Architectural shape.**

A. **Determinism audit (Phase A).** Identical Dylan source must lower to byte-identical DFM IR for the cache key to hit. The audit found two real nondeterminism sources baked into the IR as constants:

- `block_id` was minted from a process-global `AtomicU64` counter via `nod_runtime::allocate_block_id()`, then baked into the DFM as a `WordBits` constant. Fixed by deriving the id deterministically from `(parent_name, thunk_seq)` via a `DefaultHasher` (SipHash 1-3 — stable across runs in Rust stdlib), masked to fit `Word::from_fixnum`'s 63-bit domain and bit-62-set to keep it non-zero. The runtime's `register_block_fns` already replaces same-id entries, so collisions across modules (vanishingly rare given the hash space) are tolerated.
- `dispatch::allocate_cache_slot`'s returned pointer is baked into LLVM IR as an i64 immediate. Pointers are process-volatile but the *DFM* IR doesn't see them — only LLVM IR does — so this only matters for cross-process disk replay, which is out of scope for Sprint 37 (see "Storage layout" below).

Already-deterministic and re-confirmed: anon-method counter resets per `lift_anonymous_methods` call; block-form captures sort by name; `c_function_specs` builds in `m.items` order. The `dfm_ir_is_deterministic_across_two_eval_calls` regression test enforces this from now on.

B. **Cache key (Phase B–C).** 256-bit composite. Inputs: the **wrapped Dylan source string** that `eval_expr_to_string` / `eval_expr_with_items_to_string` constructs from caller inputs (already deterministic by construction — `format!()` produces byte-identical output for byte-identical inputs); the `nod-llvm` `CARGO_PKG_VERSION`; an `NOD_RUNTIME_ABI_VERSION` constant (bump on any `extern "C-unwind"` runtime ABI change); the LLVM major version; the target triple; the MCJIT opt level. The 256 bits are four `DefaultHasher` digests with distinct domain-separation seeds — well above the collision budget for any reasonable on-disk cache size and zero new transitive crate deps. `nod_dfm::format_for_cache_key` exists as the alternate cache-key surface for callers that want to key on post-lowering DFM IR (Sprint 38 will use it for separate-compilation per-function caching); today's eval-shaped hot path uses the source-string key because the pre-lowering string is 100× cheaper to hash than the post-lowering DFM text and is provably equivalent for identical inputs (the lowerer is deterministic post-Phase-A audit, so identical source → identical DFM → identical IR).

C. **Storage (Phase D–E).** Two layers, complementary because of an LLVM-C API constraint:

- **In-process JIT-output cache.** `LazyLock<Mutex<HashMap<CacheKey, ReplayFn>>>` in `nod-llvm::cache`. On hit, the entire pipeline (parse → expand → lower → codegen → MCJIT → registrations) is skipped — the cached `<eval-entry>` function pointer is called directly and `call_and_format` runs against the cached return-type tag. The leaked `LLVM Context` + `Jit` engine pair keep the JIT'd code alive forever.
- **On-disk bitcode + sidecar.** Every cold compile writes `<key>.bc` + `<key>.json` to the cache dir. The sidecar JSON tracks `created_at_unix_ms`, `accessed_at_unix_ms`, `size_bytes`, plus the ABI/LLVM/target tuple for defense-in-depth. LRU eviction sorts by `accessed_at` and trims to 500 MB (or whatever `NOD_JIT_CACHE_MAX_BYTES` says).

Cache dir resolution order: `NOD_JIT_CACHE_DIR` → `CARGO_TARGET_DIR/nod-jit-cache` → ancestor-`target/`/nod-jit-cache → `%LOCALAPPDATA%/NewOpenDylan/jit-cache`.

D. **The LLVM-C API constraint and deviation from the original brief.** The sprint plan called for installing an `llvm::ObjectCache` on the MCJIT instance. The LLVM-C API exposed by `llvm-sys` 221 and `inkwell` 0.9 **does not** bind `MCJIT::setObjectCache` — that surface is C++-only. Adding a C++ shim DLL was out of sprint scope (and would have required `build.rs` + `cc` machinery this workspace doesn't currently host). The pragmatic landing is described above: in-process replay delivers the 10× target; on-disk bitcode persists the post-codegen IR for Sprint 38's cross-process replay (AOT will fix up the baked-in runtime pointers at load time). The on-disk bitcode is observable today via `jit_cache_stats()` but cross-process replay is not yet wired.

E. **Statistics + introspection.** `nod_llvm::read_stats(dir) -> JitCacheStats { hits, misses, bytes_on_disk, entries }`. Counters are process-global. Test helpers: `in_process_clear`, `reset_stats`, `clear_cache_dir`, `evict_to(dir, max_bytes)`. The public Dylan-side `%`-primitives mentioned in the sprint brief (`%jit-cache-stats()` etc.) weren't wired: tests use the Rust APIs directly. Surfacing them as `%`-primitives is mechanical follow-up.

F. **Tests added (8).** `tests/nod-tests/tests/jit_cache.rs` + the existing `nod-llvm::cache` unit tests (5):
1. `cache_miss_then_hit_is_at_least_10x_faster` — headline.
2. `cache_invalidates_on_source_change` — different source = miss; same source = hit.
3. `cache_invalidates_on_runtime_abi_bump` — direct `cache_key` ABI-version probe.
4. `cache_stats_track_hits_and_misses` — delta accounting.
5. `lru_evicts_oldest_when_over_max` — populate 5 entries → evict to 1-byte cap → 0 entries.
6. `cache_corruption_recovers_via_recompile` — overwrite a `.bc` with garbage → next eval recompiles.
7. `dfm_ir_is_deterministic_across_two_eval_calls` — Phase A regression guard.
8. `cache_directory_respects_env_override` — `NOD_JIT_CACHE_DIR` honoured.

Test counts: baseline 556 → 569 (+13: 5 cache.rs unit tests + 8 jit_cache.rs integration tests).

**Verification.**
- `cargo build --workspace`: clean.
- `cargo test --workspace --no-fail-fast`: 569 passed, 0 failed, 9 ignored.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean. (Sprint 37 also fixed three pre-existing `missing_safety_doc` errors on Sprint 36b's arity-10/11/12 WinFFI trampolines.)
- 5x sequential flake check: 569/0/9 every run.

**Out of scope for Sprint 37 (deferred — see DEFERRED.md):**

- **True object-code-on-disk caching.** Requires either a C++ shim that wires MCJIT's `setObjectCache` or migration to ORC v2 LLJIT (which DOES expose `LLVMOrcLLJITAddObjectFile` through the C API). Sprint 38 will pick one based on the AOT design.
- **Cross-process bitcode replay.** Baked-in runtime addresses (cache slots, generic-fn pointers, runtime shim addresses) are process-volatile; cross-process replay needs a fix-up pass at module-load time. The on-disk bitcode infrastructure landed this sprint is the foundation.
- **Function-level / cross-module dependency tracking.** Sprint 37 keys on the whole DFM module; per-function caching is a Sprint 38+ refinement once separate compilation lands.
- **Hot-reload invalidation when source files change on disk.** The cache today is content-addressed, so source change = different key = miss; explicit file-watching for IDE workflows is post-Sprint-38.
- **Compressed cache files.** Bitcode compresses well; not done this sprint.
- **`%jit-cache-stats()` / `%jit-cache-clear()` Dylan-side primitives.** Rust APIs exist; surfacing them as primitives is mechanical follow-up.

### Sprint 38 — cross-process bitcode replay (partial — infrastructure landed; codegen conversion deferred to 38b)

**Goal (as briefed):** two subprocess invocations of identical Dylan source — process 1 cold-compiles and writes bitcode; process 2 loads bitcode and produces the same answer ≥10× faster.

**Outcome:** the **load-path infrastructure** landed in full — symbol-naming scheme, manifest sidecar JSON, JIT-link entry point that resolves named externs against current-process addresses, and a regression suite locking down the load path's correctness. The **codegen-side conversion** (replacing every i64-immediate runtime address bake with a `ptrtoint @global to i64` reference) is **deferred to Sprint 38b**. Without the codegen conversion the on-disk bitcode still bakes process-local addresses, so a fresh process loading that bitcode would see stale pointers. The right call given the realistic agent-session budget was to land the loader correctly + the manifest format the codegen sprint will produce, rather than half-convert the codegen sites and risk silent corruption of cache-hit paths.

**Phase A — inventory (delivered).** Audit of every place in the codebase that bakes a runtime address into LLVM IR. The bake-site map:

| # | Site | What gets baked | Sprint 38(b) replacement |
|---|---|---|---|
| 1 | `lower.rs::lower_call` (c-function path) | `info.entry_ptr` (`*const ApiStubEntry`) → `ConstValue::WordBits` | `nod_stub__<key8>__<slot>` external global; manifest `RelocKind::StubEntry { dll, symbol, signature_bytes }` |
| 2 | `lower.rs::emit_class_ref` | `class_metadata_ptr(id) \| 1` (tagged) | `nod_class_md__<key8>__<class_id>` external global; manifest `RelocKind::ClassMetadata` |
| 3 | `lower.rs::emit_class_metadata_ptr_const` | `class_metadata_ptr(id)` (raw) | same global as #2; codegen ORs the tag bit downstream |
| 4 | `lower.rs::emit_string_literal` | `intern_string_literal(s).raw()` | `nod_strlit__<key8>__<idx>`; `RelocKind::StringLiteral { text }` |
| 5 | `lower.rs::emit_symbol_literal` | `intern_symbol_literal(name).raw()` | `nod_symlit__<key8>__<idx>`; `RelocKind::SymbolLiteral { name }` |
| 6 | `codegen.rs::emit_const` `ConstValue::String` | same as #4 | same as #4 |
| 7 | `codegen.rs::retag_bool`, `untag_bool_to_i1`, `emit_const` (Bool/Integer→Boolean cases), `emit_word_class_id`, `emit_wrapper_class_check` | `imm.true_/false_/nil.raw()`, plus the untagged-wrapper variant used as fault-free fallback in branchless class-id reads | `nod_imm_true__<key8>`, `nod_imm_false__<key8>`, `nod_imm_nil__<key8>`, `nod_imm_false_wrapper__<key8>`; `RelocKind::ImmTrue/ImmFalse/ImmNil/ImmFalseWrapper` |
| 8 | `codegen.rs::emit_dispatch` | `generic_ptr_raw`, `cache_slot_raw` (+ per-field offsets baked off them) | `nod_generic__<key8>__<name>`, `nod_cache_slot__<key8>__<site_id>`; manifest `RelocKind::Generic`, `RelocKind::CacheSlot`. Field offsets stay as static IR constants — they're struct layout, not addresses |

Already process-deterministic (audited, no Sprint 38 action needed): `block_id` (Sprint 37 fix — derived from `hash(parent_name, thunk_seq)`); fixnum tag encodings; `ConstValue::Integer(n)` literals; class id field offsets within `ClassMetadata`; cache-slot field offsets within `CacheSlot`; `WordBits(0)` sentinels.

**Phase B — symbol naming + manifest types (delivered).** New `nod_llvm::symbols` module:

- `key_prefix(key)` — 16-character hex prefix derived from the Sprint 37 cache key, used as the per-module namespace component.
- Eleven naming helpers: `stub_symbol`, `cache_slot_symbol`, `generic_symbol`, `class_md_symbol`, `imm_true_symbol`, `imm_false_symbol`, `imm_nil_symbol`, `imm_false_wrapper_symbol`, `strlit_symbol`, `symlit_symbol`.
- `RelocKind` enum: `StubEntry { dll, symbol, signature_bytes }`, `CacheSlot { site_id }`, `Generic { name }`, `ClassMetadata { class_id }`, `ImmTrue`, `ImmFalse`, `ImmNil`, `ImmFalseWrapper`, `StringLiteral { text }`, `SymbolLiteral { name }`.
- `ModuleManifest { manifest_version, key_prefix, entries: Vec<RelocEntry> }` with `to_json`/`parse` round-trip — hand-rolled JSON encoder/parser to avoid pulling `serde_json` into the dep tree (Sprint 37 sidecar precedent stands).

Six unit tests in `nod-llvm`: `key_prefix_is_16_chars`, `sanitize_keeps_alphanumerics_and_underscore`, `stable_symbol_naming_is_collision_resistant` (~3000 distinct symbols, no collisions), `manifest_round_trips` (covering every `RelocKind` variant including non-ASCII string-literal text with escaped JSON characters), `manifest_version_mismatch_rejected`, `manifest_parse_rejects_garbage`.

**Phase C — codegen conversion (deferred).** The codegen sites enumerated in Phase A still bake process-local addresses as `i64` immediates today. Converting them needs:

1. A `RelocationCollector` (RefCell of `ModuleManifest`) threaded through the per-module codegen.
2. New `ConstValue` variants for richer semantic intent (`ClassRefTagged`, `ClassMetaPtr`, `StringLit`, `SymbolLit`, `StubEntry { spec_idx }`), so the lower-side knows what *kind* of address it's baking — `WordBits(u64)` today is process-mixed.
3. A `emit_reloc_addr(kind)` helper that emits `ptrtoint @nod_reloc_<symbol> to i64` instead of `i64 const_int`, and appends a `RelocEntry` to the per-module manifest.
4. Care around the *minimum-viable* conversion — class metadata, immediates, string/symbol literals are touched by almost every test; stub entries are touched by FFI tests; cache slots are touched by dispatch tests. Each site is mechanical individually but the chain across crates (DFM IR shape change → format.rs → lower.rs callers → codegen.rs lowerer → JIT-link consumer) is substantial.

Sprint 38b will land Phase C as a focused codegen refactor with the existing 584-test suite as the regression net. Sprint 38's symbol-naming + manifest infrastructure is the API contract Phase C codegens against.

**Phase D — manifest sidecar I/O (delivered).** Cache layout extended with `<key>.manifest.json`:

- `write_cache_entry_with_manifest(dir, key, bitcode, manifest)` — adds manifest sidecar to the existing Sprint 37 bitcode + `<key>.json` sidecar pair.
- `read_cache_entry_with_manifest(dir, key) -> Option<(bytes, SidecarMeta, ModuleManifest)>` — returns `None` on any of (missing manifest, manifest version mismatch, manifest `key_prefix` ≠ key's actual prefix). Caller treats `None` as cache miss → fresh compile.
- LRU eviction (`evict_to`) now also removes the sibling `.manifest.json` when it removes a `.bc` + `.json` pair — no orphan manifests on disk after LRU cycles.

Sample manifest from a hypothetical FFI-touching module:

```json
{
  "manifest_version": 1,
  "key_prefix": "7856341278becdef",
  "entries": [
    {"symbol": "nod_imm_true__7856341278becdef", "kind": "imm_true"},
    {"symbol": "nod_imm_false__7856341278becdef", "kind": "imm_false"},
    {"symbol": "nod_imm_nil__7856341278becdef", "kind": "imm_nil"},
    {"symbol": "nod_class_md__7856341278becdef__1", "kind": "class_md", "class_id": 1},
    {"symbol": "nod_class_md__7856341278becdef__7", "kind": "class_md", "class_id": 7},
    {"symbol": "nod_strlit__7856341278becdef__0", "kind": "strlit", "text": "hello"},
    {"symbol": "nod_cache_slot__7856341278becdef__42", "kind": "cache_slot", "site_id": 42},
    {"symbol": "nod_generic__7856341278becdef___", "kind": "generic", "name": "+"},
    {"symbol": "nod_stub__7856341278becdef__0", "kind": "stub", "dll": "kernel32.dll", "sym": "Beep", "sig": "020707000000000000000005"}
  ]
}
```

**Phase E — JIT-link infrastructure (delivered).** New `Jit::add_module_from_bitcode(ctx, bitcode, module_name, manifest)`:

1. Parses bitcode into a fresh inkwell `Module` (`MemoryBuffer::create_from_memory_range_copy` + `Module::parse_bitcode_from_buffer`).
2. `module.verify()` rejects malformed payloads.
3. For each manifest entry, looks up the named external global in the module. Computes the current-process address via `resolve_reloc_kind`:
   - `ImmTrue/False/Nil` → `nod_runtime::literal_pool_immediates().{true_,false_,nil}.raw()`.
   - `ImmFalseWrapper` → `false_.raw() & !1`.
   - `ClassMetadata` → `class_metadata_ptr(ClassId(id)) as u64`.
   - `StringLiteral` → `intern_string_literal(text).raw()`.
   - `SymbolLiteral` → `intern_symbol_literal(name).raw()`.
   - `CacheSlot` → `allocate_cache_slot(site_id) as u64`.
   - `Generic` → `get_or_create_generic(name) as *const _ as u64`.
   - `StubEntry` → reconstructs `ApiCallSignature` from manifest bytes, calls `allocate_stub_table` for a fresh entry, then `resolve_into_entry(dll, symbol)` to populate `fn_ptr`. Failure (DLL not present / symbol unresolved) surfaces as `JitError::Create`.
4. Installs the module in a fresh MCJIT engine (`LLVMCreateMCJITCompilerForModule` with the same `MCJMM` / opt-level / options the cold path uses).
5. After engine creation, walks each captured `(global, addr)` and calls `LLVMAddGlobalMapping` so MCJIT resolves the named external to the current-process address.
6. Also registers every standard extern shim (`nod_make`, `nod_format_out`, dispatch shims, range/sov/table/closure/cell/winffi/com/wndproc — ~90 entries total via `standard_extern_addresses()`) and cross-module method body externs (Sprint 20b's `find_method_body_ptr` resolution).
7. MCJIT finalises lazily on the first `LLVMGetFunctionAddress` call (same shape as cold path's `add_module`).

**Phase F-G — tests (partial).** New `tests/nod-tests/tests/jit_cache_xprocess.rs` ships 9 `#[serial]` tests:

1. `manifest_round_trips_through_disk` — write/read full manifest with every `RelocKind` variant.
2. `read_returns_none_when_manifest_missing` — bitcode + sidecar present, manifest absent → cache miss.
3. `read_returns_none_when_manifest_version_mismatches` — manifest with wrong `manifest_version` → cache miss.
4. `empty_manifest_round_trips` — zero-entry manifest valid (means "no relocations").
5. `lru_eviction_cleans_manifest_sidecars` — LRU eviction removes all three of `.bc` + `.json` + `.manifest.json`.
6. `manifest_carries_stub_entry_signature_bytes` — raw `ApiCallSignature` bytes round-trip through manifest JSON's hex-encoded `sig` field.
7. `manifest_version_constant_is_one` — Sprint 38 ships at v1.
8. `runtime_abi_version_is_two_after_sprint38_bump` — `NOD_RUNTIME_ABI_VERSION` bumped from 1 to 2.
9. **`add_module_from_bitcode_round_trips_a_trivial_module`** — **end-to-end smoke test of the JIT-link path**. Drives `eval_expr_to_string("1 + 2 + 3")` to cold-compile + write bitcode to disk; reads the bitcode back; creates a *fresh* `Jit` + `Context`; loads via `add_module_from_bitcode` with the empty manifest the cold path produces today; resolves `<eval-entry>` from the replayed JIT; invokes the function pointer; confirms the tagged-Word result is `6`. The test runs in-process (the cold-compiled bitcode's baked addresses are still valid because the static-area objects haven't moved) — what it proves is that the bitcode-parse + symbol-binding + finalize sequence the JIT-link path uses is sound. Once Sprint 38b lands the codegen-side conversion, this *same* code path becomes cross-process replay.

The headline `cross_process_cache_hit_is_at_least_10x_faster` subprocess-spawn test is **deferred to Sprint 38b** because measuring a 10× ratio requires the codegen-side conversion to be in place. Without it, the cache-hit subprocess would link a module whose IR bakes process-1's addresses, and crash on first use.

**Phase H — verification.**

- `cargo build --workspace`: clean.
- `cargo test --workspace --no-fail-fast`: **584 passed, 0 failed, 9 ignored** (up from 569 by +15: 6 unit tests in `nod-llvm::symbols`, 9 integration tests in `jit_cache_xprocess.rs`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- 5x sequential flake check: 584/0/9 every run.

**Phase I — docs.** SPRINTS.md retrospective (this section). DEFERRED.md gets new Sprint 38b carry-overs (codegen-side conversion, subprocess-spawn headline test, AOT-mode emission groundwork). HOW_IDE_SHELL_WORKS.md Part 8 stays accurate at the in-process-cache description until Sprint 38b actually ships cross-process behavior.

**Deviations / judgment calls.**

- **The brief's "Phase A inventory → Phase C codegen surgery → Phase F headline test" pipeline doesn't fit a single agent-session budget.** A faithful conversion of every bake site is ~500-1000 lines across nod-dfm, nod-sema, nod-llvm with subtle correctness traps (every missed site silently corrupts cache-hit paths). The pragmatic landing: ship the infrastructure (symbols + manifest + loader) with regression tests locking down the load path, defer the codegen refactor to Sprint 38b. Same trade-off Sprint 37 made when LLVM-C's missing `setObjectCache` forced the in-process-cache landing.
- **The brief considered a fall-back from subprocess-spawn tests to "fork the cache-loader code path manually."** The `add_module_from_bitcode_round_trips_a_trivial_module` test takes exactly this approach — fresh `Jit` + `Context`, replay bitcode through the new entry point, invoke the result. It's the in-process-shape proof that the loader works; Sprint 38b will turn it into the cross-process variant.
- **`NOD_RUNTIME_ABI_VERSION` bumped 1 → 2** as a forward-compatibility hedge: when Sprint 38b lands the codegen conversion, Sprint 37-era cache entries (i64-baked addresses) become *actively wrong*, not just stale. The ABI bump invalidates them at the key layer so a stale `.bc` from before the bump can't accidentally match a fresh codegen output. The downside: existing in-flight `.bc` files on developer machines miss; first compile in a Sprint 38 workspace is a cold rebuild. Acceptable.

**Out of scope (deferred — see DEFERRED.md):**

- **Codegen-side conversion of every bake site to named-global references.** Sprint 38b (immediates), 38c (class metadata + string/symbol literals), 38d (stub entries), 38e (cache slots + generic pointers).
- **Subprocess-spawn cross-process headline test.** Waits until 38e — needs every category converted.
- **AOT mode emitting `.exe`.** Sprint 39 — the relocation machinery built here is the foundation.
- **Function-level / partial-invalidation caching.** Whole-module keying stands.
- **Hot-reload-on-source-file-change.** IDE polish.
- **Compressed bitcode files.** Sprint 38 ships uncompressed; bitcode compresses well, an LRU-cap follow-up.

### Sprint 38b — immediates bake-site conversion (true / false / nil / false-wrapper)

**Goal.** First focused codegen-conversion sub-sprint following Sprint 38's infrastructure landing. The seven codegen sites that bake immediate Word values (`#t`, `#f`, `nil`, the untagged `#f` wrapper used as a fault-free fallback in branchless class-id reads) now emit external-global symbol references (`@nod_imm_true__<key>` etc.) instead of `i64` literal constants. The cold-compile path registers each external against a process-stable slot via `LLVMAddGlobalMapping`; the cross-process replay path resolves them through the manifest sidecar against fresh in-process slot addresses.

**Phase A — codegen change pattern.** Introduced `ModuleCodegenCtx` (`CacheKey` + `RefCell<ModuleManifest>`) threaded through `codegen_module`/`emit_function`/`Emit`. Added four `Emit::load_imm_*` helpers (true, false, nil, false_wrapper) that call `get_or_add_imm_global` to declare-or-reuse the named external, emit a `load i64, ptr @symbol`, and push one `RelocKind::Imm*` row to the manifest on first emit. Converted `retag_bool` / `untag_bool_to_i1` from free functions taking `&Builder` to `Emit` methods so they have access to the module + manifest; updated all 21 call sites (PrimOp comparisons, BoolAnd/Or/Not, `Terminator::If`, `ClassCheck::Integer`'s retag, `emit_wrapper_class_check`'s final retag). Converted `emit_const` to return `Result<BasicValueEnum, CodegenError>` so the Bool/Integer-as-Bool/Unit arms can emit loads. Replaced the two `false_wrapper` baked-constant sites in `emit_word_class_id` and `emit_wrapper_class_check` with `self.load_imm_false_wrapper()` calls.

**Phase B — manifest building.** `CodegenOutput` now carries a `manifest: ModuleManifest` field, populated as a by-product of `get_or_add_imm_global` calls. Added `codegen_module_with_key(ctx, fns, name, key)` as the canonical entry point; the existing `codegen_module(ctx, fns, name)` keeps its signature by synthesising a deterministic key from `cache_key_for_dfm(module_name + DFM-text)` for the four non-cache-aware call sites (stdlib load, dump-llvm, bench, run_function_to_i64). Cache-aware path (`eval_wrapped_source`) calls `codegen_module_with_key(cache_key)` directly.

**Phase C — JIT-link integration.** `Jit::add_module` captures `(GlobalValue, current-process-address)` pairs from the manifest before MCJIT engine creation, then registers each via `LLVMAddGlobalMapping` after the engine is created — symmetric with `add_module_from_bitcode`'s warm-replay binding loop. `nod-sema::eval_wrapped_source` switched from `write_cache_entry` to `write_cache_entry_with_manifest` so the manifest sidecar reaches disk alongside the bitcode.

**Phase D — process-stable immediate slots.** Discovered during first test pass: `resolve_reloc_kind` was returning the *Word bits* for `RelocKind::Imm*` (a holdover from when the manifest had no actual loader consumer). `LLVMAddGlobalMapping(@nod_imm_true, addr)` makes `&@nod_imm_true == addr`, so a `load i64, ptr @nod_imm_true` reads the i64 *at* `addr`. The Word bits are not a valid address to load from. Added `imm_*_slot_addr()` functions in `nod-runtime` exposing the stable address of a `Box::leak`ed `u64` initialised with the immediate's Word bits; `resolve_reloc_kind` now returns these slot addresses. The slots are initialised once on first read and live for the process lifetime.

**Phase E — verification.** `cargo build --workspace` clean. `cargo test --workspace`: **584 → 586** passing (+ two new tests: `sprint38b_immediate_globals_round_trip_through_cached_bitcode` and `sprint38b_bitcode_round_trip_reads_named_global`), 0 failed, 9 ignored (unchanged baseline). `cargo clippy --workspace --all-targets -- -D warnings` clean (the only warnings come from the external `newgc-core` crate, unchanged). 5x sequential flake check — 5/5 runs at 586/0/9.

**Empirical IR proof.** A cold-compile of `if (#t) 1 else 2 end` produces IR containing:

```text
@nod_imm_true__366bd96cb9a2992c = external externally_initialized global i64
@nod_imm_false__366bd96cb9a2992c = external externally_initialized global i64
  %imm.true.load = load i64, ptr @nod_imm_true__366bd96cb9a2992c, align 4
  %imm.false.load = load i64, ptr @nod_imm_false__366bd96cb9a2992c, align 4
```

— exactly the named-global shape Sprint 38 specified, with the per-module 16-char key prefix isolating distinct modules from each other. No `i64 <baked-pointer>` constants remain at the four immediate bake-site categories.

**Out of scope (Sprint 38c / 38d / 38e):**

- Class metadata pointers (Sprint 38c).
- `<byte-string>` and `<symbol>` literals (Sprint 38c).
- Win32 FFI stub entries (Sprint 38d).
- Inline-cache slots + generic-function pointers (Sprint 38e).
- The cross-process subprocess-spawn headline test (Sprint 38e — depends on all categories converted).

### Sprint 38c — static-area pointer bake-site conversion (class metadata + string + symbol literals)

**Goal.** Second focused codegen-conversion sub-sprint, mirroring Sprint 38b's pattern for three more bake-site categories: class-metadata pointers (raw and tagged), interned `<byte-string>` literal Words, and interned `<symbol>` literal Words. Every codegen site that previously baked these as `ConstValue::WordBits(<runtime-address>)` now emits a `load i64, ptr @nod_<kind>__<key>__<id>` through a per-module external global; the JIT-link path maps each global to a process-stable `Box::leak`'d `&'static u64` slot whose contents hold the per-process Word/pointer bits.

**Phase A — slot allocators in `nod-runtime`.** Added `class_metadata_slot_addr(ClassId)`, `intern_string_literal_slot_addr(&str)`, and `intern_symbol_literal_slot_addr(&str)`. Each guards a `LazyLock<Mutex<HashMap<…, &'static u64>>>`; first call for a given key allocates and leaks a fresh `u64` slot initialised with `class_metadata_ptr(id) as u64` / `intern_string_literal(text).raw()` / `intern_symbol_literal(name).raw()`. Subsequent calls return the same `*const u64`. Memoisation is per-content so multiple JIT-loaded modules referencing the same literal share one slot. For tagged class metadata, no separate slot — codegen emits the raw load then ORs `| 1` at the use site.

**Phase B — resolver wiring in `jit.rs::resolve_reloc_kind`.** `RelocKind::ClassMetadata { class_id }` previously returned `class_metadata_ptr(id) as u64` (the address as a value); same shape as the latent Sprint 38a/38b bug for the immediates. Switched to `class_metadata_slot_addr(id) as u64`. `RelocKind::StringLiteral` and `RelocKind::SymbolLiteral` similarly switched from returning the Word bits to returning the slot addresses. The IR now does `load i64, ptr @nod_*__<key>__*` to recover the bits.

**Phase C — codegen surgery.** Added three `ConstValue` variants in `nod-dfm::ir`: `ClassMetadataPtr { class_id, tagged }`, `StringLiteralRef(String)`, `SymbolLiteralRef(String)`. The four `lower.rs` bake sites (`emit_string_literal`, `emit_class_ref`, `emit_class_metadata_ptr_const`, `emit_symbol_literal`) now emit the new `ConstValue` variants instead of baking `WordBits(runtime_addr)`. `emit_const` in `codegen.rs` grew three new arms; `ConstValue::String` (Dylan source string literal) now routes through the same string-literal global. Per-module dedup tables (`string_lit_idx` / `symbol_lit_idx` in `ModuleCodegenCtx`) ensure repeated references to the same literal text share one external global, one manifest row, and one `LLVMAddGlobalMapping` call. Helper functions `load_class_metadata`, `load_string_literal`, `load_symbol_literal` on `Emit<'ctx, 'a>` build the IR-level `load i64` and, for tagged class metadata, the post-load `or i64 …, 1`. The narrowing optimiser was updated to recognise the new `ClassMetadataPtr` variant (Sprint 38c's `class_id` is carried directly in the variant; no need to do reverse-address lookup as the legacy `WordBits` arm did).

**Phase D — round-trip tests.** Added nine new tests in `tests/nod-tests/tests/jit_cache_xprocess.rs`:

- `sprint38c_class_metadata_globals_round_trip` — `size(make(<range>, from: 0, to: 5))` cold-compiles, dumps bitcode + manifest, replays in a fresh `Jit` + `Context`, asserts the same return value (6) and that the manifest carries ≥1 `RelocKind::ClassMetadata` entry.
- `sprint38c_string_literal_globals_round_trip` — `"hello"` cold-compiles to a module that returns the interned `<byte-string>` Word; replay asserts the returned Word equals the slot-cached value `*intern_string_literal_slot_addr("hello")` (the runtime's `intern_string_literal` doesn't dedup per-text, so we compare against the slot rather than a fresh call), plus verifies the tag bit.
- `sprint38c_symbol_literal_globals_round_trip` — `size(make(<range>, from: 0, to: 5))` (same expression as the class-metadata test; the `make` lowering also emits `from:`/`to:` keyword symbol literals) — asserts the manifest carries `RelocKind::SymbolLiteral { name: "from" }` and `name: "to"`, and the replay returns 6.
- Three IR-shape tests (`sprint38c_emitted_ir_has_*_external_global`) assert the bitcode contains `@nod_class_md__`, `@nod_strlit__`, `@nod_symlit__` external globals and matching `load i64, ptr @nod_*__` instructions — i.e. no baked `i64 <addr>` constants remain at the converted bake-site categories.
- Three sanity tests (`sprint38c_*_slot_addr_is_memoised`) prove the slot allocators return identical addresses for repeated inputs and distinct addresses for distinct inputs.

**Initial test breakage caught and fixed.** First test run hit two failures: `instance?(1, <integer>)` was constant-folded to `select i1 true, %true_word, %false_word` (the optimiser resolves `1`'s class statically); the bake site for `<integer>` never fires. Switched to `size(make(<range>, …))`, which genuinely needs the metadata pointer at runtime. Separately, `size("hello")` returned 0 because `collection_size` in `nod-runtime/src/collections.rs` doesn't dispatch on `<byte-string>` (size-of-strings is a pre-existing gap, not a Sprint 38c regression); switched the test to evaluate the literal directly and compare the returned Word against the slot.

**Phase E — verification.**
- `cargo build --workspace` clean.
- `cargo test --workspace --no-fail-fast`: **586 → 595** passing (+9 new sprint38c tests). 0 failed, 9 ignored (unchanged baseline).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- 5x sequential flake check: 5/5 runs at 595/0/9.
- semispace backend NOT exercised per the user's stated workflow constraint.

**Empirical IR proof.** A cold-compile of `size(make(<range>, from: 0, to: 5))` produces IR containing:

```text
@nod_class_md__8e0cda024e3e5604__1042 = external externally_initialized global i64
@nod_symlit__8e0cda024e3e5604__0      = external externally_initialized global i64
@nod_symlit__8e0cda024e3e5604__1      = external externally_initialized global i64
  %class_md.load = load i64, ptr @nod_class_md__8e0cda024e3e5604__1042, align 4
  %symlit.load   = load i64, ptr @nod_symlit__8e0cda024e3e5604__0,      align 4
  %symlit.load1  = load i64, ptr @nod_symlit__8e0cda024e3e5604__1,      align 4
```

A cold-compile of `"hello"` produces:

```text
@nod_strlit__50577853f5951f35__0 = external externally_initialized global i64
  %strlit.load = load i64, ptr @nod_strlit__50577853f5951f35__0, align 4
```

— exactly the named-global shape Sprint 38 specified. No baked `i64 <runtime-pointer>` constants remain for class metadata, string-literals, or symbol-literals.

**Dedup behaviour.** Per-module content dedup confirmed: `make(<range>, from: 0, to: 5)` emits two distinct symbol globals (`@nod_symlit__*__0` and `*__1` for `from` and `to`). Repeating `from:` in the same module would reuse `*__0`. Cross-module dedup happens at the slot-allocator layer in `nod-runtime`: two modules referencing `"hello"` get distinct globals (different keys) but both globals map to the SAME `&'static u64` slot, so the loaded Word bits are identical.

**Deviations / judgment calls.**

1. **`emit_class_ref` (tagged) path.** Sprint plan suggested optionally adding a `ConstValue::TaggedExternalLoad` variant; instead used `ConstValue::ClassMetadataPtr { class_id, tagged: bool }` to keep the class metadata family in one variant. The OR-1 happens in `Emit::load_class_metadata` at the IR level. Result: one variant for both tagged (`emit_class_ref`) and raw (`emit_class_metadata_ptr_const`) uses.

2. **`ConstValue::String` re-routed too.** The `lower.rs::Expr::String` arm still emits `ConstValue::String(decoded)` (the user-facing Dylan source-string lowering); codegen's `ConstValue::String` arm now calls `self.load_string_literal(s)` so it uses the same external-global path as `ConstValue::StringLiteralRef`. Both variants end up at the same dedup table — verified by the IR-shape test.

3. **Narrowing optimiser updated in-place.** The optimiser's reverse-lookup (find class id behind a `WordBits(addr)` const by querying the class registry) still runs for legacy `WordBits` arms that flow from non-class-ref scaffolding; for the new `ClassMetadataPtr` variant the class id is carried directly. No-op preservation for any other bake sites that still use `WordBits` (block ids, exit-procedure fixnums).

4. **NOD_RUNTIME_ABI_VERSION stays at 2.** Sprint 38a bumped 1→2; Sprint 38c is within that ABI window. The on-disk manifest format is unchanged — the `RelocKind` enum gained no new variants in 38c (the `ClassMetadata`/`StringLiteral`/`SymbolLiteral` kinds already existed from Sprint 38a's infrastructure prep). Old cached bitcode from Sprint 38b still loads correctly since 38b only used `RelocKind::Imm*` rows.

**Out of scope (Sprint 38d / 38e):**

- Win32 FFI stub entries (Sprint 38d).
- Inline-cache slots + generic-function pointers (Sprint 38e).
- The cross-process subprocess-spawn headline test (Sprint 38e — depends on all categories converted).

---

> **Sprints 39 – 46 retros pending.** Detailed retros for the AOT-EXE
> work (39a-c), user-class registration in AOT (40a-d), Win32 message
> loop + IDE shell + file menu (41a-g), real string support (42a),
> rope-backed editor + cursor + syntax colouring + gutter (43d-g),
> multi-file AOT (44), Dylan-lexer-in-Dylan (45a-b), and the
> Dylan-parser-in-Dylan milestone (46) are still in the commit log
> and task tracker but never made it into this file. Adding them is
> tracked as a documentation-debt task; the gap doesn't block forward
> work.

### Sprint 46 — Dylan-in-Dylan parser, self-host milestone — landed

Year-3 self-hosting first step: a Dylan-side parser that consumes
the same fixture corpus the Rust reader does. Built incrementally
over Sprints 46-A (define class with superclasses + slot specs),
46-B (multi-clause statements: `if/elseif/else`, `block/cleanup`,
`select/otherwise`), 46-C (for iteration headers), 46-D (define
generic), 46-E (method signatures), and a polish task for infix
word-operators (`mod`, `rem`).

**The gating milestone** — task #298, "parse the whole corpus,
then GC-stress with heavy parsing" — was the use case that
surfaced GAP-011 in the first place (`dump-node` recursion +
`acc-string` push loop = GC pressure that exposed the function-
param stale-reload bug). Pre-Sprint-48b, any fixture beyond ~35
functions panicked with `stretchy_vector_push: not a
<stretchy-vector>` in cycle 4 or 5. Post-fix, the gating run
produces:

  * Headline: `dylan-parser.dylan` (**100,186 bytes** — the
    Dylan-side parser's own source) parses to **10,948 AST lines**
    in 26 seconds wall-clock.
  * **Corpus pass rate: 30 / 37 fixtures** (81%). All seven
    failures are the same shape: `define macro …` not yet
    implemented in the Dylan-side parser (six fixtures), plus one
    token-shape issue in `dylan-lexer.dylan`. None are GC crashes;
    none are regressions.
  * **GC stress run** (`NOD_GC_TRACE` over the heaviest fixture):
    22 cycles — 11 minor + 11 major — with 570 root snapshots
    (380 AOT slab + 190 thread stack) and **1,696 root rewrites**.
    Genuine fragmentation pressure, real evacuation, zero panics,
    zero stale-pointer traces.

The corpus runner script (`/tmp/run-corpus.sh` shape — kept ad-hoc,
not committed) tallies pass/fail/byte-size/AST-line-count per
fixture; it's the seed of what could become a CI gate once the
remaining `define macro` parsing lands.

**What's queued, not done.**

  * Teach the Dylan-side parser to parse `define macro` (closes
    six of the seven failures).
  * Diagnose the `dylan-lexer.dylan` "unexpected token in
    expression" failure (the seventh).
  * Promote the corpus runner to a `nod-driver` subcommand or a
    cargo integration test so it runs automatically.
  * Sprint 45c-e — character predicates in stdlib + oracle test
    against the Rust lexer + IDE syntax-highlighter consumption.

### Sprint 47 — multi-value return + multi-binder `let` (GAP-003 fix) — landed

Closed GAP-003: `values(a, b)` lowering + `let (a, b) = expr` destructuring.
Replaces the old SBCL-style "pack two fixnums into one Word" workaround
the lexer's `offset-to-line-col` used (and the GAP-003 packed-result hack
that landed before the real multi-value path existed).

* **Phase A (`abc61e3`).** Thread-local secondary-values buffer in
  `nod-runtime/src/values.rs` — three-slot scratch area registered as a
  precise GC root via `snapshot_active_values_roots`. Two new primitives
  (`%values-store`, `%values-load`) read/write it.
* **Phase B+D (`5181da5`).** Sema lowers `values(...)` to a primary-return
  + per-extra `%values-store(i, v)` sequence; `let (a, b, ...) = call` to
  a primary bind + `let b = %values-load(1)` per extra. The IR shape stays
  flat single-value; the buffer carries the rest.
* **Phase C + tests (`a2a448b`).** AST-level policy comment + regression
  tests covering "values returns N, let binds M, M ≤ N", trailing-value
  ignore, GC-across-store-and-load.
* **Phase E (`d5f3f43`).** Retired the
  `offset-to-line-col`-packed-into-one workaround and the
  `$line-col-shift` constant. The dylan-lexer fixture now uses the
  natural `values(line, col)` / `let (line, col) = …` shape.

Cost: ~4 commits, single-day work. Unlocks any future stdlib primitive
that wants to return multiple values without struct allocations.

### Sprint 48 — `is_no_alloc` attribute (Phase A only) — partially landed

`f7867cb`: scaffolded an `is_no_alloc: bool` field on `Computation::DirectCall`
and `Computation::SealedDirectCall`, with `is_potentially_allocating_call()`
short-circuiting when it's set. The intent is the obvious one — let the
liveness pass skip safepoint scaffolding around calls known not to
allocate, saving slab slots + reload instructions on hot paths.

**Phase B and Phase C remain unshipped** (task #288). The annotation
pass (mark primitives + a fixed-point analysis over user-defined
functions) hasn't been written, nor have the corresponding tests, nor
has the docstring rewrite. The field exists, codegen reads it, but
nothing ever sets it to `true` in production. The cost is felt every
time the test crate's `Computation::DirectCall` constructors break
because they don't supply the new field (it bit me during GAP-011
verification, surfaced again as a known issue).

### Sprint 48b — GC safepoint / precise-root marathon (the GAP-011 detour) — landed

The unplanned excursion. What started as "Sprint 46's parser-corpus
milestone should just work now" cascaded into a multi-week GC investigation
that touched every layer between DFM liveness and the newgc evacuator.
Several sprints' worth of work, captured here for the record.

**What was wrong.** `nod-driver parse-dylan` on any non-trivial corpus
crashed with `stretchy_vector_push: not a <stretchy-vector>` deep inside
`acc-string`'s loop. The vector pointer being handed to push was stale —
GC had moved the object, the SSA temp still pointed at the pre-move
address, the page had been reused. Classic stale-precise-root signature,
but the obvious culprits (missing liveness, slot-map bug, slab miss)
all ruled out.

**What landed during the hunt** (each one paid back independently, even
before the bug was found):

* **Global backward live-in/out fixpoint** in `nod-dfm/src/liveness.rs`
  (`37e1f69`). The pre-existing per-block approximation was unsound for
  live-through temps; this replaces it with the textbook gen/kill +
  iterate-to-fixpoint algorithm. Necessary, verified correct, but
  insufficient on its own.
* **Vendored `newgc-core`** in-tree at `src/newgc-core` (`d12ee26`), was
  a git pin against `E:\NewGC`. Refreshed to NewGC HEAD `15b50c6` which
  carries three Lisp-team fixes past the old pin. Provenance in
  `src/newgc-core/VENDOR.md`. Vendoring meant we could add a JSONL
  collection tracer without round-tripping through the NewGC repo.
* **`NOD_GC_TRACE` JSONL tracer** in `src/nod-runtime/src/gc_trace.rs`
  (`d2b489d`). Per-cycle `collect_begin` / `root` / `root_rewrite` /
  `collect_end` events. Two zoom-in features: `NOD_GC_TRACE_WATCH=<csv
  hex>` filters to specific addresses, `NOD_GC_TRACE_FOLLOW=1`
  auto-extends the watch set across relocations.
* **`NOD_DIAG_ARG_ROOT_COVERAGE` probe** (`62f7d41`). Env-gated
  diagnostic in `nod-dfm::diagnose_arg_root_coverage` that enumerates
  every call site where a GC-typed argument is NOT in
  `safepoint_roots`. Used to test a peer-review hypothesis; the
  hypothesis didn't hold (closing 1378 gaps left the crash identical
  — `fc2f0cd`), but the probe stays as permanent diagnostic.
* **`nod-driver symbolicate` subcommand** (`c3e09e2`). Takes raw hex
  IPs from a crash backtrace and rewrites them as `name+0xNN` against
  the linker's `.map` file. Replaces ~20 minutes of by-hand `.map`
  grep with one copy-paste-able command. Lives in `nod-driver` (not
  `nod-runtime`) per the CGU-fragility rule.
* **`[GAP-011]` push probe** in `stretchy_vector_push` — `RtlCaptureStackBackTrace`
  + EXE-base print + symbolicate-hint line. Reusable for any future
  stale-precise-root crash; change the panic site, leave the shape.

**The actual fix** (`66523e1`). Located in `codegen.rs:2303-2308`: at
every block entry, function parameters were being unconditionally
rebound to their original `get_nth_param` LLVM SSA values — the
pre-GC values from the function prologue. The comment above the
rebind even acknowledges it ("restore canonical block-entry bindings
so later uses do not pick up a reload defined in a non-dominating
predecessor"), but the side effect is that every in-block safepoint
reload of a function param gets silently undone at the next block
transition. The fix: spill every GC-typed function param to a stable
home alloca at function entry (`param_homes` map), have block-entry
rebinds load from the home (not `get_nth_param`), and have
`end_safepoint` write the reloaded value BACK to the home so the next
block sees the post-GC address. Block params were already correct via
their phi path (per peer-reviewed sharpening in `976f464`).

**Gating tests post-fix**: `parse-dylan` on `jcs-40` exits 0, the full
`jit_cache_sample_items.dylan` corpus parses to 3307 lines of AST output,
and `cargo test -p nod-runtime --lib -- --test-threads=1` stays at 144
passing.

**Documentation.** The full investigation is in
`GAP-011_GC_team_writeup.md` (hypotheses tried, refutations, final
fix with non-negotiable invariants A and B). The tooling guide is at
`docs/tracing_guide.md` and walks through the canonical workflow.

**What was queued, not done.** A static post-codegen verifier (the
"alloca tracker" — walk every Word-typed alloca, prove every load is
dominated by either a fresh store or a post-safepoint reload+writeback)
would catch this class of bug at compile time forever after. The fix
makes the invariant true today; the verifier would make it enforceable
across future codegen changes. Worth landing in a focused follow-up
sprint when there's an hour for it.

### Sprint 49 — post-marathon polish bundle — landed

The fingers-crossed-no-more-GC-issues sprint. Four unrelated bits of
quality-of-life that GAP-011 left us with energy to pick up.

* **Sprint 49a (`d3a0ac2`) — `.prj` project files + `--time` flag.**
  TOML schema with three fields: `name`, `sources`, `output` (defaults
  to `<name>.exe`). Relative paths anchor at the project file's
  directory, NOT the caller's CWD — non-negotiable. `nod-driver build
  --project foo.prj` is mutually exclusive with positional inputs via
  clap's `conflicts_with` + `required_unless_present`. `--time` rides
  along on both `build` and `parse-dylan`. Lives in
  `src/nod-driver/src/project.rs` with six unit tests. Example fixture:
  `tests/nod-tests/fixtures/factorial.prj`.

* **Sprint 49b (`9f14383`) — `cond` macro.** Common-Lisp-style multi-arm
  conditional, lowers to nested `if/elseif/else`. Joins `unless`,
  `when`, `for-each`, `with-cleanup` in `stdlib.dylan`. Arities 1
  through 4 test/body pairs + `otherwise` are supported via fixed-arity
  rules; beyond 4 arms, nest. The macro engine's lack of `*` repetition
  is what caps the arity — a real fix waits for Sprint 49c-ish. Also
  threaded `"cond"` into the parser's nested-form list and the macro
  engine's depth-aware body-matcher keyword list so other
  body-shaped forms (e.g. `with-cleanup body cleanup cond ... end end`)
  parse correctly. Smoke fixture: `tests/nod-tests/fixtures/cond_smoke.dylan`
  exercising every arity × both arm paths, asserted on stdout.

* **Sprint 49c (`3203c94`) — O(N) sliding cursor in the Dylan-lexer's
  `offset-to-line-col`.** Closes the long-standing #291: `dump-tokens`
  called `offset-to-line-col(source, off)` twice per token from byte 0,
  classic O(N²) over the whole dump. Replaced with three module-level
  cache variables tracking the last `(pos, line, col)` triple; tokens
  come out in monotonically increasing order so each source byte is
  visited at most once per `dump-tokens` invocation. Defensive on
  backwards seeks (caller bug → restart from byte 0). Pure Dylan
  change; output verified identical to pre-fix on the smoke + full
  fixtures.

(The numbering "49a/b/c" is post-hoc — these three commits landed as
"Sprint 49", "Sprint 49b", and "dylan-lexer: O(N) fix" respectively;
codifying them all under 49 cleans up the freelance label.)

### Sprint 49d — Dylan-side parser: corpus to 37/37 — landed

Closed out the Sprint 46 milestone follow-ups. Three small Dylan-only
edits push the Dylan-in-Dylan parser's corpus pass rate from 30/37
(81%, Sprint 46 close) to **37/37 (100%)**:

* **`macro` and `c-function` keyword classification.** Teach
  `dylan-lexer.dylan::classify-keyword` about both, then add them to
  the parser's `is-define-body-word?` predicate. Without this they
  hit a generic "expected a define-body or define-list word" path.

* **`c-function` parameter / return-spec parsing.** Add `#"c-function"`
  to `is-function-word?` so its `(params)` / `=> (returns)` signature
  parses the same way a `function` head does — closes the
  `define c-function` Rust panic at `conditions.rs:904` that was
  swallowing five fixtures.

* **`parse-tolerant-body` for `macro` and `c-function`.** Their bodies
  contain pattern templates and FFI specs that are NOT structured
  Dylan expressions — group-balance over `(){}[]#{` and gobble tokens
  to top-level `end` instead. Dispatched via a **statement-form**
  `if` in the body slot (one setter per arm) — the earlier
  expression-form `defn-body(d) := if (…) … else … end;` triggered an
  LLVM SSA-dominance verifier failure (same family as GAP-011: the
  heap-typed `if` join had a reload that didn't dominate the use).

* **`define variable` typed-name binding.** Teach
  `parse-list-fragment` to recognise `name :: type [= expr]` after an
  expression parse, promoting the `<ast-variable-ref>` into an
  `<ast-typed-name>` and folding the optional `= rhs` into a binary
  `=` node. Closes the last failing fixture (`dylan-lexer.dylan` —
  was tripping over `define variable *line-col-cache-pos* :: <integer>
  = 0`).

Corpus runner: `for f in tests/nod-tests/fixtures/*.dylan; do
nod-driver parse-dylan "$f"; done` — **37 / 37 pass**. Every legit
Dylan in the tree now parses through the Dylan-side parser. Closes
the "remaining `define macro` parsing" and "diagnose
`dylan-lexer.dylan` token-shape failure" follow-ups noted in the
Sprint 46 retro.

Two carved-off follow-ups remain (queued, not blocking):
  * Promote the corpus runner to a `nod-driver` subcommand /
    cargo integration test (still ad-hoc).
  * Investigate the LLVM dominance failure on heap-typed
    if-as-expression — current workaround is statement-form; root
    cause is the same shape as GAP-011 and likely needs the same
    home-alloca treatment for `if` join values, not just function
    params.

### Sprint 45c — stdlib ASCII byte predicates — landed

Lift the byte-classification predicates the Dylan-side lexer was
defining locally into `stdlib.dylan` so they're available to every
Dylan source. Naming follows the Dylan tradition (`?`-suffix
predicates) with an explicit `ascii-` qualifier so future
`<character>` overloads (Sprint 42b territory) can sit alongside
without renaming churn.

New stdlib predicates, all taking `<integer>` (byte, 0..255):
  `ascii-digit?`, `ascii-hex-digit?`, `ascii-bin-digit?`,
  `ascii-oct-digit?`, `ascii-alpha?`, `ascii-alphanumeric?`,
  `ascii-uppercase?`, `ascii-lowercase?`, `ascii-whitespace?`.

The Dylan-side lexer's `is-ascii-digit?`, `is-ascii-alpha?`,
`is-bin-digit?`, `is-oct-digit?`, `is-hex-digit?`, and
`is-whitespace-byte?` are now thin aliases that delegate to the
stdlib predicates. The Dylan-grammar-specific ones (`is-name-start?`,
`is-name-cont?`, `is-name-cont-not-eq?`, `is-exponent-marker?`) stay
in `dylan-lexer.dylan` — they encode the language's identifier
alphabet, not generic ASCII.

End-to-end smoke: `dump-dylan-tokens` over an input mixing
identifiers, integers, `#xff` hex, `#b1010` binary, `#o755` octal,
whitespace, comments, and punctuation — every byte-classification
path through the new predicates fires correctly. Parser corpus still
37 / 37.

What 45c does NOT do (deferred to follow-ups):
  * `<character>`-typed overloads — waits on Sprint 42b's
    `<character>` class.
  * Sprint 45d (oracle test against the Rust lexer) and 45e (wire
    the Dylan lexer into IDE syntax colouring) — independent
    sub-sprints that need their own gates.

### Sprint 45d — lexer oracle (Rust vs Dylan) — landed

Cross-check that both lexers segment Dylan source the same way. The
two were written for different downstream consumers — `nod-reader`
feeds the parser (drops trivia, uses specific punctuation kinds like
`LPAREN`/`EQUAL`), `dylan-lexer.dylan` feeds the future IDE colourer
(keeps trivia, uses a generic `PUNCT` kind). Both shapes are valid;
the meaty correctness question is whether they *segment the same
source the same way*.

New: `tests/nod-tests/tests/lexer_oracle.rs`.

Approach: shell out to `nod-driver dump-tokens` (Rust) and `nod-driver
dump-dylan-tokens` (Dylan), parse each line into
`(start_line:col, end_line:col, kind, lexeme)`, then:
  * Drop Dylan trivia (`WS`, `COMMENT_LINE`, `COMMENT_BLOCK`).
  * Drop the Dylan preamble — Rust's `lex()` calls
    `skip_preamble()` so its stream starts after `Module: foo\n\n`;
    mirror by finding the first WS spanning ≥2 source lines and
    dropping everything before its end-line. (Line-arithmetic rather
    than lexeme-text inspection so CRLF / LF source line endings
    don't trip the check.)
  * Undo the Dylan dump's display escapes (`\s` → space,
    `\t` → tab, `\r` → CR) so whitespace inside string literals
    compares against Rust's verbatim rendering. The `\\` and `\"`
    escapes stay encoded — Rust's dump uses them identically.
  * Compare element-wise on (span, lexeme). Kind disagreements get
    counted and reported on `--nocapture` but don't fail the test —
    they're a keyword-set design conversation.

Initial corpus:
  * `oracle_hello` — 17 tokens, segmentation identical, 16 kind
    disagreements (every PUNCT vs LPAREN/RPAREN/etc).
  * `oracle_factorial` — 50 tokens, segmentation identical, 46 kind
    disagreements.
  * `oracle_cond_smoke` — `#[ignore]`d with documented disagreement:
    Rust merges `-1` into one INTEGER token; Dylan emits `-` then
    `1` and lets the parser handle the unary minus. Both defensible
    — Rust is more aggressive at lex time, Dylan is more flexible in
    subtraction contexts like `x-1`. The Dylan-side parser handles
    the split via `is-unary-op?`. Sprint-45-followup picks a winner
    and the test flips to `#[test]` then.
  * `oracle_corpus_sweep` — `#[ignore]`d informational survey across
    every `.dylan` fixture (minus the two giants — `dylan-lexer.dylan`
    and `dylan-parser.dylan` — whose Dylan-side dump takes ~2+ min
    each at present). Logs divergences to stderr but doesn't fail.
    **Headline finding:** of the 35 fixtures swept, every single
    divergence is the SAME pattern — `-N` merged-vs-split. Concrete
    hits: `cond_smoke.dylan`, `ide_helpers.dylan`, `ide_rope.dylan`.
    No keyword-set surprises, no segmentation drift, no string-lex
    differences — the corpus is one design decision away from
    full agreement. That's exactly the kind of survey result the
    oracle was built to produce.

Bugs surfaced while writing this:
  * `str::split_whitespace` treats `\r` (0x0D) as a separator —
    fatal for WS tokens whose lexeme is `\r\n\r\n` on Windows
    sources. Rewrote field parsing to only split on space/tab and
    take the lexeme as the line tail. Took an hour to track down;
    worth a callout.
  * `s.lines()` strips trailing CR before `\n` (good) but doesn't
    split at standalone CR (also good) — so it's safe for the
    dump format. The CR appears in WS token text and is parsed
    intact.

What 45d does NOT do (deferred):
  * Lex-time signed-number policy decision (cond_smoke disagreement).
  * Kind-equivalence table (PUNCT ↔ specific kinds) — would let the
    oracle assert kind agreement, not just segmentation.
  * Performance — the test shells out, building the Dylan-lexer EXE
    is cached but per-test overhead is ~1s.

### Sprint 50a — Dylan-side macro engine smoke — landed

First step on the "retire `nod-macro`" track of year-3 self-hosting.
`nod-macro` is ~1900 lines of Rust doing pattern-matching + template
substitution over `Fragment`s (a token-grouping structure between raw
tokens and parsed AST). Sprint 50a ports enough of that to expand
ONE rule — the stdlib `unless` macro — and prove the algorithm and
data shape work in Dylan.

New: `tests/nod-tests/fixtures/dylan-macro-smoke.dylan` (~350 lines)
+ `tests/nod-tests/tests/macro_engine.rs` (integration test).

Dylan-side classes mirroring nod-macro's Rust types:
  `<tok>` — minimal `(kind, text)` token (decoupled from
    `dylan-lexer.dylan`'s `<token>` for now; 50c wires the real one).
  `<fragment>` → `<token-fragment>`, `<group-fragment>`.
  `<pattern-elem>` → `<pat-literal>`, `<pat-variable>`, `<pat-group>`.
  `<template-elem>` → `<tpl-literal>`, `<tpl-substitution>`, `<tpl-group>`.
  `<binding>` + linear-list `<bindings>` (small tables, hash overhead
    not worth it).

Engine:
  * `match-pattern(pattern, call) => false-or(<bindings>)` — greedy
    left-to-right, no backtracking; same algorithm as Rust
    `match_pattern` at Sprint-17 level. Supports `#"expression"` and
    `#"body"` pattern-variable kinds. Body matcher is depth-aware on
    `end` — `if … end` inside `unless … end` doesn't claim the outer
    terminator. Mirrors Rust's `opens-end-form?` set verbatim.
  * `substitute(template, bindings) => <byte-string>` — emits text,
    same shape as Rust's `substitute` (text out; caller re-lexes).

Smoke: hand-build the `unless` rule structure + a call site
`unless x (foo) end`, run match → bindings → substitute, get
`if ( ~ x ) ( foo ) else #f end`. The slightly-loose group spacing
is a join-chunks heuristic to refine in 50b; the algorithm is
correct.

Verification:
  * `cargo test -p nod-tests --test macro_engine` — passes.
  * Parser corpus: **38 / 38** (the new fixture self-parses through
    the Dylan-side parser too).

Cost surprise: the GAP-011-family LLVM SSA-dominance issue on
heap-typed `if`-as-expression bit twice during write-up — once for
the open/close glyph picker (`let open = if (k = #"paren") "(" …`),
once for the body-end position calc. Both fixed with statement-form
rewrites. The same workaround we've used since Sprint 49d. Real fix
(home-alloca pattern at `if` joins) still queued.

What 50a does NOT do (deferred, in order):
  * **50b** — Parse real `define macro` source into the rule
    structures. ✅ landed (see below).
  * **50c** — Walk-and-expand pass over a parsed `<ast-body>`;
    wire to the parser's known-macros set; use the real `<token>`.
  * **50d** — Oracle test: Dylan-expanded vs Rust-expanded
    byte-compare, same shape as 45d.
  * **50e** — Switch AOT pipeline to consume Dylan-expanded AST.
    `cargo rm -p nod-macro` at the end.

### Sprint 50b — parse `define macro` body fragments → `<macro-def>` — landed

Replaces 50a's hand-built `unless` rule with a fragment-stream parser
that walks the same shape `nod-macro::parse_macro_def` accepts. Same
fixture, same `<macro-rule>` shape, same `unless` rule structure, but
now built from a representation closer to what a real Dylan lexer
would emit.

New classes:
  `<macro-rule>` { pattern, template } — 50a didn't need the wrapper.
  `<macro-def>`  { name, rules } — supports multi-rule defs.

New parsers in `dylan-macro-smoke.dylan`:
  `parse-pattern-elem(body, i)`  → (`<pattern-elem>`, consumed)
  `parse-template-elem(body, i)` → (`<template-elem>`, consumed)
  `parse-pattern-body` / `parse-template-body`
  `parse-rule(frags, start)`     → expects `{ … } => { … }`
  `parse-macro-def(name, body)`  → 1+ rules separated by `;`

Pattern-variable recognition mirrors `nod-macro::parse_pattern_var_head`
common arm: a `?` followed by a `#"keyword-name"` token (the lexer
glues `name:` into one token) followed by a kind ident
(`expression`, `body`, …). The explicit-spaces form `? cond : expression`
is deferred to 50c when we plug in the real lexer.

Verification: the smoke now runs TWO phases — `hand-built` (50a)
and `parsed-def` (50b) — and the integration test asserts the FULL
stdout matches byte-for-byte. Both phases produce the same expansion
(`if ( ~ x ) ( foo ) else #f end`). Parser corpus: **38 / 38**.

What 50b does NOT do (deferred to 50c+):
  * Use the real `<token>` from `dylan-lexer.dylan` (still uses local
    `<tok>`).
  * Lex source text into fragments (still hand-builds the fragment
    stream).
  * Walk a real `<ast-body>` and expand macro calls in place.
  * Multi-rule defs (the def-parser handles them but no fixture
    exercises that path yet).

### Sprint 50c-1 — token-stream → fragment-tree group-balancer — landed

The bridge between "what a lexer emits" (a FLAT token stream) and
"what the macro engine consumes" (recursive `<group-fragment>`s). Real
lexer integration in 50c-2 becomes a one-line swap of the
token-building function.

New helpers in `dylan-macro-smoke.dylan`:
  `group-open-kind(text)`  → `<symbol>` or `#f` (opener detection)
  `group-close-text(kind)` → expected close text
  `tokens-to-fragments-from(tokens, start, closer)` — recursive
  `tokens-to-fragments(tokens)` — top-level entry

Mirrors `nod-reader::fragments::Fragmenter` at the basic level —
supports `(…)`, `[…]`, `{…}`. The hash-prefixed `#(`, `#[`, `#{`
groups land in 50c-2 alongside the real lexer wiring.

Smoke now runs THREE phases on the same fixture:
  PHASE: hand-built   — Sprint 50a's path (rule built directly)
  PHASE: parsed-def   — Sprint 50b's path (fragments → def)
  PHASE: from-tokens  — Sprint 50c-1's path (tokens → fragments → def)
All three produce byte-identical `EXPAND` lines. The
`TOKENIZE: 24 def-tokens / FRAGMENT: 3 top-level frags` diagnostics
prove the group-balancer collapsed the flat stream into one
top-level fragment per `{ pattern } / => / { template }`.

Verification: integration test asserts the FULL three-phase stdout
matches byte-for-byte. Parser corpus: **38 / 38**.

What 50c-1 does NOT do (deferred to 50c-2):
  * Plug in the real `<token>` from `dylan-lexer.dylan` (still uses
    local `<tok>`). ✅ landed (see below).
  * Lex actual source text — the token stream is still hand-built.
    ✅ landed.
  * Hash-prefixed groups (`#(`, `#[`, `#{`).
  * The walk-and-expand pass over `<ast-body>`.

### Sprint 50c-2 — bundle dylan-lexer + macro engine via .prj, lex real source — landed

The smoke now uses the **real Dylan-side lexer** to produce its token
stream. End-to-end:
  **source text → `lex(<byte-string>)` → adapter → fragments →
  `parse-macro-def` → `match-pattern` → `substitute` → text**
100% Dylan, no Rust nod-macro involved.

Mechanics:
  * `dylan-macro-smoke.dylan` changes its module declaration from
    `Module: dylan-macro-smoke` → `Module: dylan-lexer` so the two
    files compile into the same module — same trick
    `dylan-parser.dylan` uses today. Standalone build still works
    (a single-file module of that name).
  * New `dylan-macro-smoke.prj` (Sprint 49a project-file infra)
    bundles `dylan-lexer.dylan` + `dylan-macro-smoke.dylan` into
    one EXE. Sources order matches the parser EXE's convention.
  * New adapter `lex-token-to-tok` translates each lexer
    `<token>` subclass (`<keyword-token>`, `<identifier-token>`,
    `<keyword-name-token>`, `<punctuation-token>`,
    `<boolean-literal-token>`) into the engine's `<tok>` shape,
    or `#f` for trivia (whitespace / comments).
  * `lex-source-to-toks(source)` calls the real `lex`, filters
    `#f`s, returns a flat `<stretchy-vector>`.
  * Symbol-to-text reverse table for keywords. Hit a real bug:
    `cond` is a lexer keyword (Sprint 49b), so `?cond` in the
    template lexed as `?` + `<keyword-token>` and got silently
    dropped by the adapter, leaving a hole in the token stream
    that crashed `parse-pattern-elem`'s peek-ahead. Fix: enumerate
    all keywords that can plausibly appear as identifier-shaped
    references inside macro bodies — `cond`, `case`, `select`,
    `while`, `for`, `block`, `method`, etc.

Smoke now runs FOUR phases on the same fixture:
  PHASE: hand-built  — Sprint 50a
  PHASE: parsed-def  — Sprint 50b
  PHASE: from-tokens — Sprint 50c-1
  PHASE: from-source — Sprint 50c-2  (LEX: 24 tokens)
All four produce byte-identical `EXPAND` lines.

Multi-file compilation flex: bundling `dylan-lexer.dylan` (~1700
lines) + `dylan-macro-smoke.dylan` (~900 lines) into one module
just works. That's a real stress test of the AOT pipeline at a
reasonable line count, and it landed without a single compiler bug.

Verification:
  cargo test -p nod-tests --test macro_engine — passes
  Parser corpus: 38/38

What 50c-2 does NOT do (deferred to 50c-3 / 50d / 50e):
  * Hash-prefixed groups (`#(`, `#[`, `#{`) in the
    token-to-fragments group-balancer. ✅ landed (see 50c-3 below).
  * Symbol-to-text exhaustiveness — the reverse table is hand-built
    and covers ~20 common keywords. ✅ landed (50c-3).
  * The walk-and-expand pass over a real `<ast-body>`. Smoke
    still operates on a single hand-shaped call site.
  * Oracle vs Rust nod-macro (50d's slot).
  * Retire `nod-macro` from the build (50e's slot).

### Sprint 50c-3 — `token-source-text` + hash-prefixed groups — landed

Two small wins that round out the engine before we tackle the
walk-and-expand pass.

**`token-source-text` replaces the keyword + punct inverse tables.**
The lexer keeps a span on every token; slicing the source via that
span recovers the original text directly. We had two hand-enumerated
tables — `keyword-symbol-to-text` (~20 keywords) and
`punct-form-to-text` (~10 forms) — both of which would silently drop
unknown entries (the Sprint 50c-2 bug). Both are gone. The adapter
is now one `token-source-text(t, source)` call per keyword/punct
token. Every keyword the lexer recognises round-trips for free; no
maintenance, no enumeration drift.

**Hash-prefixed groups (`#(`, `#[`, `#{`) in the group-balancer.**
The lexer emits `<literal-vector-open>` for `#(`,
`<literal-sequence-open>` for `#[`, and `<punctuation-token>` form
`#"hash-lbrace"` for `#{`. The adapter surfaces all three as `<tok>`
kind `#"punct"` with text `"#("` / `"#["` / `"#{"`, and
`group-open-kind` / `group-close-text` know the new opener glyphs.
Closers are bare `)` / `]` / `}` — the lexer doesn't emit `#)` etc.
Emit-frag and emit-template render the new glyphs back.

New fifth phase in the smoke (`PHASE: hash-groups`): lex
`#(a, b, c)`, group-balance, assert one top-level
`<group-fragment>` of kind `#"hash-paren"` containing 5 inner frags
(`a`, `,`, `b`, `,`, `c`). Doesn't run match/substitute — the call
site doesn't fit the unless pattern; the phase is purely a
group-balancer probe.

Verification:
  cargo test -p nod-tests --test macro_engine — passes
  Parser corpus: 38/38

What 50c-3 does NOT do (deferred to 50c-4 / 50d / 50e):
  * Walk-and-expand pass over a real `<ast-body>` — the chunky
    one. Needs bundling dylan-lexer + dylan-parser + macro engine
    + an AST-aware traversal that recognises macro call sites
    and splices expanded fragments back. Its own sprint.
  * Oracle vs Rust nod-macro (50d).
  * Retire nod-macro from the build (50e).

### Sprint 50d — `.prj` `start_function` schema field — landed

The duplicate-`main` collision that blocked Sprint 50c-4 was a
tooling choice, not a Dylan-language constraint. Open Dylan lets a
library configure its entry function via `*main-function:*`; the
nod tooling was just hardcoding `"main"`. Sprint 50d ports the
configurable-entry idea to the `.prj` schema:

```toml
name           = "dylan-macro-smoke"
sources        = [..., "dylan-parser.dylan", "dylan-macro-smoke.dylan"]
start_function = "smoke-main"   # default "main", back-compat
```

Code changes:
  * `src/nod-driver/src/project.rs` — new `start_function: Option<String>`
    field on `RawProject`, surfaces as `pub start_function: String`
    (defaulted to `"main"`) on `ResolvedProject`. Two unit tests
    cover default + explicit override.
  * `src/nod-llvm/src/aot.rs` — new public functions
    `emit_aot_entry_stubs_full` and `emit_aot_object_full` accept an
    `entry_function: &str` parameter. Older signatures stay as
    `"main"`-passing wrappers (back-compat for every existing call
    site and test).
  * `src/nod-driver/src/main.rs` — pipes the project's
    `start_function` through to `run_build_full`, uses it in the
    pre-flight "missing entry" check, and threads it into
    `emit_aot_object_full`.
  * `tests/nod-tests/fixtures/dylan-parser.dylan` — removed a
    redundant top-level `main();` invocation. Confirmed the
    standalone `parse-dylan` EXE still works because the AOT entry
    stub already wires the user's `main` as `nod_user_main` (called
    from the runtime C wrapper). Parser corpus: 38/38.
  * `tests/nod-tests/fixtures/dylan-macro-smoke.dylan` — renamed
    `main` to `smoke-main` and set `start_function = "smoke-main"`
    in `dylan-macro-smoke.prj`.

Verification:
  cargo test -p nod-tests --test macro_engine — passes
  cargo test -p nod-driver project — 8 passed (incl. 2 new)
  Parser corpus: 38/38
  Smoke EXE (built via `.prj` with start_function="smoke-main")
    runs all five phases byte-for-byte identically to before.

### Sprint 51a — first sema piece in Dylan: C3 linearisation — landed

The very first port of `nod-sema` to Dylan. C3 (`src/nod-sema/src/c3.rs`,
317 lines) is the right starting move — pure function, self-contained,
no dependencies on other sema passes, with the Rust tests already
asserting canonical outputs for every interesting shape.

New `tests/nod-tests/fixtures/dylan-c3-smoke.dylan`: a Dylan
translation of the same algorithm, oracle-tested against the values
the Rust `c3.rs` tests assert. Six shapes match byte-for-byte:

```
T1 empty class               <x>
T2 SI chain two deep         <b> <a> <object>
T3 SI chain four deep        <d> <c> <b> <a> <object>
T4 diamond                   <e> <b> <c> <a>           (= Python's E.__mro__)
T5 MI with shared grandparent <c> <a> <b> <x>          (= Python's [C, A, B, X])
T6 cycle                     ERROR inconsistent-merge for <child>
```

Implementation notes:
  * Stdlib's `<stretchy-vector>` exposes push but no pop or size
    shrink, so a "queue" is a small `<queue>` class wrapping a
    `<stretchy-vector>` plus a `head :: <integer>` index.
    `pop-front!` is O(1) — just `head := head + 1`. Cleaner than the
    Rust `VecDeque` shape and exercises Dylan's class system end-to-end.
  * Two bugs found while writing it:
    1. The pop loop combined `~ queue-empty?(q) & queue-front(q) = picked`
       — but Dylan's `&` is currently eager (the standing short-circuit
       task), so the call to `queue-front` ran even when the queue was
       empty. Nested the conditional.
    2. `queue-has-in-tail?` used `until (i = n | ...)` but when `head`
       was already at `size`, `i = head + 1 > n` and the equality
       never fired — infinite loop + out-of-bounds read. Switched to
       `>=`.
    Both are general lessons (eager-`&` and off-by-one boundary)
    rather than Dylan or compiler bugs.
  * Uses the Sprint 50d `start_function` field so the entry name is
    `c3-main` (no `main` collision with the wider stdlib once we
    bundle this with more sema pieces in 51b/51c).

New integration test `tests/nod-tests/tests/c3_oracle.rs`: builds the
`.prj`, runs the EXE, asserts exact stdout.

Verification:
  cargo test -p nod-tests --test c3_oracle — passes
  Parser corpus: **39/39** (the new fixture self-parses too)

What 51a does NOT do (deferred to 51b+):
  * Wire the Dylan-side C3 INTO the Rust sema's pipeline (would need
    a Rust-callable shim — the Dylan code stays standalone for now).
  * Port any other sema piece. C3 is the smallest; next candidates
    by size are `c3.rs` (done), `bench.rs` (skip — harness), `stdlib.rs`
    (load+parse, not very interesting), `optimise/narrowing.rs`
    (188 lines, type narrowing), `optimise/facts.rs` (271, type
    flow facts). After those, the big ones: `sidecar.rs` (904),
    `lib.rs` (1886, glue + public API), `lower.rs` (7769, AST → DFM).
  * Run on real (non-canonical) inputs from the parser corpus.

### Sprint 50c-4 — orphan-`main`-as-C-entry bug — landed

**Root cause was a one-line linkage bug, not the init-order red
herring my first hypothesis pointed at.** When `start_function != "main"`,
some other source in the bundle may still define a function named
`main` (the canonical example: bundling `dylan-parser.dylan`,
whose CLI entry happens to be `main`, with a harness whose entry
is `smoke-main`). The AOT pipeline correctly renamed `smoke-main`
to `nod_user_main`, but left the parser's `main` untouched with
External linkage.

Windows' `mainCRTStartup` (in `msvcrt.lib` / `ucrt.lib`) walks
the symbol table at startup looking for an external `main`. It
found the parser's orphan `main` and called it — BEFORE
`nod_aot_main_wrapper` got a chance to run
`nod_aot_resolve_relocs`. So every literal-string global was
still NULL, and the first `format-out` inside that orphan crashed
with "format string is not a <byte-string> (raw 0x0)".

Fix in `nod-llvm/src/aot.rs::emit_aot_entry_stubs_full`: after
renaming the chosen entry function to `nod_user_main`, scan the
module for any leftover `main` function; rename it to
`nod_orphan_main` and demote its linkage to Internal. The
synthetic C `main` the AOT pipeline emits in step 3 can then
claim the symbol name cleanly, and the C runtime finds OUR
`main` (which calls `nod_aot_resolve_relocs` then
`nod_user_main`).

Found via two probes (both behind env vars, both removed after
the fix landed):
  * `NOD_DIAG_AOT_FUNCS` in `emit_aot_entry_stubs_full` — dumped
    every defined function's name. Showed both `main` and
    `smoke-main` in the module simultaneously.
  * `NOD_DIAG_FORMAT_OUT_BT` in `nod_format_out` — would have
    given a backtrace at the bad call but the AOT EXE's symbols
    are stripped, so the trace was `<unknown>` frames. Probe
    confirmed the call WAS reached and not e.g. a static init.

Diagnosis path: bisect first (trivial 3-file bundle works → the
bug is parser-content-specific, not "any third file"), then list
functions in the module (showed the orphan `main`), then try the
fix (rename + Internal linkage), then verify Windows linker error
that confirms `mainCRTStartup` *did* expect a `main` symbol
externally and only stopped complaining once we let the synthetic
C `main` reclaim the name.

Verification:
  cargo test -p nod-tests --test macro_engine — passes
  cargo test -p nod-driver project — 8 passed (incl. the 2 from 50d)
  Parser corpus: 38/38
  **3-file bundle** (`dylan-lexer + dylan-parser + dylan-macro-smoke`)
    runs the full 5-phase smoke end-to-end. The walk-and-expand
    sprint is now unblocked.

---

### Sprint 29b — `format` + `print` + `streams` (`io` library kernel)
Slipped from the old Sprint 27 slot when Sprint 27 absorbed the FFI Phase A work. Port `opendylan-tests/sources/io/tests/format.dylan`, `print.dylan`, `streams.dylan` against ported `io` library code. Removes the `format-out` FFI shim.

### Sprint 29c — Kernel library port: arithmetic, characters, symbols
Port enough of `sources/dylan/` (`number.dylan`, `character.dylan`, `symbol.dylan`, `boolean.dylan`) that the runtime stops providing these directly and the language defines them in itself.

### Sprint 30 (planned, slipped) — Dylan-side IDE bring-up: window, message pump, editor surface
**Slipped:** the Sprint 30 slot was reclaimed by FFI Phase C (string marshaling — see "Sprint 30 — FFI Phase C" above). This IDE-bring-up plan stays on the roadmap and runs after Sprint 31's `common-dylan` port. First IDE sprint. **All Dylan code**, written against the Sprint 25b Windows FFI stack. Module `nod-dylan/ide-shell` registers a top-level window class, runs the message pump, hosts a single editable text pane and a REPL transcript pane. No syntax colouring yet, no menus — just "the compiler can open a window and let you type into it". Re-implements the scaffolding of `E:\opendylan\sources\environment\framework\` in Dylan.

### Sprint 30b — Dylan-side inspector + dispatch visualisation
With the IDE shell up, port the existing `:inspect` / `:dispatch-stats` / `:classes` REPL commands into IDE panels written in Dylan. Inspector handles every kernel class. Time-travel REPL prototype.

### Sprint 31 — `common-dylan` library port
Port `byte-vector`, `simple-format`, `simple-io`, `simple-random`, `transcendentals`, `threads/`. Run `opendylan-tests/sources/common-dylan/tests/`.

### Sprint 32 — Multi-threaded mutator + cooperative GC across threads
Thread-local TLABs, parking protocol, lock primitives in Dylan-side code. Run `opendylan-tests/sources/app/thread-test/`.

### Sprint 33 — Library-merge optimisation (v2 candidate moved up if cheap)
DFM serialisation, cache-key extension with downstream library hashes, cross-library inlining gated on sealing. May slip to post-v1.

### Sprint 34b — AOT mode — emit a standalone Windows executable
(Renumbered: Sprint 34 was reclaimed by the `<c-struct>` family work above — see "Sprint 34 — Structs".) JIT artefacts written out as a PE binary plus a shipped `nod-runtime` static lib. Cache key already covers it; mostly a packaging exercise.

### Sprint 35b — Dylan-side IDE polish: debugger, library browser, sealed-domain visualiser to v1.0 quality
(Renumbered: Sprint 35 was reclaimed by the COM / DXGI / D3D11 / D2D / DirectWrite infrastructure work above — see "Sprint 35 — COM via `windows` crate".) All in Dylan, on top of the Win32 FFI stack: source-stepping debugger, library browser with cross-references, sealed-domain visualiser usable on real programs. Re-implements the feel of `E:\opendylan\sources\environment\debugger\`, `editor/deuce/`, and `commands/`.

### Sprint 36b — macOS port (aarch64-apple-darwin first)
(Renumbered: Sprint 36 was reclaimed by the IDE-shell integration work above — see "Sprint 36 — IDE shell".) The Dylan-side IDE re-implementation against a `nsapp` / Cocoa equivalent — same shape: Dylan code calling Cocoa through `c-ffi` over a macOS analogue of the Sprint 25b FFI stack. The non-runtime crates are already platform-clean; the cost is rewriting the IDE-side Win32 bindings as Cocoa bindings. Same `c-ffi` shape, different `define interface` declarations.

---

## Dependency Graph (parallelism windows)

If a second developer joins, here is what can run in parallel.

| Sprint | Depends on | Can run in parallel with |
|---|---|---|
| 01 Workspace Skeleton | — | — |
| 02 Lexer | 01 | — |
| 03 Fragments + Parser | 02 | — |
| 04 Definitions + Body Parser | 03 | 05 LID parser (LID is independent of body grammar) |
| 05 LID + Module Graph | 02 | 04 |
| 06 DFM IR Skeleton | 04, 05 | — |
| 07 LLVM Codegen | 06 | — |
| 08 REPL Loop | 07 | 09a tagged-pointer design doc |
| 09 GC: Tagged Pointers | 08 | — |
| 10 GC: Strings, Symbols, Vectors | 09 | 12a class-syntax parsing (already done in 04) |
| 11 GC: Generational Collector | 10 | — |
| 12 Classes + Slots, Single Dispatch | 11 | 17a macro-pattern parser draft |
| 13 DFM Dispatch + Method Lookup | 12 | — |
| 14 Multiple Inheritance | 13 | 15a sealing-syntax parsing |
| 15 Sealing Analysis | 13, 14 | 17 Macro Expander (different code paths) |
| 16 Richards Subset | 15 | — |
| 17 Macro Expander Engine | 04 (just the fragment shape) | 13, 14, 15 — independent of dispatch |
| 18 Twelve Macro Shapes | 17 | 19 Conditions/NLX (independent) |
| 19 Conditions, NLX, Restarts | 11 (GC), 18 | 20 Collections (different subsystem) |
| 20 Iteration + Collections | 19 | — |

The clear parallelism windows are:
- **02 ↔ 03 ↔ 05** — the LID/manifest parser is independent of the body grammar.
- **11 ↔ 17** — once the GC is up and macros only need fragments, a macro-track and a class/dispatch-track can advance in parallel from Sprint 12 through Sprint 18.
- **15 ↔ 17/18** — sealing and macro work touch disjoint crates.

With one developer the dependency chain is essentially linear with two
short branch-and-merge points around macros (17–18) and conditions
(19). With two developers the project completes Sprints 01–20 in
roughly the time one developer takes for Sprints 01–14.

---

*This sprint plan is committed against PLAN.md and MANIFESTO.md.
Sprint retrospectives may revise sprints 17+ — sprints 01–16 are
intentionally locked.*
