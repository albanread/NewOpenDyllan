Module: dylan
Author: NewOpenDylan stdlib

// ─── Numeric operations, pure Dylan over integer arithmetic + word bitwise ──
//
// Every entry here is `define function`, so the stdlib loader rewrites each to
// a single-method generic on `<object>` (see `nod-sema/src/stdlib.rs`). That
// makes the names reachable from user code BOTH as ordinary calls (lowering to
// a `Dispatch` node) and as first-class function references (`\gcd`, `\abs`,
// passed to `map`/`reduce`/…).
//
// The bodies compose over the existing integer arithmetic (`+` `-` `*` `/`
// `mod` `rem`, the comparison operators) and the five word-level bitwise
// primitives (`%logand` / `%logior` / `%logxor` / `%lognot` / `%ash`). No new
// Rust runtime code.
//
// IMPORTANT semantics note for this back-end: integer `/` is truncating
// division (toward zero, `sdiv`) and BOTH `mod` and `rem` lower to the signed
// remainder (`srem`, sign-of-dividend). So `mod` here is NOT the DRM floor
// modulo — it equals `rem`. The functions below that need true floor / ceiling
// / round behaviour (`modulo`, `floor`, `floor/`, `ceiling`, `ceiling/`,
// `round`, `round/`) therefore compute the floor / ceiling / round correction
// EXPLICITLY rather than leaning on the `mod` operator. The truncate family
// (`truncate`, `truncate/`, `remainder`, `quotient`) maps straight onto `/`
// and `rem`.
//
// Predicates `even?` / `odd?` / `zero?` / `positive?` / `negative?` and the
// binary `min` / `max` already live in sequences.dylan — they are NOT
// redefined here.

// ─── abs — absolute value ────────────────────────────────────────────────────
//
// DRM `abs(n)`. Negate when negative; otherwise return as-is. Works for any
// integer (floats are not a separate first-class numeric here).

define function abs (n) => (a)
  if (n < 0) 0 - n else n end
end function;

// ─── negative / negated — unary negation as a referenceable function ─────────
//
// `negative` returns `-n`. Handy as a first-class value (`map(negative, v)`)
// where a bareword is needed rather than the `-` operator.

define function negative (n) => (r)
  0 - n
end function;

// ─── quotient — truncating integer quotient ─────────────────────────────────
//
// `quotient(a, b)` is the truncate-toward-zero quotient, i.e. integer `/`.
// Pairs with `remainder` so that `a = quotient(a,b) * b + remainder(a,b)`.

define function quotient (a, b) => (q)
  a / b
end function;

// ─── remainder — truncating remainder (sign of the dividend) ────────────────
//
// DRM `remainder(a, b)` = the remainder of `truncate/`, with the sign of `a`.
// In this back-end that is exactly the `rem` operator.

define function remainder (a, b) => (r)
  a rem b
end function;

// ─── modulo — floor modulo (sign of the divisor) ────────────────────────────
//
// DRM `modulo(a, b)` = the remainder of `floor/`, with the sign of `b`. Since
// `rem` here carries the sign of the dividend, correct it: when the truncated
// remainder is non-zero and its sign differs from `b`'s, add `b`.

define function modulo (a, b) => (m)
  let r = a rem b;
  if (r = 0)
    0
  elseif ((r < 0) = (b < 0))
    r
  else
    r + b
  end
end function;

// ─── truncate/ — quotient + remainder, truncating toward zero ───────────────
//
// DRM `truncate/(a, b) => (quotient, remainder)`. Both values map straight to
// the back-end's `/` and `rem`. Returns two values via the `values` intrinsic.

define function truncate/ (a, b) => (q, r)
  values(a / b, a rem b)
end function;

// ─── truncate — truncate a real toward zero ─────────────────────────────────
//
// DRM `truncate(n) => (integer, remainder)`. For integers the value is itself
// with a zero remainder; provided so `truncate(n)` resolves (the integer case
// is the tractable one — float truncation needs float support, see notes).

define function truncate (n) => (i, r)
  values(n, 0)
end function;

// ─── floor/ — quotient + remainder, rounding the quotient toward -inf ───────
//
// DRM `floor/(a, b) => (quotient, remainder)` with `0 <= |remainder| < |b|`
// and the remainder carrying the sign of `b`. Start from the truncated pair
// and, when the remainder's sign differs from `b`'s, step the quotient down by
// one and pull the remainder back into range.

define function floor/ (a, b) => (q, r)
  let tq = a / b;
  let tr = a rem b;
  if (tr = 0)
    values(tq, 0)
  elseif ((tr < 0) = (b < 0))
    values(tq, tr)
  else
    values(tq - 1, tr + b)
  end
end function;

// ─── floor — largest integer not greater than n ─────────────────────────────
//
// DRM `floor(n)`. Integer-tractable case: an integer is already floored, so it
// is returned with a zero remainder. (Float flooring needs float support.)

define function floor (n) => (i, r)
  values(n, 0)
end function;

// ─── ceiling/ — quotient + remainder, rounding the quotient toward +inf ─────
//
// DRM `ceiling/(a, b) => (quotient, remainder)`. The remainder carries the
// sign OPPOSITE to `b`. From the truncated pair: when the remainder is
// non-zero and its sign matches `b`'s, step the quotient up by one and pull
// the remainder back.

define function ceiling/ (a, b) => (q, r)
  let tq = a / b;
  let tr = a rem b;
  if (tr = 0)
    values(tq, 0)
  elseif ((tr < 0) = (b < 0))
    values(tq + 1, tr - b)
  else
    values(tq, tr)
  end
end function;

// ─── ceiling — smallest integer not less than n ─────────────────────────────
//
// DRM `ceiling(n)`. Integer-tractable case: an integer is already its own
// ceiling, returned with a zero remainder.

define function ceiling (n) => (i, r)
  values(n, 0)
end function;

// ─── round/ — quotient + remainder, rounding the quotient to nearest ────────
//
// DRM `round/(a, b) => (quotient, remainder)`, rounding to nearest with ties
// to even. Compute the floor quotient + remainder, then decide whether to bump
// up to the ceiling quotient based on twice the (non-negative magnitude of the)
// remainder versus |b|, breaking ties toward the even quotient. Built entirely
// over `abs`, `floor/`-style correction and integer comparisons.

define function round/ (a, b) => (q, r)
  let fq = a / b;
  let fr = a rem b;
  // Renormalise to floor semantics first (remainder takes the sign of b).
  let fq = if (fr = 0) fq elseif ((fr < 0) = (b < 0)) fq else fq - 1 end;
  let fr = if (fr = 0) 0 elseif ((fr < 0) = (b < 0)) fr else fr + b end;
  let ab = abs(b);
  let two-r = abs(fr) * 2;
  if (two-r < ab)
    values(fq, fr)
  elseif (two-r > ab)
    values(fq + 1, fr - b)
  elseif (modulo(fq, 2) = 0)
    // Exact half: ties to even — keep the even floor quotient.
    values(fq, fr)
  else
    values(fq + 1, fr - b)
  end
end function;

// ─── round — round a real to the nearest integer ────────────────────────────
//
// DRM `round(n)`. Integer-tractable case: an integer rounds to itself with a
// zero remainder.

define function round (n) => (i, r)
  values(n, 0)
end function;

// ─── gcd — greatest common divisor (Euclid) ──────────────────────────────────
//
// DRM `gcd(a, b)`. Non-negative result; `gcd(0, 0)` is 0. Reduce with the
// truncated remainder until the second operand is zero, taking `abs` of the
// final value so the sign is normalised.

define function gcd (a, b) => (g)
  let x = abs(a);
  let y = abs(b);
  until (y = 0)
    let t = x rem y;
    x := y;
    y := t;
  end;
  x
end function;

// ─── lcm — least common multiple ─────────────────────────────────────────────
//
// DRM `lcm(a, b)`. `lcm(0, _) = lcm(_, 0) = 0`. Otherwise `|a / gcd * b|`,
// dividing before multiplying to keep the intermediate small.

define function lcm (a, b) => (l)
  if ((a = 0) | (b = 0))
    0
  else
    abs((a / gcd(a, b)) * b)
  end
end function;

// ─── expt / power — integer exponentiation by repeated multiply ─────────────
//
// `expt(base, e)` raises `base` to a non-negative integer power `e` via
// exponentiation-by-squaring (O(log e) multiplies). Negative exponents need
// rationals / floats and are not representable here, so `e < 0` returns 0 (see
// notes). `power` is a referenceable alias.

define function expt (base, e) => (result)
  if (e < 0)
    0
  else
    let result = 1;
    let b = base;
    let n = e;
    until (n = 0)
      if (modulo(n, 2) = 1)
        result := result * b;
      end;
      b := b * b;
      n := n / 2;
    end;
    result
  end
end function;

define function power (base, e) => (result)
  expt(base, e)
end function;

// ─── logand / logior / logxor / lognot — user-facing bitwise wrappers ───────
//
// DRM `logand` / `logior` / `logxor` / `lognot` over the back-end's word
// bitwise primitives. The binary forms only (the DRM variadic `#rest` forms
// need rest-arg binding the lowerer doesn't do yet — see notes); `lognot` is
// the one's-complement unary.

define function logand (a, b) => (r)
  %logand(a, b)
end function;

define function logior (a, b) => (r)
  %logior(a, b)
end function;

define function logxor (a, b) => (r)
  %logxor(a, b)
end function;

define function lognot (a) => (r)
  %lognot(a)
end function;

// ─── ash — arithmetic shift (left for positive, right for negative count) ───
//
// DRM `ash(integer, count)`: shift `integer` left by `count` bits when `count`
// is positive, right (sign-extending) when negative. Delegates to the `%ash`
// primitive, which already implements both directions.

define function ash (n, count) => (r)
  %ash(n, count)
end function;

// ─── logbit? — test bit `index` of `n` ───────────────────────────────────────
//
// DRM `logbit?(index, integer)` => `#t` iff bit `index` (0 = least
// significant) of `integer` is set. Shift the bit down to position 0 with a
// right `ash` and mask. Negative integers read as two's-complement (the `%ash`
// right shift is sign-extending), matching the DRM's infinite-precision
// two's-complement model for the in-range bits the corpus tests.

define function logbit? (index, n) => (set?)
  %logand(%ash(n, 0 - index), 1) = 1
end function;

// ─── logand? — non-zero-intersection test ────────────────────────────────────
//
// Convenience predicate: `#t` iff `a` and `b` share at least one set bit. Not
// in the DRM by this name, but a common idiom (`logand?(flags, $mask)`) and
// cheap over `%logand`.

define function logand? (a, b) => (yes?)
  %logand(a, b) ~= 0
end function;

// ─── integer-length — bit length of the magnitude ────────────────────────────
//
// DRM `integer-length(n)`: the number of bits needed to represent `n` in
// two's-complement EXCLUDING the sign bit. For non-negative `n` this is the
// position of the highest set bit + 1 (and 0 for `n = 0`); for negative `n`
// the DRM defines it as `integer-length(-1 - n)` (the bit length of the
// one's-complement), so a value like -1 has length 0. Computed by shifting the
// normalised magnitude right until it reaches zero.

define function integer-length (n) => (len)
  let m = if (n < 0) %lognot(n) else n end;
  let len = 0;
  until (m = 0)
    m := %ash(m, -1);
    len := len + 1;
  end;
  len
end function;

// Transcendental constants (common-dylan `transcendentals` module). Plain
// float literals; single/double distinction is not yet tracked in the type
// lattice, so both precisions share the value.
define constant $single-pi :: <float> = 3.141592653589793;
define constant $double-pi :: <float> = 3.141592653589793;
define constant $single-e  :: <float> = 2.718281828459045;
define constant $double-e  :: <float> = 2.718281828459045;
