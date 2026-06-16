Module: dylan
Author: NewOpenDylan stdlib

// ─── Functional helpers, pure Dylan over existing primitives ─────────────────
//
// DRM `dylan` functional operators that are expressible with the toolbox the
// rest of the stdlib already uses (the FIP iteration primitives, the list
// primitives `%pair-alloc` / `%nil`, and `%funcall1`). Like the other stdlib
// files every `define function` is rewritten by the loader into a single-method
// generic on `<object>`, so each name is reachable from user code both as an
// ordinary call AND as a first-class `<function>` value (passed to `map`,
// `choose`, `do`, …). No new Rust runtime code.
//
// SCOPE NOTE — the DRM `apply` / `compose` / `curry` / `rcurry` spread a
// captured-or-collected argument vector back over a `<function>` value. Two
// pieces landed to unblock them:
//
//   1. `#rest` parameter COLLECTION (Sprint 60): a trailing `#rest var` on a
//      `define function` binds `var` to a freshly built
//      `<simple-object-vector>` of the extra actuals.
//   2. SPREAD-apply (`apply`, below): the runtime `nod_apply(fn, args-sov)`
//      reads the SOV's runtime length and dispatches to the matching callee
//      arity (0..8, the `MAX_APPLY_ARITY` cap — see `nod-runtime/functions.rs`).
//      The Dylan `apply` builds that SOV from a leading-args + final-sequence
//      argument list, exactly per the DRM `apply(fn, arg, …, last-seq)` shape.
//
// On top of those, the combinators below are N-ary in the COLLECTED data:
//   * `apply(fn, arg, …, seq)` — spread `seq`'s elements after the leading args.
//   * `compose(f, g, h, …)`    — N-function right-to-left composition.
//   * `curry(f, a, b, …)`      — bind N leading arguments.
//   * `rcurry(f, a, b, …)`     — bind N trailing arguments.
//   * `always(value)`          — one-argument constant function.
//
// COMBINATOR-CLOSURE ARITY — the combinators collect a variable number of
// FUNCTIONS / BOUND ARGUMENTS at definition time (via `#rest` on the top-level
// `define function`, which works), and the closures they RETURN take ONE
// call-site argument (`compose(…)(x)`, `curry(…)(x)`, …). A `#rest` parameter
// on a returned (lifted) closure is not yet collected — the closure ABI stores
// a fixed arity — so the call-site-variadic DRM form (e.g. a composite called
// with several arguments) is the one remaining gap. The one-call-site-argument
// form is what `map` / `choose` / `do` / `sort` reach for and what the corpus
// exercises.

// ─── identity ───────────────────────────────────────────────────────────────
//
// DRM `identity(object) => object`. Returns its argument unchanged. Heavily
// used as the no-op transform passed to `map` / `sort` / `remove-duplicates`
// and as a default key function.

define function identity (x) => (x)
  x
end function;

// ─── complement ──────────────────────────────────────────────────────────────
//
// DRM `complement(predicate) => complement-predicate`. Returns a function that
// calls `predicate` and negates its (boolean) result. Used to flip a predicate
// for `choose` / `remove` / `find-key` (e.g. `choose(complement(empty?), seqs)`).
// One-argument predicate form: the returned closure applies `pred` to a single
// argument via `%funcall1`, which the runtime dispatches correctly for both
// user-defined functions and built-in operator function references.

define function complement (pred) => (negated)
  method (x) ~ %funcall1(pred, x) end
end function;

// ─── apply ────────────────────────────────────────────────────────────────────
//
// DRM `apply(function, #rest arguments)` — call `function` with arguments
// SPREAD from a sequence. The DRM shape is `apply(fn, a, b, …, last-seq)`: every
// argument after `fn` EXCEPT the final one is passed straight through; the final
// argument MUST be a sequence whose elements are spread out as the remaining
// call-site arguments. So `apply(\+, #(3, 4))` is `+(3, 4)` and
// `apply(\+, 3, #(4))` is also `+(3, 4)`.
//
// We collect the post-`fn` actuals with `#rest more` (a `<simple-object-vector>`
// of all arguments after `fn`). Its LAST element is the spread sequence; the
// preceding `lead` elements are spread directly. We build one combined
// `<simple-object-vector>` argv = [lead-args …, elements-of-final-seq …] then
// hand it to `%apply` (the `nod_apply` runtime trampoline). `nod_apply` reads
// argv's runtime length and dispatches to the matching callee arity (0..8 — the
// `MAX_APPLY_ARITY` cap; a longer argv signals a wrong-number-of-arguments
// condition). The final sequence is walked with the forward-iteration protocol,
// so it can be a list, vector, range, or any FIP-registered collection.
//
//   apply(fn, seq)            — 0 leading args; spread all of `seq`.
//   apply(fn, a, b, …, seq)   — leading `a, b, …`, then spread `seq`.
//   apply(fn, #())            — `seq` empty; calls a 0-argument `fn`.

define function apply (fn, #rest more) => (result)
  let nmore = %vector-size(more);
  // `more` always holds at least the final sequence (the DRM requires a
  // trailing sequence argument). `lead` is the count of leading spread-direct
  // actuals; the element at index `lead` is the sequence to spread.
  let lead = nmore - 1;
  let seq = %vector-element(more, lead);
  let total = lead + %collection-size(seq);
  let argv = %make-sov(total);
  // Copy the leading fixed actuals verbatim into the front of argv.
  for (i from 0 below lead)
    %vector-element-setter(%vector-element(more, i), argv, i);
  end;
  // Spread the final sequence's elements after the leading actuals (FIP walk,
  // so any sequence shape — list / vector / range — works uniformly).
  let state = %fip-init(seq);
  let j = lead;
  until (%fip-finished?(state))
    %vector-element-setter(%fip-current-element(state), argv, j);
    j := j + 1;
    %fip-advance!(state);
  end;
  %apply(fn, argv)
end function;

// ─── choose-by ────────────────────────────────────────────────────────────────
//
// DRM `choose-by(predicate, test-sequence, value-sequence) => sequence`.
// Returns a fresh sequence of the elements of `value-sequence` whose
// CORRESPONDING element in `test-sequence` satisfies `predicate`. The two
// sequences are walked in lock-step with the FIP iteration protocol; the kept
// values are collected reversed then reversed back, exactly like `choose`.
// Iteration stops when the test sequence is exhausted (DRM leaves behaviour
// for unequal lengths unspecified; the test sequence drives the walk).

define function choose-by (pred, test-seq, value-seq) => (result)
  let ts = %fip-init(test-seq);
  let vs = %fip-init(value-seq);
  let acc = %nil();
  until (%fip-finished?(ts))
    let t = %fip-current-element(ts);
    let v = %fip-current-element(vs);
    if (%funcall1(pred, t))
      acc := %pair-alloc(v, acc);
    else
      #f
    end;
    %fip-advance!(ts);
    %fip-advance!(vs);
  end;
  reverse(acc)
end function;

// ─── compose ──────────────────────────────────────────────────────────────────
//
// DRM `compose(f, g, …) => composite`. Returns a function that pipes its
// argument right-to-left through the supplied functions: `compose(f, g)(x)`
// is `f(g(x))`, and `compose(f, g, h)(x)` is `f(g(h(x)))`. The N functions are
// collected with `#rest fns` (a `<simple-object-vector>`); the returned closure
// captures that vector and threads its one argument leftward, applying each
// function with `%funcall1`. `compose()` (no functions) returns the identity on
// one argument. The two-function form is the common case; the N-function fold
// is the DRM-faithful generalisation.

define function compose (#rest fns) => (composite)
  method (x)
    let n = %vector-size(fns);
    let acc = x;
    for (i from n - 1 to 0 by -1)
      acc := %funcall1(%vector-element(fns, i), acc);
    end;
    acc
  end
end function;

// ─── curry ──────────────────────────────────────────────────────────────────
//
// DRM `curry(function, arg, …) => curried`. Returns a function that, when
// called, invokes `function` with the captured leading argument(s) PREPENDED to
// the call-site argument. The leading arguments are collected with `#rest pre`
// (a `<simple-object-vector>`); the returned closure builds a fresh argument
// vector `[pre …, x]`, then spreads it over `function` with `%apply`. So
// `curry(\+, 1)(2)` is `+(1, 2)` (the canonical adder/scaler idiom) and
// `curry(f, a, b)(c)` is `f(a, b, c)` for a ternary `f`. The returned closure
// takes ONE call-site argument (see the COMBINATOR-CLOSURE ARITY note above).

define function curry (fn, #rest pre) => (curried)
  method (x)
    let n = %vector-size(pre);
    let argv = %make-sov(n + 1);
    for (i from 0 below n)
      %vector-element-setter(%vector-element(pre, i), argv, i);
    end;
    %vector-element-setter(x, argv, n);
    %apply(fn, argv)
  end
end function;

// ─── rcurry ───────────────────────────────────────────────────────────────────
//
// DRM `rcurry(function, arg, …) => rcurried`. The right-handed sibling of
// `curry`: the captured argument(s) are APPENDED after the call-site argument.
// The captured arguments are collected with `#rest post` (a
// `<simple-object-vector>`); the returned closure builds `[x, post …]` and
// spreads it over `function` with `%apply`. So `rcurry(\-, 1)(10)` is
// `-(10, 1)` (subtract a constant) and `rcurry(f, b, c)(a)` is `f(a, b, c)`.
// The returned closure takes ONE call-site argument (see the
// COMBINATOR-CLOSURE ARITY note above).

define function rcurry (fn, #rest post) => (rcurried)
  method (x)
    let n = %vector-size(post);
    let argv = %make-sov(n + 1);
    %vector-element-setter(x, argv, 0);
    for (i from 0 below n)
      %vector-element-setter(%vector-element(post, i), argv, i + 1);
    end;
    %apply(fn, argv)
  end
end function;

// ─── always ───────────────────────────────────────────────────────────────────
//
// DRM `always(object) => constant-function`. Returns a function that ignores
// its arguments and always returns the captured `object`. The DRM result
// accepts ANY number of arguments: `always(1)()`, `always(1)(99)`, and
// `always(1)(99, 98)` all return `1`. The returned `method (#rest ignore) …`
// is a lifted/escaping closure tagged FUNCTION_KIND_CLOSURE_REST, so the
// runtime collects the trailing actuals into a `#rest` SOV (discarded here)
// at every call shape.

define function always (value) => (constant-function)
  method (#rest ignore) value end
end function;

// ─── disjoin / conjoin ──────────────────────────────────────────────────────────
//
// DRM `disjoin(predicate, …) => disjunction` and `conjoin(predicate, …) =>
// conjunction`: combine predicates with short-circuiting logical OR / AND.
// `disjoin(p, q, r)(x)` returns the first non-`#f` of `p(x)`, `q(x)`, `r(x)`
// (or `#f` if all are false); `conjoin(p, q, r)(x)` returns `#f` at the first
// false predicate, else the last predicate's value (`#t` for no predicates).
// The predicates are collected with `#rest predicates` (a
// `<simple-object-vector>`); each is invoked with `%funcall1` (as in `compose`),
// and the returned closure takes ONE call-site argument (see the
// COMBINATOR-CLOSURE ARITY note above). Short-circuiting falls out of Dylan's
// `|`/`&`: `result | expr` skips `expr` once `result` is non-`#f`, and `result &
// expr` skips it once `result` is `#f` — so later predicates aren't invoked
// after the result is decided. (No `block`/`return`, which would nest a second
// closure and break capture of the outer `#rest predicates`.) The DRM
// `#rest`-argument predicate form (the combined predicate called with several
// arguments) needs `#rest` on the lifted closure — deferred.

define function disjoin (#rest predicates) => (disjunction)
  method (x)
    let n = %vector-size(predicates);
    let result = #f;
    for (i from 0 below n)
      result := result | %funcall1(%vector-element(predicates, i), x);
    end;
    result
  end
end function;

define function conjoin (#rest predicates) => (conjunction)
  method (x)
    let n = %vector-size(predicates);
    let result = #t;
    for (i from 0 below n)
      result := result & %funcall1(%vector-element(predicates, i), x);
    end;
    result
  end
end function;

// `subtype?(t1, t2)` — is every instance of type `t1` also an instance of
// `t2`? Implemented over the `%subtype?` primitive, which walks the class
// precedence list for class types. (Limited/union/singleton types aren't
// modelled in the lattice yet, so those answer #f.)
define function subtype? (type1, type2) => (subtype? :: <boolean>)
  %subtype?(type1, type2)
end function;
