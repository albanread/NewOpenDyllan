Module: dylan
Author: NewOpenDylan stdlib

// ─── List / sequence / table extensions, pure Dylan over existing primitives ──
//
// Every entry is `define function`, so the stdlib loader rewrites each into a
// single-method generic on `<object>` (see `nod-sema/src/stdlib.rs`) — making
// the names reachable from user code BOTH as ordinary calls (lowering to a
// `Dispatch` node) and as first-class `<function>` references. The bodies
// compose ONLY over the toolbox the rest of the stdlib already uses: the FIP
// iteration primitives (`%fip-init` / `%fip-finished?` / `%fip-current-element`
// / `%fip-advance!`), the list primitives (`%pair-alloc` / `%nil`), the SOV
// primitives (`%make-sov` / `%vector-element` / `%vector-element-setter` /
// `%collection-size`), the table primitives (`%table-element-or-default` /
// `%table-size`), `%funcall1` / `%funcall2`, and the `values` intrinsic. No new
// Rust runtime code.
//
// NOTHING here duplicates an existing stdlib name (checked against every file in
// STDLIB_FILES). `reverse`, `map`, `reduce`, `member?`, `count`, `find-key`,
// `find-element`, `add`, `remove`, the binary `min`/`max`, the table `size` /
// `element` / `keys` / `values` already live elsewhere and are reused here.

// ─── nth / elt — element at integer index k (0-based), via FIP ────────────────
//
// DRM has `element(seq, key)`; this back-end specialises `element` only on
// `<table>`, so a general positional sequence accessor is still missing. `nth`
// and `elt` walk the FIP forward `k` steps and read the element — covering every
// concrete sequence class (lists, vectors, stretchy vectors, strings). `nth` is
// the classic Lisp name; `elt` is the short alias. Reading past the end is the
// caller's error, exactly as `element` past `size` would be. (Generalises the
// existing `first`/`second`/`third` to an arbitrary index.)

define function nth (c, k) => (e)
  let state = %fip-init(c);
  let i = 0;
  until (i = k)
    %fip-advance!(state);
    i := i + 1;
  end;
  %fip-current-element(state)
end function;

define function elt (c, k) => (e)
  nth(c, k)
end function;

// ─── take — first k elements as a fresh vector ───────────────────────────────
//
// `take(c, k)` returns a fresh `<simple-object-vector>` of the first `k`
// elements of `c`, in order. If `k` exceeds `size(c)` the whole collection is
// returned (no out-of-range read). Common functional-prelude operation absent
// from the DRM by this name but expected by real Dylan programs that slice a
// prefix. FIP-driven, so it spans every sequence shape.

define function take (c, k) => (result)
  let n = %collection-size(c);
  let limit = if (k < n) k else n end;
  let result = %make-sov(limit);
  let state = %fip-init(c);
  let i = 0;
  until (i = limit)
    %vector-element-setter(%fip-current-element(state), result, i);
    i := i + 1;
    %fip-advance!(state);
  end;
  result
end function;

// ─── drop — all but the first k elements, as a fresh list ────────────────────
//
// `drop(c, k)` returns a fresh `<list>` of the elements of `c` after skipping
// the first `k`. If `k` exceeds `size(c)` the empty list is returned. Collected
// reversed then reversed back (the `choose`/`remove` idiom). A list result is
// the natural shape over the pair primitive; the corpus slicing cases only
// check element order / membership.

define function drop (c, k) => (result)
  let state = %fip-init(c);
  let i = 0;
  until ((i = k) | %fip-finished?(state))
    %fip-advance!(state);
    i := i + 1;
  end;
  let acc = %nil();
  until (%fip-finished?(state))
    acc := %pair-alloc(%fip-current-element(state), acc);
    %fip-advance!(state);
  end;
  reverse(acc)
end function;

// ─── sum — fold `+` over a sequence ──────────────────────────────────────────
//
// `sum(c)` returns the sum of the elements of `c` (0 for the empty sequence).
// Pure fold over `+`. Distinct from `reduce` (which needs an explicit seed +
// function) — this is the common `sum(some-numbers)` shorthand. FIP-driven.

define function sum (c) => (total)
  let state = %fip-init(c);
  let total = 0;
  until (%fip-finished?(state))
    total := total + %fip-current-element(state);
    %fip-advance!(state);
  end;
  total
end function;

// ─── product — fold `*` over a sequence ──────────────────────────────────────
//
// `product(c)` returns the product of the elements of `c` (1 for the empty
// sequence). Pure fold over `*`. Companion to `sum`.

define function product (c) => (total)
  let state = %fip-init(c);
  let total = 1;
  until (%fip-finished?(state))
    total := total * %fip-current-element(state);
    %fip-advance!(state);
  end;
  total
end function;

// ─── maximum / minimum — extrema over a sequence ─────────────────────────────
//
// The DRM `max` / `min` are variadic; this back-end provides only the BINARY
// `max(a, b)` / `min(a, b)` (sequences.dylan) because `#rest` isn't bound by the
// lowerer. `maximum` / `minimum` are the distinct SEQUENCE forms: fold the
// running extreme over the elements with `>` / `<`. The empty sequence yields
// #f (DRM would signal). Seeded with the first element so the comparison is
// always element-vs-element. FIP-driven, total.

define function maximum (c) => (m)
  let state = %fip-init(c);
  if (%fip-finished?(state))
    #f
  else
    let m = %fip-current-element(state);
    %fip-advance!(state);
    until (%fip-finished?(state))
      let x = %fip-current-element(state);
      if (x > m) m := x end;
      %fip-advance!(state);
    end;
    m
  end
end function;

define function minimum (c) => (m)
  let state = %fip-init(c);
  if (%fip-finished?(state))
    #f
  else
    let m = %fip-current-element(state);
    %fip-advance!(state);
    until (%fip-finished?(state))
      let x = %fip-current-element(state);
      if (x < m) m := x end;
      %fip-advance!(state);
    end;
    m
  end
end function;

// ─── find-last — last element satisfying `pred` ──────────────────────────────
//
// Companion to the existing `find-element` (first match): scans the WHOLE
// collection and keeps the most-recent element for which `pred` returns true,
// returning it (or #f if none match). FIP-driven; `pred` is a `<function>`
// invoked via `%funcall1`. No short-circuit — the last match needs a full walk.

define function find-last (c, pred) => (e)
  let state = %fip-init(c);
  let result = #f;
  until (%fip-finished?(state))
    let x = %fip-current-element(state);
    if (%funcall1(pred, x))
      result := x;
    else
      #f
    end;
    %fip-advance!(state);
  end;
  result
end function;

// ─── do-with-index — call fn(element, index) for side effects ─────────────────
//
// Like `do`, but the closure receives BOTH the element and its 0-based index
// (via `%funcall2`, the two-arg trampoline `reduce` uses). Returns #f. The
// natural shape for "iterate with a counter" without a manual index variable.

define function do-with-index (fn, c) => (result)
  let state = %fip-init(c);
  let i = 0;
  until (%fip-finished?(state))
    %funcall2(fn, %fip-current-element(state), i);
    i := i + 1;
    %fip-advance!(state);
  end;
  #f
end function;

// ─── interpose — insert `sep` between consecutive elements ───────────────────
//
// `interpose(sep, c)` returns a fresh `<list>` with `sep` inserted between each
// pair of adjacent elements of `c` (e.g. interpose(0, #(1,2,3)) => (1,0,2,0,3)).
// An empty or single-element input is returned unchanged in content. Collected
// reversed then reversed back. Useful for joining sequences with a separator
// element (the sequence analogue of string `join`).

define function interpose (sep, c) => (result)
  let state = %fip-init(c);
  let acc = %nil();
  let first? = #t;
  until (%fip-finished?(state))
    if (first?)
      first? := #f;
    else
      acc := %pair-alloc(sep, acc);
    end;
    acc := %pair-alloc(%fip-current-element(state), acc);
    %fip-advance!(state);
  end;
  reverse(acc)
end function;

// ─── partition — split into (matching, non-matching) ─────────────────────────
//
// DRM `partition(pred, seq) => (matching, non-matching)`: two fresh sequences,
// the first holding the elements for which `pred` is true, the second the rest,
// both in original order. Returns the two via the `values` intrinsic (receive
// with `let (a, b) = partition(...)`). Each side is collected reversed then
// reversed back. `pred` is a `<function>` invoked via `%funcall1`.

define function partition (pred, c) => (matching, non-matching)
  let state = %fip-init(c);
  let yes = %nil();
  let no = %nil();
  until (%fip-finished?(state))
    let x = %fip-current-element(state);
    if (%funcall1(pred, x))
      yes := %pair-alloc(x, yes);
    else
      no := %pair-alloc(x, no);
    end;
    %fip-advance!(state);
  end;
  values(reverse(yes), reverse(no))
end function;

// ─── last-index — index of the last element satisfying `pred` ────────────────
//
// Companion to the existing `find-key` (first match index): returns the 0-based
// index of the LAST element for which `pred` returns true, or #f if none. Full
// walk (the last match can't short-circuit). `pred` is a `<function>`.

define function last-index (c, pred) => (key)
  let state = %fip-init(c);
  let i = 0;
  let key = #f;
  until (%fip-finished?(state))
    if (%funcall1(pred, %fip-current-element(state)))
      key := i;
    else
      #f
    end;
    i := i + 1;
    %fip-advance!(state);
  end;
  key
end function;

// ─── has-key? — table key presence test ──────────────────────────────────────
//
// DRM `key-exists?` / the common `has-key?` idiom: #t iff `key` is present in
// the table. Built over `%table-element-or-default` with a FRESHLY-allocated
// sentinel pair: a new pair has a unique identity, so it can never `==` any
// stored value. If the lookup returns the sentinel, the key was absent. Avoids
// the false-negative a plain `element(t, k) ~= #f` test would give when a key's
// stored value is genuinely #f.

define function has-key? (t, key) => (present?)
  let sentinel = %pair-alloc(0, %nil());
  let got = %table-element-or-default(t, key, sentinel);
  ~ (got == sentinel)
end function;

// `key-exists?` — DRM-spelled alias of `has-key?`.

define function key-exists? (t, key) => (present?)
  has-key?(t, key)
end function;

// ─── list-reverse — reversed list (shape-preserving over the pair primitive) ──
//
// The stdlib's `reverse` returns a fresh `<simple-object-vector>` regardless of
// input shape. `list-reverse` is the LIST-shaped reverse: walks `c` via FIP and
// conses each element onto the front of an accumulator, yielding a `<list>`. For
// list-in / list-out call sites that need the result to stay a `<list>` (so
// `head` / `tail` / pair recursion keep working on it). FIP-driven, so the
// input may be any sequence; the output is always a list.

define function list-reverse (c) => (result)
  let state = %fip-init(c);
  let acc = %nil();
  until (%fip-finished?(state))
    acc := %pair-alloc(%fip-current-element(state), acc);
    %fip-advance!(state);
  end;
  acc
end function;

// ─── zip — pair up two sequences into a list of 2-element vectors ─────────────
//
// `zip(a, b)` returns a fresh `<list>` whose i-th element is the 2-element
// vector `#[a[i], b[i]]`, stopping at the shorter input. The list-of-pairs shape
// real programs reach for when walking two sequences together. Built over FIP +
// `%make-sov` + the pair primitive.

define function zip (a, b) => (result)
  let sa = %fip-init(a);
  let sb = %fip-init(b);
  let acc = %nil();
  until (%fip-finished?(sa) | %fip-finished?(sb))
    let pairv = %make-sov(2);
    %vector-element-setter(%fip-current-element(sa), pairv, 0);
    %vector-element-setter(%fip-current-element(sb), pairv, 1);
    acc := %pair-alloc(pairv, acc);
    %fip-advance!(sa);
    %fip-advance!(sb);
  end;
  reverse(acc)
end function;
