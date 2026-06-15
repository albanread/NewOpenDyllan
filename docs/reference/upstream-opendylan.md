# Upstream Open Dylan

Open Dylan is the reference Dylan implementation. Its source tree lives
externally at `E:\opendylan\` and is mirrored in the project's working notes for
code review and selective adoption. NewOpenDylan is a from-scratch
implementation with its own architecture — a **Dylan front-end and a Rust/LLVM
back-end, split at the DFM IR**, with NewGC and a hand-rolled DFM. Open Dylan is
treated as a *reference and parts catalogue*, never as a transplant donor for
compiler internals; the compiler is written fresh against the DRM.

This page captures the standing verdict on what is worth lifting, what is
reference-only, what to ignore, and the workflow for any selective adoption.

## License

`E:\opendylan\License.txt` covers the whole source tree under two permissive
licenses:

* **Functional Objects MIT-style** (1995–2004 Functional Objects, Inc.;
  2004–2023 Dylan Hackers). Standard MIT terms — use, modify, redistribute,
  sublicense, sell. Requires that the copyright notice and permission notice be
  preserved in copies and substantial portions.

* **Gwydion CMU license** for files inherited from CMU's Gwydion Dylan (~1994).
  Permissive, allows commercial use and derivative works, requires:
  1. Full copyright notice retention on copies and appropriate parts of
     derivative works.
  2. Documentation accompanying any system that incorporates Gwydion-derived
     code must acknowledge the contribution of the Gwydion Project at Carnegie
     Mellon University.

**Practical obligation when lifting any file:**

1. Preserve the original header comment block intact (Module / Synopsis /
   Author / Copyright / License / Warranty).
2. Add an `// Adapted from Open Dylan, <path/in/their/tree>, <commit-or-version>`
   line above any modifications.
3. If the original is Gwydion-derived (the header will say so), `README.md` must
   acknowledge CMU's Gwydion contribution. One line in the acknowledgements
   section is sufficient.

Both licenses are compatible with NewOpenDylan's structure.

## Verdict by directory

### `sources/dylan/` — runtime library

The Dylan standard library written in Dylan. Two distinct sub-bodies:

| Sub-body | Files (examples) | Verdict |
|---|---|---|
| **Surface library** | `collection.dylan`, `table.dylan`, `string.dylan`, `range.dylan`, `condition.dylan`, `sort.dylan`, `sequence.dylan`, `vector.dylan`, `deque.dylan`, `set.dylan`, `array.dylan`, `accumulator.dylan`, `comparison.dylan`, `functional.dylan`, `hashing.dylan`, `number.dylan`, `symbol-table.dylan` | **Lift selectively.** Each method is testable in isolation. Primitives largely already exist in NewOpenDylan or have clear equivalents. Adopt opportunistically when a missing piece surfaces. |
| **Compiler-internal library** | `class.dylan`, `class-dynamic.dylan`, `dispatch.dylan`, `dispatch-caches.dylan`, `new-dispatch.dylan`, `slot-dispatch.dylan`, `slot-descriptor.dylan`, `slot-descriptor-dynamic.dylan`, `generic-function.dylan`, `function.dylan`, `discrimination.dylan`, `domain.dylan`, `signature.dylan`, `singleton.dylan`, `subclass.dylan`, `type.dylan`, `union.dylan`, `dylan-mm.dylan` | **Ignore for transplant; reference only for semantics.** These implement Dylan's object system as Dylan, on top of Open Dylan's specific runtime layout (Boehm GC, MM-locks, dispatch-cache shape). NewOpenDylan has its own answers (`<table>`, NewGC, cache slots) with different memory layouts, metadata, and dispatch ABI. |

### `sources/dfmc/reader/` — lexer + parser

* `lexer.dylan`, `lexer-transitions.dylan`, `lexer-support.dylan`,
  `classification.dylan` — a state-table-driven lexer (the transitions table is
  auto-generated). **Reference only.** NewOpenDylan's lexer is hand-coded
  direct-style; the two approaches do not transplant cleanly. Use to cross-check
  token kinds and edge cases (unusual escapes, radix literals, symbol forms).
* `parser.dylgram` — grammar file in yacc-like format. **A big asset for the
  parser-in-Dylan work.** Use as the primary BNF reference whether the parser is
  hand-built or generator-driven.
* `infix-parser.dylan`, `fragments.dylan` — Open Dylan's infix-operator handling
  and fragment AST. Reference only; NewOpenDylan's AST shape is different.

### `sources/dfmc/macro-expander/`

```
pattern-back-end.dylan       830
template-back-end.dylan      679
pattern-to-function.dylan    572
pattern-to-code.dylan        508
expanders.dylan              272
... (15 more files)
```

The single biggest asset in the upstream tree for NewOpenDylan's macro work.
Dylan's macro system is rule-based with templates, pattern variables, hygiene,
and the full DRM spec. NewOpenDylan currently has only a small
`unless`/`when`-style macro form. When the full Dylan-macros work arrives, Open
Dylan's macro-expander is a complete, working, debugged reference implementation
of DRM macro semantics — cribbing the algorithms saves weeks of design work. It
is **not a clean transplant** (it depends on Open Dylan's fragment shape), but it
is irreplaceable as a design reference.

### Directories to ignore

* `sources/dfmc/back-end/`, `c-back-end/`, `llvm-back-end/` — Open Dylan's
  compiler back-ends. Different IR (their DFM = Dylan Flow Model, not the same as
  NewOpenDylan's DFM despite the name collision). Predates LLVM 15+ APIs.
* `sources/dfmc/harp-cg`, `harp-x86-cg`, `harp-native-cg` — Harlequin / Apple
  Dylan-era 32-bit x86 codegen. Historical.
* `sources/dfmc/flow-graph`, `optimization` — optimisation passes against Open
  Dylan's DFM. Architecture-mismatched.
* `sources/duim/`, `deuce/`, `corba/`, `ole/`, `harp-browser-support/`,
  `environment/`, `runtime-manager/`, `project-manager/` — GUI / IDE / interop
  frameworks. Out of scope.
* `sources/c-ffi/`, `c-linker/` — NewOpenDylan has its own FFI machinery.

## Fit by track

| Asset | Front-end + back-end (now) | Dylan self-host |
|---|---|---|
| Surface stdlib (collection, string, table, sort, condition, range, …) | Lift selectively now | Same |
| Compiler-internal stdlib (dispatch, class, slot-descriptor) | Ignore | Ignore |
| Reader (lexer) | Cross-check token kinds | Reference only — lexer shipped |
| `parser.dylgram` | N/A | Primary BNF for the parser-in-Dylan work |
| Macro expander | N/A | Primary design reference for the full-macros work |
| Compiler back-ends | Ignore | Ignore |

## Workflow for lifting a piece

1. **Identify the specific need.** E.g., "we want `reduce1`."
2. **Locate the method** in upstream — usually `collection.dylan` or the file
   matching the name.
3. **Check primitive dependencies.** If it calls `%primop` forms that
   NewOpenDylan does not have, decide: add the primop, or rewrite the method
   against existing primitives.
4. **Copy the method** to the appropriate file under
   `stdlib/`. Preserve the original file's header comment
   block; add an `// Adapted from Open Dylan, sources/dylan/collection.dylan,
   <commit-hash>` attribution above the method.
5. **Update `README.md`** if this is the first Gwydion-derived file landing (the
   acknowledgement only needs to be there once).
6. **Add regression tests** exercising the lifted method.
7. **Commit** with a message that records the source path.

Lifted code follows the standard NewOpenDylan style and may be
reformatted/restructured as needed — the license requires preserving the
copyright notice, not the literal source.

## Anti-patterns

* **Do not lift Open Dylan's `class.dylan` / `dispatch.dylan` / `slot-*.dylan`.**
  These are deeply tied to Open Dylan's MM (Boehm GC), dispatch-cache layout, and
  class-metadata format. NewOpenDylan's runtime contract is different.
* **Do not lift the `lexer-transitions.dylan` state table.** It is auto-generated
  against a specific lexer-generator output format we do not have.
* **Do not lift the back-end directories** under any circumstances. Different IR,
  different runtime, different era.
* **Do not lift the macro-expander wholesale as a one-shot.** Use it as a design
  reference; the actual implementation needs to be against NewOpenDylan's
  AST/fragment shape.

## See also

- [Platforms](platforms.md) — Open Dylan has separate Win32/macOS variants in its
  source tree; when lifting, split the portable piece from the platform-specific
  piece.

---

Reference: [Platforms](platforms.md) | [Performance](performance.md) | [Known limitations](known-limitations.md) | [Tracing](tracing.md) | [Architecture](../architecture.md) | [Glossary](../glossary.md)
