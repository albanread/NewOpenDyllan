# NewOpenDylan — Language Report and Implementation Plan

*Drafted 2026-05-15.*

This document has two halves. Part 1 is a survey of the OpenDylan language
and implementation, grounded in the source tree at `E:\opendylan`. Part 2
is a concrete plan for **NewOpenDylan**, a from-scratch Rust + LLVM JIT
that runs Dylan source, modelled on the shape of `E:\CL\NewCormanLisp`.

---

## Part 1 — OpenDylan: the language and the implementation

### 1.1 Heritage and identity

Dylan ("DYnamic LANguage") was designed at Apple Cambridge in the early
1990s as the successor to Apple Dylan / Apple's Newton project. Its
genetic line is unambiguous: it is **CLOS without parentheses**. The
object system, the generic-function-and-method dispatch, the condition
system, the metaobject protocol, and even small details like
`define …` uniformity, multiple values, and `block`/`cleanup` all
descend directly from Common Lisp. What changed is the surface: an
ALGOL-ish infix syntax with `define class` / `define method` /
`define generic` instead of `defclass` / `defmethod` / `defgeneric`,
keyword arguments via `key:` instead of `:key`, and `<class-name>` and
`#"symbol"` syntax that visually distinguishes types and symbols from
ordinary identifiers (see naming conventions documented in
`E:\opendylan\OVERVIEW.txt` lines 47-51).

Today Dylan is a small but active project (`dylan-lang` on GitHub). The
canonical implementation is OpenDylan, descended from Harlequin/
Functional Objects' commercial compiler. It is self-hosting: the
compiler (DFMC) is itself written in Dylan. The community is small;
real-world deployment is rare. Its historical importance is
disproportionate to its current usage — Dylan was the first widely
discussed attempt to make a Lisp-family multi-paradigm language with
mainstream syntax, and many ideas that later showed up in Julia, Rust
(trait dispatch), and Swift (protocol witness tables) were prefigured
here.

### 1.2 Module and library system

Dylan has a two-tier namespace: **libraries** contain **modules**, and
modules contain **bindings**. A *library* is the unit of compilation and
linking; a *module* is the unit of name visibility, analogous to a
Common Lisp package or an ML structure. Two libraries may both define a
module named `internal`, and the two are utterly unrelated.

Libraries are described by **LID** (Library Interchange Description)
files. The kernel library is described by `E:\opendylan\sources\dylan\
dylan.lid` (91 source files, in a specific bootstrap order: `dfmc-boot`,
`macros`, `thread-macros`, `packed-slots`, `debugging`, `boot`,
`dispatch-prologue`, `new-dispatch`, …). Platform-specific variants
sit alongside (`dylan-win32.lid`), and a per-platform registry under
`sources/registry/` chooses which LID applies — this is how
`OVERVIEW.txt` lines 26-41 describe the platform abstraction.

Inside a Dylan source file the very first form is a
`Module:` header comment giving the file's module, followed by
`define library` and `define module` forms that declare imports
(`use dylan;`) and the export list. Imports may be renamed
(`rename: { foo => bar }`), restricted (`import: { foo }`), or
re-exported (`export: { foo }`). The result is a stricter, more static
namespace model than CL packages — there is no `use-package` performed
at runtime; the module graph is fully known at compile time.

### 1.3 Type system

Dylan types are **first-class runtime values** that also participate in
compile-time analysis. The hierarchy:

- `<object>` is the root.
- `<type>` is the metatype; every type is an instance of `<type>`.
- `<class>` is a subtype of `<type>`; concrete user classes
  (`<integer>`, `<string>`, user `<point>`) are instances of `<class>`.
- Non-class types include `singleton(value)`, `type-union(t1, t2)`,
  `subclass(class)` ("the class itself, or any subclass"), and
  `limited(<integer>, min: 0, max: 255)` for bounded numeric/collection
  types.

Type tests are `instance?(x, <foo>)` and `subtype?(<a>, <b>)`. Method
dispatch is based on the **multimethod dispatch** algorithm — the set
of all methods belonging to a generic function whose specialisers are
satisfied by the actual argument types, totally ordered by argument
specificity. See `E:\opendylan\sources\dylan\dispatch.dylan` and
`new-dispatch.dylan` for the runtime side, and
`E:\opendylan\sources\dfmc\optimization\dispatch.dylan` for the
compile-time decision procedure.

### 1.4 Object system

`define class <point> (<object>) slot x :: <integer>; slot y :: <integer>;
end;` produces a class with two slots, each with auto-generated
getter and setter generic functions (`x(p)`, `x(p) := 5`). Slot
options include `init-value:`, `init-function:`, `init-keyword:`,
`required-init-keyword:`, `setter: #f` (read-only), and
`slot-allocation: class | each-subclass | instance | virtual`.

Multiple inheritance is resolved by **C3 linearisation**, the same
algorithm Python adopted. The class precedence list determines slot
layout and dispatch order. Slot layout under MI is non-trivial; Dylan
uses **fixed offsets when possible** and a fallback indirect lookup
when not — see `E:\opendylan\sources\dylan\class.dylan` lines 19-39
which declares `slots-have-fixed-offsets?-bit` as a per-class
property.

The object model is genuinely metaobject-protocol-flavoured:
`<class>` is itself a class, `make` is a generic function, and
`initialize` is a user-extensible generic. But it deliberately stops
short of the full CLOS MOP — there is no portable
`compute-effective-method` extensibility, no `change-class`, and the
slot-access protocol is less open.

### 1.5 Sealing and the optimisation model

This is the heart of Dylan's "feels dynamic, compiles like static"
thesis, and the single most important idea to inherit.

By default, classes and generic functions are **`open`** — additional
subclasses and methods may be added by other libraries. This is the
Lisp default. But Dylan adds the inverse: declarations of `sealed`
classes, `sealed` generics, and `sealed domain` clauses that promise
*"no further extensions of this dispatch shape will appear"*.

A `sealed` class cannot be subclassed outside its defining library.
A `sealed generic` cannot have methods added outside its defining
library. A `define sealed domain foo (<a>, <b>);` declaration
specifically forbids any method on `foo` whose specialisers are
subtypes of `<a>` and `<b>` from existing outside this library.

The compiler exploits sealing relentlessly. When dispatch arguments
fall inside a sealed domain, the entire applicable-methods computation
can be hoisted to compile time and the call lowered to a **direct call**
or a tiny **inline cache**. The `E:\opendylan\sources\dfmc\optimization\
dispatch.dylan` file is essentially the rule book for this analysis;
its `guaranteed-joint?` predicate (lines 39-50 of the snippet above) is
the workhorse that decides whether a static type estimate is
definitively inside a sealed shape.

Sealing turns Dylan into a language where *the user controls the
optimisation contract*. `open` is "trust me, I might extend this";
`sealed` is "trust me, I won't". The result, when used well, is
performance comparable to C++ virtual dispatch while keeping CLOS
semantics where you need them.

### 1.6 Macros

Dylan macros are **hygienic, pattern-based, and procedural-fallback-free**
in the common case. There are three flavours:

- **`define macro` ... `=> { ... }`** — pattern-rule macros, matched
  against the call site and rewritten by template substitution. Hygiene
  is automatic via the standard freshening of introduced bindings.
- **Definition macros** that expand into `define foo …` forms and
  participate in top-level definition processing.
- **Statement and function macros**, distinguished by their syntactic
  position.

Macros expand in the reader/parser stage (`E:\opendylan\sources\dfmc\
reader\` — `lexer.dylan`, `parser.dylgram`, `infix-parser.dylan`,
`fragments.dylan`) before semantic analysis. The macro expander lives
at `E:\opendylan\sources\dfmc\macro-expander\`. Because expansion
happens textually-but-hygienically over **fragments** (token trees
with source-location info), Dylan macros are powerful enough to
implement `for`, `with-open-file`, `unless`, and large parts of the
condition system as ordinary library code — and they preserve source
locations through expansion, which is non-trivial.

### 1.7 Functions and control flow

Functions are first-class. `method (x :: <integer>) x + 1 end` is the
anonymous form. `define function` is a non-generic top-level function.
Functions have **multiple return values** (`values(a, b, c)`) and
**multiple-value receive** (`let (a, b) = foo();`).

Control flow:
- `if`, `unless`, `case`, `select` (the dispatching variant), `cond`
  via macro.
- `block (return) … return(value) … exception (<error>) … cleanup …
  end;` — Dylan's structured non-local exit + exception + finally.
- The **condition system** is CLOS-shape: conditions are instances of
  `<condition>`, handlers are bound dynamically, restarts are
  first-class, and `signal` may return if a handler invokes a restart.

### 1.8 Iteration and collections

The **collection protocol** is a typed CLOS-like hierarchy rooted at
`<collection>` with branches for `<sequence>`, `<explicit-key-collection>`,
`<mutable-collection>`, etc. Iteration goes through `forward-iteration-
protocol`, which returns the seven values (initial-state, limit, next-
state, finished-state?, current-key, current-element, current-element-
setter) that comprise a uniform iterator. The `for` macro consumes that
protocol and produces tight loops; the compiler optimises away the
protocol indirection for concrete collection types when sealing
permits.

Concrete types include `<list>`, `<simple-object-vector>`,
`<stretchy-vector>`, `<deque>`, `<string>` / `<byte-string>` /
`<unicode-string>`, `<table>` (hash), `<range>`, `<set>`, plus
**limited collections** (`limited(<vector>, of: <integer>)`) for
unboxed storage.

### 1.9 Numerics

`<integer>` is a **tagged fixed-width integer** (tag-1 in the runtime,
so 30 or 62 bits depending on word size). `<double-integer>` and
`<big-integer>` extend the range. `<single-float>` and `<double-float>`
are IEEE-754. Generic arithmetic dispatches on `+`, `-`, etc., with
the usual numeric contagion (integer + float → float). The
`E:\opendylan\sources\dylan\conversion-tagged-integer.dylan` and
`integer.dylan` files implement tag manipulation in Dylan with
`primitive-…` intrinsics.

### 1.10 DFMC — the compiler

DFMC (Dylan Flow Machine Compiler) is OpenDylan's compiler, written
in Dylan. Its stages (per `OVERVIEW.txt` lines 73-91 and the directory
layout in `E:\opendylan\sources\dfmc\`):

1. **reader** — lexer + infix parser producing **fragments** (token
   trees) and then AST nodes. The grammar is in `parser.dylgram`.
2. **macro-expander** — pattern-driven hygienic expansion over
   fragments.
3. **definitions** — top-level forms become *definition objects*
   (`<class-definition>`, `<method-definition>`, …).
4. **namespace** — library/module graph resolution, name binding.
5. **modeling** — bridge layer; compile-time **model objects**
   represent runtime classes/functions for the optimiser. The `&class`
   prefix in source signals a model object.
6. **conversion** — definitions → **DFM** (Dylan Flow Machine), an
   SSA-style flow graph. Files in `E:\opendylan\sources\dfmc\flow-graph\`
   define computations, temporaries, environments.
7. **typist** — type inference over DFM, producing **type estimates**
   that feed the optimiser.
8. **optimization** — sealing-aware dispatch resolution
   (`optimization\dispatch.dylan`), inlining, dead-code, CSE,
   constant folding, tail-call, multiple-values lowering,
   closure analysis, dynamic-extent optimisation.
9. **back-end** — pluggable. There are three:
   - **`c-back-end`** — emits portable C; pairs with Boehm GC.
   - **`harp-…-back-end`** — HARP (Harlequin Abstract RISC Processor),
     direct x86 codegen with COFF emission. Pairs with MPS GC. The
     `E:\opendylan\sources\harp\` tree (`core-harp`, `coff-builder`,
     `coff-debug`, `gnu-as-outputter`) is what makes HARP a complete
     mini-assembler.
   - **`llvm-back-end`** — newer; emits LLVM bitcode via a
     hand-written LLVM IR builder in pure Dylan
     (`E:\opendylan\sources\lib\llvm\llvm-builder.dylan`,
     `llvm-bitcode.dylan`, `llvm-instruction.dylan`).
10. **linker** — combines compiled libraries; supports library-merge
    optimisations that re-run inlining across library boundaries.

The DFM IR is exposed through `flow-graph\computation.dylan`. It is
SSA in shape (each *temporary* has a single definition), with explicit
nodes for calls, dispatch, slot access, primitive ops, and control
flow. This is significant: **NewOpenDylan can lift the DFM shape
almost directly** as its mid-level IR.

### 1.11 Runtime model

`E:\opendylan\sources\lib\run-time\` is the C runtime. Key files:

- `c-run-time.c`, `c-run-time-nlx.c` — entry shims, NLX (non-local
  exit) machinery for the C back-end.
- `llvm-runtime.h`, `llvm-runtime-init.c`, `llvm-nlx.c`,
  `llvm-exceptions.c` — equivalent for the LLVM back-end.
- `boehm-collector.c`, `mps-collector.c`, `mps-dylan.c`,
  `malloc-collector.c` — three GC backends, swappable. The MPS
  (Memory Pool System, by Ravenbrook) is the production choice for
  HARP; Boehm is the default for C and LLVM.
- `posix-threads.c`, `thread-utils.c`, `stack-walker.c` — threading
  + stack walking for GC root scanning.

The object representation uses **tag bits** in the low bits of a
machine word: `0` for fixnum (`<integer>`), `1` for pointer-to-heap-
object with a `<wrapper>` header that carries class identity, slot
layout, and GC info. Calls go through a function pointer plus
environment.

Threading is preemptive with explicit `<thread>` and `<lock>`
classes. The GC is cooperative stop-the-world; safe points are
inserted by the compiler.

### 1.12 What makes it distinctive

- **`define …` uniformity.** Every top-level form is `define <thing>
  name …`. There is exactly one keyword for definitions, and macros
  extend it uniformly.
- **Sealing as a first-class optimisation contract** that the user,
  not the compiler, controls.
- **Library-merge cross-module optimisation**, which is essentially
  whole-program optimisation gated on what the user has sealed.
- **The MOP-without-the-MOP** balance: enough metaobject access for
  real reflection (`object-class`, `class-direct-superclasses`,
  generic-function methods), but not enough rope to break dispatch
  semantics.
- **Conditions and restarts**, not exceptions.

Why it didn't catch on: arriving in the mid-90s into the C++/Java
window with no killer app, no commercial backing after Apple/Harlequin
exits, and the syntactic compromise alienated both Lisp natives and
mainstream programmers. The tooling (an MFC-era Windows IDE) aged
poorly. The language itself remains genuinely interesting.

---

## Part 2 — NewOpenDylan implementation plan

The model: a Rust workspace producing a JIT-first Dylan compiler that
runs on Windows, designed to port cleanly to macOS, with LLVM as the
sole code generator. We inherit the *shape* of NewCormanLisp (NCL)
wholesale and adapt where Dylan demands.

### 2.1 Workspace skeleton

Mapping NCL crates to NewOpenDylan (`nod-*`) crates:

| NCL crate            | NewOpenDylan crate    | Role / Dylan-side concept                                                          |
| -------------------- | --------------------- | ---------------------------------------------------------------------------------- |
| `ncl-reader`         | `nod-reader`          | Lexer + infix parser (`dfmc/reader` equivalent); fragments + AST.                  |
| `ncl-reader` (split) | `nod-macro`           | Macro expander (no NCL analogue — pattern-based hygienic expansion over fragments). |
| —                    | `nod-namespace`       | Library/module graph, LID-file parsing, name resolution. No NCL analogue — CL packages are runtime, Dylan namespaces are static. |
| `ncl-ir`             | `nod-dfm`             | DFM-equivalent SSA IR. Same role as `ncl-ir`.                                      |
| `ncl-compiler`       | `nod-sema`            | Definitions → typed/sealed semantic objects; sealing analysis; type inference.     |
| `ncl-compiler`       | `nod-opt`             | DFM optimisation passes: dispatch resolution, inlining, library-merge.             |
| `ncl-llvm`           | `nod-llvm`            | LLVM bindings + JIT. Same role; possibly the same upstream `inkwell` choice.       |
| `ncl-loader`         | `nod-loader`          | Source-graph dirty tracking, generations, retirement, cache. Same as NCL.          |
| `ncl-runtime`        | `nod-runtime`         | GC, allocator, threading, C-FFI, Windows-FFI runtime stack (`%ffi-call` dispatcher, callback bridge, buffer marshalling, metadata pack — Sprint 23b, borrowed from NCL). Same role. |
| `ncl-cl`             | `nod-dylan`           | The Dylan-side stdlib (port of `sources/dylan/`).                                  |
| `ncl-driver`         | `nod-driver`          | CLI + REPL + compiler driver entry point.                                          |
| `tests/ncl-tests`    | `tests/nod-tests`     | Rust-side unit + integration tests.                                                |
| `tests/ncl-corman-demos` | `tests/nod-od-suite` | Faithful-tribute regression against a curated subset of OpenDylan sample programs. |

Differences from NCL:

- **`nod-namespace` is new.** Dylan's two-tier library/module system is
  static and load-bearing in a way CL packages are not; it deserves its
  own crate.
- **`nod-macro` is split out from the reader.** In NCL the reader is
  essentially "parse s-expressions"; reader macros are a thin layer.
  In Dylan, macros are a major compiler stage with their own pattern-
  matching engine and fragment manipulation.
- **`nod-sema` and `nod-opt` are split** where NCL has a single
  `ncl-compiler`. Dylan's sealing/dispatch analysis is substantial
  enough to warrant its own crate, and library-merge optimisation
  cares about whole-program shape that is easier to express
  separately.

Workspace `Cargo.toml` inherits NCL's settings:
- `resolver = "3"`, `edition = "2024"`.
- `workspace.lints.rust = { unsafe_op_in_unsafe_fn = "deny" }`.
- License `MIT OR Apache-2.0`.

Repository layout:

```
E:\NewOpenDylan\
  PLAN.md                this file
  MANIFESTO.md           pinned design constraints (next deliverable)
  README.md
  src\
    nod-driver
    nod-reader
    nod-macro
    nod-namespace
    nod-sema
    nod-dfm
    nod-opt
    nod-llvm
    nod-loader
    nod-runtime
    nod-dylan          Dylan-side stdlib sources live here (.dylan files)
  tests\
    nod-tests
    nod-od-suite       curated OpenDylan-compatible samples
  docs\
    GC.md
    DFM.md
    SEALING.md
    MACROS.md
```

### 2.2 Manifesto inheritance

Inherited verbatim from NCL:

- **Rust-for-the-substrate, LLVM-only, 64-bit-only, Windows-first**,
  with macOS as the second target. The IDE is **Dylan code calling
  Win32 directly through `c-ffi`**, not a Rust GUI crate; only the
  C/Win32 FFI plumbing (`%ffi-call` dispatcher, callback bridge,
  buffer marshalling, metadata pack) is Rust, borrowed from NCL.
- **No hand-written assembly.** Stack walking, GC barriers, NLX
  unwinding all go through LLVM intrinsics + Rust.
- **JIT-first, source-on-disk, image-in-memory, non-canonical cache.**
  No persistent image. The cache is keyed by `(source hash, compiler
  version, codegen flags)` and deletable.
- **Tracing GC, generational copying, precise roots via
  `gc.statepoint`-emitted stack maps** with a conservative fallback
  during bring-up.
- **OS-independence in `nod-runtime`** except the deliberately
  quarantined Windows-FFI subtree (`win_ffi.rs`, `win_callback.rs`,
  `win_buffer.rs`, `win_surface.rs`, `win_metadata.rs` — borrowed
  from NCL).
- **FFI is a feature *and* a foundation.** Dylan's `c-ffi` library is
  exposed to user code and is the *only* way the IDE talks to Win32
  — there is no Rust-side GUI substrate. We use `std::fs` /
  `std::thread` for compiler-internal needs.
- **The compiler is ours.** No commitment to DFMC's internal shapes.

Diverging from NCL where Dylan requires it:

- **Static module graph is load-bearing.** In NCL, packages are
  dynamic; you can `(make-package :foo)` at runtime. In NewOpenDylan,
  libraries and modules are declared in LID + source headers and
  resolved at compile time. This *simplifies* hot-reload — module
  topology rarely changes — but *complicates* the REPL.
- **Sealing analysis must be a first-class concern from phase 1.**
  Without it, dispatch is O(n) per call and the language is unusably
  slow. With it, the language has a real performance story.
- **Library-merge optimisation is the Dylan-shaped whole-program
  optimisation.** The cache key must therefore include downstream
  library hashes when a library is being compiled "merged".
- **Multimethod dispatch instead of single-dispatch + symbol-cell.**
  NCL's "global function in a symbol cell, atomic pointer swap"
  redefinition story does not transfer. Generic functions carry a
  **method table** (sorted list of method objects + dispatch cache)
  and `add-method`/`remove-method` is the analogous atomic operation
  on that table.
- **GC: copying generational, like NCL.** We do **not** adopt MPS — its
  C API and licensing are an integration risk we don't need. We do
  **not** use Boehm either; precise tracing through LLVM-emitted
  stack maps is what NCL committed to and we inherit that. This is a
  conscious bet that the engineering NCL has already done on
  `gc.statepoint` carries over cleanly to a Dylan object layout.
- **Headerless cons cells (NCL) → headered everywhere (Dylan).** Dylan
  has no `cons` privilege; every heap object carries a `<wrapper>`
  pointer for class identity and slot layout. Object layout is
  simpler than NCL's by one tag bit.
- **Conditions and restarts**, not CL `unwind-protect` + `handler-
  case`. Architecturally similar — dynamic handler stack, restart
  objects, two-pass signal semantics — but the API surface differs
  enough to be its own implementation effort.

### 2.3 Phase plan

Phase boundaries are chosen to mirror Dylan's own bootstrap order
(see `E:\opendylan\sources\dylan\dylan.lid` for what comes when).

**Phase 0 — Workspace skeleton (1–2 weeks).**
Crates, `Cargo.toml`, lints, CI, `nod-driver --version`. No language
features. Mirrors NCL pre-bootstrap.

**Phase 1 — Reader + AST (2–3 weeks).**
Lex Dylan source into tokens (with the prefix conventions for `<>`
class names and `#"…"` symbols). Build a fragment representation.
Parse the infix grammar into an AST. Output `dylan-format`-style
round-tripped source as a verification mode. No semantics yet.
Reference: `E:\opendylan\sources\dfmc\reader\`.

**Phase 2 — Module graph (1–2 weeks).**
Parse LID files. Build the library/module graph. Resolve `use`/
`import`/`export`/`rename`. Diagnose cycles. No code generation yet.

**Phase 3 — Minimal kernel: integers, calls, `if`, `define function`,
`define constant` (3–4 weeks).**
DFM IR with the smallest viable computation set. LLVM codegen for
i64/f64 arithmetic, branches, direct calls. No GC yet — allocate
into a leaked arena. JIT compile and run "hello, world via
`format-out`" through a single FFI shim to `stdout`. Land the
end-to-end pipeline thin.

**Phase 4 — GC + heap objects (4–6 weeks).**
Tagged pointers, `<wrapper>` headers, generational copying GC,
allocation fast path (TLAB), `gc.statepoint` emission and stack maps,
safe-point polling. Port the heap and root-finding from the existing
M2NEW/NCL work where it survives unchanged. Add `<simple-object-
vector>`, `<string>`, `<symbol>`.

**Phase 5 — Classes, slots, single dispatch (4–6 weeks).**
`define class` with slots, getters/setters, init keywords,
`initialize`, `make`. Class precedence list via C3. Slot layout with
fixed offsets. Single-dispatch generic functions as a placeholder.
At this point a substantial slice of `sources/dylan/object.dylan`,
`number.dylan`, `character.dylan` should run.

**Phase 6 — Multimethod dispatch + sealing analysis (6–8 weeks).**
Multimethod dispatch with cache (model after `dispatch-caches.dylan`).
`sealed` classes, `sealed` generics, `define sealed domain`.
Compile-time dispatch resolution for sealed shapes. Inline caches
for unsealed shapes. This phase is where Dylan stops being a slow
toy and starts being a real language. Cross-reference
`dfmc/optimization/dispatch.dylan` for the analysis rules.

**Phase 7 — Macros (4–6 weeks).**
Pattern-rule `define macro` with hygienic substitution over fragments.
Source-location preservation through expansion. Definition macros.
Statement and function macros. With macros in, large parts of
`sources/dylan/collection-macros.dylan`, condition handling, and
`for`/`with-…` forms become library code rather than compiler-builtin.

**Phase 8 — Conditions, NLX, restarts (3–4 weeks).**
`block`/`exception`/`cleanup`. `<condition>` hierarchy. Handler stack.
Restart objects. Two-pass signal. Integrates with LLVM's exception
machinery on Windows (SEH-bridged, as in our M2NEW work) and Itanium
unwind on Unix.

**Phase 9 — Collections, iteration protocol (3–4 weeks).**
Forward iteration protocol. `for` macro. `map`, `do`, `reduce`.
`<list>`, `<stretchy-vector>`, `<table>`, `<deque>`, `<range>`.
Limited collections.

**Phase 10 — FFI + the standard library port (open-ended).**
`c-ffi` library port — `define interface` and friends. Port enough
of `sources/dylan/` and `sources/io/` to run a representative subset
of OpenDylan sample programs. Treat this as the steady-state work
that fills out v1.

**Phase 11 — Library-merge optimisation, AOT path (open-ended).**
Whole-program inlining across library boundaries gated on sealing.
Cache key extension to capture downstream library hashes. AOT mode
that emits a standalone executable.

**Phase 12 — IDE / REPL / debugger — in Dylan.**
The IDE is **a Dylan program compiled by NewOpenDylan**, calling
Win32 directly through `c-ffi`. The original MFC IDE
(`E:\opendylan\sources\environment\`) is the re-implementation
target, not a port target. Its feel — inline REPL, inspector,
hot edit-and-reload — is the spec. The Rust side provides only
the Windows FFI runtime stack (Sprint 23b, borrowed from NCL).

### 2.4 Compiler architecture

```
.dylan / .lid sources
        │
        ▼
   nod-reader            tokens → fragments → AST
        │
        ▼
   nod-macro             fragment-level hygienic expansion
        │
        ▼
   nod-namespace         library/module graph, name resolution
        │
        ▼
   nod-sema              definitions, classes, generics, sealing analysis,
                          type inference
        │
        ▼
   nod-dfm               SSA IR (DFM-shape: temporaries, computations,
                          environments, control flow nodes)
        │
        ▼
   nod-opt               dispatch resolution, inlining, CSE, DCE,
                          tail-call, multiple-values lowering,
                          library-merge
        │
        ▼
   nod-llvm              IR → LLVM IR → JIT → machine code
                          (precise stack maps via gc.statepoint)
        │
        ▼
   nod-runtime           GC, threading, NLX, C-FFI, Windows-FFI runtime stack
```

**Macros** expand in `nod-macro`, before any semantic analysis. This is
deliberately the same stage placement DFMC uses — macros operate over
fragments, not AST, because they need to splice arbitrary syntax.

**Sealing analysis** kicks in twice: once in `nod-sema` to record
sealing facts attached to class and generic definitions, and once in
`nod-opt` (the `dispatch` pass) to consult those facts when resolving
each call site. Library-merge multiplies this — when libraries are
merged, sealing facts from one library may unlock optimisations in
another.

**The DFM-equivalent IR (`nod-dfm`)** is SSA-shape with named computation
node kinds. Core nodes (mirroring `flow-graph/computation.dylan`):
`<call>`, `<dispatch>` (pre-resolution generic call),
`<direct-call>` (post-resolution), `<slot-ref>` / `<slot-set!>`,
`<primitive-op>` (untyped low-level: integer add, pointer load),
`<make-environment>`, `<closure>`, `<if>`, `<bind-exit>`,
`<unbind-exit>`, `<return>`, `<values>`. Temporaries carry inferred
**type estimates** (a join-lattice over Dylan types, with `<bottom>`
and `<top>`) that propagate through the optimiser.

**The runtime/GC seam** is via LLVM `gc.statepoint`/`gc.relocate`
pairs at call sites, with the GC strategy `"statepoint-example"` or a
custom strategy in `nod-llvm`. Stack maps are decoded at GC time by
`nod-runtime` to find precise roots. This is the path NCL has already
walked.

### 2.5 Integration risks

**(a) Macro hygiene under JIT.** Dylan macros preserve source
locations through expansion (this is how the IDE reports errors in
expanded code at the original site). Under a JIT with cached
artifacts, the location-mapping table must be cache-friendly and
survive recompilation of dependencies. Design call: every fragment
carries a `<source-location>` whose `source-file` is a stable
interned id, so cache hits do not need to re-parse to relocate.

**(b) Generic-function caching.** Multimethod dispatch caches are
hot. The runtime cache (per call site? per generic?) must be
thread-safe and invalidate on `add-method`/`remove-method` without
locking the call site. Design call: per-call-site **monomorphic
inline cache** with a guard on the receiver's `<wrapper>`, falling
back to a per-generic hashtable; invalidation is a generation bump
on the generic, checked atomically.

**(c) MOP completeness.** A full MOP (CLOS-style
`compute-effective-method` extensibility) is **not** in v1. We
expose introspection — `object-class`, `class-direct-superclasses`,
`generic-function-methods` — but no user-extensible computation of
effective methods. This buys us the ability to bake dispatch
decisions at compile time without worrying about user-installed
method combinators.

**(d) Condition system.** Restarts are stack-walked, not closure-
captured, in OpenDylan's runtime. Replicating that under LLVM
unwinding on Windows is non-trivial — we need a parallel "Dylan
handler frame" intrinsic frame chain that the unwinder can walk.
Design call: handler frames are heap-allocated and threaded through
a thread-local stack; unwinding consults that chain, not the OS
unwinder. The OS unwinder is only used to run `cleanup` clauses.

**(e) FFI.** Dylan's `c-ffi` is `define interface` and is fancier than
NCL's `defun-dll`. We need full struct/union/array marshalling,
callback registration, and Windows `__stdcall` vs `__cdcall` handling.
Reuse M2NEW FFI machinery; expose through `nod-runtime/ffi`.

**(f) Library-merge cross-module optimisation.** This is genuinely
hard. It requires that the optimiser have access to DFM bodies of
all libraries in the merge, not just signatures, and that the cache
key reflect this. v1 ships **without** library-merge; v2 lights it
up. Design call: DFM bodies are serialisable into the cache; merge
mode is a driver flag that pulls them back in.

**(g) Boot ordering.** `sources/dylan/dylan.lid` shows a specific 96-
file boot sequence. We must replicate enough of it to bootstrap our
stdlib. Some of those files (`dfmc-boot.dylan`, `boot.dylan`,
`dispatch-prologue.dylan`) exist specifically to lift the compiler
into existence; we don't need them — but the rest of the order
(macros before classes before dispatch before collections before
conditions before threads) is genuine and we follow it.

### 2.6 What to skip, what to preserve

**Essential (must be in v1):**
- Two-tier library/module namespace + LID files.
- Classes with slots, MI, C3, fixed-offset layout.
- Multimethod generics with sealing-driven optimisation.
- Pattern-rule macros with hygiene and source-location preservation.
- Multiple values, `block`/`exception`/`cleanup`, condition system
  with restarts.
- Forward iteration protocol + `for` + the core collection types
  (`<list>`, `<vector>`, `<stretchy-vector>`, `<string>`, `<table>`,
  `<range>`).
- Tagged `<integer>`, `<double-float>`, `<character>`, `<symbol>`,
  `<boolean>`.
- `c-ffi` for user-facing C interop.

**Deferred to v1.x:**
- `<big-integer>` and `<double-integer>` beyond `<integer>` range.
- Full MOP introspection on dispatch (`compute-applicable-methods`
  yes; user-extensible `compute-effective-method` no).
- `<unicode-string>` (start with UTF-8 `<byte-string>`; add unicode
  the way OpenDylan layers it).
- Limited collections (`limited(<vector>, of: <integer>)`) — they're
  a performance feature, not a semantic one.

**Deferred to v2+:**
- Library-merge cross-module optimisation.
- AOT executable emission.
- macOS port (planned, not built).
- DUIM / GUI library port. DUIM is an MFC-era abstraction layer we
  have no use for; the Dylan-side IDE calls Win32 directly through
  `c-ffi`.

**Dropped entirely:**
- HARP back-end and direct COFF emission. LLVM is the only back-end.
- The C back-end. Same reason.
- The DOOD object database (`sources/lib/dood`). Out of scope.
- The MFC IDE source (`sources/environment`). Not ported; **re-implemented in Dylan** on top of the Sprint 23b Windows FFI stack, taking the upstream tree's feel — editor, debugger, library browser, project wizard — as the spec.
- The Jam build system (`sources/lib/jam`). Replaced by `cargo`.
- The OD project-manager and tools-interface. Replaced by `nod-driver`.

### 2.7 The bootstrap question

*Revised 2026-05-31. The original conclusion here was "this is not
self-hosting and we are not pursuing self-hosting." Sprints 45–51
overtook it. The corrected position — and the reasons that still
hold — follow. See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the full
statement.*

OpenDylan is self-hosting *all the way down*: DFMC is written in
Dylan, and the build chain needs a working Dylan compiler to exist.
The natural shortcut would be *use OpenDylan to emit something
NewOpenDylan can consume.* That is **not** the path we take, and that
much is unchanged:

1. Coupling to upstream's internal IR or back-end output formats
   makes us a downstream consumer of someone else's release
   schedule.
2. The interesting back-end work — sealing analysis, dispatch
   optimisation, precise GC under LLVM — happens in our compiler, not
   theirs. There is no shortcut available.
3. NCL took the same line for Common Lisp: NCL does not consume
   Corman's `.img` / `.fasl` artifacts. It re-implements the stdlib
   in its own front-end and re-runs the source.

**What changed: we DO self-host the front-end — just not the
back-end.** NewOpenDylan partially self-hosts at the DFM IR boundary,
the same division `rustc` and GHC draw:

- **Phases 0–6:** The whole compiler is Rust. Exercised on
  hand-written Dylan tests and curated `.dylan` files. No Dylan-side
  compiler code yet.
- **Phases 7–17:** Macros, conditions, collections, FFI, AOT, the
  Dylan-side IDE. Still a Rust compiler; the Dylan code is *running*
  programs (stdlib, IDE), not compiler internals.
- **Sprints 45–51 (the inflection):** The Dylan lexer and parser,
  written in Dylan as corpus exercises, turned out to work — so we
  JIT-strapped/static-linked them back into the driver and ran the
  front-end through them. The lexer is byte-identical to the Rust
  lexer; the parser agrees with it on the whole corpus. The
  empirical lesson — *the same DFM produces the same LLVM produces the
  same machine code* — means the back-end never needs to leave Rust,
  and the front-end can move to Dylan years earlier than guessed.
- **Sprints 52–55 (done / in progress):** The remaining front-end
  phases migrate to Dylan one at a time, each validated by `dump-*`
  byte-match against its Rust counterpart before becoming load-bearing:
  macro expander (52, opt-in `NOD_EXPAND_WITH_DYLAN`); sema/namespace
  (53 recording walk; 54 **load-bearing** via `--sema-with-dylan`,
  38/38 byte-identical); AST → DFM lowering (55 **load-bearing
  (opt-in)** via `--lower-with-dylan` — 55a stmts/exprs + 55b
  slots/`instance?` done, 55b dispatch/55c closures remaining). Sprint
  56 consolidates the per-stage shims into one front-end and flips the
  Dylan path to default. Live status:
  [`docs/journal/README.md`](journal/README.md).

The end state: **the front-end is Dylan, compiled by a Rust + LLVM
back-end that stays Rust forever.** Codegen, GC, JIT, and the linker
are the permanent native substrate. This is the arrangement Java has
at the *runtime* layer (HotSpot is C++, `java.lang` is Java) — but
NewOpenDylan goes further: the *compiler front-end* is in the language
too, with only the machine-code generator, the collector, and the
linker remaining in the systems language.

We do not lose the eat-our-own-dogfood signal that pure-Rust would
have cost us — we *gain* it: the Dylan front-end is dogfood, and
running both front-ends side by side in verify-mode is a stronger
correctness signal than either alone. The `nod-od-suite` regression
battery against curated OpenDylan sample programs remains the
outer gate.

### 2.8 Closing notes on tractability

What is genuinely a single-week task:
- The lexer (the state machine in `dfmc/reader/lexer.dylan` is
  mechanical to port).
- LID-file parsing.
- The C3 linearisation algorithm.
- The forward iteration protocol scaffolding.

What is a multi-month research and engineering project:
- Sealing-driven dispatch optimisation at parity with DFMC (Phase 6).
- The macro expander with full hygiene and source-location
  preservation (Phase 7).
- Library-merge cross-module optimisation (deferred to v2).
- Precise GC root finding through `gc.statepoint` with LLVM JIT —
  hard but already de-risked by the NCL/M2NEW work; we are not the
  first to walk this path in our codebase.
- The condition/restart system with correct two-pass semantics under
  Windows SEH.

What is dangerous to underestimate:
- The Dylan stdlib is **large**. 91 files in the kernel `dylan`
  library alone, plus the `common-dylan`, `io`, `collections`, and
  `system` libraries. Even with macros doing heavy lifting, this is
  a sustained porting effort that dwarfs the compiler work past
  Phase 8.
- LID-file platform conditionalisation is fiddly.
- Source-location preservation through macros is the difference
  between a usable language and a hostile one. Budget for it.

The promise, in the NCL idiom:

> A user who wrote Dylan code against OpenDylan in 2020 should be able
> to open it in NewOpenDylan in 2027, hit compile, and watch it run —
> faster, on a 64-bit JIT, on a modern toolchain, with a debugger that
> understands the source — without changing more than the LID file's
> `Platforms:` line.
