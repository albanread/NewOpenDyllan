# OpenDylanFE — Dylan front-end + Rust/LLVM back-end

A self-hosting Dylan compiler: the **front-end is written in Dylan**
(lexer, parser, macro expander, sema, AST→DFM lowering) and the
**back-end is Rust + LLVM** (DFM optimizer, LLVM codegen, AOT/JIT, GC
runtime).

This repository is the **going-forward** compiler. The earlier era — where
every Dylan front-end phase was developed by writing it in Dylan and
verifying it byte-for-byte against a parallel Rust implementation (the
"Rust oracle") — is **frozen** in the predecessor repo
[`NewOD`](https://github.com/albanread/NewOD) at the tag
**`rust-oracle-final`**. That mixed-phase build is the reference; new work
happens here.

## Architecture

```
  Dylan source
      │
      ▼
  ┌─────────────────────────── front-end (Dylan, in compiler/) ──────────┐
  │  lexer → parser → macro-expand → sema (+ C3) → AST→DFM lowering       │
  └──────────────────────────────────────────────────────────────────────┘
      │  DFM (Dylan Flow Model — the CFG IR)
      ▼
  ┌─────────────────────────── back-end (Rust + LLVM) ───────────────────┐
  │  DFM optimizer → LLVM codegen → AOT EXE / JIT ;  GC runtime           │
  └──────────────────────────────────────────────────────────────────────┘
```

- **Front-end** — `compiler/` (Dylan). See `compiler/README.md`. The
  lexer/parser are `include_str!`'d into `nod-driver`; the rest reach the
  driver through a statically-linked shim (`dylan-lex-shim.lib.obj`).
- **Back-end / runtime** — the Rust workspace under `src/`: `nod-dfm`
  (IR), `nod-opt` (optimizer), `nod-llvm` (codegen), `nod-loader`,
  `nod-runtime` + `newgc-core` (GC), `nod-winapi`, `nod-driver` (CLI).
- **Rust front-end (frozen fallback)** — `nod-reader` (lexer),
  `nod-macro`, `nod-namespace`, and the Rust sema/lowering in `nod-sema`
  remain as the per-file **fallback** the Dylan path bails to for
  constructs it doesn't cover yet. They are not developed further; they
  are retired once the Dylan front-end reaches full coverage.

## Status

| Stage | Dylan | Notes |
|---|---|---|
| Lexer | live | `--lex-with-dylan` |
| Parser | default | `--parse-with-rust` opts out |
| Macro expander | live | opt-in `NOD_EXPAND_WITH_DYLAN` |
| Sema / classes | load-bearing opt-in | `--sema-with-dylan` |
| AST→DFM lowering | partial | `--lower-with-dylan`; covers a subset, **bails to the Rust lowering** otherwise |
| Back-end (DFM→LLVM), GC | Rust | kept; not being ported |

**Near-term focus:** grow the Dylan lowering to full corpus coverage, then
make `--frontend-with-dylan` the default (`--frontend-with-rust` as the
opt-out), then retire the Rust front-end fallback.

## Build

```
cargo build --workspace
```

The Dylan front-end shim is an opt-in static library, built via the
no-shim bootstrap (see `compiler/README.md`):

```
cargo build -p nod-driver
./target/debug/nod-driver.exe build --library \
    --project compiler/dylan-lex-shim.prj \
    -o compiler/dylan-lex-shim.lib.obj
cargo build -p nod-driver        # build.rs links the .obj
```

`compiler/dylan-lex-shim.lib.obj` is a build artifact (git-ignored). On a
fresh checkout without it, the Dylan front-end flags fall back to Rust
with a clear message.

## Provenance

Seeded from `NewOD@rust-oracle-final` via `git archive` (a clean
snapshot; full history lives in the archive repo). Dual-licensed under
Apache-2.0 and MIT (`LICENSE-APACHE`, `LICENSE-MIT`).
