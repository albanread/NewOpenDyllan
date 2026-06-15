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
// number of arguments). That requires `#rest` parameter binding + a correct
// multi-arg `apply` on an arbitrary `<function>` value, neither of which the
// current lowerer/runtime supports correctly (a captured user function called
// with two arguments computes a garbage result). They are deliberately omitted
// rather than shipped wrong. The helpers here only ever apply a captured
// function to ONE argument (`%funcall1`), which is the path the runtime gets
// right.

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
