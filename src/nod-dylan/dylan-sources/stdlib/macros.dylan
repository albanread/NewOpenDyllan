Module: dylan
Author: NewOpenDylan stdlib

// ─── for-each macro ────────────────────────────────────────────────────────
//
// Sugar over the FIP primitives. Expands to a `let state = %fip-init(c);
// until (%fip-finished?(state)) ... %fip-advance!(state) end` loop. The
// `?var:name` binding is rebound on each iteration to the current
// element.
//
// Sprint 25: the body-shaped surface `for-each (x in c) body end` is
// recognised by the parser now (see `Expr::MacroCall` + the
// `known_macros` plumbing in `nod-reader/src/parser.rs`). Sprint 20b
// shipped the macro definition but couldn't call it from a separate
// file because the parser didn't know body-shaped macro syntax.

define macro for-each
  { for-each (?var:name in ?coll:expression) ?body:body end }
    => { begin
           let %fip-state = %fip-init(?coll);
           until (%fip-finished?(%fip-state))
             let ?var = %fip-current-element(%fip-state);
             ?body;
             %fip-advance!(%fip-state)
           end
         end }
end macro;

// ─── unless macro ──────────────────────────────────────────────────────────
//
// Sprint 25: retired the hardcoded `Expr::Unless` AST variant. The
// parser now treats `unless (cond) body end` as a body-shaped macro
// call (because `unless` is in the parser's known-macro set, seeded
// from this stdlib), and the rule below expands it to `if (~ cond)
// body end`. Identical compile-time output to the old hardcoded
// lowering — `if` remains the kernel primitive.

define macro unless
  { unless ?cond:expression ?body:body end }
    => { if (~ ?cond) ?body else #f end }
end macro;

// ─── when macro ────────────────────────────────────────────────────────────
//
// One-armed conditional — the natural partner to `unless`.
// `when` fires the body when the condition is true; `unless` fires it
// when the condition is false. Both expand to `if` with an `else #f`
// so the return type is always well-defined even when the body is
// not taken.
//
//   when (condition) body end
//   ⟹  if (condition) body else #f end
//
// Like `unless`, the condition is an `expression` constraint so the
// parser wraps it in a paren group before fragment-matching begins.

define macro when
  { when ?cond:expression ?body:body end }
    => { if (?cond) ?body else #f end }
end macro;

// ─── cond macro ────────────────────────────────────────────────────────────
//
// Sprint 49b: multi-arm conditional, lowers to nested `if/elseif/else`.
// The Common-Lisp shape, adapted to Dylan's macro engine. Each clause
// is `(test) (body)` — a paren-wrapped condition followed by a
// paren-wrapped body expression. The final clause uses the
// `otherwise` keyword as the default. Example:
//
//   cond
//     (x < 0)   ("negative")
//     (x = 0)   ("zero")
//     (x > 0)   ("positive")
//     otherwise ("unreachable")
//   end
//
// Expands to a straight `if/elseif/else` chain — the kernel
// primitive. No new AST variants; this is purely stdlib sugar.
//
// **Shape constraint.** Each test and body is a single
// `:expression` fragment, which means one token, identifier, literal,
// or grouped form (parens / brackets / braces). Multi-token bodies
// MUST be wrapped: `(foo(x) + 1)` not `foo(x) + 1`. The paren tax
// is the price of admission until the macro engine grows `*`
// repetition (Sprint 49c-ish) — at that point the wrapping can be
// dropped per clause and N-arm support stops being arity-bounded.
//
// **Arity cap.** This rule set supports 1 through 4 test/body pairs
// + `otherwise`. Beyond 4 arms, nest a second `cond` inside the
// `otherwise` clause. The cap is purely the number of fixed rules
// written below — extend by appending more rules in lockstep.

define macro cond
  // 1 test/body pair + otherwise.
  { cond ?t1:expression ?b1:expression otherwise ?d:expression end }
    => { if (?t1) ?b1 else ?d end }
  // 2 pairs + otherwise.
  { cond ?t1:expression ?b1:expression
         ?t2:expression ?b2:expression
         otherwise ?d:expression end }
    => { if (?t1) ?b1 elseif (?t2) ?b2 else ?d end }
  // 3 pairs + otherwise.
  { cond ?t1:expression ?b1:expression
         ?t2:expression ?b2:expression
         ?t3:expression ?b3:expression
         otherwise ?d:expression end }
    => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 else ?d end }
  // 4 pairs + otherwise.
  { cond ?t1:expression ?b1:expression
         ?t2:expression ?b2:expression
         ?t3:expression ?b3:expression
         ?t4:expression ?b4:expression
         otherwise ?d:expression end }
    => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 elseif (?t4) ?b4 else ?d end }
end macro;

// ─── with-cleanup macro ────────────────────────────────────────────────────
//
// Resource-management sugar over `block / cleanup / end`.  The cleanup
// arm is guaranteed to run whether the body exits normally or via a
// non-local exit (NLX) — see `Statement::Block` + `CleanupGuard` in
// the runtime.
//
//   with-cleanup
//     body
//   cleanup
//     cleanup-body
//   end
//   ⟹  block () body cleanup cleanup-body end
//
// The two `:body` variables split at the `cleanup` delimiter keyword.
// This works because the body matcher now uses delimiter-aware greedy
// matching (forward scan for the next Literal in the pattern) rather
// than the old trailing-count-only approach.

define macro with-cleanup
  { with-cleanup ?body:body cleanup ?cleanup:body end }
    => { block () ?body cleanup ?cleanup end }
end macro;

// ─── inc! / dec! macros ──────────────────────────────────────────────────────
//
// In-place increment / decrement of a place expression, sugar over the
// `:=` assignment primitive. Call-shaped, so `inc!(x)` parses as an
// ordinary call and the expander rewrites it before lowering.
//
//   inc!(x)      ⟹  x := x + 1
//   inc!(x, n)   ⟹  x := x + n
//
// `?place:expression` binds one fragment (a bare name or a paren group);
// a dotted/indexed place must be parenthesised — `inc!((v[i]))`.

define macro inc!
  { inc!(?place:expression) }                 => { ?place := ?place + 1 }
  { inc!(?place:expression, ?n:expression) }  => { ?place := ?place + ?n }
end macro;

define macro dec!
  { dec!(?place:expression) }                 => { ?place := ?place - 1 }
  { dec!(?place:expression, ?n:expression) }  => { ?place := ?place - ?n }
end macro;

// ─── repeat macro ────────────────────────────────────────────────────────────
//
// Run a body N times, ignoring the index. Body-shaped with a `times`
// keyword separator (the same delimiter mechanism as `with-cleanup`'s
// `cleanup`); expands to a hidden-counter `while` loop (`while` lowers in
// every context; `for` does not). `%repeat-i` is a template-introduced
// binder, hygiene-renamed (same as `for-each`'s `%fip-state`) so it can't
// collide with a caller's name.
//
//   repeat 3 times do-thing() end
//   ⟹  begin let i = 0; while (i < 3) do-thing(); i := i + 1 end end

define macro repeat
  { repeat ?count:expression times ?body:body end }
    => { begin
           let %repeat-i = 0;
           while (%repeat-i < ?count)
             ?body;
             %repeat-i := %repeat-i + 1
           end
         end }
end macro;

// NOTE: an indexed `dotimes (i below N) … end` counted loop is deferred.
// A body-shaped macro whose head is `(var <sep> expr)` with a separator
// other than `for-each`'s special-cased `in` does not parse — the head
// paren group is read as an expression and signals "expected ) after
// arguments" (which, unhandled, panics the eval engine). Tracked in
// docs/reference/known-limitations.md. Use `repeat N times … end` (no
// index) or `for-each (x in coll) … end` meanwhile.

