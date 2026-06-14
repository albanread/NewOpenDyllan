# Library / Module Graph — NewOpenDylan Sprint 05 Spec

*Drafted 2026-05-16. Implements Sprint 05 of [`../SPRINTS.md`](../SPRINTS.md); informs `nod-namespace` and `nod-loader` (both empty since Sprint 01).*

---

## 1. Status & scope

Sprint 05 lands the **first front-end pipeline stage that has memory** — every prior pass (`nod-reader` lex/parse) is per-file and stateless. After this sprint, `nod-driver dump-graph <lid>` resolves a complete Dylan library plus all of its source files into an in-memory DAG of libraries → modules → bindings, with cross-library `use` arrows reified and with every `Module: foo` header in every `.dylan` source file pointing at a real module node.

**In scope for v1:**

- LID-file parser, including the `LID:` include directive used by platform overlays.
- `dylan-package.json` parser.
- `.hdp` parser (legacy interop) — same grammar as LID, different extension.
- The per-file `Module:` header parser (a tiny extension to `nod-reader`'s comment scanner — the header is technically a sequence of `Key: value` comments).
- `define library` and `define module` form parsing in `nod-sema` (the parser already produces AST nodes; this sprint resolves them).
- Library and module graph construction with `use` / `import:` / `exclude:` / `rename:` / `prefix:` / `export:` / `create` resolution.
- Cycle detection with a structured diagnostic.
- `nod-driver dump-graph <lid>` Graphviz dump.
- IDE Library Browser panel.

**Out of scope for v1** (deferred to later sprints, noted inline below):

- Platform conditionalisation logic (`Platforms:` is parsed and recorded; the selection logic per host triple is Sprint 5.5 / a follow-up patch).
- Cross-library *sealing* checks (Sprint 15).
- Hot-reload edge invalidation on `use` / `export` changes (the *data structure* lands here with generation numbers; the *re-resolve trigger* lands in Sprint 08 when the REPL exists).
- Library-merge optimisation (Sprint 29 / v2).

---

## 2. The two-tier namespace

Dylan's two-tier namespace is the load-bearing static structure of the whole compiler. Get this wrong and every later sprint pays for it.

- A **library** is *the* compilation unit. It is the boundary of source-stamp cache invalidation, of code generation, of linker output (when AOT lands), and — most importantly per `MANIFESTO.md` lines 113-118 — of sealing decisions. Two libraries cannot share a module; a module belongs to exactly one library.
- A **module** is a namespace inside a library. It contains *bindings* (every `define` form contributes one). A library may contain many modules.
- A **binding** is the unit of name lookup. Every identifier in a `.dylan` source file is resolved against exactly one module — the module named in that file's `Module:` header.

Visibility composes:

| Reference shape | Resolution path |
|---|---|
| Identifier in file `f.dylan` (header `Module: m`) → `bar` defined in same module `m` | direct lookup in `m.bindings` |
| Identifier `bar` in module `m` of library `L` → defined in module `m2` of *same* library `L` | `m` must `use m2`; `m2` must export `bar` (via `export` or `create`) |
| Identifier `bar` in module `m` of library `L` → defined in module `m2` of library `L2` | `L` must `use L2` (with `m2` in `L2`'s export list); then *within* `L`, `m` must `use m2` |

ASCII view of the running example, the kernel `dylan` library:

```
                  ┌───────────────────────────────────┐
                  │ library: dylan                    │
                  │ source: dylan.lid (91 files)      │
                  │ overlay: dylan-win32.lid          │
                  │ (NO define library form anywhere) │
                  └─────────────────┬─────────────────┘
                                    │
       ┌────────────────────────────┼────────────────────────────┐
       │                            │                            │
   ┌───▼────────┐          ┌────────▼───────┐            ┌───────▼──────┐
   │ module:    │          │ module:        │            │ module:      │
   │ internal   │          │ dylan          │            │ dylan-user   │
   │ (file hdr) │          │ (exports the   │            │ (boot ns)    │
   │ implements │          │  DRM bindings) │            │              │
   │ everything │          └────────────────┘            └──────────────┘
   └────────────┘
```

The kernel library's module graph is *only* visible if you reconstruct it from per-file `Module:` headers and from `define module` forms scattered through `*.dylan` files. There is no top-level manifest of it. Section 7 below explains the algorithm.

---

## 3. LID file grammar

LID is a line-oriented header-style format with a precise but lightly documented grammar. The full grammar, derived from `E:\opendylan\sources\dylan\dylan.lid`, `dylan-win32.lid`, `sources/common-dylan/linux-common-dylan.lid`, `sources/common-dylan/win32-common-dylan.lid`, `sources/app/gctest/gctest.lid`, and `sources/app/thread-test/thread-test.lid`:

```
lid-file       := header-line* end-of-file
header-line    := key ":" value-text newline
                | continuation-line       # leading whitespace, continues previous value
                | blank-line
                | comment-line            # full-line; LID has NO inline comments
key            := ident-with-hyphens      # case-insensitive in practice
value-text     := free-form text to end of line; whitespace-trimmed
```

**Case insensitivity.** Compare `dylan.lid:1` (`Library:`) with `gctest.lid:1` (`library:`) — both occur in the upstream tree. The parser must lowercase the key before matching.

**Continuation.** A `Files:` value runs across many lines because each continuation line is indented (any leading whitespace counts). Example, `dylan.lid:5-95`:

```
Files:     dfmc-boot
           macros
           thread-macros
           …
           dylan-spy
```

The continuation tokens are whitespace-separated within and across continuation lines.

**File extension rule.** `Files:` entries are bare names: extension `.dylan` is implied. Paths with slashes are allowed (`win32-common-dylan.lid:24` has `machine-words/utilities`); they are platform-neutral (forward slashes).

**Comments.** The LID files in the upstream tree contain no comment marker — every line is either a header field, continuation, or blank. Free-form `Copyright:`/`License:`/`Warranty:` blocks (e.g. `dylan.lid:98-101`) are technically header values whose key carries metadata we ignore. The parser should accept any unknown key by recording it as a warning, not an error.

**Header fields observed in the upstream tree.** Document every one — the parser must accept all without erroring, even where it ignores the value.

| Key | Example source | Meaning to `nod-namespace` |
|---|---|---|
| `Library:` | `dylan.lid:1` | Required. The library name. Library names share a flat global namespace. |
| `Files:` | `dylan.lid:5` | Required for "leaf" LIDs (those not extending another via `LID:`). Whitespace-separated bare filenames, `.dylan` implied. Path segments allowed. |
| `Target-Type:` | `dylan.lid:4` (`dll`); `gctest.lid:8` (`executable`) | `dll` or `executable`. v1 maps both onto the same JIT path; AOT (Sprint 30) honours it. |
| `Executable:` | `dylan-win32.lid:4` (`DxDYLAN`) | Output binary name when target-type is executable. v1 records, does not act on. |
| `Start-Function:` | `gctest.lid:5` (`main`) | Entry-point identifier for executable target. |
| `Major-Version:`, `Minor-Version:` | `dylan.lid:2-3` | Library version pair. Recorded; not currently used for resolution (see §4 on `dylan-package.json` semver). |
| `LID:` | `dylan-win32.lid:7` (`dylan.lid`) | **Include directive.** The named LID's headers are read first; this file's headers extend/override. See "LID inheritance" below. |
| `Platforms:` | `dylan.lid:102-109`; `linux-common-dylan.lid:37-39` | Whitespace-separated triples (`x86-linux`, `x86_64-darwin`, `x86-win32`). The registry picks the matching LID for the host. v1 parses and records; selection algorithm is a Sprint 5.5 follow-up. |
| `Base-Address:` | `dylan-win32.lid:5` | Win32 preferred load address. Ignored under JIT; recorded for AOT. |
| `RC-Files:` | `dylan-win32.lid:6` | Windows resource files. Out of scope until AOT. |
| `C-Source-Files:` | `win32-common-dylan.lid:34` | Auxiliary C sources compiled with the library. Out of scope until FFI sprint. |
| `C-Libraries:` | `dylan.lid:97` | Linker libraries. Records the value; ignores it under JIT. |
| `Linker-Options:` | `dylan.lid:96` | Same. |
| `Origin:` | (rare; e.g. some test LIDs) | Provenance marker. Ignored. |
| `Library-Pack:` | (rare) | Grouping tag. Ignored. |
| `Synopsis:`, `Author:`, `Version:`, `Copyright:`, `License:`, `Warranty:` | ubiquitous | Documentation metadata. Recorded for `dump-graph` only. |

**LID inheritance.** `dylan-win32.lid:7` is `LID: dylan.lid` — this is the only way the kernel library ships a platform overlay. Semantics: load the parent LID first; merge this LID's fields on top; for *single-valued* fields (e.g. `Executable:`, `Base-Address:`) the child wins; for *list-valued* fields (`Files:`, `Platforms:`) we union, with the child appending. v1 implements include as "child fields shadow parent for single-valued; child extends parent for list-valued". This matches the upstream behaviour of `dylan-win32.lid` adding `Executable:` while reusing `dylan.lid`'s `Files:`.

**File ordering.** **Order is load-bearing.** The kernel `dylan.lid` lists files in dependency order (`dfmc-boot` first, then `macros`, then `boot`, then `new-dispatch`, then collection / class / dispatch infrastructure, terminating at `dylan-spy`). Section 1.2 of `PLAN.md` notes this directly. The graph builder must preserve `Files:` order — `nod-sema` walks files in declared order so that forward declarations match upstream's bootstrap discipline. This is not a "nice to have"; the kernel does not compile in any other order.

**`.hdp` files** use the same grammar with a different extension. The graph builder treats `.lid` and `.hdp` interchangeably.

---

## 4. `dylan-package.json` grammar

The newer JSON manifest. There is exactly one in the upstream tree: `E:\opendylan\dylan-package.json` (32 lines), at the *repo* root, *not* per-library. Its contents:

```json
{
    "name": "opendylan",
    "category": "compilers",
    "contact": "dylan-lang@googlegroups.com",
    "description": "The Open Dylan compiler, IDE, and core libraries",
    "dependencies": [
        "channels@0.1",
        "collection-extensions@0.1",
        "columnist@0.3",
        "command-line-parser@3.2.2",
        ...
        "xml-parser@0.2.1"
    ],
    "dev-dependencies": [ "dylan-reference-manual", "gendoc", "sphinx-extensions", "testworks" ],
    "url": "https://github.com/dylan-lang/opendylan",
    "version": "2026.1.0",
    "license": "DUAL",
    "license-url": "..."
}
```

Documented fields (parser must accept extra fields without erroring):

| Field | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | Package name (a *bundle* of libraries; not the same as a Dylan library). |
| `version` | string | yes | Free-form version. The `deft` package manager treats it as semver. |
| `dependencies` | array of `"name@versionspec"` strings | yes | Other packages this one needs at compile time. Each spec is `name@major.minor` or `name@major.minor.patch`. |
| `dev-dependencies` | array of strings | no | Build/test-only packages (here: `testworks`, `dylan-reference-manual`). |
| `category` | string | no | Loose taxonomy (`compilers`, `library`, `application`). |
| `contact` | string | no | Maintainer contact. |
| `description` | string | no | Free-form. |
| `url`, `license`, `license-url` | string | no | Documentation. |

**Relationship to LID.** `dylan-package.json` is a manifest about *the package* (a directory tree on disk, a unit of distribution, a thing `deft` can fetch). LID is a manifest about *a Dylan library* (a unit of compilation). One package contains one or more libraries; each library has its own LID. The two layers do not overlap. **`dylan-package.json` does not replace LID — `nod-namespace` requires both for a multi-package build.** v1 treats `dylan-package.json` strictly for dependency resolution and source-discovery (it tells the loader where to look for sibling packages); LID stays the per-library source of truth.

For Sprint 05, parsing `dylan-package.json` is mandatory but its dependency-resolver acts only on packages physically already present under `E:\opendylan\` or `E:\NewOpenDylan\`. Fetching from a registry is out of scope; `deft` integration is a follow-up.

---

## 5. The per-file `Module:` header

Every `.dylan` source file begins with a header block of `Key: value` lines — these are technically end-of-line headers parsed before the first Dylan token. The only one with semantic weight is `Module:`. From `E:\opendylan\sources\dylan\boot.dylan:1`:

```
Module:    internal
Author:    Jonathan Bachrach
Copyright: …
```

And `E:\opendylan\sources\dylan\dfmc-boot.dylan:1`:

```
Module:    dylan-user
Synopsis:  Definition of the Dylan library
```

**Semantics.** `Module: foo` declares that every `define …` form in the rest of this file contributes a binding to module `foo`, and every identifier reference in the file is resolved against module `foo`'s import/export set.

**Why this matters for the kernel.** The kernel `dylan` library has *no* `define library dylan` form. Section 2 of `OVERVIEW.txt` confirms this is intentional — the library is bootstrapped by the compiler. So the only way to discover that, say, `boot.dylan` belongs to a module called `internal`, is to read the file's `Module:` header. The graph builder cannot assume a `define library` exists for every library.

**Parser placement.** `nod-reader` already scans the leading comment / header block (Sprint 04 deliverable: "`Module:` header comment parsed and attached to the AST root"). Sprint 05 *uses* that field — `nod-namespace` reads `ast.module_header` for each parsed file when building module → file edges.

---

## 6. `define module` and `define library` forms

These are the in-Dylan syntactic forms that declare module and library structure. They are parsed as ordinary top-level forms by `nod-reader` (Sprint 04) and semantically resolved by `nod-namespace` in Sprint 05.

### 6.1 `define module`

Full syntax, distilled from `E:\opendylan\sources\common-dylan\library.dylan`:

```
define module NAME
  use OTHER-MODULE
    [, import: { ID , … } | import: all ]
    [, exclude: { ID , … } ]
    [, rename: { OLD => NEW , … } ]
    [, prefix: "STRING" ]
    [, export: { ID , … } | export: all ] ;
  create ID , ID , … ;
  export ID , ID , … ;
end [module [NAME]] ;
```

**Concrete example** (`common-dylan/library.dylan:71-147`):

```dylan
define module common-extensions
  use dylan-extensions,
    export: { <bottom>,
              <stack-overflow-error>,
              ... };
  use simple-debugging,
    export: { \assert, \debug-assert, debug-message };
  use byte-vector,
    export: { <byte-vector> };
  create <closable-object>,
         <stream>,
         close,
         integer-length,
         ...
         exit-application;
end module common-extensions;
```

Semantic notes:

- **`use M`** imports all of `M`'s exported bindings into this module. With `import:` it imports only the listed names; without `import:` it imports everything.
- **`exclude:`** removes the listed names from what's otherwise imported.
- **`rename:`** introduces the imported binding under a new local name.
- **`prefix: "p_"`** prepends `"p_"` to every imported name.
- **`export:`** within a `use` clause re-exports the imported bindings to this module's own importers. `export: all` re-exports everything.
- **`create`** declares names this module *introduces* (so other modules can import them by name) without itself defining them — useful for protocol modules that only declare a vocabulary. The actual `define class <closable-object>` etc. live in some other module (typically `common-dylan` itself or an internal implementation module) which `use`s this one and provides definitions for the created names.
- **`export`** (top-level) declares which names from this module's own definitions are visible to importers.

### 6.2 `define library`

Syntax:

```
define library NAME
  use OTHER-LIBRARY
    [, import: { MODULE-ID , … } ]
    [, export: { MODULE-ID , … } ] ;
  export MODULE-ID , MODULE-ID , … ;
end [library [NAME]] ;
```

Concrete example (`E:\opendylan\sources\common-dylan\library.dylan:10-32`):

```dylan
define library common-dylan
  use dylan,
    export: { dylan,
              finalization,
              threads };
  export
    common-dylan,
    common-extensions,
    streams-protocol,
    ...
    transcendentals;

  // For the test suite only.
  export
    common-dylan-internals;
end library common-dylan;
```

A simpler one, `E:\opendylan\sources\app\gctest\library.dylan:9-15`:

```dylan
define library gctest
  use common-dylan;
  use io;
  export gctest;
end library gctest;
```

Semantics:

- **`use L`** declares this library depends on library `L`. Modules in `L`'s export set become available for *this* library's modules to `use`. Without `import:`, all of `L`'s exported modules are visible.
- **`export`** at library level lists *modules* (not bindings) that this library makes available to consumer libraries. A module not in this list is internal even though it may be `export`-rich at the module level.
- **`export: { … }` inside a `use` clause** re-exports modules. So `common-dylan` `use dylan, export: { dylan, finalization, threads }` means a consumer of `common-dylan` can directly `use dylan` without needing `use dylan` of its own at library level — `common-dylan` has re-exported it.

### 6.3 Special-case: the kernel library

The kernel `dylan` library has **no `define library dylan` form anywhere**. Verified by exhaustive scan of `E:\opendylan\sources\dylan\` on 2026-05-16 — none of the 91 `.dylan` files contains the string `define library`. The closest is `dfmc-boot.dylan` whose entire body is `boot-dylan-definitions();` (called by the compiler at bootstrap).

This is intentional: `dylan` is the library every other library implicitly `use`s and is supplied by the implementation. Our compiler must therefore **bootstrap the `dylan` library node from the LID alone** — no `define library` is parsed for it, but it still exists as a Library object in the graph, with name `dylan`, with its 91 files, and with whatever export set the `define module dylan ... end` form (in `boot.dylan` or wherever) declares.

The Sprint 05 acceptance criterion "Loads `dylan.lid` (91 files) and produces a complete module graph" should therefore be read as **two passes**:

1. Parse the LID. Build a Library node named `dylan`. Register its 91 source files in declared order.
2. Walk those 91 source files. For each, read its `Module:` header and any `define module` / `define library` forms it contains. Construct the modules. (For the kernel, `define library` is absent; for every other library, it's present in a file conventionally named `library.dylan`.)

If after the second pass any file's `Module:` header names a module that no `define module` form ever declared, emit a structured diagnostic. (Some kernel files reference module `internal` whose definition is implicit; the parser may need to either tolerate this for the kernel via an allowlist or accept that the bootstrap defines `internal` purely by being mentioned. v1 chooses the second: an undeclared module mentioned in a `Module:` header materialises as an *implicit module* node with no exports — this matches upstream's behaviour of the compiler synthesising it.)

---

## 7. In-memory graph representation

Proposed Rust types for `nod-namespace`. Pseudo-code; refine in implementation.

```rust
pub struct LibraryId(u32);
pub struct ModuleId(u32);
pub struct BindingId(u32);
pub struct Symbol(u32);          // interned identifier

pub struct Library {
    pub id: LibraryId,
    pub name: Symbol,
    pub uses: Vec<LibraryUse>,           // resolved `use L` from define library
    pub modules: Vec<ModuleId>,          // every module belonging here
    pub exports: Vec<ModuleId>,          // exported (visible to consumers)
    pub source_lid: PathBuf,             // LID file
    pub source_package_json: Option<PathBuf>,
    pub source_library_dylan: Option<PathBuf>, // file containing define library, if any
    pub files: Vec<PathBuf>,             // in declared order — significant
    pub generation: u64,                 // bumped on any change
    pub diagnostics: Vec<Diagnostic>,
}

pub struct LibraryUse {
    pub library: LibraryRef,             // resolved or unresolved
    pub imported_modules: Option<Vec<Symbol>>, // None == all exported
    pub reexported_modules: Vec<Symbol>,
}

pub struct Module {
    pub id: ModuleId,
    pub library: LibraryId,
    pub name: Symbol,
    pub uses: Vec<ModuleUse>,
    pub creates: Vec<Symbol>,            // names declared by `create`
    pub exports: Vec<Symbol>,            // names made visible
    pub bindings: HashMap<Symbol, BindingId>, // defined-in-this-module
    pub source_files: Vec<PathBuf>,      // every .dylan with Module: this-name
    pub generation: u64,
}

pub struct ModuleUse {
    pub module: ModuleRef,
    pub import: Import,                  // All | Listed(Vec<Symbol>)
    pub exclude: Vec<Symbol>,
    pub rename: Vec<(Symbol, Symbol)>,
    pub prefix: Option<String>,
    pub reexport: Reexport,              // None | All | Listed(Vec<Symbol>)
}

pub enum LibraryRef { Resolved(LibraryId), Unresolved(Symbol) }
pub enum ModuleRef  { Resolved(ModuleId),  Unresolved { library: Option<Symbol>, module: Symbol } }
```

**Cross-library lookup.** Resolving a name `foo` in module `m` of library `L`:

1. If `foo` is defined in `m.bindings`, done.
2. Else for each `u: ModuleUse` in `m.uses`: if `foo` (after `rename` reversal, `prefix` strip) is in `u.module`'s effective export set and not in `u.exclude` and (if `u.import` is `Listed`) in the imported list — match found.
3. If `u.module` lives in a different library `L2`, the lookup additionally requires that `L2` exports the module and that `L` `use`s `L2`. This is a *check* at use-time, not at lookup-time: the graph builder validates `use` clauses against library `use` clauses and rejects illegal references with a diagnostic.

**Intra-library cross-module lookup.** Same algorithm, but step 3 collapses (library check is trivial). Both paths go through the same `ModuleUse` traversal — the library boundary is enforced *at graph construction* by validating that every module's `use` clause is satisfied by a corresponding library-level `use` plus library-level `export`.

---

## 8. Per-library and per-module generation numbers

`MANIFESTO.md` lines 172-196 commits to live incremental compilation:

> Saving a file is the compile trigger. … Compilation is per-definition, not per-file. … Per-library generation numbers for hot reload. Editing one library does not invalidate others unless cross-library exports changed shape.

The graph data structures here are the foundation for that. Specifically:

- Every `Library` and every `Module` carries a `generation: u64` field, bumped on any structural change to it (use-clause edit, export-list edit, create-list edit, addition or removal of a defined binding).
- Per-binding generations come later (Sprint 13's inline cache hooks them); the graph layer cares only about *shape* changes.
- A `LibraryRef::Resolved(id)` cache entry is valid only as long as the target library's generation has not advanced past the resolution's recorded generation.
- A cross-library `use` only invalidates the consumer when the producer's *export shape* changes — adding a new internal module to a producing library does not bump anything visible to consumers. This implements the manifesto's "editing one library does not invalidate others unless cross-library exports changed shape" exactly.

Lift from NCL: `E:\CL\NewCormanLisp\src\ncl-loader\` already implements a generation-counted dependency graph with `GenerationCounter`, retirement on stale references, and source-stamp-keyed cache lookup. The NCL design assumes a flat namespace (CL packages); we extend it to two tiers by giving libraries *and* modules their own counters. The cache key NCL uses — `(source hash, compiler version, codegen flags, LLVM version)` per `MANIFESTO.md` line 335 — extends in NewOpenDylan with the library-graph generation, so that a hot-edit to a re-exported module invalidates downstream caches deterministically.

`nod-loader` (which gets actually populated this sprint or early in Sprint 08) hosts the dirty-tracking state; `nod-namespace` provides only the graph and the generation fields.

---

## 9. Sealing and library boundaries

Sealing is Sprint 15's deliverable, not this one. The hook here is in the data structures:

- Every `Library` carries a `sealed_classes: HashSet<Symbol>` and `sealed_generics: HashSet<Symbol>` set, populated when Sprint 15's analysis runs. The graph layer guarantees the *containers* exist and that sealing facts are attached to the *defining library*, not to global state.
- By default, sealing facts are visible only inside the library that introduced them. Cross-library sealing is opt-in via explicit `define sealed domain` declarations — the analyser surfaces those across library boundaries through the library's exported sealed-domain list, which is again attached to the `Library` node here.
- Full design: `E:\NewOpenDylan\docs\SEALING.md` (currently a one-paragraph stub from Sprint 01; fleshed out in Sprint 15).

Library-merge optimisation (Sprint 29 / v2 candidate per `PLAN.md` §2.5(f)) is the only feature that crosses library sealing boundaries; the data structures here support it without commitment.

---

## 10. Test plan

Ordered from trivial to the kernel. Every entry is an LID under `E:\NewOpenDylan\opendylan-tests\sources\` (paths relative to that root, mirroring the upstream tree per `INVENTORY.md`).

| # | LID | What it exercises |
|---|---|---|
| 1 | `app/thread-test/thread-test.lid` | Tiniest valid LID: three keys (`library:`, `executable:`, `files:`), one file. Smoke-test the parser. |
| 2 | `app/gctest/gctest.lid` | Slightly larger: `start-function:`, `target-type: executable`, `major-version:`/`minor-version:`. Three files. Includes a `library.dylan` with `define library gctest use common-dylan; use io; export gctest; end`. First exercise of `use` + `export`. |
| 3 | `app/dylan-playground/dylan-playground.lid` | Single-library application with a non-trivial module set. Exercises mid-size `define library` + several `define module` forms. |
| 4 | `app/flying-squares/flying-squares.lid` | Application using a GUI library — exercises multi-library `use` chains down to `common-dylan`. |
| 5 | `common-dylan/win32-common-dylan.lid` + `common-dylan/linux-common-dylan.lid` | Two platform-specific LIDs for the same library name. Exercises `Platforms:` field parsing and platform-conditional selection. (Selection logic itself is Sprint 5.5.) |
| 6 | `common-dylan/library.dylan` (paired with whichever platform LID applies) | Largest realistic `define library` + many `define module` forms — `common-dylan`, `common-extensions`, `simple-format`, `simple-random`, `streams-protocol`, `byte-vector`, `byte-storage`, `machine-words`, `transcendentals`. Exercises `create`, multi-clause `use` with `import:`, `export:`, `exclude:`, deep cross-module `use` chains within one library. |
| 7 | `common-dylan/tests/common-dylan-test-suite.lid` + `common-dylan/tests/common-dylan-test-suite-app.lid` | The `testworks` test-suite + test-app pair convention (per `INVENTORY.md` §1.1). Exercises a `dll` library and an `executable` library that `use`s it. |
| 8 | `dylan/dylan.lid` | **The kernel.** 91 files. No `define library`. Module graph rebuilt entirely from per-file `Module:` headers + scattered `define module` forms. Verifies the bootstrap branch of the graph builder. |
| 9 | `dylan/dylan-win32.lid` (with `LID: dylan.lid`) | Exercises the `LID:` include directive. Verifies that field merge (single-valued override, list-valued union) produces the right combined library. |
| 10 | `testing/benchmarks/richards/simple-richards.lid` | Single-library benchmark consuming the kernel; small enough to run end-to-end once codegen is up (Sprint 16). Used here only for graph construction. |

For each, the Sprint 05 test asserts: the LID parses; the library is constructed; every file's `Module:` header resolves to a real module; every `use` / `export` resolves or produces a structured "unresolved" diagnostic; the generated `dump-graph` is a well-formed Graphviz document that `dot -Tpng` consumes without warnings.

The IDE Library Browser is exercised by opening each LID in the panel and visually confirming the tree shape (manual smoke test — automating IDE tests is a Sprint 31 deliverable).

---

## 11. Open questions

Candid list of what this spec cannot pin down without further excavation.

1. **`Module:` header in `.hdp` files.** Some `.hdp` files in the upstream tree have key sets I have not exhaustively catalogued (`OPEN-DYLAN:`, `Version:` interpolations like `Version: $HostName$`). The parser must tolerate them; the meaning of `$HostName$` (build-time substitution) is unclear and deferred.

2. **Platform selection algorithm.** When `dylan.lid` declares `Platforms: arm-linux x86-freebsd …` (no `x86_64-pc-windows-msvc`) and `dylan-win32.lid` declares `Platforms: x86-win32`, our Windows host is supposed to pick the latter via the upstream `sources/registry/` mechanism. We have not yet ported the registry; for Sprint 05, the driver takes an explicit `--platform` flag and the user picks. Replacing this with auto-detection is a Sprint 5.5 follow-up.

3. **Re-export shape under `export: all`.** When module M does `use N, export: all` and N later gains a new export, does M's re-export set grow retroactively? The DRM says yes (re-exports are by-reference). Our generation-bump rule must therefore treat M's effective export set as recomputed on N's generation bump — straightforward, but worth noting because the alternative ("snapshot on use") is tempting and wrong.

4. **`dylan-package.json` for the kernel.** Upstream has exactly one `dylan-package.json`, at the repository root. The Sprint 05 spec assumes every package has one; the kernel's "package" is therefore the whole `opendylan` repo, not the kernel library specifically. This is fine for our purposes but means the "every library has a package.json" claim is wrong — every *package* has one, and one package contains many libraries. We document but do not enforce.

5. **`create` without follow-up `define`.** A module may `create` a name and never have any other module supply a definition for it (e.g. it's used only as a protocol marker). Is that an error? The DRM says no; the compiler should emit a warning if no implementation appears. We will emit a deferred-warning at the end of graph construction; the implementation choice is "warn at sema, not at namespace".

6. **`define module` outside `library.dylan`.** Some libraries put `define module` forms in separate files (`foo-module.dylan` next to `library.dylan`). The graph builder finds them by walking every file in the LID looking for top-level `define module` / `define library` forms, *not* by special-casing filenames. This is more work than the file-name heuristic upstream uses but is correct. Confirmed by spot-check of `sources/dfmc/` libraries.

7. **Module names colliding with library names.** Library `dylan` exports module `dylan`. Library `common-dylan` exports module `common-dylan`. There's no rule preventing this — disambiguation is contextual (library names live in library namespace, module names in their library's module namespace, and `use` clauses make the context unambiguous). The graph types reflect this through separate `LibraryId` / `ModuleId` newtypes; the diagnostic messages must be careful to qualify the kind when reporting.
