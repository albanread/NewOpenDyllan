# Macro boundary — kernel forms vs Dylan macros

> **Where new control-flow surface lives, and the rules that keep it
> from drifting into hardcoded AST by default.**

Companion to `docs/STDLIB_BOUNDARY.md` (Rust vs Dylan) and
`docs/UPSTREAM_OPENDYLAN.md` (lift workflow).

## Goal

NewOpenDylan has the right machinery: `nod-macro` is a 1,805-line
real macro engine supporting multi-rule `define macro`, pattern
variables, hygiene, statement-position recognition. Sprint 25
already retired hardcoded `Expr::Unless` in favour of body-shaped
`Expr::MacroCall`.

What's missing is *discipline going forward*. Every new surface form
("a `select` statement", "a `with-mutex` block", "a sugary `repeat`
loop") is a temptation to add an `Expr::*` or `Statement::*` variant
to the AST. **This document says no.** New surface lands as
`define macro` in `stdlib.dylan`; the AST stays kernel-shaped.

## Five rules

### 1. Default: new surface forms are macros in `stdlib.dylan`

Any new control-flow keyword, iteration form, binding-shape, or
"sugar" goes in as a `define macro` in
`src/nod-dylan/dylan-sources/stdlib.dylan` (or a sibling
`.dylan` file). New `Expr::*` or `Statement::*` variants in
`nod-reader::ast` are the exception, not the default.

### 2. Legitimate reasons to add a hardcoded AST variant (the gate)

The frozen kernel list, justified by what they actually require:

| Variant | Why it's hardcoded |
|---|---|
| `Expr::If` | Branching primitive; everything else lowers to it |
| `Expr::Begin` | Sequencing; the basic compound expression |
| `Expr::Let` | Binding; introduces names into the lexical scope |
| `Expr::Method` / `LocalMethod` | Function-literal primitive |
| `Expr::Call` / `BinOp` / `UnOp` / `Paren` | Call shape, infix operators |
| `Expr::Ident` / `Integer` / `String` / `Char` / `Bool` / `Symbol` / `Float` | Atoms |
| `Expr::Assign` (via `BinOp::Assign`) | Mutation primitive |
| `Item::DefineFunction` / `DefineMethod` / `DefineGeneric` / `DefineClass` / `DefineConstant` / `DefineVariable` / `DefineMacro` | Definitional forms |
| `Statement::Block` (with `cleanup`/`exception` arms) | Coupled to `nod_run_block` runtime + signal/handler mechanism — NLX coordination crosses the macro boundary |

**This list is frozen.** New entries require explicit conversation,
not silent addition. Everything else is a macro.

### 3. Pre-flight: write the `define macro` first

Before adding an `Expr::*` or `Statement::*` variant, write the
`define macro` version in `stdlib.dylan`. If the macro engine can
express the rule (with current or modest extensions), ship it.
Fall back to hardcoded AST only when:

- The form needs to introduce a *new control-flow primitive* the
  lowering can't synthesise from `if`/`begin`/`let`/`block` (vanishingly
  rare in practice), OR
- The form's runtime requires Rust-side cooperation that can't be
  expressed as a stdlib function call (e.g., the signal/NLX boundary
  that `block / cleanup` straddles).

Anything else is a macro. "It's easier to add to the parser" is not a
legitimate reason.

### 4. When the macro engine can't express it, extend the engine

If a macro you want to write can't be expressed in `nod-macro`'s
current pattern language (e.g., it needs auxiliary `rule` clauses,
or a new pattern-variable kind, or cross-file definition lookup),
that's a *macro-engine sprint*, not "add it to the AST."

Macro-engine extensions are themselves bounded and tractable. The
known deferred extensions (per `nod-macro/src/lib.rs`):

- Auxiliary `rule` clauses for multi-clause forms (e.g., `for`'s
  many clause shapes)
- `with-*` macros — need cleanup-aware expansion
- Cross-file macro use (currently the parser's known-macros set is
  per-file)
- Definition macros (`define table`, `define inline function` —
  parsed but not expanded)

Each unlocks a family of stdlib macros. Pay the engine cost once,
get many surface forms for free.

### 5. Watch the trend

Track these counts informally at sprint boundaries:

```
# `define macro` count in src/nod-dylan/dylan-sources/*.dylan
grep -c "^define macro" src/nod-dylan/dylan-sources/*.dylan

# Hardcoded control-flow Expr/Statement variants
grep -cE "    (For|While|Until|Case|Block|Cond|Select|With)" src/nod-reader/src/ast.rs
```

The left-column number should grow. The right-column number should
stay flat (or shrink via migrations).

## Frozen kernel forms (the right-hand column above)

Restated for emphasis — the forms that **stay hardcoded**, with the
reason each can't be a macro:

- **`Expr::If`** — every other branching form lowers TO this. Can't
  itself lower to anything else.
- **`Expr::Begin`** — sequence-of-expressions is a parser concept;
  needed before any macro can be expanded.
- **`Expr::Let`** — introduces names into lexical scope; the macro
  expander needs lexical scope to do hygiene.
- **`Expr::Method` / `LocalMethod`** — function literal; the
  closure-conversion pass operates on this shape directly.
- **Item-level definitional forms** — they create top-level
  bindings the loader registers; can't be macros that expand to
  something else, because they ARE the something else.
- **`Statement::Block`** — couples to `nod_run_block` /
  `CleanupGuard` / NLX in the runtime. The `cleanup` and `exception`
  arms can't be desugared to anything cheaper without breaking the
  signal-handler chain.

## Things that look kernel-y but should be macros

- **`case` / `cond` / `select`** — branching sugar. All lower to
  nested `if`. `Expr::Case` is currently hardcoded; it's a retirement
  candidate (Sprint 25-style work — same treatment as `Expr::Unless`).
- **`for` / `while` / `until`** — iteration sugar. All lower to
  recursive function calls or `loop`/`break` primitives. Currently
  hardcoded as `Statement::*` variants; retirement candidates.
- **`with-*`** — resource-management sugar. Expand to `block(...)
  cleanup ... end`. Needs the engine extension for cleanup-aware
  expansion. Frequent in real Dylan code.
- **`repeat` / `loop`** — control-flow sugar. Macros.
- **`when`** — one-armed conditional. Already a macro shape; not yet
  in `stdlib.dylan` but should be (alongside `unless`).
- **`iterate`** — named-recursion sugar. Macro.

## Worked examples

**Example A — "I want a `cond` form."**
- Rule 4 check: macro engine can express multi-rule pattern (`cond
  test1 => body1; test2 => body2; otherwise => body-else end`)?
  Currently no — needs auxiliary `rule` extension OR a different
  pattern formulation (e.g., recursive expansion `cond a => b; rest`
  → `if a b else cond rest end`).
- Rule 1: macro. Choose the recursive-expansion form, write `define
  macro cond` in `stdlib.dylan`. No AST change.

**Example B — "I want `case`."**
- Currently `Expr::Case` exists. Rule 2 check: is `case` a kernel
  form? No — it lowers to nested `if (x == k1) ... else if ...`.
- Migration mode: same treatment as Sprint 25's `Expr::Unless`
  retirement. Add `define macro case` in stdlib, retire `Expr::Case`,
  delete sema lowering. ~Sprint 47-ish work.

**Example C — "I want `with-open-file`."**
- Rule 4 check: macro engine can express `with-open-file (stream =
  path) body end` → `block (stream) body cleanup close(stream) end`?
  Yes if we lift the "macros that expand to block-with-cleanup"
  restriction. That's an engine extension, scope: small.
- Rule 1: macro. Write `define macro with-open-file` in stdlib once
  engine supports it.

**Example D — "I want `Statement::Switch`."**
- This would be C-style switch. Rule 1: doesn't qualify as kernel;
  it's just `case` with eager fall-through. Rule 4: writable as a
  macro over nested `if`. → New hardcoded AST variant rejected; if
  the user wants this, it goes in stdlib as `define macro switch`.

**Example E — "I want `iterate (loop (x = 0)) ... end`."**
- Named-recursion sugar. Macros over `let foo = method (x) ... end;
  foo(0)`. Pure macro, no AST change.

## Positive porting plan (preempt drift)

The user's question: "can we preempt drift by positively porting
macros now?" — yes, and Open Dylan's macro-expander + stdlib give us
a ready inventory. Two waves:

### Wave 1 — engine extensions (no language surface yet)

These unlock families of subsequent macros. Sprint-sized each.

1. **Auxiliary `rule` clauses inside `define macro`** — enables
   multi-clause forms like `for`. (See `nod-macro/src/lib.rs` header
   comment "Still deferred.")
2. ~~**`when` macro**~~ **DONE** — `when` ships as a `define macro` in
   `stdlib.dylan` (alongside `unless` / `cond`), expanding through the
   engine like any other stdlib control-flow form.
3. **Cleanup-aware macros** — `with-*` family. Needs the macro
   engine to know about the `block / cleanup` shape.
4. **Cross-file macro use** — currently the parser's known-macros
   set is per-file. Lift to a process-global registry shared with
   the merged module namespace.

### Wave 2 — surface form ports (consume Wave 1's extensions)

Each is a focused sprint, mostly Dylan code with attribution to
Open Dylan:

1. **`case` retirement** — replace hardcoded `Expr::Case` with
   `define macro case` in stdlib. Same shape as Sprint 25's
   `Expr::Unless` retirement.
2. **`cond` macro** — lifted from Open Dylan `collection-macros.dylan`
   or written fresh as recursive-expansion.
3. **`select` macro** — Open Dylan has this; lift.
4. **`when` macro** — pair to `unless`.
5. **`while` / `until` macros** — replace `Statement::While` /
   `Statement::Until`. Tail-recursive expansion via `iterate` once
   that exists, or via `block + loop`.
6. **`for` macro** — biggest port. Requires auxiliary-rule
   extension. Replaces `Statement::For` entirely.
7. **`with-open-file` / `with-lock` / `with-cleanup`** — the `with-*`
   family. Each is ~10 lines of Dylan once cleanup-aware expansion
   exists.
8. **`iterate`** — named recursion. Macro over `let f = method (x)
   ... end; f(initial)`.

After Wave 2, the AST is fully kernel-shaped: only `If`, `Begin`,
`Let`, `Method`, definitional items, and `Block` (with its
runtime-coupled `cleanup`) remain. Every other control-flow form
lives in Dylan.

### Wave 3 (later) — Dylan ecosystem ports

Once Wave 2 is done, every Open Dylan library with macros becomes
liftable. The threshold for "did this lift cleanly?" is the macro
engine's expressiveness; raise that and a long list of stdlib
extensions become free.

## How this links to other policies

- **`docs/STDLIB_BOUNDARY.md`** — macros add to the Dylan side, not
  the Rust side; this is one mechanism by which `stdlib.dylan` grows.
- **`docs/UPSTREAM_OPENDYLAN.md`** — macros lifted from Open Dylan
  preserve attribution and follow the same workflow as stdlib lifts.
  Open Dylan's `collection-macros.dylan`, `thread-macros.dylan`,
  `condition-extras.dylan`, and `dfmc/macro-expander/` are direct
  reference material.

## Enforcement

Light-touch, mirrors STDLIB_BOUNDARY:

1. **Sprint planning** — when scoping a new surface form, default
   to macro. The pre-flight (Rule 3) is the gate.
2. **Code review** — PRs touching `src/nod-reader/src/ast.rs` to add
   `Expr::*` or `Statement::*` get an explicit Rule-2 check. Frozen
   kernel list?
3. **Sprint retros** — Rule 5 trend check on `define macro` count vs
   hardcoded variants.

## File pointers (where this policy is referenced)

- `src/nod-dylan/dylan-sources/stdlib.dylan` — header banner
- `src/nod-macro/src/lib.rs` — crate-root comment
- `src/nod-reader/src/ast.rs` — comment at the `Expr` and `Statement`
  enum definitions (the temptation surface)
- `src/nod-sema/src/lower.rs` — comment at the top (control-flow
  lowering is the second temptation surface — "I'll add another arm
  to this match")
