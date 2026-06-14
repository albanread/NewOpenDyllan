# Architecture: a Dylan front-end on a Rust + LLVM back-end

NewOpenDylan is a **Dylan front-end on a Rust + LLVM back-end, split at
the DFM intermediate representation.**

The front-end — lexer, parser, macro expander, semantic analysis, and
AST → DFM lowering — is written in Dylan and self-hosts. The back-end —
DFM → LLVM codegen, the garbage collector, the JIT, the AOT linker, the
runtime, and the FFI plumbing — is Rust + LLVM, permanently. **DFM IR is
the contract between them.**

This is the same division `rustc` draws (Rust front-end, LLVM back-end)
and that GHC draws (Haskell front-end, native/LLVM back-end): the
language hosts the parts that benefit from the language; the systems
substrate hosts the parts that benefit from the substrate. Neither side
tries to be the other.

## The boundary: DFM IR

Everything in the architecture hangs off one decision: the front-end and
back-end meet at **DFM** (the Dylan Flow Machine, a typed-SSA
control-flow-graph IR — see [the DFM reference](compiler/dfm.md)).

```
  Dylan source
       │
  ┌────┴───────────────────────────────────┐
  │  FRONT-END  (Dylan, self-hosting)       │
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

DFM is reviewable text at every phase (`nod-driver dump-dfm`). The
back-end consumes DFM and nothing above it: it neither knows nor cares
how the source became DFM, only that the IR is well-formed. That clean
cut is what lets the front-end live in Dylan while the back-end lives in
Rust + LLVM.

## What lives where

### Front-end — Dylan

The front-end is written in Dylan. Its sources live under
[`compiler/`](../compiler/):

| Phase                 | Dylan source                                     |
|-----------------------|--------------------------------------------------|
| **Lexer**             | `compiler/dylan-lexer.dylan`                      |
| **Parser**            | `compiler/dylan-parser.dylan`                     |
| **Macro expander**    | `compiler/dylan-macro.dylan`                      |
| **Semantic analysis** | `compiler/dylan-sema.dylan`, `compiler/dylan-c3.dylan` |
| **AST → DFM lowering**| `compiler/dylan-lower.dylan`                      |

The front-end runs through to DFM and hands that IR to the back-end. (A
few lowering constructs are still being completed — see
[known limitations](reference/known-limitations.md).)

### Back-end — Rust + LLVM, permanent

| Component          | Crate                          | Role                                                                |
|--------------------|--------------------------------|--------------------------------------------------------------------|
| DFM → LLVM codegen | `nod-llvm`                     | Emits LLVM IR for DFM and drives machine-code generation.          |
| DFM optimizer      | `nod-opt`                      | DFM-level optimization passes before codegen.                      |
| Garbage collector  | `nod-runtime` · `newgc-core`   | Precise generational collector; `unsafe`, raw pointers, `gc.statepoint` lowering, TLABs. |
| JIT engine         | `nod-llvm`                     | LLVM MCJIT, in-image, with Win64 SEH registration.                 |
| AOT linker         | `nod-driver`                   | Object-file emission + `link.exe` orchestration.                   |
| Runtime            | `nod-runtime`                  | Class metadata, dispatch caches, conditions, the tagged-`Word` value representation. |
| FFI plumbing       | `nod-runtime` · `nod-winapi`   | Win64 calling-convention dispatcher, callback bridge, COM.         |
| Loader             | `nod-loader`                   | Object/bitcode loading for the JIT cache.                          |
| Driver / CLI       | `nod-driver`                   | The command-line front door: `eval`, `build`, the `dump-*` commands, project files. |

The back-end is **not** a bootstrap scaffold to be retired. It is the
permanent native substrate, maintained in Rust + LLVM. Codegen is "emit
good IR for LLVM," not a place Dylan adds value; the GC, JIT, and linker
are systems code that wants `unsafe`, raw pointers, and a C/C++ ABI
surface.

## How the Dylan front-end is built

The front-end is Dylan source, so it is compiled the same way any Dylan
program is — by this compiler's own back-end — and then linked into the
driver:

1. **The Dylan front-end is AOT-compiled as a static library.**
   `nod-driver build --library` produces a `.obj` with source-language
   symbol names preserved, no synthetic `main`, and the relocation
   resolver promoted to an external symbol. This artifact is "the shim."
2. **The shim is static-linked into `nod-driver`.** The build script
   finds the `.obj`, hands it to the linker, and sets a cfg flag. The
   front-end's entry points are now `extern "C"` symbols in the driver
   process.
3. **The front-end hands its output to the back-end across a wire
   format.** Output crosses the Dylan↔Rust boundary as a flat,
   fixed-shape record stream — never a shared data structure (see
   [wire-format discipline](#wire-format-discipline) and the
   [self-hosting](compiler/self-hosting.md) page).

This is genuine self-hosting: the compiler's own front-end is one of the
Dylan programs it compiles.

## Wire-format discipline

The hard-won rule: **across the front-end/back-end boundary you do not
pass a data structure — you pass bytes both sides agree on.** Inside one
process in one language, handing someone a `Vec<Token>` is free; the type
*is* the interface. The instant the boundary is a compiled-language seam
(different allocator, different type system, different notion of
"string"), the data structure on one side does not exist on the other.
The only shared thing is a byte layout — and that layout *is* the
interface.

So every front-end output that crosses the seam has a **wire-format spec
agreed before either side's code**, and neither the Dylan emitter nor the
back-end reader is allowed to disagree with it. The patterns that earned
their place:

- **Lock the contract first.** The wire formats are documented and both
  sides obey them; specifying the wire up front is why the emitters match
  the readers.
- **Cheapest shape that carries the data, not the most natural.** Rich
  Dylan `<ast-*>` classes flatten to fixed-size integer records
  (`kind, span_lo, span_hi, subtree_size`); the back-end reconstructs
  whatever local shape it needs. Each side's type system stays a private
  implementation detail.
- **Spans, not values, when both sides hold the source.** The emitter
  ships `(VariableRef 527..537)`; the reader does `&src[527..537]`. The
  source string is the blob both already have; the wire carries indices
  into it. Don't ship data you can address.

See [self-hosting](compiler/self-hosting.md) for the wire-format
specifications.

## Design invariants

- **64-bit-first, Windows-first, macOS-second.** See [platforms](reference/platforms.md).
- **JIT-first, no image on disk.** Programs JIT in-image (`eval`) or
  AOT-compile to a standalone Win64 `.exe` (`build`); there is no
  serialized heap image to load.
- **Our own precise GC.** The collector is precise (not conservative),
  generational, and consumes safepoint roots emitted by the JIT/AOT.
- **Sealing as a first-class concern.** Sealing drives compile-time
  dispatch resolution; it is part of the language and the performance
  story, not an afterthought. See [dispatch & sealing](compiler/dispatch-and-sealing.md).
- **The IDE is a Dylan program.** It is written in Dylan and calls Win32
  directly through `c-ffi`, compiled by our own back-end.
- **We do not consume OpenDylan compiler artifacts.** Not DFMC output,
  not HARP, not `.img`/`.fasl`. The Dylan front-end is written fresh
  against the Dylan Reference Manual and compiled by our own back-end.
  Upstream Open Dylan is a [reference and parts catalogue](reference/upstream-opendylan.md), never a transplant donor.
- **The back-end stays Rust + LLVM.** "Front-end in Dylan" is not a step
  toward "everything in Dylan." DFM is the floor. Codegen, GC, the JIT,
  and the linker are Rust + LLVM for the life of the project.

---
*Next: [compiler overview](compiler/overview.md) · [DFM IR](compiler/dfm.md) · [self-hosting](compiler/self-hosting.md) · [docs home](README.md)*
