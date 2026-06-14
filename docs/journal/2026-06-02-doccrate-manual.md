# 2026-06-02 — A diagram-rich manual, browsable in DocCrate

*Stood up `docs/manual/` — a 24-page, diagram-rich tour of the Dylan
language and the NewOpenDylan compiler — and wired DocCrate
(`tools/doccrate/`) as the native viewer + a `--testsnap`→PNG render gate.
Agents researched each subsystem from source; every page was reviewed
against the code and visually verified before it landed.*

## Goal

The `docs/` tree is rich in *design notes* (ARCHITECTURE, MANIFESTO, the
197 KB SPRINTS log, gap write-ups) but had no clean, navigable,
*explanatory* manual — and nothing exploiting diagrams. Build one: document
the language and the compiler in detail, with Mermaid diagrams, browsable
offline in DocCrate.

## What we did

1. **Vendored DocCrate** (the native Direct2D Markdown viewer) into `tools/doccrate/` as a
   self-contained `doc-crate.exe`, plus `Browse-Docs.ps1` (open the manual)
   and `Test-Render.ps1` — which drives `doc-crate.exe --testsnap <page.md>`,
   captures the `screen.png` it emits, kills the GUI, and archives the PNG.
   That PNG is the **verification gate**: a broken diagram renders as a
   visible `mermaid error:` line, so every page was eyeballed before it shipped.
2. **Wrote the authoring contract** (`tools/doccrate/AUTHORING.md`) after
   probing what Selkie actually renders. Hard constraints, learned the hard
   way: **no `subgraph`** (label degrades to a vertical column of chars),
   **no `<br/>`** (prints literally), classDiagram generics use **tildes**
   (`Vec~T~`), sequence diagrams have **no `loop`/`alt`/`opt`/`par`** yet,
   wide inheritance must be **split**. Use `{{hexagon}}` to mark the DFM cut line.
3. **Fanned out research agents**, one page per subsystem, in four waves
   (front-end compiler, back-end compiler, core language, systems language),
   each bound to the contract + a gold-standard `compiler/overview.md` + precise
   source pointers. **Reviewed every page against the source** (cite
   `crate/src/file.rs:LINE`), test-rendered each, fixed.
4. **24 pages**: `index`, `getting-started`, `glossary`; `compiler/`
   (overview, reader, macro-expander, sema, namespace, dfm, codegen,
   jit-and-aot, runtime, gc, ffi, driver, self-hosting); `language/`
   (overview, syntax, types-and-classes, generic-functions, macros,
   modules-and-libraries, conditions, sealing). All 285 internal links
   resolve; DocCrate's recursive scan auto-groups the subfolders in the sidebar.

## Discovered

Reviewing agent drafts against source turned up several places where the
**implementation has moved past the high-level docs** — now corrected in the
manual (and worth fixing in the design docs):

- **The in-image JIT is LLVM MCJIT, not ORC.** `jit.rs:740` calls
  `LLVMCreateMCJITCompilerForModule`; `ARCHITECTURE.md` called it "LLVM ORC"
  (now corrected).
  The manual documents MCJIT and notes the discrepancy.
- **The GC layout that NewOpenDylan actually uses is `DylanLayout`**
  (`nod-runtime/src/dylan_layout.rs`); `LispLayout`/`TinyLayout` in
  `newgc-core` are the NewCormanLisp + test layouts. An agent initially
  documented the siblings.
- **FFI trampoline arity is 0..=12** (`winffi.rs:262`, `arg_kinds:[u8;12]`),
  not the README's stale 0..=8 — raised for `CreateWindowExW`.
- Honest implementation edges the manual now states plainly: only
  `instance:` slot allocation; `init-function:`/complex slot defaults unwired;
  `singleton`/`limited` absent; `select` unimplemented; `*` macro repetition
  absent (`cond` capped at 1–4 arms); `define library`/`module` parse but
  use/export resolution is stubbed (`graph.rs:3`); restart invocation a stub.

The cross-check cuts both ways, same as verify-mode: two independent readings
(agent draft + my source review) disagree exactly where a doc is stale.

## Where it leaves us

- `pwsh tools/doccrate/Browse-Docs.ps1` opens the whole manual. Editing a
  page? `pwsh tools/doccrate/Test-Render.ps1 -File docs/manual/<page>.md`
  and read the PNG.
- Snapshot PNGs are gitignored (`tools/doccrate/snaps/`). The 8.5 MB
  `doc-crate.exe` is currently untracked under `tools/` — decide whether to
  commit it or gitignore it.
- Optional follow-ups: link the manual from `README.md`; correct the
  "ORC"→MCJIT wording in README/ARCHITECTURE; add an IDE-in-Dylan tour page.
  The front-end migration page (`docs/manual/compiler/self-hosting.md`) will
  want updating as macro/sema/lowering move to Dylan.
