# OpenDylan test corpus — coverage and applicability

A snapshot of the upstream OpenDylan test tree lives under
`opendylan-tests/sources/` (see `opendylan-tests/INVENTORY.md`). This page
records, empirically, how much of the **language and standard-library** portion
NewOpenDylan can handle today, what blocks the rest, and the ranked work that
would unlock the most. DUIM, CORBA, OLE, Win32, network, database, deuce, and
environment suites are out of scope here.

## How this was measured

For every `.dylan` file in the language/stdlib suites
(`dylan/tests`, `dylan/apple-dylan-test-suite`, `common-dylan/tests`,
`collections/tests`, `io/tests`, `system/tests`, `testing/cmu-test-suite`,
`testing/benchmarks` — 161 files) we ran the driver's `dump-tokens` (lex),
`dump-ast` (parse), and, on candidates, `dump-dfm`/`build` (the full pipeline).

## Headline result

| Stage | Result | Notes |
|------|--------|-------|
| **Lex** | **161 / 161** | The lexer handles all real OpenDylan source. |
| **Parse** (`dump-ast`, lenient) | **118 / 161 (73%)** | The standalone parser accepts most real Dylan syntax (was 101 before the body-skip fix below). |
| **Full compile** (`dump-dfm`/`build`) | **0 real tests** | Blocked before codegen for every real test/benchmark. |

So: we can **read** the corpus, we can **parse** most of it, but we cannot yet
**compile** any real test. The gap is not one bug — it is two structural
blockers plus a cluster of concrete parser gaps.

## Why nothing compiles yet (applicability)

Every suite depends on libraries NewOpenDylan does not implement. No suite uses
the bare `dylan` library alone.

| Suite | Files | Key deps | Classification |
|---|---|---|---|
| `dylan/tests` | 15 | `dylan-extensions`, `common-dylan`, `io`, **testworks** | needs testworks + libs |
| `dylan/apple-dylan-test-suite` | 31 | **testworks** (28/31) | needs testworks |
| `common-dylan/tests` | 35 | `common-dylan`, `system`, `simple-*`, `machine-words`, **testworks** | needs testworks + stdlib port |
| `collections/tests` | 17 | `collections` (bit-vector/set/plists), **testworks** | needs testworks + libs |
| `io/tests` | 10 | `io` (streams/print/format), `system`, **testworks** | needs libs + testworks |
| `system/tests` | 14 | `system` (date/file-system/locators), `io`, **testworks** | needs libs + testworks |
| `testing/cmu-test-suite` | 4 | `common-dylan`, `simple-format`, `dispatch-engine` (no testworks) | needs libs + language features |
| `testing/benchmarks` | 35 | `common-dylan`, `simple-format`; many add testworks/`transcendentals` | needs testworks harness + stdlib port |

**The two structural blockers:**

1. **The testworks harness is not in the tree.** `define test` (~84 files),
   `define suite` (~72), `check-equal`/`check-true`/`assert-*` (~83),
   `define benchmark` (~17), `define sideways` (~14). Until these definer macros
   exist, the bodies of nearly the entire corpus are unreachable.
2. **The OpenDylan library stack is not ported.** `common-dylan`, `io`,
   `collections`, `system`, and the `simple-*` / `machine-words` /
   `transcendentals` / `dispatch-engine` libraries the headers `use`.

These are large, deliberate undertakings — most of the corpus is **"not
applicable as a full compile" until they land**. The benchmarks come closest
(only `common-dylan` + `simple-format`), but each benchmark body is wrapped in a
`define benchmark` (testworks-family) macro.

## Parser gaps (actionable now)

Of the `dump-ast` failures (60 originally; 43 after the body-skip fix in #1
below), the causes cluster tightly. These are concrete,
self-contained parser fixes (all in `src/nod-reader/src/parser.rs` unless noted)
that are valuable independent of the library/testworks work — and several also
turn a hard *parse wall* into a clean fall-through. Ranked by files unlocked:

1. ✅ **Fixed — bare nested `end` in an unknown `define`-macro body.** Unknown
   define-words (testworks `define test`/`suite`/`benchmark`) route to
   `parse_define_other` → `skip_body_to_matching_end`, which only treated a
   nested `end` as nested when followed by a known keyword (`end if`). A bare
   `end;` (the normal close of `for`/`if`/`block`) was misread as the form
   terminator, closing it early → the real `end test;` dangled as `unexpected
   token KwEnd`. Replaced the keyword-peek heuristic with real block-depth
   tracking (block-opener keyword pushes, `end` pops; depth-0 `end` terminates).
   Corpus parse 101 → 118; `KwEnd` failures 43 → 22 (the rest are nested
   *body-macro* calls not yet in the block-opener set, e.g. testworks
   `with-test-unit`).
2. **Body-shaped macro calls not recognized (~9 files).** The expr/statement
   dispatch hardcodes `if/begin/let/.../while/until/block`; body-shaped macros
   (`unless`, `when`, `with-open-file`, `dynamic-bind`, …) only parse if seeded.
   `dump-ast` seeds an empty set. Fix: seed the stdlib/core body-macro names
   (`unless`/`when` are core `dylan` macros the Dylan-side parser already treats
   as built-ins).
3. **`define`-forms with no `end` (5 files).** `define sealed domain make (…);`,
   `define table $t = {…};`, `define thread variable *tv* = …;`,
   `define benchmark x = expr;` — `parse_define_other` always `expect(KwEnd)`.
   Fix: accept `;`/`= expr;`-terminated define-forms.
4. **`for` iteration-clause forms (~6 files).** `var = init then next`,
   keyword clauses `until:` / `while:` / `finally:`, and `var keyed-by k in c`
   are unparsed (`parse_for_clause`). (Separately, `for` is also not yet *lowered*
   — 51 files use it.)
5. **`;` after a return signature (3+ files, common style).**
   `define method f (…) => (x :: <integer>);` — the body parser trips on the
   leading `;`. Fix: consume an optional `;` after `maybe_return_sig`.
6. **Keyword-symbol in value position (2 files).** `#(year:, 1800)` and
   `as(<string>, Foo:)` — a bare `name:` constant. Fix: accept `KeywordColon`
   as a symbol atom; disambiguate from keyword args by look-ahead.
7. **Escaped names in import specs (2 files).** `import: { \without-bounds-checks }`
   — `parse_import_set` rejects `EscapedIdent`. Fix: accept and strip the `\`.
8. **Adjacent string-literal concatenation (1+).** `"a\n" "b\n"` — Dylan folds
   adjacent strings. Fix: fold in the string atom.
9. **`.=hash` operator-named slot access (1).** `x.=hash` lexes as `=` + `hash`.
   Fix in the lexer (operator-shaped names) or dot-access.

Done so far: #1 (body-skip) recovered +17 (parse 101 → 118). The remaining
gaps #2–#9 cover most of the 43 residual failures.

## Cross-cutting robustness issue

The full pipeline (`dump-dfm`/`build`) routes through the stricter Dylan-side
parser, and a parse/expansion error there is **signalled as a `<condition>` with
no handler, which panics the process** (`src/nod-runtime/src/conditions.rs:904`)
with a crash dump instead of a clean diagnostic. This fires on essentially every
real corpus file. Installing a top-level condition handler in the `eval`/`dump`/
`build` entry points (render the condition, exit non-zero) would convert the
corpus from an all-or-nothing crash wall into an incremental, debuggable target.
Tracked in [known limitations](known-limitations.md).

## Recommended order of attack

1. **Top-level condition handler** (small) — stop panicking; emit diagnostics.
   Makes everything below measurable.
2. **Parser gap #1** (`skip_body_to_matching_end`) — the single biggest parse
   unlock (~30 files) and a prerequisite for reading any testworks body.
3. **Parser gaps #2–#9** — incremental, each backed by corpus files.
4. **`for` lowering** and **`#key`/`#rest` binding** — needed by ~51 and many
   files respectively once they parse.
5. **Stub/port testworks definer macros** (`test`/`suite`/`check-*`/`benchmark`)
   — unblocks the *bodies* of nearly the whole corpus.
6. **Port the library stack** (`common-dylan`, `io`, `collections`, `system`) —
   the long pole; turns "lowers, then missing-binding error" into real compiles.

The language core already compiles to EXE in isolation (`define
function`/`method`/`class` with typed slots + `init-keyword:`, recursion,
integer arithmetic, `let`/`if`/`while`, `when`/`unless`, `#(…)` lists, `make` +
slots, `<table>`/`<stretchy-vector>`, `format-out`) — so the corpus becomes a
usable incremental target as the steps above land.

---
*Reference: [Known limitations](known-limitations.md) · [Platforms](platforms.md) · [Architecture](../architecture.md) · the snapshot inventory at `opendylan-tests/INVENTORY.md`.*
