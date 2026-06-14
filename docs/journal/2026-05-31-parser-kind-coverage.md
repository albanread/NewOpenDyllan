# 2026-05-31 — Parser kind-coverage: the extend-and-test grind begins

*Sprint 51e. Commits `db0b082` (harness) … `b86d1de` (DefineClass), and
onward. Continuation of [the front-end self-hosting session](2026-05-31-front-end-self-hosting.md).*

## Goal

Start filling out the Dylan front-end for real, using the loop the
coverage harness enables: pick the highest-frequency `Error` construct,
write its `emit-node` method (Dylan) + `Kind` variant (Rust) + wire-doc
row, rerun the harness, watch coverage climb and the next target
surface. "Two compilers, measured."

## What we did

1. **Coverage harness (`db0b082`).** `dylan_parse_coverage.rs` sweeps
   the fixtures through `dump-dylan-ast`, classifies every `Error` node
   by the leading word of its source span, prints a ranked punch-list.
   Baseline: **77% of corpus AST nodes structured** (4622/5941), with
   the punch-list `<unspanned 0..0>` 923, `if` 192, `define-class` 104,
   `until` 86, tail.

2. **Span backfill for containers (`cf2c410`).** Container nodes
   (`<ast-body>`, `<ast-call>`, `<ast-binary-op>`) carry no leading
   `<token>`, so `span-of` returned `(0,0)`. Added
   `backfill-span-from-children`: after a container's children emit,
   recover its span as the union of descendant spans (bottom-up).
   `dump-dylan-ast hello.dylan` went from `Body 0..0` everywhere to
   `Body 8..547 … Body 527..547`.

3. **DefineClass / DefineMethod / DefineGeneric (`b86d1de`).** First
   real kind extension. `function`/`method` are `<ast-body-definition>`
   (body-word dispatch); `class`/`generic` are *dedicated* nodes. New
   emit-methods for all. Coverage **77% → 79%** (+696 structured
   nodes); `define-class` and `define-generic` left the punch-list.

## Why

The order — harness first, then span backfill, then a kind — was
deliberate: build the dashboard before the grind so it tells you what
to do and confirms each step. Span backfill went first among the fixes
because it's low-risk and improves every node's span (which the
eventual `ast::Module` build will need), and because measuring whether
it moved the Error count was itself a diagnostic (it didn't — see
below).

## Discovered

1. **Span backfill helps containers, not leaves — and that's a
   finding, not a disappointment.** The backfill left the coverage
   numbers *identical* (still 923 unspanned). That proved the 923
   unspanned `Error`s are **childless leaves**, not containers:
   nodes like `let`/`<ast-local-decl>` whose span lives in a
   type-specific slot (`ldecl-word`) the catch-all `emit-node` can't
   reach, and which the parser doesn't copy to `node-token`. So
   backfill-from-children structurally *cannot* declassify them.
   The reframe: **the unspanned bucket isn't a span bug to patch — it's
   the same missing-kind work as the spanned bucket, just invisible
   until each kind lands.** Declassifying ≡ structuring.

2. **`define class` / `define generic` are dedicated AST nodes, not
   body-definitions — and the harness caught me getting it wrong.**
   First cut mapped `class` as an `<ast-body-definition>` body-word.
   It compiled, ran, and did *nothing* — `define-class` stayed at 104
   in the punch-list. The harness surfaced the dead code immediately
   (same way `--verify-parse` caught the Rust parser's `cond` gap last
   session). Real dispatch: `parse-definition` routes `class` →
   `parse-class-definition` → `<ast-class-definition>`, a node with its
   own `class-supers`/`class-slots` slots. The dashboard pays for
   itself: a silent mistarget became a visible "number didn't move."

3. **Emit the children and the next punch-list item appears.** Emitting
   the class slot-specs as `DefineClass` children surfaced `slot` (188)
   — a target that had been *inside* the Error blob, invisible. The
   loop is self-revealing: each kind you structure exposes its
   substructure as the next ranked target. `<ast-slot-spec>` sets
   `node-token` (parser line 1259), so the slots come through spanned
   and cleanly classified.

4. **The lowerer rejects empty `begin` blocks.** First backfill cut had
   `if (cond) <comment-only> else … end`; the empty then-branch lowered
   to "empty `begin` block not lowered". Rewrote as a single positive
   condition (`if (hi > 0)` — a real span always has `hi > lo >= 0`).
   A Dylan-subset gotcha worth remembering: don't write comment-only
   branches.

## The run: 77% → 97%

The loop ran clean through the whole punch-list in one sustained
session. Each row is one commit (Dylan emit-method + Rust `Kind`
variant + wire-doc row + harness rerun):

| Commit | Kind(s) added | Coverage | Punch-list effect |
|--------|---------------|---------:|-------------------|
| `b86d1de` | DefineClass / DefineMethod / DefineGeneric | 79% | define-class, define-generic cleared; `slot` surfaced |
| `e74e0e8` | Statement family (`<ast-statement>`) | **88%** | if/until/while/cond/unless all cleared in one node; node count tripled (6697→21086) as bodies opened up |
| `fbd4fbf` | LocalDecl (`let`) | **95%** | the single biggest leaf contributor; unspanned 2100→1205 |
| `472caed` | SlotSpec | 96% | class story complete (DefineClass → supers + typed slots); `slot` cleared |
| `24fd0b3` | DotCall/Subscript/UnaryOp/KwArg/ParenList | **97%** | subscript `[` cleared; unspanned 1226→824 |

The **Statement** node was the standout: one `<ast-statement>` node
covers if/until/while/begin/select/block/for, so a single emit-method
cleared three named punch-list items *and* tripled the explored node
count by descending into every statement body. The "structure a
container, its children surface as the next target" dynamic compounded
all the way down.

## Discovered (continued)

5. **The literal long tail is blocked on a parser limitation, not an
   emitter one — and that's where the mechanical loop ends.** After
   the expression cluster, the remaining ~824 unspanned `Error`s are
   the literal subtypes (`<ast-boolean-lit>`, `<ast-symbol-lit>`,
   `<ast-char-lit>`, `<ast-float-lit>`, `<ast-ratio-lit>`) plus the
   signature machinery (param-list/return-spec/typed-name). The
   literals store their *decoded value* (`lit-value`, `lit-name`,
   `lit-codepoint`, `lit-raw`) but **no source token** —
   `parse-constant`/`parse-leaf` build them via `make(<ast-…-lit>,
   value: …)` without `node-token(n) := tok`. So `span-of` returns
   `(0,0)` and there's no way to recover a span from the node.

   This matters: structuring them now would emit `BooleanLit`/
   `SymbolLit` with span `0..0` — which raises the coverage % but is
   **hollow**, because the eventual `DylanAst → ast::Module` build
   couldn't recover *which* boolean/symbol/char it was. That would be
   gaming the metric. The honest fix is a small **parser** change:
   set `node-token` on each literal at parse time, consistent with how
   every other node works (wire format carries spans, host re-reads
   source — never values on the wire). It's a different character of
   work (touches the self-hosting target, deserves its own care) than
   the mechanical emitter loop, so it's a clean stopping point.

## Where it leaves us

**Coverage: 97%** (33641/34466 nodes). All the major structural kinds
are done: definitions (function/class/method/generic), the statement
family, `let`, slots, and the core expression forms. Remaining
punch-list: `824 unspanned` (literal subtypes + signature machinery)
and `1 hash`.

**The repeatable loop, proven across 5 commits:** pick top punch-list
item → Dylan emit-method + Rust `Kind` variant + wire-doc row →
rebuild shim + relink driver → rerun harness → number climbs, next
target surfaces → commit. ~3 file edits per kind. verify-parse held
`ok` throughout (emitter changes can't affect the accept/reject
verdict — that's the untouched `parse-dylan`).

**Two genuinely different next steps** (a fork worth a deliberate
choice, not more grind):
  1. **Parser change to retain literal source spans** — set
     `node-token` on the literal nodes in `parse-constant`/`parse-leaf`,
     then add the literal emit-kinds. Unblocks the last ~824 and
     completes coverage. Small but touches the parser.
  2. **The `DylanAst → ast::Module` translator** — the actual
     `--parse-with-dylan` payoff: turn the wire tree into the canonical
     Rust AST so the Dylan parser can *replace* `parse_module` for
     compatible files, with verify-style fallback. This is the
     bigger-value step and doesn't need 100% kind coverage first
     (it can fall back on any `Error`).

## Addendum — fork #1 taken: literal source spans + kinds 20–24

We took fork #1 first, because it's the keystone the translator
(fork #2) needs: a literal node with no span is a literal whose
*value* the host can't recover. Done in one pass:

1. **Parser: retain the literal token (span).** `parse-leaf`,
   `parse-constant`, `parse-binary-operand`, and the bare-keyword-arg
   site all built literal nodes via `make(<ast-…-lit>, value: …)`
   without `node-token(n) := tok`. The literal subtypes store the
   *decoded value* (`lit-value`/`lit-name`/`lit-codepoint`/`lit-raw`),
   never the token — so `span-of` returned `(0,0)`. Added
   `node-token(n) := tok` at **every** literal make-site (integer,
   float, ratio, char, boolean, symbol, keyword-name-as-symbol, the
   `#next`/`#rest`/… pseudo-symbols, and the two symbol sites outside
   `parse-leaf`).

2. **Emitter + decoder + doc: kinds 20–24.** `BoolLit`=20, `CharLit`=21,
   `SymbolLit`=22, `FloatLit`=23, `RatioLit`=24 — five leaf
   `emit-node` methods (Dylan), five `Kind` variants + `from_i64` +
   `name` (Rust, all three sites to dodge E0004), five
   `DYLAN_AST_WIRE.md` rows, and the `format_node` leaf-payload preview
   extended so `dump-dylan-ast` shows the source slice.

**Result: coverage 97% → 99%** (34515/34558 nodes), `unspanned`
**824 → 42**. All literal buckets left the punch-list. Remaining: 42
unspanned + 1 `punct:'#'` (signature machinery + a hash form), heavily
concentrated in the FFI fixture `ide_win_calls.dylan` (`define
c-function`) and `stdlib-min.dylan`.

**Discovered — the metric can lie; the corpus scan doesn't.** The
coverage harness counts `Error` nodes, so the moment a literal becomes
a `SymbolLit` (not an `Error`) it leaves the unspanned bucket *even if
its span is `0..0`*. The first relink showed 99% — but a targeted dump
of `richards-shape.dylan` surfaced `(SymbolLit 0..0 "")`: a hollow node
the metric happily counted as "structured." Two parser make-sites
(`parse-binary-operand`'s standalone keyword-name, and the bare-keyword
argument) had been missed. The honest check isn't the percentage — it's
a **whole-corpus scan for `Lit 0..0`**, which went to **0** only after
both were fixed. Lesson logged: when a metric improves, verify the
*thing the metric is a proxy for*, not the metric.

verify-parse spot-checked `ok` on hello/factorial/dylan-parser/point/
richards-shape — the change is span-only and cannot move the
accept/reject verdict.

**Now genuinely unblocked for fork #2.** Every literal across the
corpus carries a real span; the translator can recover `i128`/`f64`/
`String`/`char`/`bool`/symbol from `&src[span]`. Next: the
`DylanAst → ast::Module` translator + `--parse-with-dylan` with
fall-back-on-Error.
