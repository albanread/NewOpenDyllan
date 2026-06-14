# Upstream Open Dylan — adoption notes

Open Dylan is the reference Dylan implementation. Its source tree lives
externally at `E:\opendylan\` and is mirrored in the project's working
notes for code-review and selective adoption. **NewOpenDylan is a from-
scratch implementation** with its own architecture (Rust frontend +
LLVM/Factor backends, NewGC, hand-rolled DFM). Open Dylan is treated as
a *reference and parts catalogue*, never as a transplant donor for
compiler internals.

This document captures the standing verdict on what's worth lifting,
what's reference-only, what to ignore, and the workflow for any
selective adoption.

## License

`E:\opendylan\License.txt` covers the whole source tree under two
permissive licenses:

* **Functional Objects MIT-style** (1995–2004 Functional Objects, Inc.;
  2004–2023 Dylan Hackers). Standard MIT terms — use, modify,
  redistribute, sublicense, sell. Requires the copyright notice and
  permission notice be preserved in copies and substantial portions.

* **Gwydion CMU license** for files inherited from CMU's Gwydion Dylan
  (~1994). Permissive, allows commercial use and derivative works,
  requires:
  1. Full copyright notice retention on copies and appropriate parts
     of derivative works.
  2. Documentation accompanying any system that incorporates Gwydion-
     derived code must acknowledge the contribution of the Gwydion
     Project at Carnegie Mellon University.

**Practical obligation when lifting any file:**

1. Preserve the original header comment block intact (Module/Synopsis/
   Author/Copyright/License/Warranty).
2. Add a `// Adapted from Open Dylan, <path/in/their/tree>, <commit-or-version>`
   line above any modifications.
3. If the original is Gwydion-derived (the header will say so),
   `README.md` must acknowledge CMU's Gwydion contribution. One line
   in our acknowledgements section is sufficient.

Both licenses are compatible with NewOpenDylan's existing structure.

## Verdict by directory

### `sources/dylan/` — runtime library (96 files)

The Dylan standard library written in Dylan. Two distinct sub-bodies:

| Sub-body | Files (examples) | Verdict |
|---|---|---|
| **Surface library** | `collection.dylan`, `table.dylan`, `string.dylan`, `range.dylan`, `condition.dylan`, `sort.dylan`, `sequence.dylan`, `vector.dylan`, `deque.dylan`, `set.dylan`, `array.dylan`, `accumulator.dylan`, `comparison.dylan`, `functional.dylan`, `hashing.dylan`, `number.dylan`, `symbol-table.dylan` | **Lift selectively.** Each method is testable in isolation. Primitives largely already exist in NewOpenDylan or have clear equivalents. Adopt opportunistically when a sprint surfaces a missing piece. |
| **Compiler-internal library** | `class.dylan`, `class-dynamic.dylan`, `dispatch.dylan`, `dispatch-caches.dylan`, `new-dispatch.dylan`, `slot-dispatch.dylan`, `slot-descriptor.dylan`, `slot-descriptor-dynamic.dylan`, `generic-function.dylan`, `function.dylan`, `discrimination.dylan`, `domain.dylan`, `signature.dylan`, `singleton.dylan`, `subclass.dylan`, `type.dylan`, `union.dylan`, `dylan-mm.dylan` | **Ignore for transplant; reference only for semantics.** These implement Dylan's object system AS Dylan, on top of Open Dylan's specific runtime layout (Boehm GC, MM-locks, dispatch-cache shape). NewOpenDylan already has its own answers from Sprint 22 (`<table>`), Sprint 23 (NewGC), Sprint 38e (cache slots). Different memory layouts, different metadata, different dispatch ABI. |

### `sources/dfmc/reader/` — lexer + parser

* `lexer.dylan`, `lexer-transitions.dylan`, `lexer-support.dylan`,
  `classification.dylan` — state-table-driven lexer (transitions table
  is auto-generated). **Reference only.** NewOpenDylan's Sprint 45a/b
  lexer is hand-coded direct-style; the two approaches don't transplant
  cleanly. Use to cross-check token kinds and edge cases (unusual
  escapes, radix literals, symbol forms).
* `parser.dylgram` — grammar file in yacc-like format. **Big asset for
  Track 2 / Sprint 46 (parser-in-Dylan).** Use as primary BNF reference
  whether the parser is hand-built or generator-driven.
* `infix-parser.dylan`, `fragments.dylan` — Open Dylan's infix-operator
  handling and fragment AST. Reference only; NewOpenDylan's AST shape
  is different.

### `sources/dfmc/macro-expander/` — ~4,500 lines

```
pattern-back-end.dylan       830
template-back-end.dylan      679
pattern-to-function.dylan    572
pattern-to-code.dylan        508
expanders.dylan              272
... (15 more files)
```

**The single biggest asset in the upstream tree** for NewOpenDylan's
year-3 plan. Dylan's macro system is rule-based with templates, pattern
variables, hygiene, the full DRM spec. NewOpenDylan currently has only
a tiny `unless`/`when`-style macro form. When the "real Dylan macros"
sprint arrives, Open Dylan's macro-expander is a complete, working,
debugged reference implementation of DRM macro semantics — cribbing
the algorithms saves weeks of design work. **Not a clean transplant**
(depends on Open Dylan's fragment shape), but irreplaceable as design
reference.

### Directories to ignore

* `sources/dfmc/back-end/`, `c-back-end/`, `llvm-back-end/` — Open
  Dylan's compiler back-ends. Different IR (their DFM = Dylan Flow
  Model, not the same as NewOpenDylan's DFM despite the name collision).
  Predates LLVM 15+ APIs.
* `sources/dfmc/harp-cg`, `harp-x86-cg`, `harp-native-cg` — Harlequin/
  Apple Dylan-era 32-bit x86 codegen. Historical.
* `sources/dfmc/flow-graph`, `optimization` — optimisation passes
  against Open Dylan's DFM. Architecture-mismatched.
* `sources/duim/`, `deuce/`, `corba/`, `ole/`, `harp-browser-support/`,
  `environment/`, `runtime-manager/`, `project-manager/` — GUI / IDE /
  interop frameworks. Out of scope.
* `sources/c-ffi/`, `c-linker/` — NewOpenDylan has its own Sprint 27–32
  FFI machinery.

## Fit with the three tracks

| Asset | Track 1 (Rust+LLVM, now) | Track 2 (Dylan self-host) | Track 3 (Factor VM) |
|---|---|---|---|
| Surface stdlib (collection, string, table, sort, condition, range, …) | Lift selectively now | Same | Same |
| Compiler-internal stdlib (dispatch, class, slot-descriptor) | Ignore | Ignore | Ignore |
| Reader (lexer) | Cross-check Sprint 45 token kinds | Reference only — Sprint 45a/b shipped | Same |
| `parser.dylgram` | N/A | **Primary BNF for parser-in-Dylan sprint** | Same |
| Macro expander | N/A | **Primary design reference for "real macros" sprint** | Same |
| Compiler back-ends | Ignore | Ignore | Ignore |

## Workflow for lifting a piece

1. **Identify the specific need.** E.g., "Sprint 50 wants `reduce1`."
2. **Locate the method** in upstream — usually `collection.dylan` or
   the file matching the name.
3. **Check primitive dependencies.** If it calls `%primop` forms that
   NewOpenDylan doesn't have, decide: add the primop, or rewrite the
   method against existing primitives.
4. **Copy the method** to the appropriate file under
   `src/nod-dylan/dylan-sources/`. Preserve the original file's header
   comment block; add an `// Adapted from Open Dylan, sources/dylan/
   collection.dylan, <commit-hash>` attribution above the method.
5. **Update `README.md`** if this is the first Gwydion-derived file
   landing (the acknowledgement only needs to be there once).
6. **Add regression tests** in `tests/nod-tests/tests/sema.rs` or
   `tests/nod-tests/tests/aot_dylan.rs` exercising the lifted method.
7. **Commit with message** `Lift <method>(...) from Open Dylan <path>`
   and the standard Claude co-author trailer.

Lifted code follows the standard NewOpenDylan style and may be
reformatted/restructured as needed — the license requires preserving
the copyright notice, not the literal source.

## Anti-patterns

* **Don't lift Open Dylan's class.dylan / dispatch.dylan / slot-*.dylan.**
  These are deeply tied to Open Dylan's MM (Boehm GC), dispatch-cache
  layout, and class-metadata format. NewOpenDylan's runtime contract
  is different.
* **Don't lift the lexer-transitions.dylan state table.** It's
  auto-generated against a specific lexer-generator output format we
  don't have.
* **Don't lift the back-end directories** under any circumstances.
  Different IR, different runtime, different era.
* **Don't lift the macro-expander wholesale as a one-shot.** Use it as
  a design reference for the macro sprint; the actual implementation
  needs to be against NewOpenDylan's AST/fragment shape.
