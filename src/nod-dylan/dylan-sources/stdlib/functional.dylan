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
// SCOPE NOTE — the DRM `compose` / `curry` / `rcurry` / `always` are variadic
// (they build closures that re-apply their captured function to an arbitrary
// number of arguments). The full variadic forms still need `#rest` parameter
// COLLECTION (binding a trailing `#rest` to a freshly-allocated argument vector)
// plus a multi-arg `apply` on an arbitrary `<function>` value; the lowerer does
// not yet collect `#rest` into a sequence, so those remain deferred. What IS now
// supported (and was the real blocker) is calling an arbitrary captured / value
// `<function>` with 2+ arguments — fixed in `make_function_ref` so a directly
// registered user `define function` shadows the stdlib single-method generic of
// the same name instead of mis-dispatching into it. So the FIXED-ARITY forms of
// the combinators — the ones the corpus actually reaches for — ship here:
//   * `compose(f, g)`        — two-function composition.
//   * `curry(f, arg)`        — bind ONE leading argument.
//   * `rcurry(f, arg)`       — bind ONE trailing argument.
//   * `always(value)`        — one-argument constant function.
// The N-function `compose` and the multi-captured-arg `curry`/`rcurry` (and the
// 0-/2-arg `always` closures) await `#rest` collection.

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
// is `f(g(x))`. The DRM form is variadic in the number of functions; the
// two-function form shipped here is the overwhelmingly common case (and the
// one the corpus reaches for). The returned closure captures `f` and `g` and
// applies each with `%funcall1`, the single-argument funcall path. (The
// N-function form needs `#rest` collection over the function list — deferred.)

define function compose (f, g) => (composite)
  method (x) %funcall1(f, %funcall1(g, x)) end
end function;

// ─── curry ──────────────────────────────────────────────────────────────────
//
// DRM `curry(function, arg, …) => curried`. Returns a function that, when
// called, invokes `function` with the captured leading argument(s) PREPENDED
// to the call-site arguments. The single-captured-argument form shipped here
// returns `method (x) function(arg, x) end`: it binds ONE leading argument and
// accepts ONE more at call time, applying the captured `function` value with
// `%funcall2` (the multi-arg funcall path fixed this sprint). This covers the
// canonical `curry(\+, n)` / `curry(\*, n)` adder/scaler idioms. (Binding more
// than one leading arg, or accepting a variable number of trailing args, needs
// `#rest` collection — deferred.)

define function curry (fn, arg) => (curried)
  method (x) %funcall2(fn, arg, x) end
end function;

// ─── rcurry ───────────────────────────────────────────────────────────────────
//
// DRM `rcurry(function, arg, …) => rcurried`. The right-handed sibling of
// `curry`: the captured argument(s) are APPENDED after the call-site arguments.
// The single-captured-argument form returns `method (x) function(x, arg) end`,
// applying the captured `function` value with `%funcall2`. Canonical use is
// `rcurry(\-, n)` (subtract a constant) and `rcurry(\<, n)` (a "less-than-n"
// predicate). (Multi-arg forms need `#rest` collection — deferred.)

define function rcurry (fn, arg) => (rcurried)
  method (x) %funcall2(fn, x, arg) end
end function;

// ─── always ───────────────────────────────────────────────────────────────────
//
// DRM `always(object) => constant-function`. Returns a function that ignores
// its arguments and always returns the captured `object`. The DRM result
// accepts any number of arguments; the one-argument form shipped here returns
// `method (ignore) value end`, which is the shape `map` / `find-key` / default
// callbacks reach for (a constant transform over one element). (The argument-
// agnostic 0-/N-arg form needs `#rest` collection — deferred.)

define function always (value) => (constant-function)
  method (ignore) value end
end function;
