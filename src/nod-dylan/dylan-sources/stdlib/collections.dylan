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

