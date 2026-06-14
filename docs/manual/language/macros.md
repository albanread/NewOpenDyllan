# Macros — extending the language

A `define macro` adds new syntax that is rewritten to ordinary Dylan
**before** the compiler sees it. This is how `unless`, `when`, `cond`,
and `with-cleanup` are defined in NewOpenDylan — not as hardcoded AST
variants, but as a handful of lines in
`src/nod-dylan/dylan-sources/stdlib.dylan`. Every form you write with
`define macro` is expanded by `nod-macro` between the parser and sema,
so downstream phases (type inference, codegen, GC) see only the kernel
forms they already know.

## Why macros

Dylan's design separates a small **kernel** of hardcoded forms — `if`,
`begin`, `let`, `method`, definitional items, `block` — from an open
**surface layer** expressed in Dylan itself. Everything that can be
defined in terms of the kernel *should* be: that keeps the compiler
small, makes the language extensible without touching a single Rust
file, and lets the stdlib grow as the macro engine matures.

The policy is documented in `docs/MACRO_BOUNDARY.md`. Its first rule:
"Any new control-flow keyword, iteration form, binding-shape, or sugar
goes in as a `define macro` in `stdlib.dylan`." New `Expr::*` or
`Statement::*` variants in the AST are the exception, not the default.

Sprint 25 retired the last hardcoded control-flow variant that could be
expressed as a macro (`Expr::Unless`). The five forms now defined in
stdlib are the model for everything that follows.

## Writing a macro

A `define macro` has one or more **rules**. Each rule is a **pattern**
(written inside `{ … }`) and a **template** (after `=>`). The pattern
names match-capture sites with `?`-prefixed **pattern variables**,
optionally constrained by a kind annotation after `:`. The template
substitutes the captured fragments back in.

Here is the complete `unless` macro from `stdlib.dylan:485-488`:

```dylan
define macro unless
  { unless ?cond:expression ?body:body end }
    => { if (~ ?cond) ?body else #f end }
end macro;
```

When the expander sees `unless (x > 0) do-something() end`, it:

1. Matches the call site against the pattern.
   `?cond:expression` captures `(x > 0)` as one fragment (a grouped
   paren counts as one expression-fragment).
   `?body:body` greedily captures `do-something()` — everything between
   the condition and the `end` keyword.
2. Instantiates the template, substituting the captured fragments:
   `if (~ (x > 0)) do-something() else #f end`.
3. Re-parses that text as ordinary Dylan. Sema sees an `if` expression;
   it never sees `unless` at all.

Here is the `when` macro from `stdlib.dylan:504-507`:

```dylan
define macro when
  { when ?cond:expression ?body:body end }
    => { if (?cond) ?body else #f end }
end macro;
```

Both expand to `if` with an `else #f` arm so the return type is
always well-defined even when the body is not taken.

## Diagram: from definition to expanded AST

```mermaid
flowchart TD
    DEF[define macro NAME] --> COL[collect_macros builds MacroTable]
    USER[User writes NAME call] --> PAR[Parser emits Expr MacroCall]
    PAR --> EXP[expand_one matches pattern]
    COL --> EXP
    EXP --> BIND[Bindings map built for ?vars]
    BIND --> TPL[Template instantiated]
    TPL --> REPARSE[Re-lexed and re-parsed]
    REPARSE --> AST[Ordinary AST node]
    AST --> SEMA[Sema and lowering]
```

The expansion engine is documented in detail in
[Macro expander](../compiler/macro-expander.md). This page covers only
the user-facing syntax.

## Diagram: the `unless` rule walked

```mermaid
flowchart LR
    CALL[unless cond body end] --> LIT1{{Literal: unless}}
    LIT1 --> VAR1[?cond:expression]
    VAR1 --> VAR2[?body:body]
    VAR2 --> LIT2{{Literal: end}}
    LIT1 -.->|matches| UNL[ident token unless]
    VAR1 -.->|captures one fragment| CE[(cond fragment)]
    VAR2 -.->|greedy to end| BE[(body fragments)]
    LIT2 -.->|matches| ENDT[kw-end token]
    CE --> TPL[if - ~ cond - body else #f end]
    BE --> TPL
```

The `?cond:expression` variable binds a single fragment (a paren group
such as `(x > 0)` counts as one fragment because the grouper wraps it).
The `?body:body` variable is greedy: it consumes all remaining fragments
up to the next pattern literal — here, the `end` keyword.

## Pattern variables and constraints

Pattern variables are written `?name:kind` (or `?name` with a default
kind of `expression`). The supported constraint kinds come from the
`PatternKind` enum in `src/nod-macro/src/lib.rs:113-143`:

| Constraint | What it matches | Bound as |
|---|---|---|
| `:expression` | One fragment: a single token, or a grouped paren/bracket/brace | Fragment sequence |
| `:name` | Exactly one identifier token | Single token |
| `:body` | Greedy: all remaining fragments up to the next pattern literal; depth-aware for `end`-terminated forms | Fragment sequence |
| `:variable` | A let-binder shape: bare `Ident`, or `Ident :: <type>` triple | Fragment sequence |
| `:macro-arg` | Like `:expression`, stops at comma (comma handling is Sprint 19+, currently aliases `:expression`) | Fragment sequence |
| `:parameter-list` | A paren-wrapped parameter list `(x, y :: <integer>)` | Fragment sequence |
| `:constraint` | Explicit constraint form `?x:{ <expr> }` (v1 aliases `:expression`; constraint check is Sprint 19+) | Fragment sequence |

Names outside this set that the engine recognises as forward-compat
taxonomy (`case-body`, `type`, `case-expression`, `definition`) are
aliased to `Expression` today (`lib.rs:440`, `parse_kind_word`).

In a multi-rule definition, rules are tried left to right; the first
match wins. There is no within-rule backtracking. Write the
most-specific rule first.

## Hygiene

Template-introduced identifiers that appear in **binding position**
(after `let`, in method parameter names) are renamed to
`name__nod_hyg_{nonce}`, where `nonce` is a per-expansion monotonic
counter minted by `ExpansionCtx::fresh_nonce` (`lib.rs:1279`). This
prevents a macro's internal temporaries from accidentally capturing
names the caller already uses.

Identifiers in **reference position** are emitted unchanged so they
resolve against the call site's scope, not the macro's definition scope.

Dylan keywords and core type names in `is_template_no_rename`
(`lib.rs:848`) are never renamed even in binding position. The
no-rename set includes `if`, `else`, `elseif`, `begin`, `let`,
`method`, `block`, `cleanup`, `values`, and the built-in type names
such as `<integer>`, `<boolean>`, and `<object>`.

The `for-each` macro in stdlib demonstrates hygiene in practice: the
internal variable `%fip-state` (stdlib.dylan:467) begins with `%`,
which is a Dylan convention for private names. Hygiene renaming adds an
extra layer of protection so even an identically-named caller variable
does not collide with the macro's iteration state.

## Stdlib macros

These are the macros NewOpenDylan ships today, all defined in
`src/nod-dylan/dylan-sources/stdlib.dylan:464-586`:

| Macro | Lines | Surface | Expands to |
|---|---|---|---|
| `for-each` | 464-474 | `for-each (?var:name in ?coll:expression) body end` | FIP loop over a collection using `%fip-init` / `%fip-finished?` / `%fip-current-element` / `%fip-advance!` |
| `unless` | 485-488 | `unless cond body end` | `if (~ cond) body else #f end` |
| `when` | 504-507 | `when cond body end` | `if (cond) body else #f end` |
| `cond` | 540-562 | `cond (t1) (b1) … otherwise (d) end` | Nested `if/elseif/else` chain (1–4 arms, see below) |
| `with-cleanup` | 583-586 | `with-cleanup body cleanup cleanup-body end` | `block () body cleanup cleanup-body end` |

The `for-each` macro is a call-shaped form: the variable and collection
appear before the body. The body-shaped forms (`unless`, `when`, `cond`,
`with-cleanup`) use the `NAME … end` surface and are recognised by the
parser via a seed set of known-macro names in `nod-reader`.

## Current limits

**No `*` repetition.** The pattern language has no repetition operator
yet. A pattern cannot say "one or more `(test) (body)` pairs." This
means every multi-arm form must be written as a fixed set of rules
covering each supported arity.

`cond` is the most visible consequence. Its definition
(`stdlib.dylan:540-562`) contains four explicit rules covering 1, 2, 3,
and 4 test/body pairs plus an `otherwise` default. A `cond` with five
or more arms requires nesting a second `cond` inside the `otherwise`
clause:

```dylan
cond
  (x < 0) ("negative")
  (x = 0) ("zero")
  (x = 1) ("one")
  (x = 2) ("two")
  otherwise
    (cond
       (x = 3) ("three")
       otherwise ("many")
     end)
end
```

The cap is purely the number of fixed rules written — adding more rules
extends it. The same limit would apply to any other multi-arm form
(`case`, `select`, `for`) until repetition lands.

`*` repetition is tracked as **Sprint 49e** in `README.md`:

> Sprint 49e — `*` repetition in the macro engine. Removes the
> fixed-arity cap on `cond` and unlocks clean N-arm `case`,
> `select`, `for`, `with-*`.

Until Sprint 49e lands, the `cond` body of each clause must also be a
single `:expression` fragment (i.e. a paren-wrapped group for any
multi-token expression). The `cond` comment in stdlib explains: "The
paren tax is the price of admission until the macro engine grows `*`
repetition." (`stdlib.dylan:533`)

**Same-file restriction (Sprint 18).** A macro and its call sites must
currently live in the same parsed module. Cross-file macro use is
queued for Sprint 19. The stdlib macros work because `stdlib.dylan` is
compiled together with the user file as a single merged module.

**No auxiliary `rule` clauses.** The `for` statement's many clause
shapes (from-to, from-below, in-collection, etc.) would each need a
separate sub-rule. That engine extension is queued in `MACRO_BOUNDARY.md`
Wave 1.

**Definition macros not expanded.** `define table`, `define test`, and
similar `define`-headed user macro calls are parsed but not expanded
(the catch-all `Item::DefineOther` path). Expression-position and
statement-position macro calls work fully.

## How it is implemented

The expansion engine lives in `src/nod-macro/` and is documented in
[Macro expander](../compiler/macro-expander.md). Key points for the
curious reader:

- The expander works on **fragments** (a token-group representation),
  not on parsed AST nodes. It re-lexes the call-site span to get a flat
  token/group sequence, then pattern-matches against that.
- Pattern matching is greedy and left-to-right with no within-rule
  backtracking (`lib.rs:542`).
- Expansion is bottom-up: inner macro calls are expanded before the
  outer call is attempted. Recursive macros are bounded by a depth limit
  of 64 (`DEFAULT_DEPTH_LIMIT`, `lib.rs:241`).
- After template substitution, the result text is re-lexed and
  re-parsed as an expression. Spans are rewritten to point back to the
  original call site so error messages and the IDE show the user's
  source, not the expansion.

See also [Reader: lexer and parser](../compiler/reader.md) for how the
parser seeds its known-macro set (enabling the body-shaped `NAME … end`
recognition) and [Semantic analysis](../compiler/sema.md) for what
happens to the expanded AST.

---
[Manual home](../index.md) · [Modules and libraries](modules-and-libraries.md)
