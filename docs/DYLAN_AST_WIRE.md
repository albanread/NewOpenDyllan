# Dylan AST Wire Format — Sprint 51d

Tree-shaped sibling of `DYLAN_TOKEN_WIRE.md`. Same philosophy — a
stretchy-vector of fixed-size integer records the Dylan parser fills
and the host walks. Different layout because an AST is a tree, not a
flat stream.

The contract here is the load-bearing piece for actually using the
Dylan-side parser's output (`--parse-with-dylan`). The lexer wire
format proved the pattern works for flat streams; this proves it for
trees.

Wire-format version: **1.0** (v1 covers the subset hello.dylan +
factorial.dylan use — `Body`, `DefineFunction`, `Call`,
`VariableRef`, `StringLit`, `IntegerLit`, `BinaryOp`. Sprint 51e
extends).

---

## 1. Record layout

One node = one fixed-size 4-`<integer>` record inside a single
`<stretchy-vector>`:

```
offset 0   kind             — i64, kind code from §3
offset 1   span_lo          — i64, source byte offset (start)
offset 2   span_hi          — i64, source byte offset (end, exclusive)
offset 3   subtree_size     — i64, total record count of THIS node's
                              subtree (self + every descendant in
                              pre-order). For a leaf, this is 1.
```

Records are packed pre-order: parent first, then its children
recursively. Sibling boundaries are computed via `subtree_size`:

```
let parent_at = i;
let first_child = i + 4;                 // 4 ints per record
let second_child = first_child + 4 * records[first_child + 3];
let third_child  = second_child + 4 * records[second_child + 3];
...
```

The host walks the tree by recursive descent: read a record, dispatch
on `kind` to a per-kind builder, recurse on children inside the
builder. No explicit child-count is needed because each kind knows
how many children it has at the wire-format level (§3 documents the
shape; the builder asserts).

> **Why subtree_size, not first_child_offset / next_sibling_offset?**
> Single field, always-correct for skipping a subtree, no
> indirection. `subtree_size == 1` is the leaf check. The host walker
> needs no allocator state — it carries one cursor index that
> advances as records are consumed.

---

## 2. Calling convention

```c
// One C function exported by dylan-lex-shim.dylan.
uint64_t dylan_parse_emit(uint64_t source_bs);
```

* `source_bs` — a Dylan `<byte-string>` Word, the source bytes to parse.
* Return value — a Dylan `<stretchy-vector>` Word holding `4N`
  fixnums in row-major (record × field) layout.

The vector is owned by the Dylan heap. The host walks it
synchronously and copies what it needs out before any subsequent
allocation could move the vector.

---

## 3. Kind table (Sprint 51d v1)

Kind ordinals are stable. New kinds go at the bottom, never inserted
in the middle. The Rust-side dispatch table in
`src/nod-driver/src/dylan_parse_wire.rs` MUST stay aligned with this
section; a Sprint-51e check asserts agreement on every corpus
fixture.

| Ord | Name             | Children (pre-order, in this slot order)            | Notes                                            |
|-----|------------------|-----------------------------------------------------|--------------------------------------------------|
|   0 | `Body`           | N constituents (any kind)                           | Top-level module body OR a function body block. |
|   1 | `DefineFunction` | `DefName`, `ParamList`, `ReturnSpec`?, `Body` — in that order | Sprint 51e: children are dispatched by KIND, not position. `DefName` carries the name token's span; `ReturnSpec` is present only when an `=>` appeared. The function span itself is the `function` keyword token. |
|   2 | `Call`           | 1 × callee (any expr kind), N × arg (any expr kind) | First child is callee; the rest are args.       |
|   3 | `VariableRef`    | (leaf)                                              | `name` is `&src[span_lo..span_hi]` verbatim.    |
|   4 | `StringLit`      | (leaf)                                              | Span covers the quoted form; host strips quotes + decodes escapes. |
|   5 | `IntegerLit`     | (leaf)                                              | Span covers the digit run.                       |
|   6 | `BinaryOp`       | 2 × operand (left, right)                           | Operator is the single token at span_lo of the BinaryOp record's gap between children — host parses from `&src`. |
|   7 | `Error`          | (leaf)                                              | The Dylan parser bailed on this constituent.    |
|   8 | `DefineClass`    | `DefName`, then N × super-expr, then N × `SlotSpec`  | Sprint 51e. Dedicated `<ast-class-definition>`. Span is the `class` keyword; the `DefName` child carries the class name token. Children dispatched by kind: `SlotSpec` → slot, anything else → a superclass expr. |
|   9 | `DefineMethod`   | `DefName`, `ParamList`, `ReturnSpec`?, `Body`       | Sprint 51e. Same signature-child shape as `DefineFunction`. `<ast-body-definition>` body-word `method`; span is the keyword token. |
|  10 | `DefineGeneric`  | (leaf)                                              | Sprint 51e. Dedicated `<ast-generic-definition>`; span is the `generic` keyword. Signature recovered from `&src`. |
|  11 | `Statement`      | 1 × Body (leading body), then N × StatementClause   | Sprint 51e. The whole `<ast-statement>` family — `if`/`until`/`while`/`begin`/`select`/`block`/`for`. Span is the leading keyword; host identifies the statement from `&src`. For `if`, the condition is the leading body's first child. The `for` iteration header is NOT yet emitted (deferred). |
|  12 | `StatementClause`| 1 × Body (clause body)                              | Sprint 51e. One trailing clause (`else`/`elseif`/`cleanup`/`exception`/`otherwise`). Span is the clause keyword; for `elseif`, the condition is the clause body's first child. |
|  13 | `LocalDecl`      | 1 × Body (binding pattern + `= init`)               | Sprint 51e. `let <pattern> = <init>`. Span is the `let` keyword. The body holds the binding (variable-ref, or paren-list for `let (a, b) = …`) then the init expression. |
|  14 | `SlotSpec`       | `DefName`, then optional `SlotAlloc`/`SlotInitKw`/`SlotRequired`/`SlotType`/`SlotInit` | Sprint 51e. One slot inside a `DefineClass`. Span stays the `slot` word; the `DefName` child carries the slot name. All metadata children are KIND-tagged and order-independent (rows 31–35). |
|  15 | `DotCall`        | 1 × receiver expr                                   | Sprint 51e. `receiver.name`. Span backfills from the receiver (the `.name` is a trailing token, not a node — host reads it from `&src`). |
|  16 | `Subscript`      | 1 × receiver, then N × index arg                    | Sprint 51e. `receiver[args]`. Span backfills over receiver + args. |
|  17 | `UnaryOp`        | 1 × operand                                         | Sprint 51e. Prefix `OP operand`. Span is the operator token. |
|  18 | `KwArg`          | 1 × value expr                                      | Sprint 51e. `key: value` keyword argument. Span is the keyword token. |
|  19 | `ParenList`      | N × item                                            | Sprint 51e. `(a, b)` / `(e :: <type>)` — a multi-item or typed parenthesised head (clause heads, etc.). Span backfills over the items. |
|  20 | `BoolLit`        | (leaf)                                              | Sprint 51e. `#t` / `#f`. Span covers the literal; host re-reads `&src[span]` to recover the boolean. The parser now retains the source token (`node-token`) so the span is real, not `0..0`. |
|  21 | `CharLit`        | (leaf)                                              | Sprint 51e. `'a'`. Span covers the quoted char form; host strips quotes + decodes escapes from `&src`. |
|  22 | `SymbolLit`      | (leaf)                                              | Sprint 51e. `#"foo"` or `foo:` (keyword-name). Span covers the literal; host recovers the symbol name from `&src`. |
|  23 | `FloatLit`       | (leaf)                                              | Sprint 51e. `3.14`. Span covers the digit/exponent run; host parses the float from `&src`. |
|  24 | `RatioLit`       | (leaf)                                              | Sprint 51e. `1/3`. Span covers the `num/den` form; host parses the ratio from `&src`. |
|  25 | `ParamList`      | N × `Param`, then optional `VarMarker`              | Sprint 51e. A function/method parameter list. Each required parameter is a `Param`; a trailing `VarMarker` signals `#rest`/`#key`/`#all-keys`/`#next` (which the v1 host translator doesn't model → it falls back to the Rust parser for the whole file). |
|  26 | `ReturnSpec`     | N × `ReturnValue`, then optional `VarMarker`        | Sprint 51e. The `=> (…)` clause. Emitted as a definition child ONLY when an `=>` was present (so a missing `ReturnSpec` ⟺ `return_: None`; an empty `ReturnSpec` ⟺ `Some(ReturnSig { values: [] })`). A trailing `VarMarker` signals `#rest` in the return spec. Span is the `=>` token. |
|  27 | `DefName`        | (leaf)                                              | Sprint 51e. The definition's name token; host reads `&src[span]` for the name string. |
|  28 | `Param`          | 0–1 child: the type expr                            | Sprint 51e. One required parameter. Span is the parameter NAME token (always the name). An optional single child is the `:: type` expression. `name = &src[span]`. |
|  29 | `VarMarker`      | (leaf, span 0..0)                                   | Sprint 51e. Sentinel inside a `ParamList`/`ReturnSpec` meaning "this list has variadic syntax (`#rest`/`#key`/`#all-keys`/`#next`) the v1 host doesn't reconstruct." The host treats any `VarMarker` as Unsupported and falls back to the Rust parser. |
|  30 | `ReturnValue`    | 0–1 child: the type expr                            | Sprint 51e. One return value. Span is the value's leading token. If a type child is present → `name = Some(&src[span])`, `type = child`. If NO child → `name = None`, `type = Ident(&src[span])` (a bare return type like `=> (<integer>)`, where the Dylan parser stores the type AS the token). |
|  31 | `SlotAlloc`      | (leaf)                                              | Sprint 51e. A slot's allocation adjective token (`class`/`each-subclass`/`virtual`/`constant`). ABSENT ⟺ `Instance`. Host reads `&src[span]` → `SlotAllocation`. |
|  32 | `SlotInitKw`     | (leaf)                                              | Sprint 51e. A slot's init-keyword NAME token (e.g. `x:`). Host reads `&src[span]` and strips the trailing `:` → `init_keyword`. |
|  33 | `SlotRequired`   | (leaf, span 0..0)                                   | Sprint 51e. Marker: the slot used `required-init-keyword:` (→ `required_init_keyword = true`). |
|  34 | `SlotType`       | 1 × type expr                                       | Sprint 51e. Wraps a slot's `:: type` expression. (The Rust `Slot` dump doesn't print the type, but the translator keeps it for a usable `SlotDef`.) |
|  35 | `SlotInit`       | 1 × init expr                                       | Sprint 51e. Wraps a slot's `= init` / init-value expression. |
|  36 | `HashLit`        | N × element expr                                    | Sprint 51e. A `#(…)` list / `#[…]` vector / `#{…}` set literal. Span is the OPEN token (`#(`/`#[`/`#{`); the host reads `&src[span_lo+1]` to pick the synthetic constructor name (`#list`/`#vector`/`#set`) and rebuilds `Expr::Call(Ident(name), elements)` — exactly nod-reader's `parse_hash_literal` lowering. The improper-list tail `#(a . b)` is NOT emitted (an `<ast-list-lit>` carrying a tail emits `Error`, so the host falls back), matching the Rust parser, which doesn't model it either. |
|  37 | `DefineBinding`  | 1 × Body (the list-fragment binding)                | Sprint 51e. `define constant`/`variable NAME [:: TYPE] = INIT` (an `<ast-list-definition>`). Span is the `constant`/`variable` keyword — the host reads `&src[span]` to pick `Item::DefineConstant` vs `DefineVariable`. The single Body child holds the binding (a `BinaryOp(binder = init)`, the binder a `VariableRef`/`Param`), decoded by the same path as `LocalDecl`. Definition modifiers arrive as leading `Modifier` children (kind 38), past which the host finds the Body by kind. |
|  38 | `Modifier`       | (leaf)                                              | Sprint 51e.4. One definition adjective (`sealed`/`open`/`abstract`/`concrete`/`primary`/`free`/`inline`/`not-inline`/`sideways`/`domain`). Span is the adjective word's token. Emitted as leading children of a definition node (`DefineFunction`/`DefineMethod`/`DefineGeneric`/`DefineClass`/`DefineBinding`); the host maps each `&src[span]` via `Modifier::from_word` and collects them in source order into the item's `modifiers`. Empty ⟺ no children, so unmodified definitions are byte-unchanged. |

v1 deliberately excluded `DefineMethod`, `DefineConstant`,
`DefineVariable`, `DefineClass`, `DefineGeneric`, the `Statement`
family, `Let`, and the rich `<ast-literal>` subhierarchy beyond
`StringLit` + `IntegerLit`. Sprint 51e added all of these (kinds
8–24), one kind per micro-PR. Still outstanding: `DefineConstant` /
`DefineVariable` as dedicated kinds (currently `DefineFunction`-shaped
or `Body` constituents), and signature machinery (param-lists,
return-specs) on the definition kinds — the host parses those from
`&src` for now.

Fall-back rule: when the Dylan parser produces a node whose kind
isn't in this table yet, the emitter writes an `Error` record
covering the offending span. The host's verify-mode (Sprint 51c)
continues to validate the **accept/reject** verdict; the replace
path falls back to the Rust parser for the entire file when any
`Error` record appears.

---

## 4. Span semantics

`span_lo` and `span_hi` are UTF-8 byte offsets into the source
buffer the host passed in via `source_bs`. They match the Rust-side
`Span { lo, hi }` after decoding. The host validates `lo ≤ hi ≤
source.len()` per record on read.

A whole-file `Body` has `span_lo == 0`, `span_hi == source.len()`.

---

## 5. Endianness, alignment, stability

* Every Dylan `<integer>` is an immediate fixnum on the wire (low bit
  = 0, value-shifted by 1). The host unboxes via
  `Word::as_fixnum().unwrap() as i64`.
* No explicit endianness — the host runs in-process, both sides
  agree on native word order.
* The format is **stable across compiler versions** for a given
  major.minor tag. v1 is `1.0`. New kinds bump minor; layout changes
  bump major.

---

## 6. Out-of-scope (deferred to 51e and beyond)

* **String content** for `VariableRef`, `StringLit`, etc. — the host
  re-extracts from `&src` via the span. We don't carry a parallel
  string pool yet. Sprint 51e revisits if profile shows the
  span-resolve loop is a hot spot.
* **Modifiers, params, return specs** on `DefineFunction` — v1
  treats them as part of the function's outer span; the host parses
  them with the Rust parser for now. Sprint 51e adds dedicated kinds.
* **Diagnostics** beyond a single `Error` marker — Sprint 51e adds a
  parallel error-detail stretchy-vector.
* **`ast::Module` construction** — **done (Sprint 51e).**
  `src/nod-driver/src/dylan_to_ast.rs` converts the wire tree into the
  canonical `ast::Module`, and `--parse-with-dylan` uses it to replace
  `parse_module` for the files it fully reconstructs (with fall-back to
  the Rust parser on any `Unsupported`/`Error`). The
  `dylan_parse_translate` harness gates the two parsers' AST dumps as
  byte-identical and reports the translated-vs-fell-back tally. v1
  translates `define function`/`method` whose bodies are expression
  statements over the cheap expr subset; classes, `BinaryOp`,
  statement bodies, and `let` are the next increments.

---

## 7. Parser+macro inputs (Sprint 52 addendum)

Sprint 52 ports the macro expander (`nod-macro`) to Dylan and runs it
**inside the Dylan front-end, before this wire is emitted** — locus
decision **(B)** in `specs/52-macro-expander-dylan.md`. This section is
the one committed contract change that decision requires. **No new wire
record, no new kind, no new calling convention is added** — the
expanded AST travels over the *existing* wire (§1–§3) exactly as the
already-expanded Rust-side AST does today. The host stays oblivious to
whether expansion ran: it reconstructs an `ast::Module` and lowers it,
unchanged.

### 7.1 Pipeline order under locus (B)

```
Rust today:   parse ──► expand ──► [wire] ──► reconstruct ──► lower
Sprint 52:    parse ──► expand ──► [wire] ──► reconstruct ──► lower
              └──────── Dylan side ────────┘  └──── host ────┘
```

The *logical* order is identical — expansion precedes lowering. Only
the wire boundary moves to *after* expansion, and parse+expand are now
both Dylan-side. The wire therefore carries kernel-shaped, macro-free
AST: the host never sees a macro call, just as it never does after the
Rust expander runs today. Consequently **no `MacroCall` kind is ever
emitted on the wire** — a residual macro call in the Dylan-side tree is
a Dylan-side expansion bug, caught by the verify gate (§7.4), not a wire
concern.

### 7.2 Inputs the Dylan parse-and-expand entry receives

The parse shim's entry point is extended from "source to parse" to the
following three inputs. (a) already existed for `dylan_parse_emit`; (b)
and (c) are the Sprint 52 additions.

| # | Input | Origin | Purpose |
|---|-------|--------|---------|
| (a) | **source to parse** — a `<byte-string>` Word | host (`dylan_parse_emit(source_bs)`, §2) | the module text |
| (b) | **stdlib macro source** — `<byte-string>` Word(s), the stdlib `.dylan` blob(s) | host passes the same source the Rust `nod-sema::stdlib` loader reads (`src/nod-dylan/dylan-sources/stdlib.dylan`) | the Dylan side lexes+parses it and collects its `define macro`s into the macro table — no def-wire needed, the defs *are* source the Dylan side reads directly |
| (c) | **stdlib macro-name seed** — already defined by 51e.3 (`nod_sema::stdlib_macro_names()`) | host | parse-time recognition of body-shaped macro call sites (`cond`/`case`/`unless`/`when`/`for-each`/…) so the parser does not error on tokens like `KwOtherwise` that appear only inside those forms |

The in-file `define macro`s (a module defining its own macros) are
collected by the Dylan side from the module source itself (input (a)) —
no extra input. Macro *definitions* never cross the wire as data; they
are always collected from source, Dylan-side.

### 7.3 Why no def-wire (the (A)-vs-(B) crux)

Architecture (A) would serialise macro *definitions* across a new
AST-input wire so a host-side shim could drive expansion. (B) avoids it
entirely: the stdlib's macros are *Dylan source*, and the Dylan
front-end already self-parses the corpus — so it lexes+parses the
stdlib source and collects the defs into its own table (input (b)). The
fewer byte-contracts, the fewer parity hazards; (B) adds **zero** new
contracts beyond documenting inputs (b)/(c) here.

### 7.4 Verify gate (unchanged machinery)

Verify-mode (`NOD_VERIFY_EXPAND=1`) runs both `Rust parse → Rust expand`
and `Dylan parse+expand → reconstruct`, and asserts the two
`ast::Module`s are **byte-identical** — the exact `dump-expanded`
comparison 51e already uses, now with the Dylan side having expanded
before emitting. Spans are included where load-bearing (the dump prints
them), so hygiene renames and span-rewrites are part of the gate.
Divergence fails loudly; the Rust result proceeds.

### 7.5 Fresh-checkout safety

Identical to the parser's rule (51e.6): the Dylan expander installs only
when the parse+macro shim is statically linked
(`cfg!(dylan_lex_shim_linked)`). A fresh checkout with no shim stays on
the Rust expander — no install, no per-file fall-back noise. A module
whose expansion the Dylan side cannot complete falls back to Rust
`parse → expand` for the whole module.
