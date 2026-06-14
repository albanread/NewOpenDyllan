# 2026-06-01 — The translator payoff: Paren-transparent dump, `:=` precedence, 9→14/36

*Sprint 51e, continued. Cashing in the precedence migration: removing the
translator's nested-binop guard. It took two more real fixes to land —
one cosmetic-but-principled, one a genuine bug in the Dylan-in-Dylan
parser that the "two compilers" gate flushed out. Follows
[the flat-precedence migration](2026-05-31-flat-precedence-pragma.md).*

## Goal

After the DRM-flat migration, both parsers agree on flat precedence for
non-pragma files, so the `dylan_to_ast` translator's conservative
"decline any nested binary operator" guard should come off and the
previously-blocked files (factorial-body-style `a * b + c * d`, the GC
repros with `i := i + 1` loops) should translate byte-identically. That
was billed as the payoff of the whole arc.

## What we did

1. **Removed the guard — and immediately hit a *different* divergence.**
   `point.dylan` (`(xx * xx) + (yy * yy)`) and three gap repros diverged.
   Not precedence: the **Rust parser keeps an `Expr::Paren` wrapper** for
   a parenthesised sub-expression, while the Dylan parser drops single
   grouping parens transparently (`parse-paren-fragment` returns the lone
   item). So the translated tree had no `Paren` nodes and the dump
   differed.

2. **Made the dump formatter `Paren`-transparent** (`fmt_expr`, nod-reader
   `ast.rs`). Peel `Expr::Paren` wrappers *before* indenting; the inner
   node prints in that slot with no wrapper line. Justification: under
   flat precedence the **tree shape already encodes grouping losslessly**
   — `a + (b + c)` parses right-nested, `a + b + c` left-nested; distinct
   trees with or without a Paren marker. So `Expr::Paren` is syntactic
   *provenance*, like a span (which the dump already omits), not
   structure. This is the oracle the translation gate diffs against, so
   making it blind to Paren lets the two parsers agree on semantics
   without bolting fragile src-based paren-recovery onto the translator.
   The AST node stays — lowering still consumes it, and it's transparent
   to lowering anyway, so the translated (Paren-free) module lowers
   identically. `point.dylan` translated. (11/36.)

3. **The gate then caught a real bug in the Dylan-in-Dylan parser: `:=`.**
   `i := i + 1` parsed as `(i := i) + 1` on the Dylan side but `i := (i + 1)`
   on the Rust side. The Dylan parser had lumped `assign` into its flat
   `is-binary-op?` set, treating `:=` as just another left-associative
   binop. But `:=` is assignment — lowest precedence, right-associative
   (DRM), exactly as nod-reader's `parse_assign` sits above `parse_binary`.
   Fixed by dropping `assign` from `is-binary-op?` and adding a
   `parse-expression` layer that parses a flat binary expression, then —
   if `:=` follows — recurses on the right (right-assoc) above a renamed
   `parse-binary-expression`. Rebuilt the shim `.lib.obj`, relinked the
   driver. The three gap repros translated. **(14/36, zero divergences.)**

4. **Fixed two stale precedence tests** the migration had missed.
   `parser.rs`'s `precedence_mul_over_add` and `mod_rem_are_mul_level`
   still asserted C-precedence (`1 + 2 * 3 == 1 + (2*3)`); under the
   flat default they'd been failing since the migration commit (the
   migration's green-check never ran `--test parser`). Rewrote them to
   assert the correct flat behaviour (`1 + 2 * 3 == (1 + 2) * 3`;
   `mod`/`rem` flat, not multiplicative) and renamed accordingly.

## Discovered

- **The gate keeps earning its keep.** Two of the three things this
  session were bugs the byte-identical diff surfaced that neither parser
  would have reported alone: the Paren-representation mismatch (a
  *design* difference between the two ASTs) and the `:=` mis-precedence
  (a flat-out wrong tree the Dylan parser had been emitting silently —
  the corpus self-parse test only checks it doesn't *crash*, not that
  the tree is right). The student-catches-teacher pattern again, plus
  student-catches-self.
- **Paren is provenance, not structure.** The clean way to reconcile two
  ASTs that disagree on cosmetic wrappers is to make the *comparison*
  ignore the cosmetic detail (as it already does for spans), not to
  force one side to fabricate it. Trying to recover grouping parens from
  `&src` is genuinely ambiguous — a grouping `(` and a call `(` are
  indistinguishable once the subtree extent excludes the call's own `)`.
- **A global default change leaves stragglers in the unlikeliest test
  files.** The migration audited fixtures and the stdlib but not the
  Rust parser's own precedence unit tests. Re-running *every* at-risk
  suite (not just the headline gate) is the only way to find them.

## Where it leaves us

The translator handles **14/36** fixtures byte-identically, up from 9,
with the original guard gone. The fall-back punch-list, ranked:

```
  9  Precedence: c file — Dylan parser is flat-only      (needs the
                                                          Dylan parser to
                                                          learn the pragma)
  5  Dylan parser emitted an Error node                  (parser coverage)
  2  call to body-macro "when"                           (macro seeding)
  2  class has modifiers (not on the wire yet)           (wire enrichment)
  1  expression Error / SymbolLit / cond / top-BinaryOp  (assorted)
```

**Next** (any order): teach the Dylan-in-Dylan parser the `Precedence`
pragma so the 9 grandfathered files can agree per-file instead of
falling back; chase the 5 `Error`-node fixtures (what does the Dylan
parser still bail on?); put definition modifiers (`sealed`/`open`/…) on
the wire so `class has modifiers` clears.
