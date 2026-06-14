# Glossary

The vocabulary used throughout this manual. Each term links to the page that
covers it in depth.

## A–D

**AOT** — *ahead-of-time* compilation. `nod-driver build` emits an object file and
links a standalone Win64 `.exe`. Contrast **JIT**. See [JIT & AOT](compiler/jit-and-aot.md).

**C3 linearization** — the algorithm that turns a class's (possibly multiple-inheritance)
superclass graph into a single, deterministic *class precedence list* (CPL). The CPL is
how multiple dispatch decides which method is most specific. See
[Generic functions](language/generic-functions.md) and `nod-sema/src/c3.rs`.

**Computation** — one SSA instruction in **DFM**: `Const`, `PrimOp`, `DirectCall`,
`Call`, `Dispatch`, `SealedDirectCall`, `TypeCheck`, `LoadSlot`, `StoreSlot`,
`WriteBarrier`. Each defines exactly one result temporary. See [DFM](compiler/dfm.md).

**Condition** — a heap-allocated class instance representing an error, warning, or other
signalable situation. Handlers are searched dynamically by class. See
[Conditions](language/conditions.md).

**Controlled dynamism** — Dylan's central idea: write open, fully dynamic code first, then
add type declarations and **sealing** so the compiler can specialise it. Prototype and
production in one language. See [Language overview](language/overview.md).

**CPL** — *class precedence list*, the linearized superclass order produced by **C3**.

**DFM** — the *Dylan Flow Machine* IR: a typed, phi-free, block-parameter **SSA**
representation. It is the permanent contract between the front-end and the back-end —
the architectural cut line. See [DFM](compiler/dfm.md).

**DirectCall / SealedDirectCall** — a **Computation** that calls a statically-resolved
target. The resolver rewrites a runtime `Dispatch` into one of these when **sealing** lets
it prove the method set is closed; `SealedDirectCall` keeps a fallback chain for
`next-method`. See [Sealing](language/sealing.md).

**Dispatch** — selecting which method of a **generic function** to run. Dylan uses
*multiple dispatch*: the choice depends on the classes of all specialised arguments. See
[Generic functions](language/generic-functions.md).

**DylanLayout** — NewOpenDylan's implementation of the **HeapLayout** trait
(`nod-runtime/src/dylan_layout.rs`), teaching the collector the tagged-**Word** and
**wrapper** conventions. See [GC](compiler/gc.md).

## E–L

**FIP** — *forward-iteration protocol*, the abstraction Dylan collections share so `for`,
`map`, `do`, etc. iterate uniformly. See [Runtime](compiler/runtime.md).

**Fixnum** — an integer small enough to live directly in a tagged **Word** (63-bit signed,
±2^62). Larger integers (`<big-integer>`) are deferred. See [Runtime](compiler/runtime.md).

**Generic function (GF)** — an open set of methods sharing one name; calling it dispatches
to the most specific applicable method. See [Generic functions](language/generic-functions.md).

**HeapLayout** — the zero-sized trait that makes **newgc-core** language-agnostic. Each
language supplies its own implementation (`DylanLayout` here; `LispLayout`/`TinyLayout` are
siblings). See [GC](compiler/gc.md).

**Hygiene** — a macro expander property: bindings a macro introduces are renamed
(`name__nod_hyg_{nonce}`) so they can't capture or be captured by user names. See
[Macros](language/macros.md).

**IAT** — *import address table*, the Win32 mechanism the AOT linker uses so a built `.exe`
can call system DLLs. See [FFI](compiler/ffi.md).

**JIT** — *just-in-time* compilation: codegen's LLVM IR is compiled and run in-process for
`eval`/REPL. The engine is LLVM's **MCJIT**. See [JIT & AOT](compiler/jit-and-aot.md).

**LID file** — a small manifest declaring a **library**: its name, source files, and used
libraries. The working namespace driver today. See
[Modules & libraries](language/modules-and-libraries.md) and [Namespaces](compiler/namespace.md).

**Library / Module** — Dylan's two-level namespace. A *library* is the unit of compilation,
linking, and sealing; a *module* is a namespace inside a library that controls name
visibility. See [Modules & libraries](language/modules-and-libraries.md).

## M–R

**MCJIT** — the LLVM JIT engine NewOpenDylan uses
(`LLVMCreateMCJITCompilerForModule`) to compile and run code in-image. *Not* ORC; a
future move to ORC v2 LLJIT is noted as a possibility in `docs/DEFERRED.md`. See
[JIT & AOT](compiler/jit-and-aot.md).

**Mark-evacuate** — the collector style: mark the live objects, then evacuate (copy)
survivors into fresh space, leaving the old pages to be reclaimed wholesale. The heap is
organized as fixed-size **pages**. See [GC](compiler/gc.md).

**Multiple dispatch** — method selection based on the classes of *all* specialised
arguments, not just a receiver. See [Generic functions](language/generic-functions.md).

**next-method** — inside a method body, calls the next-most-specific applicable method;
backed by `SealedDirectCall.fallback_chain` or the runtime method chain. See
[Generic functions](language/generic-functions.md).

**Pattern rule** — a macro's `{ pattern } => { template }` clause. **Pattern variables**
(`?name:constraint`) capture fragments of the call and are substituted into the template.
See [Macros](language/macros.md).

**Precise roots** — at each allocating call site, codegen spills the live GC roots to
stack slots and reloads them after the call, so the collector can relocate objects safely.
"Precise" (the collector knows exactly what is a pointer) as opposed to conservative. See
[Codegen](compiler/codegen.md) and [GC](compiler/gc.md).

## S–Z

**Safepoint / statepoint** — the bracketed region around an allocating call where GC may
run and roots are spilled/reloaded (`nod_jit_begin_safepoint` / `…_end_safepoint`). A
future move to LLVM's `gc.statepoint` intrinsics is on the roadmap. See [Codegen](compiler/codegen.md).

**Sealing** — a promise that a class won't be subclassed, a **GF** won't gain methods, or a
domain won't gain methods, *outside its home library*. It lets the compiler turn runtime
**dispatch** into a **DirectCall**. See [Sealing](language/sealing.md).

**Slot** — an instance field of a class. Slots get auto-generated getter and setter
**generic functions**. See [Types & classes](language/types-and-classes.md).

**SSA** — *static single assignment*: each value is defined once. DFM is *typed* SSA and
*phi-free* — join values flow as **block parameters** on jumps, not phi nodes. See
[DFM](compiler/dfm.md).

**Stub table / trampoline** — the per-module table of Win32 function stubs and the
arity-specific Win64 trampolines that marshal arguments across the FFI boundary. See
[FFI](compiler/ffi.md).

**TLAB** — *thread-local allocation buffer*: a chunk of heap a thread bump-allocates from
without locking, the GC's fast allocation path. See [GC](compiler/gc.md).

**Verify-mode** — running a Dylan-migrated front-end phase alongside its Rust original and
asserting they agree (`--verify-parse`). "Two compilers are better than one." See
[Self-hosting](compiler/self-hosting.md).

**Wire format** — a flat, fixed byte layout both sides of the Rust↔Dylan seam agree on
(tokens, AST). Front-end phases cross the boundary as bytes, never as shared data
structures. See [Self-hosting](compiler/self-hosting.md).

**Word** — the tagged 64-bit runtime value: bit 0 clear → **fixnum** (`n << 1`), bit 0 set
→ heap pointer (`ptr | 1`). The ABI between JIT'd code and the runtime. See
[Runtime](compiler/runtime.md).

**Wrapper** — the header word on every heap object carrying its class id and GC flags
(mark, tenured, pinned, forwarded). See [Runtime](compiler/runtime.md).

---
[Manual home](index.md)
