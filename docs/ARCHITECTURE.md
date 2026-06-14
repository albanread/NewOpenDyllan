# NewOpenDylan — Architecture: Dylan front-end, Rust/LLVM back-end

*Drafted 2026-05-31, ratifying what Sprints 45–51 proved in code.*

This is the canonical statement of NewOpenDylan's permanent shape. It
**supersedes** the earlier "the compiler stays in Rust forever, Dylan
is only stdlib + apps" framing in [`MANIFESTO.md`](MANIFESTO.md)'s
Bootstrap section and [`PLAN.md`](PLAN.md) §2.7. Those documents have
been updated to point here; this is the source of truth for the
front-end / back-end division.

## The one-sentence version

**NewOpenDylan is a Dylan front-end on a Rust + LLVM back-end, split
at the DFM intermediate representation.** The front-end (lexer, parser,
macro expander, semantic analysis, AST → DFM lowering) migrates to
Dylan and self-hosts. The back-end (DFM → LLVM codegen, the garbage
collector, the JIT, the AOT linker, the runtime, the FFI plumbing)
stays in Rust + LLVM permanently. DFM IR is the contract between them.

This is the same division `rustc` draws (Rust front-end, LLVM
back-end), that GHC draws (Haskell front-end, native/LLVM back-end),
that every mature self-hosting-front-end compiler draws. The language
hosts the parts that benefit from the language; the systems substrate
hosts the parts that benefit from the substrate. Neither side tries to
be the other.

## Why this, and why now

The original plan (PLAN.md §2.7) deliberately ruled out self-hosting:
"NewOpenDylan is a Rust compiler for Dylan that ships a Dylan standard
library. The same arrangement Java has (HotSpot is C++, `java.lang` is
Java)." That was the right call **for the bring-up years** — writing a
parser, sema, and lowering in Rust got us to a working JIT without a
chicken-and-egg bootstrap.

Then Sprints 45–51 happened faster than projected:

- **Sprint 45** wrote the Dylan lexer *in Dylan*, as a corpus exercise.
- **Sprint 46** wrote the Dylan parser *in Dylan*, and it parsed the
  whole test corpus.
- **Sprint 51b** JIT-strapped the Dylan lexer into `nod-driver` and
  ran the front-end through it — byte-identical to the Rust lexer.
- **Sprint 51c** ran the Dylan parser in verify-mode against the Rust
  parser; they agreed on every fixture (and the one divergence was a
  *Rust* parser gap the Dylan parser had already got right).
- **Sprint 51d** had the Dylan parser emit a real AST across a wire
  format that the Rust side decodes.

The empirical result: **the Dylan front-end works, three years ahead
of the original "year-3 self-hosting" guess.** The insight that
unlocked it — *"code gen is code gen; if the Dylan front-end produces
the same DFM the Rust front-end does, LLVM emits the same machine
code"* — means there is **no reason to ever port the back-end**. DFM
is the cut line. Below it, Rust + LLVM is not a temporary scaffold; it
is the permanent, correct home for codegen, GC, and the linker.

So the architecture is reframed, not because the old plan failed, but
because the front-end migration succeeded early and revealed the
durable shape underneath.

## The boundary: DFM IR

Everything in the architecture hangs off one decision: **the front-end
and back-end meet at DFM** (the Dylan Flow Machine typed-SSA IR, see
[`DFM.md`](DFM.md)).

```
  Dylan source
       │
  ┌────┴───────────────────────────────────┐
  │  FRONT-END  (migrating to Dylan)        │
  │                                         │
  │  lex → parse → macro-expand → sema →    │
  │  AST → DFM lowering                     │
  └────┬───────────────────────────────────┘
       │   DFM IR  ◄── the permanent contract
  ┌────┴───────────────────────────────────┐
  │  BACK-END  (Rust + LLVM, permanent)     │
  │                                         │
  │  DFM → LLVM codegen → { JIT | AOT obj } │
  │  garbage collector (precise, gc.statepoint)
  │  runtime (classes, dispatch, conditions)│
  │  FFI plumbing (Win64 c-ffi, callbacks)  │
  │  AOT linker (object emission + link.exe)│
  └─────────────────────────────────────────┘
       │
  machine code (JIT in-image, or a Windows EXE)
```

DFM is reviewable text at every phase (`nod-driver dump-dfm`). Because
both a Rust-emitted DFM module and a Dylan-emitted DFM module are *the
same data structure with the same semantics*, the back-end cannot tell
which front-end produced it — and does not care. That is what makes the
migration safe: each front-end phase can be swapped to Dylan and
validated against the Rust phase by comparing output (tokens, AST,
DFM), with the back-end held constant.

## What lives where

### Front-end — migrating to Dylan, self-hosting

| Phase            | Status                          | Dylan source                                   |
|------------------|---------------------------------|------------------------------------------------|
| **Lexer**        | ✅ live (`--lex-with-dylan`)     | `tests/nod-tests/fixtures/dylan-lexer.dylan`   |
| **Parser**       | ✅ **default** in the real pipeline (`--parse-with-rust` opts out; Rust = fall-back + verify oracle) | `…/dylan-parser.dylan` |
| **Macro expander** | ✅ Rust default; Dylan port **live (opt-in)** via `NOD_EXPAND_WITH_DYLAN` (Sprint 52) | `…/dylan-macro*.dylan` |
| **Sema / namespace** | ✅ **load-bearing (opt-in)** via `--sema-with-dylan` / `NOD_SEMA_WITH_DYLAN` (Sprint 54): the back-end consumes the Dylan `SemaModel`, gated by `dump-sema` byte-match (38/38). Rust stays the default until Sprint 56. | `…/dylan-sema.dylan` |
| **AST → DFM lowering** | ◐ **load-bearing (opt-in)** via `--lower-with-dylan` / `NOD_LOWER_WITH_DYLAN` (Sprint 55): the Dylan lowering emits DFM (as `dump-dfm` text, re-parsed host-side) that the same Rust back-end passes consume. 55a stmts/exprs + 55b slot accessors / `instance?` byte-match (≈15 corpus fixtures, 0 wrong dumps); 55b `make`/dispatch + 55c closures/blocks still fall back to Rust. | `…/dylan-lower.dylan` |

The front-end's eventual home is Dylan source compiled by our own
back-end. Until each phase lands in Dylan, its Rust implementation
remains the shipping path; the Dylan version runs alongside in
verify-mode first, then takes over behind a `--…-with-dylan` flag, then
becomes the default.

### Back-end — Rust + LLVM, permanent

| Component         | Crate            | Why it stays Rust                                   |
|-------------------|------------------|-----------------------------------------------------|
| DFM → LLVM codegen | `nod-llvm`      | LLVM's API is C/C++; inkwell is the Rust binding. Codegen is "emit good IR for LLVM," not a place Dylan adds value. |
| Garbage collector | `nod-runtime`    | Precise GC needs `unsafe`, raw pointers, `gc.statepoint` lowering, TLABs. Systems code. Shared with the sibling portfolio. |
| JIT engine        | `nod-llvm`       | LLVM MCJIT + Win64 SEH registration. C++ ABI surface. (A future move to ORC v2 LLJIT is noted in `DEFERRED.md`.) |
| AOT linker        | `nod-driver`     | Object-file emission + `link.exe` orchestration.    |
| Runtime           | `nod-runtime`    | Class metadata, dispatch caches, conditions, the tagged-`Word` representation. |
| FFI plumbing      | `nod-runtime` / `nod-winapi` | Win64 calling-convention dispatcher, callback bridge. |

The back-end is **not** a bootstrap scaffold to be retired. It is the
permanent native substrate, maintained in Rust, shared with NewM2 /
NewCP / NewCormanLisp / NewBCPL / NewFB where the code is portfolio-
common (the GC core, the JIT memory manager, the Windows FFI stack).

## The migration mechanism (proven, Sprint 51b–d)

Each front-end phase migrates to Dylan by the same five-step pattern.
This is the repeatable cadence, not a one-off:

1. **Write the phase in Dylan.** It already runs as a corpus exercise
   (the lexer and parser did).
2. **AOT-compile it `--library`.** `nod-driver build --library`
   produces a `.obj` in `AotShape::StaticLibrary` mode: source-language
   symbol names preserved, no synthetic `main`, the resolver
   (`nod_aot_resolve_relocs`) promoted to an external symbol.
3. **Static-link it into `nod-driver`.** `build.rs` finds the `.obj`,
   passes it to the linker, and sets a cfg flag. The phase's entry
   points are now `extern "C"` symbols in the driver process — the
   NewCP "M2 modules replace Rust" pattern, applied to ourselves.
4. **Bridge across a wire format.** Front-end output crosses the
   Rust↔Dylan boundary as a flat, fixed-shape record stream — never a
   shared data structure (see "Wire-format discipline" below).
   `docs/DYLAN_TOKEN_WIRE.md` (tokens) and `docs/DYLAN_AST_WIRE.md`
   (AST) are the locked contracts.
5. **Gate behind a flag, verify, then default.** A `--…-with-dylan`
   flag (and `NOD_…` env var) selects the Dylan phase. **Verify-mode**
   runs both implementations and compares output, surfacing any
   divergence loudly, before the Dylan phase becomes the default and
   the Rust phase is retired.

Two failure-safety properties fall out of this for free:

- **No-shim builds still work.** A fresh checkout without the `.obj`
  compiles fine; the flag prints a "build the shim first" message and
  falls back to the Rust phase. The Dylan front-end is opt-in until
  it's the default.
- **Verify-mode catches both directions.** It found a *Rust* parser
  bug (a missing `cond` form the Dylan parser handled) on its first
  run. Two front-ends that must agree is a stronger correctness signal
  than either alone — "two compilers are better than one."

## Wire-format discipline

The one hard-won lesson: **across the front-end/back-end boundary, you
do not pass a data structure — you pass bytes both sides agree on.**
Inside one process in one language, "hand someone a `Vec<Token>`" is
free; the type *is* the interface. The instant the boundary is a
compiled-language seam (different allocator, different type system,
different notion of "string"), the data structure on one side does not
exist on the other. The only shared thing is a byte layout — and that
layout *is* the interface.

So every front-end phase that crosses the seam gets a **wire-format
spec committed before either side's code**, and neither the Dylan
emitter nor the Rust reader is allowed to disagree with it. Patterns
that earned their place:

- **Lock the contract first.** `DYLAN_TOKEN_WIRE.md` /
  `DYLAN_AST_WIRE.md` exist as docs that both sides obey. The spec
  going up first is why the lexer and parser emitters matched the Rust
  readers on first try.
- **Cheapest shape that carries the data, not the most natural.** Rich
  Dylan `<ast-*>` classes flatten to fixed-size integer records
  (`kind, span_lo, span_hi, subtree_size`); the host reconstructs
  whatever local shape it needs. Each side's type system is a private
  implementation detail.
- **Spans, not values, when both sides hold the source.** The emitter
  ships `(VariableRef 527..537)`; the reader does `&src[527..537]`. The
  source string is the blob both already have; the wire carries indices
  into it. Don't ship data you can address.

## What this changes in the older docs

- **MANIFESTO.md** core decision 1 and the Bootstrap section: the
  "reader, macro expander, sealing analyser, IR builder, compiler
  driver … written in Rust" list described the *bring-up* state. The
  permanent state is: front-end → Dylan, back-end (GC, codegen, JIT,
  linker, runtime, FFI) → Rust. The IDE-in-Dylan and Win32-from-Dylan
  commitments are unchanged and now sit naturally alongside a
  front-end-in-Dylan commitment.
- **PLAN.md §2.7** "The bootstrap question": the conclusion flips from
  "not self-hosting, ever" to "front-end self-hosts; back-end is
  permanent Rust." The *reasons* given in §2.7 for not consuming
  OpenDylan's artifacts remain valid — our Dylan front-end is **our**
  code compiled by **our** back-end, not a downstream consumer of
  upstream's IR.

## What this does NOT change

- **64-bit-first, Windows-first, macOS-second, JIT-first, no image on
  disk, our own precise GC, sealing as a first-class concern, IDE
  written in Dylan calling Win32 through `c-ffi`.** All MANIFESTO
  commitments outside the bootstrap question stand.
- **We still do not consume OpenDylan compiler artifacts.** Not DFMC
  output, not HARP, not `.img`/`.fasl`. The Dylan front-end is written
  fresh against the DRM, compiled by our own back-end.
- **The back-end is not going anywhere.** "Front-end in Dylan" is not a
  step toward "everything in Dylan." DFM is the floor. Codegen, GC,
  JIT, and the linker are Rust + LLVM for the life of the project.

## Roadmap (front-end migration)

| Sprint band | Phase migrating to Dylan        | Gate                                  |
|-------------|---------------------------------|---------------------------------------|
| 45–46       | lexer, parser (corpus exercise) | parse the whole corpus                |
| 51b         | lexer (live)                    | byte-identical to Rust lexer          |
| 51c         | parser (verify)                 | accept/reject agreement on corpus     |
| 51d         | parser (AST emit)               | `dump-dylan-ast` round-trips          |
| 51e         | parser → **default** in the real pipeline | Dylan parser is the default; Rust = fall-back + verify oracle; 28/36 corpus byte-identical (macro fixtures close in 52); full sweep green |
| 52+         | macro expander → Dylan          | verify-mode against Rust expander     |
| 53+         | sema / namespace → Dylan        | verify-mode against Rust sema         |
| 54 ✅       | sema **load-bearing** (`--sema-with-dylan`); back-end consumes the Dylan `SemaModel` | **done** — `dump-dfm` byte-identical from the Dylan sema model (38/38) |
| 55 ◐        | AST → DFM lowering → Dylan; flip **load-bearing** via `--lower-with-dylan`. 55a stmts/exprs ✅, 55b slots/`instance?` ✅ (`make`/dispatch remaining), 55c closures/blocks remaining | `dump-dfm` byte-identical Dylan-vs-Rust on ≈15 fixtures; the rest fall back to Rust |
| 56          | consolidation: one DFM handoff, `--…-with-dylan` flags retired | full sweep green at default; the per-stage shim round-trips (the ~50× cost) gone |
| —           | **front-end fully self-hosted** | every phase Dylan, back-end unchanged |

**Pre-54 prerequisite (the on-ramp):** finish the 53.x Dylan sema
recording walk, and fix the shim class-id drift (`aot.rs` registration
determinism). Everything from 54 on routes through the static-linked
Dylan shim, so deterministic class/symbol identity across the wire
crossing is a hard precondition, not a polish item. Detailed phasing:
[`journal/2026-06-06-roadmap-54-56.md`](journal/2026-06-06-roadmap-54-56.md).

When the last row lands, NewOpenDylan compiles its own front-end with
its own back-end, and the only Rust in the compile path is the part
that should always have been Rust: the machine-code generator, the
collector, and the linker.

---

*This document is the architecture's source of truth. The schedule
lives in [`SPRINTS.md`](SPRINTS.md); the design line that does not move
lives in [`MANIFESTO.md`](MANIFESTO.md); the IR contract lives in
[`DFM.md`](DFM.md); the wire contracts live in `DYLAN_TOKEN_WIRE.md`
and `DYLAN_AST_WIRE.md`.*
