Module: dylan
Author: NewOpenDylan stdlib

// ─── Sequence + number helpers, pure Dylan over existing primitives ─────────
//
// Everything here is `define function`, so the stdlib loader rewrites each to
// a single-method generic on `<object>` (see `nod-sema/src/stdlib.rs`). That
// makes the names reachable from user code BOTH as ordinary calls (which lower
// to a `Dispatch` node) and as first-class function references (`\member?`,
// passed to `map`/`do`/`find-key`, …). The bodies compose over the FIP
// primitives (`%fip-init` / `%fip-finished?` / `%fip-current-element` /
// `%fip-advance!`), the list primitives (`%pair-alloc` / `%nil`), the SOV
// primitives (`%make-sov` / `%vector-element-setter` / `%collection-size`) and
// `%funcall1` — the same toolbox `reduce` / `map` / `do` already use in
// collections.dylan. No new Rust runtime code.

// ─── integer range constants ────────────────────────────────────────────────
//
// The DRM `$minimum-integer` / `$maximum-integer` bound the `<integer>`
// (fixnum) range. This compiler tags fixnums in 63 bits, so the limits are
// ±2^62 (see `FIXNUM_MIN_I128` / `FIXNUM_MAX_I128` in nod-sema/src/lower.rs).
// The stdlib loader harvests `define constant NAME = <int>` into the
// process-global constants table (see stdlib.rs) and strips it, so a bare
// `$maximum-integer` in user code lowers straight to the integer literal.

define constant $maximum-integer = 4611686018427387903;   //  2^62 - 1
define constant $minimum-integer = -4611686018427387904;  // -2^62

// The DRM `$machine-word-size` is the bit width of a raw `<machine-word>`.
// This back-end targets 64-bit; user code uses it as a shift amount /
// width literal (`ash(1, $machine-word-size - 4)`, …).
define constant $machine-word-size = 64;

// ─── integer predicates ─────────────────────────────────────────────────────
//
// DRM number predicates. `mod` / `rem` are infix operators in this Dylan
// (parsed by nod-reader), lowering to `ModInt` / `RemInt` PrimOps — so these
// are plain integer arithmetic, no primitive needed.

define function even? (n) => (yes?)
  (n mod 2) = 0
end function;

define function odd? (n) => (yes?)
  (n mod 2) ~= 0
end function;

define function zero? (n) => (yes?)
  n = 0
end function;

define function positive? (n) => (yes?)
  n > 0
end function;

define function negative? (n) => (yes?)
  n < 0
end function;

// ─── first / second / third — nth element via FIP ───────────────────────────
//
// Walk the collection's FIP state forward `k` steps and read the element.
// Works for every collection class registered with the FIP (lists, vectors,
// stretchy vectors, …), so a single body covers all sequence shapes. No
// bounds checking beyond what the FIP itself enforces (reading past the end
// is the caller's error, exactly as `element` past `size` would be).

define function first (c) => (e)
  %fip-current-element(%fip-init(c))
end function;

define function second (c) => (e)
  let state = %fip-init(c);
  %fip-advance!(state);
  %fip-current-element(state)
end function;

define function third (c) => (e)
  let state = %fip-init(c);
  %fip-advance!(state);
  %fip-advance!(state);
  %fip-current-element(state)
end function;

// ─── last — final element via a full FIP walk ───────────────────────────────
//
// Carries the most-recently-seen element across the loop; the value left in
// `e` when the state finishes is the last element. An empty collection yields
// #f (DRM signals an error there; #f keeps the helper total without a
// condition surface).

define function last (c) => (e)
  let state = %fip-init(c);
  let e = #f;
  until (%fip-finished?(state))
    e := %fip-current-element(state);
    %fip-advance!(state);
  end;
  e
end function;

// ─── member? — linear search with `=` ───────────────────────────────────────
//
// Returns #t when `value` is `=` to some element of `c`, else #f. (DRM's
// `member?` returns the matching tail for lists; this boolean form matches how
// the corpus tests use it — `check-true("", member?(x, c))`.) The `test:`
// keyword variant lowers to extra positional args at the call site, which this
// single-method generic accepts at lower time; the default `=` is used.

define function member? (value, c) => (found?)
  let state = %fip-init(c);
  let found = #f;
  until (%fip-finished?(state) | found)
    if (%fip-current-element(state) = value)
      found := #t;
    else
      %fip-advance!(state);
    end;
  end;
  found
end function;

// ─── any? / every? — quantifiers over one collection ────────────────────────
//
// `pred` is a first-class `<function>` value, invoked via `%funcall1` (the
// same trampoline `map` / `do` use). `any?` short-circuits on the first true
// result; `every?` short-circuits on the first false. Single-collection form
// only — the DRM multi-collection variadic form needs `#rest`, which the
// lowerer doesn't bind yet (see report).

define function any? (pred, c) => (result)
  let state = %fip-init(c);
  let hit = #f;
  until (%fip-finished?(state) | hit)
    let r = %funcall1(pred, %fip-current-element(state));
    if (r)
      hit := r;
    else
      %fip-advance!(state);
    end;
  end;
  hit
end function;

define function every? (pred, c) => (result)
  let state = %fip-init(c);
  let ok = #t;
  until (%fip-finished?(state) | (~ ok))
    if (%funcall1(pred, %fip-current-element(state)))
      %fip-advance!(state);
    else
      ok := #f;
    end;
  end;
  ok
end function;

// ─── find-key — first key whose element satisfies `pred` ────────────────────
//
// Returns the integer index (the key, for sequences) of the first element for
// which `pred` returns true, or #f if none. Built on FIP + a running counter,
// so it works for any sequence shape. (For explicit-key collections the DRM
// returns the true key; this counter form is the sequence specialisation,
// which is what the corpus exercises.)

define function find-key (c, pred) => (key)
  let state = %fip-init(c);
  let i = 0;
  let key = #f;
  until (%fip-finished?(state) | (key ~= #f))
    if (%funcall1(pred, %fip-current-element(state)))
      key := i;
    else
      i := i + 1;
      %fip-advance!(state);
    end;
  end;
  key
end function;

// ─── reduce1 — reduce with the first element as the seed ─────────────────────
//
// Like `reduce` but the initial accumulator is the collection's first element
// rather than an explicit seed; the fold then runs over the remaining
// elements. An empty collection yields #f (DRM signals an error). `fn` is a
// `<function>` value, called via `%funcall2`.

define function reduce1 (fn, c) => (result)
  let state = %fip-init(c);
  if (%fip-finished?(state))
    #f
  else
    let acc = %fip-current-element(state);
    %fip-advance!(state);
    until (%fip-finished?(state))
      acc := %funcall2(fn, acc, %fip-current-element(state));
      %fip-advance!(state);
    end;
    acc
  end
end function;

// ─── reverse — fresh reversed vector ────────────────────────────────────────
//
// Allocates a `<simple-object-vector>` of the same length and fills it
// back-to-front via FIP. Returns a vector regardless of input shape (the DRM
// preserves type; shape-preserving reverse for lists / strings lands when the
// per-class machinery does — see report). Good enough to unblock the
// `reverse(seq)` call sites that only check element order.

define function reverse (c) => (result)
  let n = %collection-size(c);
  let result = %make-sov(n);
  let state = %fip-init(c);
  let i = n - 1;
  until (%fip-finished?(state))
    %vector-element-setter(%fip-current-element(state), result, i);
    i := i - 1;
    %fip-advance!(state);
  end;
  result
end function;

// ─── add — prepend onto a list ──────────────────────────────────────────────
//
// `add(list, new)` ⇒ `pair(new, list)`. This is the DRM behaviour for
// `<list>` (cons onto the front, sharing the tail). For other sequence shapes
// the DRM returns a fresh sequence; that needs per-class allocation and is
// deferred — the list form is what the corpus `add(collection, new-element)`
// cases reach for.

define function add (c, new) => (result)
  %pair-alloc(new, c)
end function;

// ─── choose — keep elements satisfying `pred` ───────────────────────────────
//
// Returns a fresh list of the elements for which `pred` returns true, in
// original order. Built by collecting into a reversed list then reversing it
// back into a vector — composes only over existing primitives. (DRM preserves
// the input type; the list/vector result here is enough for the corpus
// `choose(pred, seq)` cases that compare element membership.)

define function choose (pred, c) => (result)
  let state = %fip-init(c);
  let acc = %nil();
  until (%fip-finished?(state))
    let x = %fip-current-element(state);
    if (%funcall1(pred, x))
      acc := %pair-alloc(x, acc);
    else
      #f
    end;
    %fip-advance!(state);
  end;
  reverse(acc)
end function;

// ─── remove — drop elements `=` to `value` ──────────────────────────────────
//
// Returns a fresh vector of the elements NOT `=` to `value`, in order. Built
// the same way as `choose` (collect-reversed then reverse). The `test:` /
// `count:` keyword variants lower their keywords to trailing positional args
// at the call site (accepted by this single-method generic at lower time); the
// default `=` / unbounded count is used.

define function remove (c, value) => (result)
  let state = %fip-init(c);
  let acc = %nil();
  until (%fip-finished?(state))
    let x = %fip-current-element(state);
    if (x = value)
      #f
    else
      acc := %pair-alloc(x, acc);
    end;
    %fip-advance!(state);
  end;
  reverse(acc)
end function;

// ─── empty? — size-zero test, as a first-class generic ──────────────────────
//
// `empty?(x)` as a CALL is intercepted by the lower-time list-builtin shortcut
// (`%empty?` → `nod_empty_p`) before generic dispatch, so calls keep their fast
// path unchanged. This generic exists so a BAREWORD `empty?` (passed to
// `find-key`, `map`, … as a `<function>` value) resolves to a function-ref
// instead of erroring as an undefined ident. The body is the general
// size-based test for the dispatched case.

define function empty? (c) => (yes?)
  %collection-size(c) = 0
end function;

// ─── aref — array/sequence element read ─────────────────────────────────────
//
// `aref(a, i)` reads element `i`. Two-arg (linear) form here, built on
// `%vector-element`; multi-dimensional `aref(a, i, j, …)` needs `<array>`
// (not yet a registered class) and is deferred. Exists primarily so `aref` is
// referenceable as a value (`apply(aref, list(a, 3))`).

define function aref (a, i) => (e)
  %vector-element(a, i)
end function;

// ─── max / min — binary numeric extrema ─────────────────────────────────────
//
// Two-argument forms only. The DRM `max` / `min` are variadic (`#rest`), which
// the lowerer can't bind yet (see report), so this covers the common
// `max(a, b)` / `min(a, b)` call sites; wider arities fall through to dispatch
// with extra args (compiles, runs the 2-arg method).

define function max (a, b) => (m)
  if (a > b) a else b end
end function;

define function min (a, b) => (m)
  if (a < b) a else b end
end function;

// ─── pair / head / tail — first-class references to the list builtins ───────
//
// `pair`, `head`, `tail` are intercepted as CALLS by the lower-time list-builtin
// shortcut (→ `%pair-alloc` / `%pair-head` / `%pair-tail`) before generic
// dispatch, so direct calls keep their fast path. These generics exist purely
// so a BAREWORD reference (e.g. `reduce(pair, seed, c)`) resolves to a
// function-ref instead of erroring. The bodies delegate to the same
// primitives, so the dispatched path is also correct.

define function pair (h, t) => (p)
  %pair-alloc(h, t)
end function;

define function head (p) => (h)
  %pair-head(p)
end function;

define function tail (p) => (t)
  %pair-tail(p)
end function;

// ─── list — construct a <list> ───────────────────────────────────────────────
//
// The DRM `list` is variadic (`list(a, b, c, …)`). The lowerer doesn't bind
// `#rest` parameters in function bodies yet (see report), so this is the
// one-argument constructor `list(x) ⇒ pair(x, #())`. Its real job in the
// corpus is to make the BAREWORD `list` resolve as a first-class function
// value (`reduce(list, seed, c)`): defining it registers a `list` generic so
// the function-ref path finds it instead of erroring. Multi-argument call
// sites (`list(a, b)`) lower through the dispatch path with their extra args
// regardless (they compile; the 1-arg body runs).

define function list (x) => (l)
  %pair-alloc(x, %nil())
end function;

// ─── add! / add-new / add-new! — list mutators (cons-on-front) ───────────────
//
// `add!` is the (possibly-destructive) sibling of `add`; for `<list>` it's
// `pair(new, c)` exactly like `add` (no in-place mutation of an immutable
// list). `add-new` / `add-new!` cons only when `new` is not already a member,
// matching the DRM "set-like add" behaviour. All compose over `member?` + the
// pair primitive.

define function add! (c, new) => (result)
  %pair-alloc(new, c)
end function;

define function add-new (c, new) => (result)
  if (member?(new, c))
    c
  else
    %pair-alloc(new, c)
  end
end function;

define function add-new! (c, new) => (result)
  add-new(c, new)
end function;

// ─── nth-element setters ─────────────────────────────────────────────────────
//
// `first(s) := v` lowers to `first-setter(v, s)` (value-first arg order). These
// write through `%vector-element-setter` at the fixed index, so they mutate
// `<simple-object-vector>` / stretchy-vector backing in place and return the
// value. (Lists aren't index-mutable through this primitive; the setter form
// is exercised by the corpus on vector receivers.)

define function first-setter (value, s) => (value)
  %vector-element-setter(value, s, 0);
  value
end function;

define function second-setter (value, s) => (value)
  %vector-element-setter(value, s, 1);
  value
end function;

define function third-setter (value, s) => (value)
  %vector-element-setter(value, s, 2);
  value
end function;

define function last-setter (value, s) => (value)
  %vector-element-setter(value, s, %collection-size(s) - 1);
  value
end function;

// ─── sort — fresh ascending vector (insertion sort over a copy) ─────────────
//
// Copies the input into a `<simple-object-vector>` via `map` (identity), then
// insertion-sorts in place with the default `<` ordering and returns it. O(n²)
// — fine for the small corpus inputs; a merge sort lands when `<sequence>`
// growth warrants it. The DRM `sort` is non-destructive and accepts a `test:`
// keyword; the keyword lowers to a trailing positional arg the single-method
// generic absorbs, and the default `<` order is used.

define function nod-identity (x) => (x) x end function;

define function sort (c) => (result)
  let v = map(nod-identity, c);
  let n = %collection-size(v);
  let i = 1;
  until (i = n)
    let key = %vector-element(v, i);
    let j = i - 1;
    let placed = #f;
    until ((j < 0) | placed)
      let vj = %vector-element(v, j);
      if (vj > key)
        %vector-element-setter(vj, v, j + 1);
        j := j - 1;
      else
        placed := #t;
      end;
    end;
    %vector-element-setter(key, v, j + 1);
    i := i + 1;
  end;
  v
end function;

define function sort! (c) => (result)
  sort(c)
end function;

// ─── reverse! — destructive reverse (delegates to fresh reverse) ────────────
//
// We don't have an in-place sequence reverse primitive; this returns a fresh
// reversed vector (same observable element order as `reverse!`). Honest about
// being non-destructive — see report.

define function reverse! (c) => (result)
  reverse(c)
end function;

// ─── subsequence-position — index of first occurrence of a subsequence ──────
//
// Naive scan: returns the integer index at which `pattern` first occurs in
// `big`, or #f. Generalises `find-substring` (strings.dylan) to any sequence
// by reading elements via `element`-style FIP copies into vectors first, then
// comparing element-by-element with `=`.

define function subsequence-position (big, pattern) => (idx)
  let b = map(nod-identity, big);
  let p = map(nod-identity, pattern);
  let nb = %collection-size(b);
  let np = %collection-size(p);
  if (np = 0)
    0
  elseif (np > nb)
    #f
  else
    let last = nb - np;
    let i = 0;
    let hit = #f;
    until ((i > last) | (hit ~= #f))
      let k = 0;
      let ok = #t;
      until ((k = np) | (~ ok))
        if (%vector-element(b, i + k) = %vector-element(p, k))
          k := k + 1;
        else
          ok := #f;
        end;
      end;
      if (ok)
        hit := i;
      else
        i := i + 1;
      end;
    end;
    hit
  end
end function;

// ─── replace-subsequence! — splice `insert` in for the head of `big` ────────
//
// Non-destructive here (we lack an in-place splice primitive): returns a fresh
// vector that is `insert` followed by the tail of `big` after `size(insert)`
// elements. Matches the corpus cases that replace a leading run; the optional
// `start:` / `end:` keyword bounds lower to trailing positional args the
// single-method generic absorbs (defaults used). See report.

define function replace-subsequence! (big, insert) => (result)
  let b = map(nod-identity, big);
  let ins = map(nod-identity, insert);
  let nb = %collection-size(b);
  let ni = %collection-size(ins);
  let tail-len = if (ni > nb) 0 else nb - ni end;
  let out = %make-sov(ni + tail-len);
  let i = 0;
  until (i = ni)
    %vector-element-setter(%vector-element(ins, i), out, i);
    i := i + 1;
  end;
  let j = 0;
  until (j = tail-len)
    %vector-element-setter(%vector-element(b, ni + j), out, ni + j);
    j := j + 1;
  end;
  out
end function;

// ─── replace-elements! — map a predicate-selected subset in place ───────────
//
// `replace-elements!(seq, pred, new-fn)` replaces each element for which
// `pred` is true by `new-fn(element)`, mutating a copy and returning it.
// Composes `map` (copy), FIP scan, and `%vector-element-setter`.

define function replace-elements! (c, pred, new-fn) => (result)
  let v = map(nod-identity, c);
  let n = %collection-size(v);
  let i = 0;
  until (i = n)
    let x = %vector-element(v, i);
    if (%funcall1(pred, x))
      %vector-element-setter(%funcall1(new-fn, x), v, i);
    else
      #f
    end;
    i := i + 1;
  end;
  v
end function;

// ─── copy-sequence — fresh same-length vector copy (arity-1) ─────────────────
//
// DRM `copy-sequence(seq)` returns a fresh sequence with the same elements.
// strings.dylan already owns the shape-preserving `<byte-string>` methods
// (arities 1 and 3), which outrank this `<object>` rewrite for byte-strings;
// this body is the general fallback for vectors / lists / stretchy vectors,
// returning a `<simple-object-vector>` (the corpus copy-sequence cases only
// check element identity / order, not result class). The bounded
// `copy-sequence(seq, start:, end:)` variant is intentionally NOT added as an
// `<object>` arity-2/3 method: at the call site `start:` / `end:` keywords
// collapse to bare positional values (the keyword name is dropped during
// lowering — see lower.rs `%kw-arg`), so a 2-arg body cannot tell `start: n`
// from `end: n` and would silently compute the wrong slice. Arity-1 is the
// only unambiguous general form, so it is the only one we provide here.

define function copy-sequence (c) => (result)
  map(nod-identity, c)
end function;

// ─── fill! — set every element to `value` (arity-2) ─────────────────────────
//
// DRM `fill!(seq, value)` stores `value` into every element and returns the
// (mutated) sequence. Walks the backing store by index via
// `%vector-element-setter`, so it mutates `<simple-object-vector>` /
// stretchy-vector receivers in place. The bounded `fill!(seq, value, start:,
// end:)` form is omitted for the same keyword-collapse reason as
// `copy-sequence`: `start:` / `end:` reduce to indistinguishable positional
// args, so a ranged body would mutate the wrong span. Arity-2 (fill the whole
// sequence) is unambiguous and is what the bulk of the corpus reaches for.

define function fill! (s, value) => (s)
  let n = %collection-size(s);
  let i = 0;
  until (i = n)
    %vector-element-setter(value, s, i);
    i := i + 1;
  end;
  s
end function;

// ─── position — index of the first element `=` to `target` (arity-2) ────────
//
// DRM `position(seq, target)` returns the integer key (index, for a sequence)
// of the first element `=` to `target`, or #f when none matches. FIP-driven so
// it covers every concrete sequence class. The keyword variants (`test:`,
// `start:`, `end:`, `skip:`) collapse to extra positional args at the call
// site, raising the arity past 2, so this method is selected only for the
// plain `position(seq, target)` form — the unambiguous one (the keyword forms
// live in testworks-gated files and aren't reached yet).

define function position (c, target) => (key)
  let state = %fip-init(c);
  let i = 0;
  let key = #f;
  until (%fip-finished?(state) | (key ~= #f))
    if (%fip-current-element(state) = target)
      key := i;
    else
      i := i + 1;
      %fip-advance!(state);
    end;
  end;
  key
end function;

// ─── count — number of elements satisfying `pred` (arity-2) ─────────────────
//
// DRM `count(predicate, sequence)` counts the elements for which `predicate`
// returns true. `pred` is a first-class `<function>` value, invoked via
// `%funcall1` (the `map` / `do` trampoline). FIP-driven, total, returns an
// `<integer>`. (The bit-vector `count(vector, bit-value:)` form is a different,
// `<bit-vector>`-class operation living in a library-gated suite; it never
// reaches this `<object>` method.)

define function count (pred, c) => (n)
  let state = %fip-init(c);
  let n = 0;
  until (%fip-finished?(state))
    if (%funcall1(pred, %fip-current-element(state)))
      n := n + 1;
    else
      #f
    end;
    %fip-advance!(state);
  end;
  n
end function;

// ─── find-element — first element satisfying `pred` (arity-2) ───────────────
//
// DRM `find-element(collection, predicate)` returns the first element for which
// `predicate` is true, or #f (its `failure:` default) when none matches.
// Companion to the existing `find-key` (which returns the index); this returns
// the element itself. FIP-driven, so it spans every sequence shape.

define function find-element (c, pred) => (e)
  let state = %fip-init(c);
  let found = #f;
  let result = #f;
  until (%fip-finished?(state) | found)
    let x = %fip-current-element(state);
    if (%funcall1(pred, x))
      found := #t;
      result := x;
    else
      %fip-advance!(state);
    end;
  end;
  result
end function;

// ─── remove-duplicates — drop later `=`-duplicates, keep first occurrence ────
//
// DRM `remove-duplicates(seq)` returns a fresh sequence with duplicate elements
// (under `=`) removed, preserving the first occurrence and original order.
// Built by scanning a vector copy and collecting, for each element, only those
// not already `=` to an earlier kept element. O(n²) — fine for the small corpus
// inputs. Returns a `<simple-object-vector>`; the `test:` keyword variant
// collapses to a trailing positional arg this single-method generic absorbs,
// and the default `=` is used.

define function nod-seen-before? (v, upto, x) => (yes?)
  let j = 0;
  let hit = #f;
  until ((j = upto) | hit)
    if (%vector-element(v, j) = x)
      hit := #t;
    else
      j := j + 1;
    end;
  end;
  hit
end function;

define function remove-duplicates (c) => (result)
  let v = map(nod-identity, c);
  let n = %collection-size(v);
  let acc = %nil();
  let i = 0;
  until (i = n)
    let x = %vector-element(v, i);
    if (nod-seen-before?(v, i, x))
      #f
    else
      acc := %pair-alloc(x, acc);
    end;
    i := i + 1;
  end;
  reverse(acc)
end function;

// `remove-duplicates!` is the (notionally destructive) sibling; we lack an
// in-place sequence-shrink primitive, so it returns the same fresh result as
// `remove-duplicates` (same observable elements / order). Honest about being
// non-destructive — see report.

define function remove-duplicates! (c) => (result)
  remove-duplicates(c)
end function;

// ─── union / intersection / difference — set ops over `=` (arity-2) ─────────
//
// DRM set-style operations on two sequences, comparing elements with `=` and
// returning a fresh `<list>` (the DRM allows any sequence result; a list is the
// natural shape over the pair primitive, and the corpus cases only check
// membership / size of the result).
//
//   union(s1, s2)        — every element of s1, plus the s2 elements not in s1.
//   intersection(s1, s2) — the s1 elements that also appear in s2.
//   difference(s1, s2)   — the s1 elements that do NOT appear in s2.
//
// All compose over the existing `member?` (linear `=` search) + the list
// primitives, so no new runtime support is needed. `member?` here is the
// boolean form already defined above. Duplicates within an input are preserved
// in the s1-derived portions (DRM leaves de-duplication to `remove-duplicates`);
// `union` de-dups only across the boundary (skips s2 elements already present
// in s1), matching the common "merge two sets" usage.

define function intersection (s1, s2) => (result)
  let state = %fip-init(s1);
  let acc = %nil();
  until (%fip-finished?(state))
    let x = %fip-current-element(state);
    if (member?(x, s2))
      acc := %pair-alloc(x, acc);
    else
      #f
    end;
    %fip-advance!(state);
  end;
  reverse(acc)
end function;

define function difference (s1, s2) => (result)
  let state = %fip-init(s1);
  let acc = %nil();
  until (%fip-finished?(state))
    let x = %fip-current-element(state);
    if (member?(x, s2))
      #f
    else
      acc := %pair-alloc(x, acc);
    end;
    %fip-advance!(state);
  end;
  reverse(acc)
end function;

define function union (s1, s2) => (result)
  let acc = %nil();
  // Collect all of s1 (reversed), then append the s2 elements not in s1.
  let st1 = %fip-init(s1);
  until (%fip-finished?(st1))
    acc := %pair-alloc(%fip-current-element(st1), acc);
    %fip-advance!(st1);
  end;
  let st2 = %fip-init(s2);
  until (%fip-finished?(st2))
    let x = %fip-current-element(st2);
    if (member?(x, s1))
      #f
    else
      acc := %pair-alloc(x, acc);
    end;
    %fip-advance!(st2);
  end;
  reverse(acc)
end function;
