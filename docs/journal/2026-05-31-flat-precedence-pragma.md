# 2026-05-31 — DRM flat precedence by default, `Precedence: c` as the migration bridge

*Sprint 51e. The "two compilers" gate found a real precedence bug in
the Rust parser; fixing it correctly turned out to be a corpus-wide
change. Continuation of
[the translator arc](2026-05-31-dylan-to-ast-translator.md).*

## Goal

The `--parse-with-dylan` byte-identical gate surfaced that the Rust
front-end parser climbs **C-style operator precedence**, while the
Dylan-in-Dylan parser is **flat left-associative** — the DRM rule (all
binary operators share one precedence; `3 + 4 * 5` is `(3+4)*5 = 35`).
The Dylan parser is right; the Rust reference parser was silently
mis-parsing every mixed-operator expression. Make the front-end DRM-correct.

## What we did

1. **Rust parser → flat (`parse_binary`).** Collapsed the six
   precedence-climbing functions (`parse_or`…`parse_pow`) into one flat,
   left-associative loop over all binary operators. `:=` stays
   right-assoc/lowest; unary stays tighter.

2. **Discovered the blast radius is the whole hand-written corpus.** The
   entire Dylan codebase was authored *assuming C precedence* — not just
   arithmetic, but pervasive boolean/comparison chains in predicates:
   `b = 95 | b = 33 | …` (is-name-char?), `c >= 48 & c <= 57`
   (is-digit?). Under flat-left these silently become nonsense
   (`((b = 95) | b) = 33`). The self-hosted lexer's character
   classification broke wholesale → garbage tokens → runtime crash.
   This is *latent debt*: that code was never valid DRM Dylan; it only
   "worked" because the bootstrap parser shared the same wrong
   assumption.

3. **The decision (with the user): `Precedence:` header pragma, default
   flat.** Rather than a big-bang rewrite (risky, long tail of "missed
   one → silent wrong grouping"), a one-line `Precedence: c` module
   header grandfathers a legacy file into C-style climbing. The parser
   carries both modes; `parse_assign` branches on the header. Default is
   **flat (correct Dylan)** — so real third-party Dylan code, which
   never heard of the pragma, parses correctly with zero annotation; the
   legacy corpus opts out explicitly. A migration bridge: parenthesize
   files and drop the pragma over time; retire the pragma when the
   corpus is clean.

4. **Grandfathered the legacy corpus** with `Precedence: c`: the
   self-hosting trio (`dylan-lexer`/`dylan-parser`/`dylan-lex-shim`), the
   IDE + collections fixtures, **and the stdlib** (`stdlib.dylan` +
   `win32-constants.dylan`). Parenthesized `point.dylan` and the two
   `richards-shape` fixtures instead (they're small and now flat-correct,
   so the translator gate can keep agreeing on them). Updated the value
   tests whose bare evals are now flat (`1 + 2 * 3 = 9`, not 7) and
   parenthesized the inline-source closure/arity tests.

## Discovered

- **The stdlib was the hidden keystone.** The value tests passed even
  with a flat-compiled stdlib because simple `eval` doesn't call the
  char-class predicates — but the self-hosting *shim* does, so it
  crashed. The shim merges the stdlib `LoweredModule` into every
  compiled program, so a flat-miscompiled stdlib poisons everything that
  exercises it. The grep for "which fixtures break" missed it because the
  stdlib lives in `src/nod-dylan/`, not `tests/fixtures/`. Lesson: when a
  global default changes, the audit must follow *every* compiled source,
  including the ones that are `include_str!`'d invisibly.
- **"Fix the reference, not the corpus" cuts both ways.** Both
  precedence models break the code written for the other. The real
  question was never "which is right" (DRM says flat) but "which body of
  code must be correct" — and the answer (real Dylan eventually) points
  at flat-by-default, with the pragma absorbing the migration cost.
- **Per-file precedence is a footgun as a permanent feature** (a reader
  must check the header to know how `a + b * c` groups) — which is
  exactly why it's scoped as a *transitional* bridge, not a forever knob.

## Where it leaves us

The front-end is **DRM-correct by default**, the legacy corpus is
unbroken via `Precedence: c`, and the whole suite is green (value
tests, macro/AOT/oracle suites, the translate gate, coverage). The Rust
parser carries both modes behind the header pragma.

**Next:**
- **Remove the translator's nested-binop fallback guard** — with both
  parsers flat for non-pragma files, nested binops now *agree*, so the
  guard can come off and the previously-blocked files translate. (The
  original payoff of this whole arc.)
- **Migrate `Precedence: c` files to parens** one at a time, dropping
  the pragma, until the corpus is all flat and the pragma retires.
- **Teach the Dylan-in-Dylan parser the pragma** (it currently ignores
  the header) so the translate gate can agree per-file on `Precedence: c`
  sources rather than relying on fall-back.
