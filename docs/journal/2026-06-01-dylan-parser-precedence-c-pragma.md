# 2026-06-01 — The Dylan parser learns `Precedence: c`

*Sprint 51e.1. The Dylan-in-Dylan parser now honours the `Precedence: c`
module-header pragma — the legacy C-style operator ladder, gated on the
header, mirroring nod-reader `parser.rs`'s `precedence_c` path. Follows
[the Rust-side flat-precedence migration](2026-05-31-flat-precedence-pragma.md)
and [the translator payoff](2026-06-01-translator-payoff-paren-and-assign.md).*

## Goal

Clear the "9 Precedence: c file" fall-back bucket in
`dylan_parse_translate`. Those grandfathered corpus files were declined
wholesale by the translator because the Dylan parser was flat-only and
couldn't reproduce the Rust parser's C-style operator grouping. Teach the
Dylan parser the pragma: flat by default (DRM), C-ladder when the header
opts in.

## What we did

1. **Surfaced the pragma to the parser.** The `Precedence:` header lives
   in the source preamble (the `Key: value` block) that the lexer SKIPS,
   so the parser never sees it through the token stream. Added
   `precedence-c-header?(source)` in `dylan-lex-shim.dylan` — it re-scans
   the raw source's preamble byte range (`[0, preamble-end)`) for a header
   whose key is `Precedence` and value is `c`, both compared
   case-insensitively after trimming. This mirrors the Rust detection in
   `parser.rs:137-142` and `dylan_to_ast.rs:87-92`
   (`eq_ignore_ascii_case`). The shim's `dylan-parse-emit` now calls the
   new `parse-dylan-with-precedence(tokens, precedence-c-header?(source))`,
   threading the verdict onto a new `ts-precedence-c?` slot on
   `<token-stream>`. (The zero-arg `parse-dylan`, used by the standalone
   EXE and `dylan-parse-collect`, delegates with `#f` — unchanged.)

2. **Built the C-style ladder in `dylan-parser.dylan`,** a faithful
   level-for-level mirror of `parser.rs`'s `parse_or → parse_and →
   parse_cmp → parse_add → parse_mul → parse_pow`:
   - `parse-c-or`   — `|`                              (Or)
   - `parse-c-and`  — `&`                              (And)
   - `parse-c-cmp`  — `= == ~= ~== < > <= >=`          (comparison)
   - `parse-c-add`  — `+ -`                            (additive)
   - `parse-c-mul`  — `* /` + word ops `mod` `rem`     (multiplicative)
   - `parse-c-pow`  — `^`, RIGHT-associative           (exponentiation)

   `parse-expression` routes through `parse-c-or` when `ts-precedence-c?`,
   else the flat `parse-binary-expression`; `:=` stays above both,
   right-assoc, regardless of the pragma (matches `parse_assign`). All
   levels are left-assoc except `parse-c-pow` (recurses on the rhs). The
   shared leaf is `parse-binary-operand` (= Rust `parse_unary` plus the
   keyword-name→symbol handling), so only operator GROUPING differs
   between the two Dylan modes, never the leaf shape. Crucially `=>` and
   `..` are NOT operators in the C-ladder (the flat `is-binary-op?` treats
   them as infix; the Rust C-ladder does not), so a C-mode chain stops at
   them — faithful to `parse_or…parse_pow`.

3. **Removed the translator's wholesale `Precedence: c` reject**
   (`dylan_to_ast.rs`, the early-return that declined every C-precedence
   file). Its own comment said "Removed once the Dylan parser learns the
   pragma." The `BinaryOp` arm already reconstructs nesting faithfully
   (it recovers the operator from the source gap and the dump diffs tree
   shape), so a now-correctly-C-nested wire tree translates directly.

## Discovered

- **The "9" bucket was a first-reported-reason artifact.** Removing the
  reject did NOT lift the tally to 23 as the punch-list implied. The
  `Precedence: c` check was the *first* `Unsupported` in `to_ast_module`,
  so it masked that all 9 files ALSO hit other blockers. With the pragma
  honoured and the reject gone, the 9 rebucket to their real next reason:
  `if clause "elseif"` (ide_rope, ide_syntax, rope), `expression
  LocalDecl` (ide_helpers, nod-ide), `expression Subscript`
  (dylan-macro-smoke), `expression UnaryOp` (dylan-parser), `let:
  non-simple binder` (dylan-lexer), and a nested `Error` node
  (unified_ide). Every one of those is a **translator-coverage gap**
  (`dylan_to_ast.rs`) — the domain of 51e.2, not the pragma. So 51e.1 is
  functionally complete (pragma honoured, ladder proven, zero
  divergences, zero regressions), but the corpus tally only climbs once
  51e.2 lands the missing `translate_expr`/`translate_statement` arms.

## Validation

- Three throwaway probes (since removed) proved the ladder is correct AND
  active, with byte-identical Rust/Dylan dumps:
  - flat `x + y * z` → `(* (+ x y) z)` (default unchanged — flat
    left-assoc on both sides);
  - C-prec `x + y * z` → `(+ x (* y z))` (multiplication binds tighter);
  - C-prec `a + b mod c ^ d ^ a` → `(+ a (mod b (^ c (^ d a))))`
    (`^` highest + right-assoc, `mod` multiplicative, `+` lowest).
- `dylan_parse_translate`: passes, **0 divergences, 0 regressions**; the
  "9 Precedence: c file" bucket is eliminated (14/36 holds — see
  Discovered for why it isn't 23 yet).
- `dylan_parse_coverage`: **99% structured, 43 Error (42 unspanned)** —
  identical Error punch-list to baseline; node total rose 39858→40401
  (the C-ladder emits the extra nesting) with no new Error kinds.

## Where it leaves us

The pragma gap is closed: the Dylan parser is no longer flat-only. The
9 grandfathered files now produce C-nested trees that match nod-reader
byte-for-byte wherever the translator can read them. The remaining
fall-backs for those files are pure translator-coverage gaps for 51e.2
(`elseif` clauses, `LocalDecl`/`Subscript`/`UnaryOp` at expression
position, the nested `Error` in unified_ide). Files changed:
`tests/nod-tests/fixtures/dylan-parser.dylan`,
`tests/nod-tests/fixtures/dylan-lex-shim.dylan`,
`src/nod-driver/src/dylan_to_ast.rs`.
