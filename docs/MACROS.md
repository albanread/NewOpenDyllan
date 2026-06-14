# NewOpenDylan Macros — design stub

*Pattern-rule macros shipped (Sprint 17+); `define macro` is the default home for new control-flow surface — `unless`, `when`, `cond`, `with-*` all live in `stdlib.dylan` as macros. Maintained reference: [`manual/compiler/macro-expander.md`](manual/compiler/macro-expander.md).*

Dylan has **hygienic pattern-rule macros**, defined via `define macro …`.
They are essential — much of the `dylan` kernel library is macro-driven, so
nothing meaningful in `dylan-sources/` runs until macros work. See PLAN.md §4
and SPRINTS.md Sprints 17–18.

Surface:

- **Pattern variables** — `?name`, `?:expression`, `?:body`, `?:variable`,
  `?:type`, `?:name`, etc. — typed match constraints.
- **Aux rules** — `=>` clauses for sub-expansion.
- **Hygiene** — bindings introduced by the macro are renamed; references in
  the macro's defining scope are preserved.
- **Source-location preservation** — every expanded form carries a span that
  links back to the macro use site, surfaced in IDE error views.

Where macro expansion lives:

- **`nod-macro`** (own crate): pattern matcher + expander. Runs between the
  parser (`nod-reader`) and the namespace resolver (`nod-namespace`). The
  expander emits fully-formed AST nodes with span back-pointers; sema and
  later phases see the post-expansion tree.

Open questions:
- Macro-defining macros — bootstrap order in incremental compilation.
- `define macro` redefinition under live incremental compilation.
- Module-level vs library-level macro export — interaction with the namespace
  graph.

Sealing visualiser ↔ macro expander: when a macro expands into a sealed call,
the visualiser shows both the user-written source span and the
post-expansion form.

See SPRINTS.md Sprints 17–18 for the implementation slice.
