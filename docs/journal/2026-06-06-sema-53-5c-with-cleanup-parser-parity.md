# 2026-06-06 — Sprint 53.5c: body-shaped statement-macro parsing (the last macro sema divergence)

*53.5(1) broadened the Dylan-side sema byte-match gate to 33 corpus
fixtures and scoped the two remaining divergences. This entry closes one
of them — `macro-when-cleanup` — by teaching the Dylan-side parser to
parse NAME-token body-shaped statement-macro calls, matching nod-reader.*

## Goal

`tests/nod-tests/fixtures/macro-when-cleanup.dylan` was the last
macro-using fixture where the Dylan sema walk diverged from the Rust
oracle. It has two `define function`s — `test-when` (uses the `when`
macro) and `test-with-cleanup` (uses `with-cleanup … cleanup … end`). The
Dylan walk emitted `fn test-when` but **dropped `fn test-with-cleanup`**.
Close the divergence at the root and gate the fixture.

## What

Two helpers added to `tests/nod-tests/fixtures/dylan-parser.dylan`,
mirroring nod-reader (`src/nod-reader/src/parser.rs`):

- `is-known-statement-macro? (name) => (yes?)` — #t for `"when"` and
  `"with-cleanup"`. Mirrors nod-reader's `known_macros` dispatch
  (parser.rs:808-812), restricted to the body-shaped stdlib macros the
  **Dylan lexer does not reserve as keyword-tokens**.
- `peek-name-opens-body-statement? (ts) => (yes?)` — the no-paren
  body-shape lookahead (parser.rs:944-976): from the token after the
  name, track bracket depth across all bracket/brace/hash-open kinds;
  return #t iff a depth-0 `end` is reached with body content seen and no
  unmatched closer first.

The leaf parser's name-token branch now PEEKS the name and, when both
predicates hold, routes to `parse-statement` (which already consumes
`WORD body (CLAUSE-SEP body)* end` — `cleanup` is a clause separator it
handles). Otherwise the existing bare variable-ref path is unchanged.

Gate: `macro-when-cleanup` added to `FIXTURES` in
`tests/nod-tests/tests/sema_topnames.rs` (now **34**); the doc comment
marks the divergence closed.

## Why it dropped the function — and the diagnosis that was wrong

The first diagnosis blamed `with-cleanup`: an unrecognised macro whose
`cleanup` clause desyncs the body parser. That is *a* real defect, but it
was **not** what dropped `test-with-cleanup`. Grounding in the lexer
overturned it.

The Dylan lexer (`tests/nod-tests/fixtures/dylan-lexer.dylan:~808-869`)
reserves `cond`, `unless`, `for`, `cleanup`, `block`, `select`, … as
keyword-tokens — but **not `when`**. So `when` lexes as a plain
`<identifier-token>`, and `is-begin-word?`'s `#"when"` arm is dead code
(it can never match a keyword-token, because `when` is never one).

So the actual failure was in the FIRST function:

```
define function test-when (x) => (result)
  when (x > 3)
    42
  end          ← when's end
end function;   ← test-when's end
```

`when` parsed as a bare call `when(x > 3)`; its `end` was then
mis-consumed as `test-when`'s function-`end`; and `test-when`'s *real*
`end` prematurely terminated the top-level body — abandoning everything
after it, including `test-with-cleanup`. `macro-when-only.dylan` stayed
green throughout only because `test-when` is its sole definition, so the
abandoned tail was just its own `end function` (no top-name lost).

The fix therefore had to cover **both** `when` and `with-cleanup` (both
are non-reserved body-macro names). The implementing agent caught this
and flagged it; it was confirmed independently here via the lexer
keyword list and `dump-dylan-tokens` (`when` → `IDENTIFIER`).

This is the third time this session a surface-level diagnosis was off and
only grounding/verification corrected it (GAP-011 #2's empty-table sort;
the `kernel-arith` "missing constant line" display artifact; now `when`
vs `with-cleanup`). The standing lesson holds: **verify against the
lexer / oracle, do not reason from the parser's apparent keyword list.**

## Design notes / scope

- This mirrors nod-reader's `known_macros` (seeded from stdlib
  `define macro`s: `for-each`/`unless`/`when`/`cond`/`with-cleanup`) but
  is restricted to the two names the Dylan lexer leaves as identifiers.
  `unless`/`cond`/`for` are keyword-tokens → already reach
  `parse-statement` via `is-begin-word?`. `for-each` is call-shaped (not
  body-shaped) and not in this corpus → left to the call path.
- Only the NO-PAREN lookahead path is implemented (the shape `with-cleanup`
  and `when` take). nod-reader also has a paren-head path with an
  `is_call_continuation` disambiguation (parser.rs:978-1035); no corpus
  fixture needs it, so it's deferred.
- A general known-macros registry (source `define macro` self-registration
  like parser.rs:2762) is also deferred — no fixture defines and then
  body-calls a non-keyword macro. The two-name predicate is the minimal
  faithful subset; widen it (or add registration) when a fixture demands.
- The dead `#"when"` arm in `is-begin-word?` is left as-is (harmless,
  pre-existing, out of scope) — noted here so it isn't mistaken for live.

## Verification (all re-run by the reviewer, not trusted from the agent)

- `sema_topnames` (`--ignored`): **34/34 MATCH**, incl.
  `macro-when-cleanup`; 1 passed, 0 failed. `dylan-sema.exe` on the
  fixture now prints `fn test-with-cleanup arity=1 return=Top`, byte-equal
  to `--parse-with-rust dump-sema`.
- `dylan_parse_translate`: 1 passed (the `--parse-with-dylan`
  byte-identical gate held; agent observed translated count rose to
  36/46 — the change makes the Dylan parser behave *more* like nod-reader).
- `dylan_parse_coverage`: 1 passed (`macro-when-cleanup` 16/16 structured).
- `dylan_parser`: 25 passed. `sema_self_host`: 1 passed.
- `git status`: only the two intended files changed; no `src/` crate touched.

Dylan-only fixture change → the full Rust sweep is correctly skipped; the
five parser+sema gates above are the relevant guards.

## Where it leaves us

The Dylan sema recording model now byte-matches the Rust oracle across
**34** corpus fixtures, all four sections. **One** sema divergence
remains: anonymous-method lifting `__anon-method-N` (`rope`, `ide_rope`,
`unified_ide`, `nod-ide`) — a recursive expression walk that must
replicate the Rust lifter's traversal order (53.5b). After that, the
load-bearing step is the **`--sema-with-dylan` + verify-mode**
integration (retire the oracle crutch), then Sprint 54 (`lower_with_model`).
