# 2026-05-31 — DylanAst → ast::Module: the parser starts *replacing* parse_module

*Sprint 51e, fork #2. Continuation of
[the kind-coverage grind](2026-05-31-parser-kind-coverage.md). The
literal-span fix (fork #1) unblocked this.*

## Goal

Build the thing the whole AST wire format was for: a translator that
turns the Dylan parser's wire tree into the **canonical**
`nod_reader::ast::Module`, wired behind `--parse-with-dylan` so the
Dylan parser can *replace* `parse_module` for the files it fully
understands — with a verify-style fall-back to the Rust parser on
anything it can't yet reconstruct, and a **byte-identical** gate
proving "translated" means "the two parsers agree," not "didn't crash."

## What we did

1. **Wire enrichment — function signatures (kinds 25–30).** The wire
   carried only a definition's *body*; `ast::Item::DefineFunction`
   needs `{name, params, return_, body}`. Reshaped the
   `<ast-body-definition>` emitter to emit, as children dispatched by
   KIND (not position): `DefName`(27, the name token), `ParamList`(25,
   with `Param`(28) children carrying an optional type-expr child),
   `ReturnSpec`(26, emitted *only* when an `=>` is present, with
   `ReturnValue`(30) children), and the `Body`. `VarMarker`(29) is a
   sentinel child for `#rest`/`#key`/`#all-keys`/`#next`, which the v1
   host declines. Matching Rust `Kind` variants + `DYLAN_AST_WIRE.md`
   rows.

2. **`dylan_to_ast.rs` — the translator.** `to_ast_module(tree, src) ->
   Result<Module, Unsupported>`. Header re-scanned host-side with
   `scan_preamble` (the Dylan parser doesn't model it — see below);
   `DefineFunction`/`DefineMethod` rebuilt from the signature children;
   bodies → `Vec<Statement>`; expressions for the cheap subset
   (`Ident`, `String`, `Integer`, `Float`, `Bool`, `Call`). Anything
   else returns `Unsupported`.

3. **`--parse-with-dylan` flag.** In `run_dump_ast`: try the Dylan
   parse + translate; print and return on `Ok`; on *any*
   `Unsupported`/wire error, fall through to `parse_module_with_macros`.
   Deliberately does **not** imply `--lex-with-dylan` — the Rust
   fallback keeps the Rust lexer so a fallback's AST is identical to
   plain `dump-ast`, keeping the gate measuring the *translator*.

4. **Translation-coverage gate (`dylan_parse_translate.rs`).** Runs
   both `dump-ast` and `--parse-with-dylan dump-ast` over the corpus;
   asserts byte-identical stdout on every fixture; tallies
   translated-vs-fell-back and ranks the fall-back reasons as the
   next-increment punch-list. Asserts ≥ `hello.dylan` translates.

**Result: hello.dylan translates byte-identically via the Dylan
parser** — the first time the Dylan front-end's output, lifted to the
canonical AST, exactly equals the Rust parser. 1/34 translated; 33
fall back (cleanly).

## Why

The bar was deliberately **byte-identical `format_ast_module`**, not
"lowers OK." A weaker bar (does it compile? does it run?) would let
subtle structural disagreements slide; equality of the canonical dump
is the strongest cheap proof that the two parsers built the *same*
tree. The flag is the verify-mode philosophy taken one step further:
51c ran both parsers and compared accept/reject; this runs both and
compares the **whole AST**, then *uses* the Dylan one when they agree.

The fall-back-on-`Unsupported` design means the output is never wrong,
only "translated" or "fell back" — so the gate can ratchet: each
increment teaches the translator one more kind and watches the
translated count rise, exactly like the node-coverage harness ratchets
the structured-node count. Two dashboards now: *nodes structured*
(emitter side, 99%) and *files translated* (translator side, 1/34).

## Discovered

1. **`format_ast_module` prints no spans — the comparison is
   span-independent.** This collapsed a whole imagined difficulty.
   We feared having to make every span exactly match; in fact the dump
   is purely names / structure / values / operators / modifiers. The
   translator threads real spans through anyway (so the `Module` is
   usable downstream), but the *gate* doesn't care, which is why a
   coarse-span wire format is still enough to prove AST equality.

2. **`ast::Expr::String` stores the RAW quoted source slice, not the
   decoded value.** The Rust parser keeps `"\"hello\\n\""` verbatim —
   so translation is literally `&src[span]`, no escape decoding. The
   wire philosophy ("spans not values, host re-reads source") turned
   out to match the AST's own representation exactly.

3. **The Dylan parser lexes the module header as ordinary body
   forms.** `Module: hello` shows up in the wire as a `SymbolLit
   "Module:"` + `VariableRef "hello"` pair at the top of the Body —
   the Dylan parser has no header concept. The host owns the header
   (re-scan with `scan_preamble`) and skips body forms that lie inside
   the preamble. Clean division of labour: trivial header parsing stays
   Rust-side; the Dylan parser does the items.

4. **The bare-return-type asymmetry maps cleanly.** `=> (<integer>)`:
   the Dylan parser stores the type AS the value's token (tok =
   `<integer>`, type = #f), while Rust models it as `name: None, type:
   Ident("<integer>")`. The rule "ReturnValue with no type-child →
   name None, type Ident(span); with a child → name Some(span), type
   child" reconciles them without a special case.

5. **The byte-identical gate immediately earned its keep — and exposed
   a subtle translator bug.** First run: two divergences
   (`stdlib-min`, `ide_win_calls`). Both emitted a too-*empty* Module
   instead of falling back. Cause: their `define macro` /
   `define c-function` forms emit as **unspanned `Error 0..0`** nodes,
   and the header-skip heuristic (`span_hi <= body_start`) treated
   `span_hi == 0` as "inside the preamble" → silently dropped them →
   `Ok(empty)` instead of `Unsupported`. The lesson: *"skip the header"
   and "an unspanned node" look identical under a `<=` test.* Fix:
   never treat `span_hi == 0` as a header form, and force `Unsupported`
   on any `Error` node. This is precisely the failure the gate exists
   to catch — a silent wrong translation that a weaker "did it run?"
   check would have waved through.

## Where it leaves us

`--parse-with-dylan` is live and authoritative for the files it
understands; the gate guarantees it can never silently diverge from
the Rust parser. **Translated: 1/34** (`hello.dylan`). The fall-back
punch-list — the next-increment to-do list, ranked — is:

| Count | Reason | Next increment |
|------:|--------|----------------|
| 13 | top-level `DefineClass` | translate `Item::DefineClass` (supers + slots) |
| 6 | `Error` node | (genuinely unparsed — emitter work, not translator) |
| 6 | expression `BinaryOp` | `BinOp` w/ operator recovered from `&src` gap |
| 4 | expression `LocalDecl` | `let` → `Statement::Let` |
| 2 | expression `Statement` | `if`/`while`/`block` → `Expr`/`Statement` |
| 1 | top-level `BinaryOp` | — |

The obvious next move is **`BinaryOp` + `Statement(if)` + `LocalDecl`**,
which together flip `factorial.dylan` (and much of the corpus) to
translated — the operator-from-`&src` recovery is the one genuinely
new technique. Each is one translator function + (where needed) a wire
tweak, measured by the translated count climbing.

## Addendum — BinaryOp + `if`: 1/34 → 4/34, and the macro-seeding divergence

Took the next increment immediately. Pure translator work — no wire
change (all the needed kinds already emit).

1. **`BinaryOp` → `Expr::BinOp`.** The operator token isn't a node — it
   lives in the source *gap* between the operands. The wrinkle: a
   node's own span may not cover its children (a `Call`'s record span is
   just its `(` paren — `110..111` while its subtree is `101..116`), so
   bounding the gap by the operand *records'* spans reads the wrong
   bytes. Fix: a `subtree_extent` helper that takes the min-lo/max-hi
   over each operand's whole subtree, then `&src[lhs.hi .. rhs.lo]`
   trimmed → the operator string → `parse_binop`. `n * factorial(n-1)`
   resolves `*` correctly because `rhs`'s leftmost extent is the
   callee `factorial` (101), not the `Call` record (110).

2. **`Statement(if)` → `Expr::If`.** The wire `if` is a `Statement`
   whose first child is a leading `Body` of `[cond, then-forms…]` and
   whose trailing `StatementClause` children are `else`/`elseif`. v1
   builds `If { cond, then_: Begin(then-forms), else_: Begin(else-body) }`
   — both arms `Begin`-wrapped, matching the Rust parser exactly. `elseif`
   (nested-If desugaring) and non-`if` statement keywords fall back.

**Result: 4/34** — `hello`, `factorial`, `gap011-jcs-min-crash`,
`sprint09-add` all translate byte-identically.

**Discovered — the two parsers disagree on macro calls, and the gate
caught it.** The increment turned two `macro-when-*` fixtures from
"fell back" into *divergences*: the translator produced a *different*
AST than Rust. Root cause is genuinely deep — **the Rust `dump-ast`
seeds the parser with the stdlib body-macro names
(`when`/`unless`/`cond`/…), but the Dylan parser has no macro
knowledge.** So `when (x > 3) 42 end`: Rust folds it into one
`(MacroCall "when")`; the Dylan parser parses it as a plain *call*
`when(x>3)` plus a dangling `42`. Both *accept* the file (verify-parse
is happy), but the trees differ. With `BinaryOp`/`Call` now
translatable, the translator faithfully rebuilt the Dylan parser's
*wrong-for-this-purpose* tree — diverging from Rust.

The honest fix (not a metric game): the translator declines any call
to a known body-macro name (`is_body_macro`, kept in sync with the
seed list) and falls back. This is correct *because the Dylan parser
literally cannot represent the form the way Rust does* — the real fix
is teaching the Dylan parser macro seeding + a `MacroCall` wire kind,
which is its own increment. Until then, fall back. The lesson reprised:
the byte-identical gate is the thing that turned "I added BinaryOp and
4 files translate 🎉" into "…and 2 files now silently lie" — exactly
the regression a weaker check waves through.

**Now 4/34.** Remaining punch-list: `top-level DefineClass` (13, the
big one), `Error` nodes (6, emitter-side), `LocalDecl` (4, but the
`let`-bearing fixtures also hit classes so it won't flip whole files
until `DefineClass` lands), plus a `cond` statement, a stray
`binary operator "\""` (safe fall-back), and a top-level `BinaryOp`.
The highest-leverage next step is **`DefineClass`** — and, the deeper
structural one, **teaching the Dylan parser the macro set** so
macro-heavy files stop falling back.

## Addendum — DefineClass + LocalDecl + KwArg, and a real Rust precedence bug

Took on `DefineClass` (the 13-fixture lever) plus the `let`/keyword-arg
support its bodies need. Two translator-only steps then one wire step:

1. **`LocalDecl` → `Statement::Let`, `KwArg` → `%kw-arg`.** The Dylan
   parser models `let x = e` as a single `=`-`BinaryOp` inside the
   LocalDecl body (binder=lhs, init=rhs); v1 handles a single untyped
   binder. A `key: value` arg becomes the Rust parser's synthetic
   `%kw-arg(Symbol("key:"), value)` call.

2. **Wire-enrich `SlotSpec` (kinds 31–35) + class name `DefName`.** The
   `Slot` dump needs name/allocation/required/init-keyword — none of
   which the old `SlotSpec` carried. New KIND-tagged children:
   `SlotAlloc` (allocation adjective span), `SlotInitKw` (init-keyword
   token; host strips `:`), `SlotRequired` (marker), `SlotType`/
   `SlotInit` (wrapped exprs); the slot/class names ride a reused
   `DefName` child. `translate_class`/`translate_slot` reconstruct
   `Item::DefineClass`.

**Validation matters more than the count here.** With the precedence
fork below, every *existing* class fixture falls back before fully
translating — so the gate never actually checks my class output. That's
not honest "done." I added `translate-class.dylan`: a class (super,
`init-keyword:` slot, `required-init-keyword:` slot) + a single-call
accessor, deliberately free of the blockers, so it **fully translates
and the gate validates the DefineClass/slot path byte-identically**.
Lesson: a translator branch no fully-translating fixture exercises is
unverified, no matter how clean the code looks. **5/35.**

### Discovered — the Rust parser has the wrong Dylan operator precedence

`point.dylan`'s `xx * xx + yy * yy` *diverged* once classes translated:
  * Rust: `(+ (* xx xx) (* yy yy))` — C-style, `*` binds tighter.
  * Dylan-in-Dylan: `(* (+ (* xx xx) yy) yy)` — flat, left-associative.

The Dylan parser is **right**. Per the DRM, all Dylan binary operators
share one precedence and are left-associative (`3 + 4 * 5` is `35`, not
`23`). The Rust parser (`parse_assign → parse_or → parse_and →
parse_cmp → parse_add → parse_mul`) climbs C-style precedence — a real
bug: it mis-parses every mixed-operator expression, and the compiled
result is wrong (point computes the wrong distance-squared). The gate
surfaced a latent correctness bug in the *reference* parser — the
sharpest "two compilers, one truth" moment yet.

Reconciling it (rewrite the Rust expression parser to flat
left-assoc) has broad blast radius — it changes the AST for every
mixed-operator expression and the runtime semantics of existing
programs, and will churn many expected-AST tests. That's its own
careful, test-heavy task, NOT a DefineClass side-quest. For now the
translator **conservatively declines any nested binary operator** (the
exact place the two precedence models can disagree); single binops
(`n = 0`, `n * f(x)`) still translate, so factorial is unaffected.
Flagged for a dedicated fix.

**Now 5/35.** New fall-back punch-list leaders: `statement "until"`/
`"while"` (loops → `Statement::Until`/`While`), `Error` nodes (6,
emitter-side), `nested binary op` (the precedence fork — blocked on the
Rust fix), `class has modifiers` (modifiers not on the wire), and the
macro-seeding `when`/`cond` group. Next levers: **loops**
(`until`/`while`), **definition modifiers on the wire**, and the two
deeper structural fixes (**Rust operator precedence**, **Dylan-parser
macro seeding**).

## Addendum — loops + an operator-extraction bug: 5/35 → 9/36

Two translator-only steps, no wire change:

1. **`while`/`until` → `Statement::While`/`Until`.** Refactored the
   statement handling into `translate_statement` (statement position)
   and `translate_statement_as_expr` (expression position, wraps loops
   in `Expr::Stmt`), with a shared `translate_stmts` helper for body
   sequences. The wire `while`/`until` is a `Statement` whose leading
   `Body` is `[cond, body-forms…]` — same shape as `if`. New fixture
   `translate-loop.dylan` validates it (the existing loop fixtures all
   hit the precedence fork in their bodies).

2. **Operator-extraction bug — the `") +"` mystery, now fixed.** Seven
   fixtures fell back on `binary operator "\""`-ish reasons. Root cause:
   when the left operand is a **call or parenthesised expression**
   (`f(x) + y`), the closing `)` isn't covered by any child's span, so
   it landed in the operator gap → the gap was `") + "`, not `" + "`.
   The old code `gap.trim()` then failed to match a known operator. Fix:
   `operator_in_gap` strips ALL parens and whitespace from the gap
   (operators never contain either), recovering just `+`. Same latent
   bug fixed in `let`'s `=` check. This is a genuine translator bug the
   gate's fall-back tally surfaced — not a coverage gap.

**Result: 9/36** (+`gc_precise_two_makes` ×2, +`mutual`). The
`binary operator` bucket is gone. Dominant remaining blocker:
**`nested binary op` (11)** — entirely the Rust-precedence fork, which
the spawned parser-fix task unblocks in one shot. The rest:
`Error` nodes (6, emitter-side / genuinely unparsed), `when`/`cond`
macros (3), `class has modifiers` (2), and singletons (`elseif`,
`SymbolLit`, a typed/destructuring `let`, a top-level binop).
