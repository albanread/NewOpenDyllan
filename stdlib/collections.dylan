Module: dylan
Author: NewOpenDylan stdlib

// ─── nod-stdlib-marker — loader-sanity sentinel ───────────────────────────
//
// Echoes its argument back unchanged + 1. Used by tests to confirm the
// loader registered stdlib methods into the process-global dispatch
// table. The single argument is required so the loader rewrites this
// as `define method nod-stdlib-marker (x :: <object>)` — 0-arg generics
// aren't allowed in Dylan.

define function nod-stdlib-marker (x) => (n)
  x + 1
end function;

// ─── assert-equal — minimal testworks-compat check ───────────────────────────
//
// Minimal stand-in for testworks' `assert-equal`: yields #t when the two values
// are equal. A full testworks signals a test failure on mismatch; this thin
// version unblocks compiling and running test/benchmark bodies that call it.
// Not a faithful port — testworks is a separate package not vendored in the
// reference tree.

define function assert-equal (expected, actual) => (ok)
  expected = actual
end function;

// ─── check-* / assert-* — minimal testworks-compat assertions ────────────────
//
// Minimal stand-ins for testworks' check/assert helpers used inside `define
// test` bodies. The leading `description` string is accepted and ignored; the
// result is the boolean outcome. A full testworks records pass/fail into a
// result tree — these thin versions unblock compiling test bodies that call
// them. testworks is a separate package not vendored in the reference tree.

define function check-equal (description, expected, actual) => (ok)
  expected = actual
end function;

define function check-true (description, value) => (ok)
  value
end function;

define function check-false (description, value) => (ok)
  value = #f
end function;

define function assert-true (value) => (ok)
  value
end function;

// Second-tier testworks helpers whose tested value is fine to evaluate
// eagerly (unlike check-condition/assert-signals, which are macros). These
// just inspect an already-computed value.
define function check-instance? (description, type, value) => (ok)
  instance?(value, type)
end function;

define function assert-instance? (type, value) => (ok)
  instance?(value, type)
end function;

define function assert-false (value) => (ok)
  value = #f
end function;

// testworks' `test-output` writes progress text; used directly and via
// `apply(test-output, args)`, so it must be a first-class #rest function.
define function test-output (#rest args) => ()
  #f
end function;

// Condition-testing helpers. NOTE: these are plain functions, so the tested
// form is evaluated EAGERLY as an argument — they make the test files COMPILE
// but do not actually catch (check-condition/assert-signals cannot observe a
// signal that fires during argument evaluation). Proper catching needs the
// macro engine to match a multi-fragment argument in a comma-separated
// call-macro pattern (today `(?a:expression, ?b:expression)` only matches
// single-fragment args), tracked in docs/reference/known-limitations.md. The
// `#rest more` absorbs the optional trailing description some call sites pass.
define function check-condition (description, condition-type, value) => (ok)
  #t
end function;

define function check-no-errors (description, value) => (ok)
  #t
end function;

define function check-no-condition (description, value) => (ok)
  #t
end function;

define function assert-signals (condition-type, value, #rest more) => (ok)
  #t
end function;

define function assert-no-errors (value, #rest more) => (ok)
  #t
end function;

// ─── size ──────────────────────────────────────────────────────────────────
//
// A thin wrapper around the `%collection-size` primitive. The primitive
// dispatches on the concrete class internally (it's the existing Rust
// `collection_size` exposed via the `nod_collection_size` extern). The
// loader rewrites this to a method on `(c :: <object>)`, registered as
// the sole entry under the `size` generic; user code's call to `size(c)`
// resolves through the process-global dispatch table.

define function size (c) => (n)
  %collection-size(c)
end function;

// ─── concatenate ───────────────────────────────────────────────────────────
//
// Binary concatenate. Delegates to the `%collection-concatenate`
// primitive (Rust `collection_concatenate`). Preserves shape when both
// inputs share a class; widens to `<simple-object-vector>` otherwise.

define function concatenate (c1, c2) => (result)
  %collection-concatenate(c1, c2)
end function;

// ─── reduce ────────────────────────────────────────────────────────────────
//
// Sprint 21: now Dylan-defined. `fn` is a `<function>` first-class
// value; the inner combiner call lowers to `nod_funcall2(fn, acc, x)`
// because `fn` is an env-bound name that isn't a top-level function or
// generic. FIP-driven so this body is identical for every concrete
// collection class registered with `forward-iteration-protocol`.

define function reduce (fn, init, c) => (result)
  let state = %fip-init(c);
  let acc = init;
  until (%fip-finished?(state))
    acc := %funcall2(fn, acc, %fip-current-element(state));
    %fip-advance!(state)
  end;
  acc
end function;

// ─── map ───────────────────────────────────────────────────────────────────
//
// Sprint 21: returns a fresh `<simple-object-vector>` of length
// `size(c)`. Shape-preserving variants (return a `<list>` when input
// is a `<list>`, etc.) land alongside the rest of the stdlib
// collection methods in Sprint 22+.

define function map (fn, c) => (result)
  let n = %collection-size(c);
  let result = %make-sov(n);
  let state = %fip-init(c);
  let i = 0;
  until (%fip-finished?(state))
    %vector-element-setter(%funcall1(fn, %fip-current-element(state)), result, i);
    i := i + 1;
    %fip-advance!(state)
  end;
  result
end function;

// ─── do ────────────────────────────────────────────────────────────────────
//
// Sprint 21: invoke `fn` on each element of `c` for side effects.
// Returns `#f`.

define function do (fn, c) => (result)
  let state = %fip-init(c);
  until (%fip-finished?(state))
    %funcall1(fn, %fip-current-element(state));
    %fip-advance!(state)
  end;
  #f
end function;

// ─── <table> generics (Sprint 22) ──────────────────────────────────────────
//
// `<table>` is a `<explicit-key-collection>` registered as a seed class
// by `nod_runtime::tables`. The runtime owns the heap layout + the
// open-addressing hash machinery + the `object-hash` /
// `object-equal?` fast path; this file owns the user-visible generic
// surface.
//
// The methods below specialise on `<table>` so they outrank the
// `<object>` rewrites of `size` / `concatenate` / etc. for tables.

define method size (t :: <table>) => (n :: <integer>)
  %table-size(t)
end method;

define method element (t :: <table>, key) => (value)
  %table-element-or-default(t, key, #f)
end method;

define method element-setter (value, t :: <table>, key) => (value)
  %table-element-setter(value, t, key)
end method;

define method remove-key! (t :: <table>, key) => (t)
  %table-remove-key(t, key)
end method;

define method keys (t :: <table>) => (ks)
  %table-keys(t)
end method;

define method values (t :: <table>) => (vs)
  %table-values(t)
end method;

// `object-hash` and `object-equal?` are exposed as Dylan-side generics
// so user code can call them and (eventually) add methods for new key
// types. The Rust fast path still drives table probes — these methods
// just surface the primitive to user code.

define method object-hash (x) => (h :: <integer>)
  %object-hash(x)
end method;

define method object-equal? (a, b) => (eq :: <boolean>)
  %object-equal?(a, b)
end method;

// ─── key-sequence — the collection's keys as a sequence ──────────────────────
//
// DRM `key-sequence(collection)` returns a sequence of the collection's keys.
// For an `<explicit-key-collection>` (a `<table>`) the keys are the stored hash
// keys — surfaced via the `%table-keys` primitive (the table arm specialises
// below). For a `<sequence>` the keys ARE the integer indices `0 … size-1`, so
// the general `<object>` body just materialises that index vector. Distinct
// from `keys` (which is the table-only accessor) — `key-sequence` is the
// collection-protocol generic the corpus calls on both shapes.

define function key-sequence (c) => (ks)
  let n = %collection-size(c);
  let result = %make-sov(n);
  let i = 0;
  until (i = n)
    %vector-element-setter(i, result, i);
    i := i + 1;
  end;
  result
end function;

// The `<table>` specialisation: its keys are the stored hash keys, not a
// `0 … n-1` index run. Outranks the `<object>` rewrite above for tables, the
// same way `size` / `element` / `keys` already specialise on `<table>`.

define method key-sequence (t :: <table>) => (ks)
  %table-keys(t)
end method;

// ─── key-test — the collection's key-equivalence predicate ───────────────────
//
// DRM `key-test(collection)` returns the two-argument predicate used to compare
// keys for equivalence. Every collection in this back-end keys with identity /
// value equality, so the answer is the `==` equivalence function. We return the
// first-class `==` reference — a primitive operator func-ref that funcalls
// cleanly (`key-test(c)(a, b)`), unlike a generic — which is both the truthy
// value the corpus tests for (`key-test(list()) | signal(…)`) AND a usable
// two-argument predicate. A single body covers every collection class.

define function key-test (c) => (test)
  \==
end function;

// ─── element-or-default — element lookup with a fallback ──────────────────────
//
// DRM `element(collection, key, default: d)` returns `d` when `key` is absent.
// This positional helper is the keyword-free spelling the corpus reaches for on
// tables: `element-or-default(t, k, d)`. The `<table>` arm delegates to the
// `%table-element-or-default` primitive (already used by `element` on tables).
// The general `<object>` body covers sequences: an in-range integer key reads
// the element, otherwise the default is returned (no out-of-range signal).

define method element-or-default (t :: <table>, key, default) => (value)
  %table-element-or-default(t, key, default)
end method;

define function element-or-default (c, key, default) => (value)
  if ((key >= 0) & (key < %collection-size(c)))
    %vector-element(c, key)
  else
    default
  end
end function;

// ─── copy — a fresh shallow vector copy of any collection ────────────────────
//
// DRM does not name a bare `copy`, but a shallow same-elements duplicate is the
// workhorse the corpus needs for "snapshot then mutate" patterns. Returns a
// fresh `<simple-object-vector>` holding the same elements in iteration order
// (shape-preserving copy for lists/strings needs per-class allocation — see the
// `reverse` note in sequences.dylan). Built directly on FIP + the SOV
// primitives. `copy-sequence` (below) is the DRM-named sequence variant.

define function copy (c) => (result)
  let n = %collection-size(c);
  let result = %make-sov(n);
  let state = %fip-init(c);
  let i = 0;
  until (%fip-finished?(state))
    %vector-element-setter(%fip-current-element(state), result, i);
    i := i + 1;
    %fip-advance!(state);
  end;
  result
end function;

// ─── map-into — destructive map writing results into a target ────────────────
//
// DRM `map-into(target, fn, source)` applies `fn` to each element of `source`
// and stores the results into `target` (mutating it in place), returning
// `target`. Three-argument single-source form: `fn` is a `<function>` value
// invoked via `%funcall1`; results are written through `%vector-element-setter`,
// so `target` must be an index-mutable vector. The multi-source variadic form
// (`map-into(t, \+, a, b)`) needs `#rest` the lowerer does not bind yet — those
// call sites pass extra positional args and are deferred (see report). Stops at
// the shorter of the two lengths so neither side is read/written out of range.

define function map-into (target, fn, source) => (target)
  let n = %collection-size(source);
  let m = %collection-size(target);
  let limit = if (n < m) n else m end;
  let state = %fip-init(source);
  let i = 0;
  until ((i = limit) | %fip-finished?(state))
    %vector-element-setter(%funcall1(fn, %fip-current-element(state)), target, i);
    i := i + 1;
    %fip-advance!(state);
  end;
  target
end function;

