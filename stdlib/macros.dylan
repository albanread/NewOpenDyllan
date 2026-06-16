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
// MUST be wrapped: `(foo(x) + 1)` not `foo(x) + 1`.
//
// **No arity cap (Sprint 49c).** The macro engine now supports
// zero-or-more repetition: a brace group immediately followed by `...`
// matches a SEQUENCE of clauses, and the matching `{ … } ...` splice in
// the template re-emits one expansion per clause. So a SINGLE rule
// handles 1, 2, 3, … N clauses. The `{ ?test:expression
// ?body:expression } ...` unit captures every `(test) (body)` pair; the
// template's `{ elseif (?test) ?body } ...` splices an `elseif` arm for
// each. The leading `if (#f) #f` is a never-firing sentinel so the
// chain is a uniform `elseif*` (it keeps every captured clause inside
// the repetition splice — no special-cased first clause — and the dead
// arm is folded away by lowering). Behaviour is identical to the old
// 4-rule chain; the only change is the cap is gone.

define macro cond
  { cond { ?test:expression ?body:expression } ... otherwise ?default:expression end }
    => { if (#f) #f { elseif (?test) ?body } ... else ?default end }
end macro;

// ─── when-let / if-let macros ──────────────────────────────────────────────
//
// Bind-and-test conditionals. They evaluate an initialiser, bind it to a
// name, and branch on its truth — with the binding in scope for the
// taken branch. The idiomatic "look it up, use it if found" shape:
//
//   when-let (v = lookup(k))
//     use(v)                  // v is in scope here
//   end
//   ⟹  begin let v = lookup(k); if (v) use(v) else #f end end
//
//   if-let (v = lookup(k))
//     use(v)                  // then-branch: v in scope, found
//   else
//     not-found()             // else-branch: v was #f
//   end
//   ⟹  begin let v = lookup(k); if (v) use(v) else not-found() end end
//
// The head is a single `(name = expr)` binding clause. `?var:name` is the
// binder (substituted verbatim — the user's chosen name, NOT hygiene-
// renamed, so it's visible in the body); `?init:body` matches every
// fragment after `=` up to the head's closing `)`, so a multi-token
// initialiser like `lookup(k)` needs no extra parens. The `else` keyword
// splits `if-let`'s two body arms via the engine's delimiter-aware body
// matcher (same mechanism as `with-cleanup`'s `cleanup`). `when-let`
// supplies an `else #f` so its value is well-defined when the test fails.
//
// Both wrap the `let` + `if` in a `begin` so the binding scopes over the
// test and body yet the whole form is a single expression.

define macro when-let
  { when-let (?var:name = ?init:body) ?body:body end }
    => { begin
           let ?var = ?init;
           if (?var) ?body else #f end
         end }
end macro;

define macro if-let
  { if-let (?var:name = ?init:body) ?then:body else ?else:body end }
    => { begin
           let ?var = ?init;
           if (?var) ?then else ?else end
         end }
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

// ─── with-/without-bounds-checks macros ──────────────────────────────────────
//
// Element bounds-check control. In stock Dylan these are optimization hints
// that elide (or force) array/vector bounds checks for the enclosed body. We
// don't currently elide checks, so both forms simply run the body. Defining
// them as macros also teaches the parser the `NAME … end` statement shape.
//
//   without-bounds-checks v[i] := x end   ⟹   begin v[i] := x end
//
define macro without-bounds-checks
  { without-bounds-checks ?body:body end } => { begin ?body end }
end macro;

define macro with-bounds-checks
  { with-bounds-checks ?body:body end } => { begin ?body end }
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

// ─── assert / debug-assert macros ─────────────────────────────────────────────
//
// Convenience macros over the existing `error` condition path. An assertion
// that fails raises an `<error>` (via the `error` builtin), aborting the
// program with the supplied (or a default) message — the same surface the
// runtime already uses for unhandled conditions.
//
//   assert(expr)            ⟹  if (expr) #f else error("assertion failed") end
//   assert(expr, message)   ⟹  if (expr) #f else error(message) end
//
// (It expands to the kernel `if`, not the `unless` macro — a macro that
// expands to another body-shaped macro can't re-parse, because the inner
// expansion re-parse doesn't carry the body-shaped macro name set.)
//
// Call-shaped (parses as an ordinary call; the expander rewrites it before
// lowering). `?value:body` matches the whole test expression inside the
// parens, so a multi-token test needs no extra parens: `assert(x > 0)`.
// The two-argument rule is listed first so its trailing `, ?message` peels
// the message off before the one-argument rule (whose `?value:body` would
// otherwise greedily swallow the comma) is tried.
//
// **Message arity.** The reference `assert` forwards a testworks-style
// variadic `format-string, format-arguments…` pair to `assertion-failure`.
// That helper and a variadic `error` are not vendored here (the `error`
// builtin is arity-1), so this port accepts the no-message and
// single-message-string forms — enough for the common `assert(cond)` and
// `assert(cond, "why")` call sites. A richer formatted message must be
// pre-built by the caller: `assert(cond, concatenate("bad: ", name))`.

define macro assert
  { assert(?value:body, ?message:expression) }
    => { if (?value) #f else error(?message) end }
  { assert(?value:body) }
    => { if (?value) #f else error("assertion failed") end }
end macro;

// `debug-assert` mirrors `assert`. The reference gates the check on a
// `debugging?()` predicate (so release builds elide it); that predicate is
// not vendored here, so this port always performs the check — which is the
// safe behaviour (a debug-assert that fires on a false test is never wrong;
// only the release-elision optimisation is dropped). Identical expansion to
// `assert`.

define macro debug-assert
  { debug-assert(?value:body, ?message:expression) }
    => { if (?value) #f else error(?message) end }
  { debug-assert(?value:body) }
    => { if (?value) #f else error("debug assertion failed") end }
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

// ─── collecting / collect macros ──────────────────────────────────────────────
//
// List accumulation. `collecting () … collect(x) … end` gathers every value
// passed to `collect` and returns them, in collection order, as a list.
//
//   collecting ()
//     collect(1);
//     collect(2);
//     collect(3)
//   end                                    ⟹  #(1, 2, 3)
//
// **How it works.** `collecting` binds a hidden accumulator to the empty
// list, runs the body, and returns the accumulator reversed:
//
//   collecting () BODY end
//   ⟹  begin let %collect-acc = #(); BODY; reverse(%collect-acc) end
//
// `collect(x)` prepends to that accumulator (O(1) — the final `reverse`
// restores order):
//
//   collect(x)  ⟹  %collect-acc := pair(x, %collect-acc)
//
// **Shared accumulator name.** `collect` is a SEPARATE macro from
// `collecting`, so the two must agree on the accumulator's name —
// `%collect-acc` is referenced literally in both templates. This is the
// reference's `?=_collector` unhygienic-injection trick, done here by
// pinning `%collect-acc` in the macro engine's no-rename set
// (`is_template_no_rename` in `nod-macro/src/lib.rs`) so the `let` binder
// in `collecting` is NOT hygiene-renamed and `collect`'s reference resolves
// to it.
//
// **Scope rule.** `collect(x)` must appear textually (lexically) inside a
// `collecting () … end` so the `%collect-acc` binding is visible; calling
// `collect` outside one is an unbound-name error.
//
// **No nesting.** A `collecting` form must not enclose another. Both share
// the fixed `%collect-acc` name, and a macro-emitted `let` in EXPRESSION
// position inserts its binder into the surrounding env rather than opening a
// fresh nested scope (see `Expr::Let` lowering in `nod-sema`), so a nested
// `collecting` clobbers the outer accumulator instead of shadowing it.
// Single, non-nested accumulator only. (The reference's named multi-
// collector `collecting (vars) … end` shape needs the `?=` / `##`
// injection the engine doesn't have — see the skip note in the report.)

define macro collecting
  { collecting () ?body:body end }
    => { begin
           let %collect-acc = #();
           ?body;
           reverse(%collect-acc)
         end }
end macro;

define macro collect
  { collect(?value:body) }
    => { %collect-acc := pair(?value, %collect-acc) }
end macro;

// ─── benchmark definition macro ──────────────────────────────────────────────
//
// The first NewOpenDylan DEFINITION macro: `define benchmark NAME () body end`
// expands to a plain `define function NAME () body end`. It exercises the macro
// engine's definition-macro path — the substituted expansion re-parses as a
// top-level item (a definition), not an expression. The gabriel benchmark suite
// wraps each benchmark body in `define benchmark`.

define macro benchmark
  { define benchmark ?name:name () ?body:body end }
    => { define function ?name () ?body end }
  // No-`end` assignment shorthand: `define benchmark NAME = EXPR;` names a
  // benchmark whose body is EXPR (gabriel suite: `define benchmark takr =
  // testtakr;`). Becomes a nullary function that evaluates EXPR.
  { define benchmark ?name:name = ?val:expression }
    => { define function ?name () ?val end }
end macro;

// ─── benchmark-repeat — minimal testworks-compat timing wrapper ───────────────
//
// `benchmark-repeat (iterations: N) body end` runs the body and yields its
// value. A full testworks repeats the body N times for timing; this minimal
// version runs it once — the RESULT is identical, only the timing is dropped —
// which is enough to compile + run benchmark bodies. Not a faithful port:
// testworks is a separate package not vendored in the reference tree.

define macro benchmark-repeat
  { benchmark-repeat ?opts:expression ?body:body end }
    => { ?body }
end macro;

// ─── testworks define test / define suite (minimal) ──────────────────────────
//
// Definition macros for the testworks harness (a separate package, not vendored
// in the reference tree). `define test NAME (HEAD) body end` becomes a plain
// `define function`; `define suite NAME () … end` becomes a no-op function (the
// suite's `test`/`suite` listing is dropped — running suites needs a real
// runner). Enough to compile test bodies past the harness wrapper. The
// `check-*` helpers used inside test bodies live in collections.dylan.
//
// The head is `?opts:parameter-list` (any `(…)` group), so it accepts both the
// bare `()` form AND testworks keyword-property-list heads like
// `(description: "…")` / `(expected-to-fail-reason: "…")`; the options are
// metadata, not parameters, so they are discarded. The expansion always emits a
// nullary function (`define function NAME () body end`) — the test function is
// real and CALLABLE (build+run verified), never silently dropped.

define macro test
  { define test ?name:name ?opts:parameter-list ?body:body end }
    => { define function ?name () ?body end }
  { define test ?name:name ?body:body end }
    => { define function ?name () ?body end }
end macro;

define macro suite
  { define suite ?name:name () ?body:body end }
    => { define function ?name () #f end }
  { define suite ?name:name ?body:body end }
    => { define function ?name () #f end }
end macro;

// ─── iterate macro ───────────────────────────────────────────────────────────
//
// Dylan's named-let loop. `iterate NAME (v1 = i1, v2 = i2, …) BODY end`
// declares a self-recursive `local method NAME (v1, v2, …) BODY end` and
// immediately calls `NAME(i1, i2, …)`. The body iterates by tail-calling
// `NAME` with the next argument values; the loop's value is whatever the
// final (non-recursive) arm returns. Example — sum 1..5:
//
//   iterate loop (i = 1, acc = 0)
//     if (i > 5) acc else loop(i + 1, acc + i) end
//   end                                    ⟹  15
//
// **Variadic bindings (no arity cap).** The `( { ?var = ?init } , ... )`
// repetition matches a comma-separated sequence of `name = expr` binding
// clauses; the template splices the names into the local method's
// parameter list (`( { ?var } , ... )`) and the inits into the priming
// call (`( { ?init } , ... )`). One rule handles 1, 2, 3, … N bindings.
//
// **Why `begin … end`.** A self-recursive loop is inherently two forms —
// declare the method, then call it — so the template wraps them in a
// `begin` block to satisfy the macro engine's single-expression
// re-parse. `local method` lowers via the `Statement::Local` lift path
// even inside `begin` (see the `parse_local_expr_compat` change in
// `nod-reader/src/parser.rs`, which always emits `Statement::Local`).
//
// `iterate` is in the parser's block-opener set, so the no-head-paren
// recogniser (`iterate NAME (…) … end`) parses it as a body-shaped macro
// call; the `NAME` and binding group are part of the opaque body the
// macro engine re-lexes and pattern-matches.

define macro iterate
  { iterate ?loop:name ( { ?var:name = ?init:expression } , ... ) ?body:body end }
    => { begin
           local method ?loop ( { ?var } , ... ) ?body end;
           ?loop( { ?init } , ... )
         end }
end macro;

// NOTE: an indexed `dotimes (i below N) … end` counted loop is deferred.
// A body-shaped macro whose head is `(var <sep> expr)` with a separator
// other than `for-each`'s special-cased `in` does not parse — the head
// paren group is read as an expression and signals "expected ) after
// arguments" (which, unhandled, panics the eval engine). Tracked in
// docs/reference/known-limitations.md. Use `repeat N times … end` (no
// index) or `for-each (x in coll) … end` meanwhile.

