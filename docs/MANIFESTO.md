# NewOpenDylan — Manifesto and Declaration of Intent

*Drafted 2026-05-15*

The commitments behind NewOpenDylan. The plan in [`PLAN.md`](PLAN.md)
is the *how*; this is the *what we will not compromise on*.

> **Architecture (ratified 2026-05-31):** NewOpenDylan is a **Dylan
> front-end on a Rust + LLVM back-end**, split at the DFM IR. The
> front-end (lexer, parser, macros, sema, AST → DFM lowering) migrates
> to Dylan and self-hosts; the back-end (DFM → LLVM codegen, GC, JIT,
> AOT linker, runtime, FFI) stays Rust + LLVM permanently. This
> supersedes the original "compiler stays in Rust forever" framing in
> core decision 1 and the Bootstrap section below — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md) for the full statement and the
> Sprint 45–51 evidence that drove the change.

## What this is

NewOpenDylan is a **true revival** of the Dylan programming language —
a from-scratch Rust+LLVM implementation that returns Dylan to working
condition on modern 64-bit Windows (and then macOS), with a graphical
IDE, live inspection, and live incremental compilation built in from
day one. It is not a port, not a preservation effort, not a museum.
It is the Dylan that should exist in 2026.

We keep the language as the Dylan Reference Manual defines it. We
keep the demos, the test corpus, the spirit of CLOS-with-syntax. We
replace the implementation, the IDE, the GC, the runtime, and the
build story.

## The original, and why we are reviving it

The Dylan language emerged from Apple's Cambridge lab in the early
1990s as a serious attempt to give CLOS a syntax mainstream
programmers would accept. The technical result is one of the most
intellectually dense object-oriented languages ever shipped: multiple
dispatch, multiple inheritance with C3 linearisation, a complete
metaobject protocol, hygienic pattern macros, sealing-driven
optimisation, conditions and restarts, all built on first-class
functions and types-as-values.

The technology did not fail. The product did. Apple cancelled the
project in 1995. Harlequin's Dylan offering became Functional
Developer; Functional Developer became Open Dylan in 2003.
[`dylan-lang/opendylan`](https://github.com/dylan-lang/opendylan) is
the living open-source descendant, but its surface — Win32-only IDE,
HARP back-end with the LLVM port in slow progress, MPS GC, build
chain that requires a working Dylan compiler to bootstrap — has
drifted away from anything a curious 2026 developer can pick up in
an afternoon.

We do not need to invent Dylan. We need to make it runnable, hackable,
and visible again.

## Core decisions

NewOpenDylan is **compiler-first**, **Rust-for-the-substrate**,
**LLVM-based**, **64-bit-first**, **Windows-first**, and
**IDE-built-by-the-compiler**.

1. **Rust + LLVM for the back-end, Dylan for the front-end and the
   surface — split at the DFM IR.** The back-end — DFM → LLVM
   codegen, the garbage collector, the JIT, the AOT linker, the
   runtime (classes, dispatch, conditions), and the C/Win32 FFI
   plumbing — is written in safe Rust where possible and
   clearly-scoped `unsafe` where necessary, and **stays Rust + LLVM
   permanently**. Workspace lint `unsafe_op_in_unsafe_fn = "deny"` is
   inherited from our sibling projects.

   The **front-end** — lexer, parser, macro expander, semantic
   analysis / namespace resolution, AST → DFM lowering — was written
   in Rust to bring the system up, and **migrates to Dylan**,
   compiled by our own back-end. DFM IR is the contract between the
   two halves: a Dylan-emitted DFM module and a Rust-emitted DFM
   module are the same data structure with the same semantics, so the
   back-end is indifferent to which front-end produced it. Sprints
   45–51 proved this works — the Dylan lexer and parser run inside the
   driver today. See [`ARCHITECTURE.md`](ARCHITECTURE.md) and the
   revised Bootstrap section below.

   Everything user-facing on top of the back-end — the IDE, the live
   inspector, the library browser, the sealed-domain visualiser, the
   REPL surface, every window the user actually touches — is **Dylan
   code, compiled by our own compiler, calling Win32 directly through
   `c-ffi`**. See core decision 8. The front-end-in-Dylan commitment
   and the IDE-in-Dylan commitment are the same shape: Dylan hosts
   everything above the DFM/codegen line.

2. **No hand-written assembly.** Every piece of upstream Open Dylan
   that *had* to be assembly — call frames, GC barriers, multimethod
   dispatch shims, condition unwinding — is rewritten in Rust or
   lowered through LLVM. If a sequence demands specific instructions,
   it goes through `core::arch` intrinsics or LLVM IR, not `.asm`
   files.

3. **LLVM is the code generator.** Open Dylan's HARP back-end emits
   x86 directly; its experimental LLVM back-end is incomplete.
   NewOpenDylan goes Dylan → tokens → namespace-resolved AST →
   DFM-inspired typed SSA IR → LLVM IR → machine code, JIT-first,
   with reviewable textual dumps at every phase.

4. **64-bit from day one.** Tagged pointers, fixnum range, object
   layout, FFI marshalling, and GC layout all assume a 64-bit address
   space. No 32-bit build will be shipped.

5. **Windows-first, macOS second, not Windows-only.** First supported
   target is `x86_64-pc-windows-msvc`. Second is
   `aarch64-apple-darwin` (Apple Silicon) and
   `x86_64-apple-darwin`. The OS-specific surface — windowing,
   filesystem, threads, graphics — lives behind a thin Rust shim. The
   rest of the system has no platform awareness. macOS is not retrofit
   — it is the second milestone.

6. **JIT-first, with live incremental compilation as the central
   premise.** The compiler runs inside the live process. Save a file
   and the affected functions are recompiled, installed under the
   loader's generation discipline, and reachable from the REPL on the
   next call. There is no separate edit/build/run cycle. The compiler
   *is* the editor's evaluator.

7. **REPL and live inspection are first-class, not bolt-on.** The
   REPL has the same view of the image the compiler has. The
   inspector can drill into any live object — class, slot, method,
   generic, condition, stack frame — and follow references. Method
   tables, sealed-domain rosters, and dispatch caches are visible.
   This is what made Lisp and Smalltalk environments productive; we
   bring it back for Dylan.

8. **Compiler first. The IDE is built *by* the compiler, in
   Dylan, calling Win32 directly.** Open Dylan's MFC IDE
   (`E:\opendylan\sources\environment\` — editor `deuce`,
   debugger, profiler, project-wizard, property-pages, commands,
   reports, source-control, splash-screen, …) is the
   *re-implementation target*, not a port target. We rewrite it as
   a **Dylan program** — editor, REPL, inspector, debugger, library
   browser, sealed-domain visualiser, live-profile views —
   compiled and JIT-loaded by NewOpenDylan itself.

   **Win32 from Dylan, not from Rust.** The IDE talks to Win32
   directly through Dylan's `c-ffi`. Window creation, message
   pump, GDI/Direct2D rendering, common controls, clipboard, file
   dialogs, registry, COM where genuinely needed — all bound by
   `define interface` declarations in Dylan code. There is no
   Rust-side GUI abstraction layer. The Rust runtime only provides
   the **FFI plumbing** that makes those `c-ffi` calls work:
   x64-Windows calling convention dispatcher, callback bridge
   (Win32 `WndProc` → Dylan closure), foreign-buffer primitives,
   metadata-pack-driven function declarations. This stack is
   borrowed wholesale from NewCormanLisp's `win_ffi.rs` /
   `win_callback.rs` / `win_buffer.rs` / `win_surface.rs` /
   `win_metadata.rs` family (designed in `E:\CL\NewCormanLisp\docs\WINDOWS_FFI.md`,
   phased Phase 1–6). We adopt it nearly verbatim and let the
   Dylan side decide what windows look like.

   The sequencing this implies:
   1. **Compiler first.** Reader → parser → namespace → DFM →
      LLVM → JIT → GC → classes → sealed dispatch (PLAN.md
      phases 0–6). No IDE in this window. Headless `nod-driver`
      and `cargo test` are how we know we are alive.
   2. **`c-ffi` and the Windows FFI stack** are brought up next,
      lifting the Phase 1–6 design from NCL. This is a runtime
      task, not a UI task: `%ffi-call` dispatcher, callback
      bridge, buffer marshalling, the Win32 metadata pack.
   3. **Then the IDE is written *in Dylan*.** `define interface`
      against `User32.dll`, `Gdi32.dll`, `ComCtl32.dll`,
      `D2D1.dll`, etc.; the message pump is Dylan code; the
      window classes are Dylan-side registered through the
      callback bridge. The first IDE we ship is the first
      non-trivial Dylan program NewOpenDylan compiles.

   This is the same shape NewCormanLisp committed to: the original
   MFC IDE is not ported, and Win32 lives behind `%ffi-call`
   rather than behind a Rust abstraction. We adopt it for the same
   reason — keeping the Rust side narrow and forcing the language
   to be expressive enough to host its own tools is the right
   bootstrap pressure. The Rust runtime is a **language runtime**,
   not a GUI toolkit.

9. **Our own GC.** No Boehm, no MPS. A precise tracing collector
   written in Rust, instantiating the design we have proven across
   the sibling-compiler portfolio. Dylan's object model — class
   headers, multiple inheritance layout, sealed-class predictability
   — is friendly territory for a precise generational copying GC, and
   we exploit that. Details in the GC section below.

10. **Sealing is a first-class compile-time concern.** Dylan's
    speed-on-feels-dynamic-code story is sealing. We make sealing
    analysis visible — both in dumps and in the IDE — and we treat
    it as load-bearing for optimisation. Library-merge optimisation
    (the cross-module pass that exploits sealing across compilation
    units) is on the roadmap, not deferred indefinitely.

11. **No image format on disk.** The image lives in memory; the
    source lives on disk; that is the only direction of persistence.
    The compiled-artefact cache is non-canonical, regenerable, and
    deletable at will, following the model already pinned in
    NewCormanLisp. Source files are what `git` sees.

12. **Workspace ephemera is ephemeral.** REPL bindings, half-formed
    expressions, and inspector windows live in the image and die on
    restart. If the user wants persistence they save a file.

13. **Open source, open process, no proprietary inputs.** Implemented
    against the Dylan Reference Manual and Open Dylan's open-source
    source tree. The compiler ships under a permissive licence
    compatible with Open Dylan's. No proprietary Dylan distribution
    is consumed as source material at any phase.

## The garbage collector

The GC design inherits from the
[NewCormanLisp GC](../../CL/NewCormanLisp/docs/GC.md) — a precise
tracing collector written in pure Rust — and adapts for Dylan's
object model. Headlines:

- **Precise root finding via LLVM `gc.statepoint`.** No conservative
  stack scanning past the bring-up phase. The same approach NCL is
  taking; we share the safepoint-poll lowering pass between the two
  projects.
- **Generational copying**, young + old generations, plus a pinned
  static area for compiled code, sealed-class metadata, and the
  loaded image.
- **Multi-threaded mutator, stop-the-world collector.** Each Dylan
  thread has its own thread-local allocation buffer (TLAB); the
  fast path takes no locks. Collection is cooperative: mutators
  poll a flag at safe points and park voluntarily.
- **Headerless cons-equivalent allocations** where the type can be
  proven monomorphic at the call site. Everything else carries an
  8-byte header pointing at the class.
- **Software card-marking write barrier.** Hardware-assist via page
  protection is out of scope for v1; consistent with the rest of
  the portfolio.
- **Class metadata is pinned, not collected.** Sealed classes in
  particular live forever — they are part of the program, not the
  heap.
- **Multimethod dispatch caches are GC roots.** The dispatch
  apparatus participates in collection; method specialisers and
  signature objects move when their classes move.

The GC lives entirely in `nod-runtime` in pure Rust. The OS appears
only through a `PageAllocator` shim. `nod-runtime` contains zero FFI
imports — including in the GC.

## Live incremental compilation

This is the headline user-facing commitment. The model:

- **Saving a file is the compile trigger.** No `make`, no `dylan
  build`, no IDE button. The editor reports the change to the
  compiler; the compiler re-parses, re-types, re-seals, re-codegen,
  and re-installs.
- **Compilation is per-definition, not per-file.** Changing one
  `define method` recompiles that method, not its library. The
  module-graph loader, modelled on NCL's, tracks per-definition
  dirty state and edge invalidation.
- **Live methods are upgraded in place.** New method instances
  replace old ones in the generic function's method list under the
  same generation discipline NCL uses for redefinition. The old
  method retires once no live frame can still reach it.
- **Sealing breaks are surfaced, not silently propagated.** If a
  redefinition would break a sealed-domain assumption that the
  optimiser has already exploited, the compiler reports it as a
  structured diagnostic and refuses the patch until the user
  responds. The dependent compiled code can be invalidated and
  recompiled; we will not let a sealed-domain violation hide.
- **REPL evaluation goes through the same pipeline.** Typing
  `1 + 2 * 3` at the prompt builds the same DFM nodes a saved file
  would and runs through the same codegen path. There is no
  "interpreter mode."

## Live inspection

The inspector is the second commitment. Its surface:

- **Any object is inspectable.** Click an object in the REPL output
  and a window opens with its class, its slots, and traversable
  references to other live objects. Same for stack frames, methods,
  generics, conditions in flight, restart handlers, threads, and
  modules.
- **Sealed-domain visualiser.** A live view of which generics are
  sealed over which classes, which methods are participating in
  inline-cached dispatch, and which calls the optimiser has folded
  into direct calls. This is the optimiser surfacing itself.
- **The compiler's own data structures are inspectable.** Token
  streams, AST nodes, type lattices, DFM blocks, LLVM IR — all live
  in the image and the inspector can show them. The compiler is
  written in Rust but its outputs are first-class Dylan-side
  inspectable values.
- **Time-travel for evaluation.** The REPL records the last *N*
  evaluations with their before/after image deltas; the inspector
  can step back. This is bounded by image size and is configurable.

## The library / module graph

Dylan's two-tier namespace — libraries containing modules — is a
real static graph, not a flat package. The loader treats it as such:

- **A library is a compilation unit; a module is a namespace.**
  Cross-library references go through library exports; intra-library
  cross-module references go through module exports. Both are
  resolved at the loader, not at codegen.
- **LID files (`*.lid`) drive the library manifest.** We parse them
  faithfully. `dylan-package.json` (the newer JSON manifest) is the
  preferred format going forward; both are supported.
- **Per-library generation numbers** for hot reload. Editing one
  library does not invalidate others unless cross-library exports
  changed shape.
- **Sealed-domain decisions are library-local by default.**
  Cross-library sealing is opt-in via explicit `define sealed
  domain` declarations.

## Reuse across the sibling-compiler portfolio

NewOpenDylan is the sixth project in a portfolio of from-scratch
Rust+LLVM compilers we maintain together:

| Project        | Language                | Status     |
|----------------|-------------------------|------------|
| NewM2          | Modula-2 (PIM4 + ISO)   | Phase 5+   |
| NewCP          | Component Pascal        | Active     |
| NewCormanLisp  | Common Lisp             | Active     |
| NewBCPL        | BCPL                    | Active     |
| NewFB          | FreeBASIC               | Active     |
| **NewOpenDylan** | **Dylan**             | **Phase 0** |

We share infrastructure where reuse is possible:

- **JIT memory manager + SEH integration** — borrowed wholesale from
  NewM2's `newm2-llvm/src/jit_mm.rs`. Win64 SEH `RtlAddFunctionTable`
  registration is solved once across the portfolio.
- **Precise GC core** — adapted from NCL's `ncl-runtime`. The
  `gc.statepoint` lowering pass, the cooperative-park protocol, the
  TLAB design, the card-marking barrier — same code, different
  type system on top.
- **Loader / module-graph crate shape** — directly from NCL, which
  itself inherits from NewCP's `newcp-loader`. Source-stamp
  invalidation, generation-pinned execution scopes, retired
  artefacts: same algorithms.
- **Windows FFI stack** — borrowed wholesale from NCL's
  `ncl-runtime/src/win_ffi.rs`, `win_callback.rs`, `win_buffer.rs`,
  `win_surface.rs`, `win_metadata.rs` family, designed in
  [`E:\CL\NewCormanLisp\docs\WINDOWS_FFI.md`](file:///E:/CL/NewCormanLisp/docs/WINDOWS_FFI.md)
  (phases 1–6: surface bootstrap, `%ffi-call` calling-convention
  dispatcher, metadata-pack loader, foreign-buffer primitives,
  callback bridge). This is the **only** Win32 we have in Rust.
  Everything else — windows, message pump, controls, drawing —
  lives on the Dylan side, reached through `c-ffi` over this
  stack.
- **Compiler-phase visibility convention** — every crate exposes
  `format_*` dumps; the driver can stop after any phase. NewM2
  set the convention; we follow it.
- **LLVM pin and `inkwell` version** — pinned to the same major
  version as the rest of the portfolio. Bumps are coordinated.
- **Test harness convention** — runnable tests in their natural
  language alongside a Rust harness in `tests/` that drives the
  full compile pipeline and captures output. NewM2 and NCL share
  this; NewOpenDylan adopts it.
- **`unsafe_op_in_unsafe_fn = "deny"`** workspace lint.
- **Manifesto-as-design-constraint convention** — this very
  document follows the format established by NewM2's and NCL's
  manifestos.

We do **not** share AST nodes, IR opcodes, or sema. Each language
has its own. The shared layer is the runtime, the GC, the JIT
plumbing, and the conventions.

## Bootstrap

*Revised 2026-05-31. The original text here said "NewOpenDylan does
not self-host; the compiler stays in Rust permanently." Sprints 45–51
overtook that. The corrected commitment:*

**The front-end self-hosts; the back-end is permanent Rust + LLVM.**
NewOpenDylan partially self-hosts at the DFM IR boundary — the same
division `rustc` (Rust front-end, LLVM back-end) and GHC (Haskell
front-end, native back-end) draw. The front-end (lexer, parser, macro
expander, sema, AST → DFM lowering) migrates to Dylan, compiled by our
own back-end. The back-end (DFM → LLVM codegen, GC, JIT, AOT linker,
runtime, FFI) stays in Rust + LLVM for the life of the project.

This is **not** the upstream Open Dylan bootstrap, where DFMC is a
Dylan program *all the way down* and the build chain needs a working
Dylan compiler to exist. There is no chicken-and-egg: the back-end
that compiles the Dylan front-end is Rust, always present, never
itself written in Dylan. We escaped the bootstrap cost by writing the
*back-end* in Rust — and that cost, once paid, also bought a back-end
good enough that the front-end could move to Dylan early and cheaply.

How the migration works in practice — write each front-end phase in
Dylan, AOT-compile it `--library`, static-link it into the driver,
bridge its output across a committed wire format, gate it behind a
`--…-with-dylan` flag, validate in verify-mode against the Rust phase,
then make it the default — is documented in
[`ARCHITECTURE.md`](ARCHITECTURE.md).

The kernel `dylan` library (`E:\opendylan\sources\dylan\`) is still
ported as *runnable Dylan code* against our compiler. That work is
unchanged; it now sits alongside the front-end migration rather than
being the only Dylan-side code the project will ever contain.

**We still do not consume OpenDylan compiler artifacts** — not DFMC
output, not HARP, not `.img`/`.fasl`. Our Dylan front-end is *our*
code, written fresh against the DRM and compiled by *our* back-end. We
are not a downstream consumer of upstream's IR or release schedule.

## What NewOpenDylan is *not*

- **Not Open Dylan.** We share no code with the existing
  implementation. We share the language, the test corpus, the
  spirit, and a permissive licence. We do not share DFMC, HARP,
  the IDE, the runtime, the GC, the LID parser, or the build
  chain. We respect the upstream work; we are not it.
- **Not a 32-bit compiler.** Open Dylan still supports x86; we do
  not.
- **Not Linux-first.** Linux support is welcome later but is not
  on the roadmap before macOS.
- **Not an interpreter.** Every form goes through the full
  compiler. The REPL is the JIT.
- **Not a Dylan dialect.** We implement the DRM as it stands
  plus the extensions that ship in upstream Open Dylan today.
  We are not a vehicle for language experimentation.
- **Not Apple's Dylan.** The Apple Cambridge prefix syntax is
  out of scope. We implement the infix syntax that became the
  DRM and that everyone since has used.
- **Not a CL clone.** Despite the CLOS lineage, Dylan is a
  separate language with its own semantics. NewCormanLisp is the
  sibling project for Common Lisp; the two will share a runtime
  and a GC, not a front end.
- **Not an IDE written in Rust.** Only the C/Win32 FFI plumbing
  (`%ffi-call` dispatcher, callback bridge, buffer marshalling,
  metadata pack) is Rust, borrowed from NCL. The IDE itself —
  editor, REPL, inspector, debugger, library browser, sealed-domain
  visualiser, live-profile views — is **Dylan code, compiled by
  this compiler, calling Win32 directly through `c-ffi`**,
  re-implementing the shape of Open Dylan's
  `sources/environment/` tree. See core decision 8.
- **Not a GUI toolkit in Rust.** No `iGui`, no Direct2D shim, no
  event-mailbox abstraction on our side. Dylan code drives the
  Win32 message pump itself.
- **Not bound to any external IDE.** VS Code, Emacs, and the like
  are welcome to talk to our Language Server Protocol shim, but
  our integrated IDE is the load-bearing surface and is where
  live inspection actually lives.

## Versioning policy

- **Rust:** stable channel, current MSRV pinned in `Cargo.toml`
  and bumped quarterly across the portfolio.
- **LLVM:** pinned to a single major version, same as the rest of
  the portfolio. Bumps are coordinated and tracked in
  [`PLAN.md`](PLAN.md).
- **Dylan language version:** DRM + the OD extensions that exist
  in [`E:\opendylan`](file:///E:/opendylan) at the time of v1
  release.
- **Cache key:** `(source hash, compiler version, codegen flags,
  LLVM version)`. Any component change invalidates the cache.

---

*This manifesto is committed ahead of code. The plan in
[`PLAN.md`](PLAN.md) is the schedule; this is the line we will
not move.*
