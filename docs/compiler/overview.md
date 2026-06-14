# Compiler Overview

NewOpenDylan turns Dylan source into running machine code two ways — **JIT**
(compile and execute in-image, for `eval` and the REPL) and **AOT** (emit an
object file and link a standalone Win64 `.exe`). Both share one pipeline and
diverge only at the very end.

The pipeline JITs and AOT-builds non-trivial programs. The front-end — lexer,
parser, macro expander, semantic analysis, and AST→DFM lowering — is written in
Dylan. See [Self-hosting](self-hosting.md).

## The pipeline

Source flows top-to-bottom through the front-end, crosses the **DFM** boundary,
and is turned into code by the back-end:

```mermaid
flowchart TD
    SRC[Dylan source .dylan] --> LEX[Lexer - tokens]
    LEX --> PAR[Parser - AST]
    PAR --> MAC[Macro expander - expanded AST]
    MAC --> SEMA[Sema - resolve, type, plan dispatch]
    SEMA --> LOW[AST to DFM lowering]
    LOW --> DFM{{DFM IR - typed SSA - the cut line}}
    DFM --> CG[LLVM codegen - LLVM IR]
    CG --> FORK{JIT or AOT?}
    FORK -->|eval / REPL| JIT[MCJIT - run in-image]
    FORK -->|build| OBJ[Emit .obj]
    OBJ --> LNK[link.exe - resolve IAT]
    LNK --> EXE[Standalone Win64 .exe]
```

The library/module graph ([`nod-namespace`](namespaces.md)) feeds the front-end
the namespace context — which library a file belongs to and what each module
imports — but is not itself a stage in the per-expression flow above.

## The crate map

Each box above is owned by a crate. The split between "front-end" and "back-end"
is the DFM line: the Dylan front-end sits above it (its sources live under
`compiler/`), the Rust + LLVM back-end below. The crates below are the
back-end-side reference implementations and boundary plumbing — the front-end
proper is the Dylan source.

| Stage | Crate | Side | Page |
|-------|-------|------|------|
| Lexer + Parser (reference) | `nod-reader` | back-end-side reference | [Reader](reader.md) |
| Macro expander (reference) | `nod-macro` | back-end-side reference | [Macros](macro-expander.md) |
| Sema + lowering (reference) | `nod-sema` | back-end-side reference | [Sema](sema.md) |
| Library/module graph | `nod-namespace` | back-end-side reference | [Namespaces](namespaces.md) |
| DFM IR | `nod-dfm` | the boundary | [DFM](dfm.md) |
| DFM optimizer | `nod-opt` | back-end | [DFM](dfm.md) |
| LLVM codegen | `nod-llvm` | back-end | [Codegen](codegen.md) |
| JIT + AOT object/link | `nod-llvm`, `nod-driver` | back-end | [JIT & AOT](jit-and-aot.md) |
| Runtime, dispatch, conditions | `nod-runtime` | back-end | [Runtime](runtime.md) |
| Garbage collector | `newgc-core` | back-end | [GC](gc.md) |
| Win32 FFI, callbacks, COM | `nod-winapi`, `nod-runtime` | back-end | [FFI](ffi.md) |
| Live-binding loader | `nod-loader` | back-end | [Driver](driver.md) |
| CLI / REPL / build orchestration | `nod-driver` | driver | [Driver](driver.md) |

The twelve crates split cleanly across the DFM boundary. The Dylan front-end
(`compiler/dylan-lexer.dylan`, `dylan-parser.dylan`, `dylan-macro.dylan`,
`dylan-sema.dylan`, `dylan-c3.dylan`, `dylan-lower.dylan`) produces DFM; the
back-end crates (`nod-opt`, `nod-llvm`, `nod-runtime`, `newgc-core`,
`nod-winapi`, `nod-loader`) consume it. The reference crates `nod-reader`,
`nod-macro`, `nod-sema`, and `nod-namespace` document the same phases on the
back-end side. `nod-dfm` is the contract that sits between front-end and
back-end, and `nod-driver` is the conductor that wires them all together —
the compiled Dylan front-end is linked into it as a static-library shim.

## Two execution modes

The single fork in the pipeline is whether codegen's output is executed
in-process or written to disk and linked:

- **JIT** (`eval`, REPL) — codegen hands LLVM IR to the MCJIT, which compiles it,
  registers Win64 SEH unwind info, and calls it inside the compiler process. The
  stdlib is pre-compiled into a long-lived JIT engine on first eval. See
  [JIT & AOT](jit-and-aot.md).
- **AOT** (`build`) — codegen injects a synthetic `main`, emits a `.obj` via
  LLVM's `TargetMachine`, and links it against `nod_runtime.lib` with `link.exe`,
  resolving Win32 imports through the IAT. Multi-file builds merge every file's
  AST into one module *before* lowering, so definitions are visible across files.
  See [Driver](driver.md).

## Inspecting the pipeline

The driver (`nod-driver`) exposes every stage as a dump command — the best way
to learn the compiler is to watch one expression flow through them:

| Command | Stops after | Shows |
|---------|-------------|-------|
| `nod-driver dump-tokens f.dylan` | lexer | the token stream |
| `nod-driver dump-ast f.dylan` | parser | the AST |
| `nod-driver dump-graph f.lid` | namespace | library/module graph (Graphviz) |
| `nod-driver dump-dfm f.dylan` | lowering | the DFM IR |
| `nod-driver dump-llvm f.dylan` | codegen | textual LLVM IR |
| `nod-driver eval '1 + 1'` | JIT | the evaluated result |
| `nod-driver build f.dylan -o f.exe` | link | a standalone `.exe` |

Each of these commands runs the Dylan front-end and stops at the named stage.
The full reference is on [Driver](driver.md).

## The DFM boundary, in one paragraph

DFM (the Dylan Flow Machine IR) is reviewable text at `dump-dfm`. The Dylan
front-end emits DFM and the back-end consumes it; the two meet only at this
data structure. Because DFM is a fixed contract — the same data structure with
the same semantics regardless of who produced it — the front-end and back-end
evolve independently behind it. The IR contract is [DFM](dfm.md); how the Dylan
front-end is built and linked in is [Self-hosting](self-hosting.md).

## Where in the code

| Path | Lines | Responsibility |
|------|-------|----------------|
| `src/nod-driver/src/main.rs` | ~1500 | CLI, subcommand dispatch, every dump path, the build pipeline |
| `src/nod-reader/src/` | ~6900 | lexer, parser, AST, Dylan pretty-printer |
| `src/nod-sema/src/lower.rs` | ~7800 | the AST → DFM lowering core |
| `src/nod-dfm/src/ir.rs` | ~730 | the DFM data types |
| `src/nod-llvm/src/codegen.rs` | ~5000 | DFM → LLVM IR |
| `src/nod-runtime/src/` | ~25000 | object model, dispatch, conditions, FFI, GC glue |
| `src/newgc-core/src/page_heap/` | ~9000 | the collector |

## See also

- [DFM: the IR](dfm.md) — the contract the whole architecture hinges on
- [Self-hosting](self-hosting.md) — how the Dylan front-end is compiled and linked in
- [Driver](driver.md) — the CLI, build modes, and dump subcommands
- [Architecture](../architecture.md) — the canonical architecture statement
- [Language overview](../language/overview.md) — what the compiler is compiling

---
Next: [DFM: the IR](dfm.md) · See also [Architecture](../architecture.md) · [Self-hosting](self-hosting.md)
