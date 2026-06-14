# Getting Started

How to build NewOpenDylan, evaluate an expression, watch the pipeline, and compile a
Dylan program to a standalone Win64 `.exe`. Everything here runs from the workspace root.

> **Status:** NewOpenDylan is a work-in-progress. `cargo build --workspace` is green and
> the commands below work, but this is a design diary with running code, not a release.
> JIT (`eval`) is the primary path; AOT (`build`) works in **debug mode** today.

## Prerequisites

- **Windows 10/11, 64-bit.** The runtime and FFI are Windows-first.
- **Rust** (stable) — install from [rustup.rs](https://rustup.rs).
- **LLVM 22.1** — the back-end links against it (configured in `.cargo/config.toml`).
- **`link.exe`** (the MSVC linker, from Visual Studio Build Tools) — only needed for AOT
  `build`.

## A first session

```mermaid
flowchart LR
    BUILD[cargo build --workspace] --> EVAL[eval - run an expression]
    EVAL --> DUMP[dump-dfm - watch the pipeline]
    DUMP --> WRITE[write a .dylan file]
    WRITE --> EXE[build - make an .exe]
```

## Build the compiler

```
cargo build --workspace
```

This builds every crate — the front-end (`nod-reader`, `nod-macro`, `nod-sema`,
`nod-namespace`), the IR (`nod-dfm`), the back-end (`nod-llvm`, `nod-runtime`,
`newgc-core`), and the driver (`nod-driver`). See the [compiler overview](compiler/overview.md)
for the map.

## Run an expression

The fastest way to see the compiler work end-to-end is to JIT one expression:

```
cargo run -p nod-driver -- eval "1 + 1"
```

`eval` parses, expands macros, runs sema, lowers to DFM, generates LLVM IR, JIT-compiles
it with [MCJIT](compiler/jit-and-aot.md), and prints the result.

## Watch the pipeline

The driver exposes every stage as a dump command — run them on a fixture to watch one
program flow through the compiler:

```
cargo run -p nod-driver -- dump-tokens tests/nod-tests/fixtures/factorial.dylan
cargo run -p nod-driver -- dump-ast    tests/nod-tests/fixtures/factorial.dylan
cargo run -p nod-driver -- dump-dfm    tests/nod-tests/fixtures/factorial.dylan
cargo run -p nod-driver -- dump-llvm   tests/nod-tests/fixtures/factorial.dylan
```

Each stops one step deeper: tokens → AST → DFM IR → LLVM IR. The full list is on the
[driver page](compiler/driver.md).

## Your first program

A minimal Dylan program (`tests/nod-tests/fixtures/hello.dylan`):

```dylan
Module: hello

define function main () => ()
  format-out("hello\n");
end function main;
```

Every source file opens with a `Module:` header (see
[modules & libraries](language/modules-and-libraries.md)). The
[`area-shapes.dylan`](language/generic-functions.md) fixture is a richer starting point —
classes, a generic function, and methods in 36 lines.

## Compile to a standalone `.exe`

```
cargo run --bin nod-driver -- build tests/nod-tests/fixtures/area-shapes.dylan -o area-shapes.exe
```

This emits an object file and links it against `nod_runtime.lib` into a standalone Win64
binary that needs no runtime installed. Multi-file builds merge every file's AST before
lowering; a `.prj` project file can hold the file list. See [JIT & AOT](compiler/jit-and-aot.md)
and the [driver page](compiler/driver.md).

> **Caveat:** release-mode AOT currently hits a linker collision (`LNK2005 nod_user_main`);
> debug-mode AOT works. The whole IDE (`nod-ide.exe`) is itself a Dylan program AOT-built
> this way.

## The Dylan-in-Dylan front-end

NewOpenDylan's lexer and parser are written *in Dylan* and run inside the driver. You can
exercise them directly:

```
cargo run -p nod-driver -- dump-dylan-tokens tests/nod-tests/fixtures/hello.dylan
cargo run -p nod-driver -- parse-dylan        tests/nod-tests/fixtures/factorial.dylan
cargo run -p nod-driver -- --verify-parse dump-ast tests/nod-tests/fixtures/point.dylan
```

`--verify-parse` runs the Dylan parser and the Rust parser side by side and asserts they
agree. This is the front-end [self-hosting](compiler/self-hosting.md) story.

## Browse this manual

```
pwsh tools/doccrate/Browse-Docs.ps1
```

This opens the manual in DocCrate, the native Markdown viewer bundled in
`tools/doccrate/`. To re-render a single page to a PNG (useful when editing):
`pwsh tools/doccrate/Test-Render.ps1 -File docs/manual/<page>.md`.

## Where to go next

- [Language overview](language/overview.md) — what Dylan is and the feel of the code.
- [Compiler overview](compiler/overview.md) — the pipeline and the crate map.
- [Glossary](glossary.md) — the vocabulary.

---
[Manual home](index.md) · [Language overview](language/overview.md) · [Compiler overview](compiler/overview.md)
