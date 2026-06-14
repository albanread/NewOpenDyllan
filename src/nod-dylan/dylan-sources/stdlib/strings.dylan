Module: dylan
Author: NewOpenDylan stdlib

// ─── Sprint 42a: <byte-string> methods ────────────────────────────────────
//
// `<byte-string>` is a byte-payload heap object whose layout + GC
// traversal lives in `nod-runtime/src/strings.rs` and whose minimum
// primitive surface (allocate, size, byte-get, byte-set, bulk-copy)
// lives in the same module behind the `%byte-string-*` primitives. All
// user-visible operations below are PURE Dylan over those primitives —
// no Rust shims per-op, per the Sprint 42a architecture rule.
//
// Byte values are `<integer>`s in the 0..255 range. We don't have a
// `<character>` literal surface yet (Sprint 42b), so case-mapping
// methods use the integer code-point arithmetic ('a' = 97, 'A' = 65).
// Sprint 42b will lift these to full Unicode case folding via
// `<unicode-string>` (UTF-16).

define method size (s :: <byte-string>) => (n :: <integer>)
  %byte-string-size(s)
end method;

// NB: `empty?` cannot be specialised on `<byte-string>` until the
// Sprint 16 lower-time list-builtin shortcut in
// `nod-sema/src/lower.rs::ListBuiltin::from_name` is retired.
// Today every `empty?(x)` call lowers directly to `%empty?` →
// `nod_empty_p` (the list-specific shim) without going through
// generic dispatch, so any method defined here would be unreachable.
// User code on byte-strings should write `size(s) = 0` for now.
// Tracked: DEFERRED.md — Sprint 16 list-builtins → stdlib generics.

define method element (s :: <byte-string>, i) => (byte :: <integer>)
  %byte-string-element(s, i)
end method;

define method element-setter (v, s :: <byte-string>, i) => (v)
  %byte-string-element-setter(v, s, i)
end method;

// ── concatenate(s1, s2) → fresh <byte-string> ──────────────────────────────
//
// Allocate `size(s1) + size(s2)` bytes and two bulk copies fill them.
// The result is freshly heap-allocated; `s1` and `s2` are unmodified.

define method concatenate (s1 :: <byte-string>, s2 :: <byte-string>) => (s :: <byte-string>)
  let n1 = %byte-string-size(s1);
  let n2 = %byte-string-size(s2);
  let out = %byte-string-allocate(n1 + n2);
  %byte-string-copy!(out, 0,  s1, 0, n1);
  %byte-string-copy!(out, n1, s2, 0, n2);
  out
end method;

// ── copy-sequence(s, start, stop) → fresh <byte-string> ────────────────────
//
// Sprint 42a — no `#key` keyword-arg support at user call sites yet,
// so the bounds-only form takes positional `(s, start, stop)`. A
// `copy-sequence(s)` shorthand (full copy) is provided as a separate
// method on arity 1. Clamping: negative starts clamp to 0; stops past
// size clamp to size; an inverted range yields the empty string.

define method copy-sequence (s :: <byte-string>) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let out = %byte-string-allocate(n);
  %byte-string-copy!(out, 0, s, 0, n);
  out
end method;

define method copy-sequence (s :: <byte-string>, bstart, bstop) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let lo = if (bstart < 0) 0 elseif (bstart > n) n else bstart end;
  let hi = if (bstop < lo) lo elseif (bstop > n) n else bstop end;
  let len = hi - lo;
  let out = %byte-string-allocate(len);
  %byte-string-copy!(out, 0, s, lo, len);
  out
end method;

// `subsequence` is a near-duplicate of `copy-sequence` in shape — they
// differ in DRM only by which is the "preferred" name for general
// sequences. Both produce a fresh byte-string here.

define method subsequence (s :: <byte-string>) => (out :: <byte-string>)
  copy-sequence(s)
end method;

define method subsequence (s :: <byte-string>, bstart, bstop) => (out :: <byte-string>)
  copy-sequence(s, bstart, bstop)
end method;

// ── starts-with?(s, prefix), ends-with?(s, suffix) → <boolean> ─────────────
//
// Byte-by-byte prefix / suffix check. Empty prefix / suffix always
// matches; a prefix / suffix longer than the string never matches.

define function nod-byte-string-substr-eq?
    (s :: <byte-string>, s-off :: <integer>, t :: <byte-string>, n :: <integer>)
 => (eq :: <boolean>)
  let i = 0;
  let mismatch = #f;
  until (i = n | mismatch)
    if (%byte-string-element(s, s-off + i) ~= %byte-string-element(t, i))
      mismatch := #t;
    else
      #f
    end;
    i := i + 1;
  end;
  ~ mismatch
end function;

define method starts-with? (s :: <byte-string>, prefix :: <byte-string>) => (b :: <boolean>)
  let np = %byte-string-size(prefix);
  let ns = %byte-string-size(s);
  if (np > ns)
    #f
  else
    nod-byte-string-substr-eq?(s, 0, prefix, np)
  end
end method;

define method ends-with? (s :: <byte-string>, suffix :: <byte-string>) => (b :: <boolean>)
  let nu = %byte-string-size(suffix);
  let ns = %byte-string-size(s);
  if (nu > ns)
    #f
  else
    nod-byte-string-substr-eq?(s, ns - nu, suffix, nu)
  end
end method;

// ── find-substring(haystack, needle) → false-or(<integer>) ─────────────────
//
// Returns the byte index of the FIRST occurrence of `needle` in
// `haystack`, or `#f` if not found. Naive scan; good enough for an IDE
// "find next" feature on file-sized buffers.
//
// Edge cases: an empty needle matches at index 0; a needle longer than
// the haystack returns #f. (DRM defines `find-substring` similarly to
// CL's `search` but byte-indexed for byte-strings.)

define method find-substring (haystack :: <byte-string>, needle :: <byte-string>)
 => (idx)
  find-substring(haystack, needle, 0)
end method;

define method find-substring (haystack :: <byte-string>, needle :: <byte-string>, fstart :: <integer>)
 => (idx)
  let nh = %byte-string-size(haystack);
  let nn = %byte-string-size(needle);
  if (nn = 0)
    if (fstart < 0) 0 elseif (fstart > nh) nh else fstart end
  elseif (nn > nh)
    #f
  else
    let lo = if (fstart < 0) 0 else fstart end;
    let last = nh - nn;
    let i = lo;
    let hit = #f;
    until (i > last | hit)
      if (nod-byte-string-substr-eq?(haystack, i, needle, nn))
        hit := i;
      else
        i := i + 1;
      end;
    end;
    if (hit) hit else #f end
  end
end method;

// ── as-uppercase(s), as-lowercase(s) → fresh <byte-string> ─────────────────
//
// Sprint 42a — ASCII-only case mapping. Bytes in 'a'..'z' (97..122)
// shift to 'A'..'Z' (65..90); other bytes unchanged. Sprint 42b lifts
// to full Unicode case mapping via `<unicode-string>` (UTF-16).

define method as-uppercase (s :: <byte-string>) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let out = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    let b = %byte-string-element(s, i);
    let upper = if ((b >= 97) & (b <= 122))  // 'a'..'z'
                  b - 32                  // shift to 'A'..'Z' (65..90)
                else
                  b
                end;
    %byte-string-element-setter(upper, out, i);
    i := i + 1;
  end;
  out
end method;

define method as-lowercase (s :: <byte-string>) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let out = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    let b = %byte-string-element(s, i);
    let lower = if ((b >= 65) & (b <= 90))   // 'A'..'Z'
                  b + 32                  // shift to 'a'..'z' (97..122)
                else
                  b
                end;
    %byte-string-element-setter(lower, out, i);
    i := i + 1;
  end;
  out
end method;

// ─── Sprint 45c: ASCII byte predicates ────────────────────────────────────
//
// Shared by the Dylan-side lexer (Sprint 45b) and any future Dylan code
// that wants to classify bytes (IDE syntax colourer, hand-rolled parsers
// for config / `.prj` / log formats, etc). Each takes a byte (an
// <integer> in 0..255) and returns a <boolean>. There is no
// <character> class yet (Sprint 42b is queued for that) — when it
// lands these will gain `<character>` overloads alongside.
//
// Naming follows the Dylan tradition: predicate-suffix `?`, ASCII
// qualifier in the head so the same names can later carry full-Unicode
// twins on `<character>` without renaming churn.

define function ascii-digit? (b :: <integer>) => (yes? :: <boolean>)
  (b >= 48) & (b <= 57)           // '0'..'9'
end function;

define function ascii-hex-digit? (b :: <integer>) => (yes? :: <boolean>)
  ((b >= 48) & (b <= 57))         // '0'..'9'
    | ((b >= 65) & (b <= 70))     // 'A'..'F'
    | ((b >= 97) & (b <= 102))    // 'a'..'f'
end function;

define function ascii-bin-digit? (b :: <integer>) => (yes? :: <boolean>)
  (b = 48) | (b = 49)             // '0' | '1'
end function;

define function ascii-oct-digit? (b :: <integer>) => (yes? :: <boolean>)
  (b >= 48) & (b <= 55)           // '0'..'7'
end function;

define function ascii-uppercase? (b :: <integer>) => (yes? :: <boolean>)
  (b >= 65) & (b <= 90)           // 'A'..'Z'
end function;

define function ascii-lowercase? (b :: <integer>) => (yes? :: <boolean>)
  (b >= 97) & (b <= 122)          // 'a'..'z'
end function;

define function ascii-alpha? (b :: <integer>) => (yes? :: <boolean>)
  ((b >= 65) & (b <= 90)) | ((b >= 97) & (b <= 122))
end function;

define function ascii-alphanumeric? (b :: <integer>) => (yes? :: <boolean>)
  ((b >= 48) & (b <= 57)) | ((b >= 65) & (b <= 90)) | ((b >= 97) & (b <= 122))
end function;

// Standard ASCII whitespace: space, tab, LF, CR, FF. Matches the C
// `isspace` set minus VT (which Dylan source never uses meaningfully).
define function ascii-whitespace? (b :: <integer>) => (yes? :: <boolean>)
  (b = 32) | (b = 9) | (b = 10) | (b = 13) | (b = 12)
end function;

// ─── DRM byte-code predicate aliases ──────────────────────────────────────
//
// The DRM/common-dylan names (`whitespace?`, `alphabetic?`, `digit?`, …) are
// defined on `<character>`. We don't yet have a primitive to extract a
// fixnum code-point from a `<character>` value (a char literal lowers to a
// raw i32, not a tagged fixnum — see report, "blocked on missing primitive
// char->code"), so the `<character>` overloads can't be written in pure
// Dylan today. What we CAN offer is the same predicate spelled over an
// <integer> byte code, which is exactly what byte-string element reads give
// you (`element(s, i)` → a 0..255 fixnum). These thin aliases delegate to
// the `ascii-*` workhorses above so hand-rolled scanners over byte-strings
// can use the familiar DRM names. (`define function` ⇒ single `<object>`
// method, so a bare `whitespace?` is also a usable first-class value for
// `choose`/`any?`/`find-key`.)

define function whitespace? (b) => (yes?)
  ascii-whitespace?(b)
end function;

define function alphabetic? (b) => (yes?)
  ascii-alpha?(b)
end function;

define function digit? (b) => (yes?)
  ascii-digit?(b)
end function;

define function alphanumeric? (b) => (yes?)
  ascii-alphanumeric?(b)
end function;

define function uppercase? (b) => (yes?)
  ascii-uppercase?(b)
end function;

define function lowercase? (b) => (yes?)
  ascii-lowercase?(b)
end function;

// ─── as-uppercase! / as-lowercase! — in-place ASCII case mapping ───────────
//
// The destructive siblings of `as-uppercase` / `as-lowercase`: they mutate
// `s` in place (overwriting each byte) and return it, rather than allocating
// a fresh copy. Useful when the caller owns a scratch buffer it built with
// `%byte-string-allocate` and wants to normalise case without a second
// allocation. ASCII-only, identical mapping to the non-`!` forms.

define method as-uppercase! (s :: <byte-string>) => (s :: <byte-string>)
  let n = %byte-string-size(s);
  let i = 0;
  until (i = n)
    let b = %byte-string-element(s, i);
    if ((b >= 97) & (b <= 122))           // 'a'..'z'
      %byte-string-element-setter(b - 32, s, i);
    end;
    i := i + 1;
  end;
  s
end method;

define method as-lowercase! (s :: <byte-string>) => (s :: <byte-string>)
  let n = %byte-string-size(s);
  let i = 0;
  until (i = n)
    let b = %byte-string-element(s, i);
    if ((b >= 65) & (b <= 90))            // 'A'..'Z'
      %byte-string-element-setter(b + 32, s, i);
    end;
    i := i + 1;
  end;
  s
end method;

// ─── string-equal? — case-INSENSITIVE byte-string equality ────────────────
//
// DRM `string-equal?` compares two strings ignoring ASCII case (and is the
// natural spelling the corpus reaches for, `string-equal?(a, b)`). Bytes are
// folded to lowercase before comparison so "Hello" string-equal? "HELLO".
// Differing lengths are never equal. Case-SENSITIVE equality is just `=` on
// the byte runs (`string-equal-sensitive?` below) — provided for symmetry.

define function nod-fold-lower (b :: <integer>) => (lb :: <integer>)
  if ((b >= 65) & (b <= 90)) b + 32 else b end
end function;

define method string-equal? (s1 :: <byte-string>, s2 :: <byte-string>) => (eq :: <boolean>)
  let n1 = %byte-string-size(s1);
  let n2 = %byte-string-size(s2);
  if (n1 ~= n2)
    #f
  else
    let i = 0;
    let mismatch = #f;
    until ((i = n1) | mismatch)
      if (nod-fold-lower(%byte-string-element(s1, i))
            ~= nod-fold-lower(%byte-string-element(s2, i)))
        mismatch := #t;
      else
        i := i + 1;
      end;
    end;
    ~ mismatch
  end
end method;

// Case-SENSITIVE byte-for-byte equality (the `\=` test, named so it can be
// passed as a first-class value to `member?`/`find-key`/…).
define method string-equal-sensitive? (s1 :: <byte-string>, s2 :: <byte-string>) => (eq :: <boolean>)
  let n1 = %byte-string-size(s1);
  let n2 = %byte-string-size(s2);
  if (n1 ~= n2)
    #f
  else
    nod-byte-string-substr-eq?(s1, 0, s2, n1)
  end
end method;

// ─── trim / trim-left / trim-right — strip leading/trailing whitespace ─────
//
// Returns a fresh byte-string with ASCII whitespace removed from the
// front (`trim-left`), the back (`trim-right`), or both ends (`trim`). The
// interior is untouched. An all-whitespace (or empty) string trims to "".
// Built on `copy-sequence` so the result is a fresh, independent allocation.

define method trim-left (s :: <byte-string>) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let lo = 0;
  until ((lo = n) | (~ ascii-whitespace?(%byte-string-element(s, lo))))
    lo := lo + 1;
  end;
  copy-sequence(s, lo, n)
end method;

define method trim-right (s :: <byte-string>) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let hi = n;
  until ((hi = 0) | (~ ascii-whitespace?(%byte-string-element(s, hi - 1))))
    hi := hi - 1;
  end;
  copy-sequence(s, 0, hi)
end method;

define method trim (s :: <byte-string>) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let lo = 0;
  until ((lo = n) | (~ ascii-whitespace?(%byte-string-element(s, lo))))
    lo := lo + 1;
  end;
  let hi = n;
  until ((hi = lo) | (~ ascii-whitespace?(%byte-string-element(s, hi - 1))))
    hi := hi - 1;
  end;
  copy-sequence(s, lo, hi)
end method;

// ─── string-position — index of a single byte (char-code) in a string ─────
//
// Returns the index of the first byte `=` to code `c` at or after `bstart`,
// or #f. The single-char companion to `find-substring`. `bstart` defaults
// to 0 via the 2-arg method. (DRM `position` is the general-sequence form;
// this is the byte-string fast path the corpus uses for "find this char".)

define method string-position (s :: <byte-string>, c :: <integer>) => (idx)
  string-position(s, c, 0)
end method;

define method string-position (s :: <byte-string>, c :: <integer>, bstart :: <integer>) => (idx)
  let n = %byte-string-size(s);
  let i = if (bstart < 0) 0 else bstart end;
  let hit = #f;
  until ((i >= n) | (hit ~= #f))
    if (%byte-string-element(s, i) = c)
      hit := i;
    else
      i := i + 1;
    end;
  end;
  hit
end method;

// ─── count-substrings — number of NON-overlapping occurrences ─────────────
//
// Counts how many times `needle` occurs in `haystack` without overlap (each
// match advances the scan past the whole needle). An empty needle returns 0
// (avoids an infinite "match at every position" reading). Built on the
// existing `find-substring(haystack, needle, start)` scan.

define method count-substrings (haystack :: <byte-string>, needle :: <byte-string>) => (n :: <integer>)
  let nn = %byte-string-size(needle);
  if (nn = 0)
    0
  else
    let count = 0;
    let pos = 0;
    let nh = %byte-string-size(haystack);
    let done = #f;
    until (done)
      let hit = find-substring(haystack, needle, pos);
      if (hit = #f)
        done := #t;
      else
        count := count + 1;
        pos := hit + nn;
        if (pos > nh) done := #t end;
      end;
    end;
    count
  end
end method;

// ─── replace-substrings — replace every occurrence of `old` with `new` ────
//
// Returns a fresh byte-string in which each non-overlapping occurrence of
// `old` in `s` is replaced by `new`. `old` and `new` may differ in length.
// An empty `old` returns `s` unchanged (no sensible insertion point). The
// result is sized exactly (count * (|new| - |old|) delta) and filled with
// two cursors via `%byte-string-copy!`.

define method replace-substrings (s :: <byte-string>, old :: <byte-string>, new :: <byte-string>)
 => (out :: <byte-string>)
  let no = %byte-string-size(old);
  if (no = 0)
    copy-sequence(s)
  else
    let ns = %byte-string-size(s);
    let nn = %byte-string-size(new);
    let hits = count-substrings(s, old);
    let out-len = ns + (hits * (nn - no));
    let out = %byte-string-allocate(out-len);
    let src = 0;        // read cursor into `s`
    let dst = 0;        // write cursor into `out`
    let done = #f;
    until (done)
      let hit = find-substring(s, old, src);
      if (hit = #f)
        // copy the remaining tail verbatim
        let rest = ns - src;
        %byte-string-copy!(out, dst, s, src, rest);
        dst := dst + rest;
        done := #t;
      else
        // copy the gap [src, hit) then the replacement
        let gap = hit - src;
        %byte-string-copy!(out, dst, s, src, gap);
        dst := dst + gap;
        %byte-string-copy!(out, dst, new, 0, nn);
        dst := dst + nn;
        src := hit + no;
      end;
    end;
    out
  end
end method;

// ─── repeat-string — concatenate `s` `n` times ─────────────────────────────
//
// Returns a fresh byte-string that is `s` repeated `n` times (n <= 0 → ""),
// e.g. `repeat-string("ab", 3)` ⇒ "ababab". Sized exactly and filled with
// `n` bulk copies — useful for padding / rule lines in CLI output.

define method repeat-string (s :: <byte-string>, n :: <integer>) => (out :: <byte-string>)
  let len = %byte-string-size(s);
  let times = if (n < 0) 0 else n end;
  let out = %byte-string-allocate(len * times);
  let i = 0;
  until (i = times)
    %byte-string-copy!(out, i * len, s, 0, len);
    i := i + 1;
  end;
  out
end method;

// ─── string-reverse — shape-preserving reverse of a byte-string ───────────
//
// The generic `reverse` (sequences.dylan) always returns a
// `<simple-object-vector>`; for a byte-string that loses the string shape.
// This returns a fresh REVERSED `<byte-string>` (so it's still printable
// with `%s` and concatenable), e.g. `string-reverse("abc")` ⇒ "cba".

define method string-reverse (s :: <byte-string>) => (out :: <byte-string>)
  let n = %byte-string-size(s);
  let out = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    %byte-string-element-setter(%byte-string-element(s, n - 1 - i), out, i);
    i := i + 1;
  end;
  out
end method;

// ─── string-to-integer — parse a signed decimal byte-string ───────────────
//
// Parses an optional leading '+'/'-' then a run of ASCII digits into an
// <integer>. Leading ASCII whitespace is skipped; parsing stops at the
// first non-digit (so "42px" → 42, "  -7" → -7). A string with no digits
// (e.g. "" or "abc") yields 0. This is the common decimal-only form; the
// DRM `base:` / `default:` keywords lower to trailing positional args the
// single-method generic absorbs (decimal/0 defaults are used) — see report.

define method string-to-integer (s :: <byte-string>) => (n :: <integer>)
  let len = %byte-string-size(s);
  let i = 0;
  // skip leading whitespace
  until ((i = len) | (~ ascii-whitespace?(%byte-string-element(s, i))))
    i := i + 1;
  end;
  // optional sign
  let neg = #f;
  if (i < len)
    let c = %byte-string-element(s, i);
    if (c = 45)        // '-'
      neg := #t; i := i + 1;
    elseif (c = 43)    // '+'
      i := i + 1;
    end;
  end;
  // digit run
  let acc = 0;
  let scanning = #t;
  until ((i = len) | (~ scanning))
    let c = %byte-string-element(s, i);
    if ((c >= 48) & (c <= 57))      // '0'..'9'
      acc := (acc * 10) + (c - 48);
      i := i + 1;
    else
      scanning := #f;
    end;
  end;
  if (neg) - acc else acc end
end method;

// ─── split — break a string into a LIST of substrings on a separator ──────
//
// `split(string, separator)` returns a `<list>` of the substrings of
// `string` delimited by `separator`. `separator` may be a single byte code
// (an <integer>) or a multi-byte `<byte-string>`. Consecutive separators
// produce empty-string segments and trailing/leading separators produce a
// leading/trailing "" (matching common-dylan's split: `split("a/", '/')` ⇒
// ("a", ""), `split("/", '/')` ⇒ ("", "")). An empty string splits to one
// empty segment ("").
//
// A LIST is returned (not a vector) so the result is walkable with
// `head`/`tail`/`size` and `empty?` in user code — there is no public
// `<simple-object-vector>` element accessor yet (see report). The DRM
// `split` also returns the count as a 2nd value and accepts
// `start:`/`end:`/`count:` keywords; those keyword args lower to trailing
// positionals the single-method generic absorbs (defaults used).
//
// Implementation: walk left→right collecting segment start indices, cons
// each `copy-sequence` segment onto an accumulator, then reverse the
// accumulator into list order via a second cons-walk.

// Reverse a <list> back into a fresh <list> (sequences.dylan's `reverse`
// yields a vector; split needs a list result so callers can walk it).
define function nod-list-reverse (l) => (r)
  let acc = %nil();
  let p = l;
  until (p = %nil())
    acc := %pair-alloc(%pair-head(p), acc);
    p := %pair-tail(p);
  end;
  acc
end function;

define method split (s :: <byte-string>, sep :: <integer>) => (parts)
  // single-byte separator
  let n = %byte-string-size(s);
  let acc = %nil();
  let seg-start = 0;
  let i = 0;
  until (i > n)
    if ((i = n) | (%byte-string-element(s, i) = sep))
      acc := %pair-alloc(copy-sequence(s, seg-start, i), acc);
      seg-start := i + 1;
    end;
    i := i + 1;
  end;
  nod-list-reverse(acc)
end method;

define method split (s :: <byte-string>, sep :: <byte-string>) => (parts)
  // multi-byte (string) separator. An empty separator yields the whole
  // string as a single segment (no split point).
  let nsep = %byte-string-size(sep);
  if (nsep = 0)
    %pair-alloc(copy-sequence(s), %nil())
  else
    let n = %byte-string-size(s);
    let acc = %nil();
    let seg-start = 0;
    let i = 0;
    let done = #f;
    until (done)
      let hit = find-substring(s, sep, i);
      if (hit = #f)
        acc := %pair-alloc(copy-sequence(s, seg-start, n), acc);
        done := #t;
      else
        acc := %pair-alloc(copy-sequence(s, seg-start, hit), acc);
        seg-start := hit + nsep;
        i := seg-start;
      end;
    end;
    nod-list-reverse(acc)
  end
end method;

// ─── join — concatenate a sequence of strings with a separator ────────────
//
// `join(strings, separator)` returns a single `<byte-string>` formed by
// concatenating the elements of `strings` (a list / vector / any FIP
// collection of byte-strings) with `separator` (a `<byte-string>`) between
// adjacent elements — the inverse of `split`. An empty input yields "", a
// singleton yields that element verbatim (no separator). The separator is
// NOT added before the first or after the last element.
//
// FIP-driven so it accepts any collection shape (the `split` result list,
// a `#[...]` vector, …). Uses `concatenate` to build the result; O(n²) in
// total length for very long inputs, which is fine for the corpus sizes.

define method join (strings, separator :: <byte-string>) => (out :: <byte-string>)
  let state = %fip-init(strings);
  if (%fip-finished?(state))
    ""
  else
    let acc = %fip-current-element(state);
    %fip-advance!(state);
    until (%fip-finished?(state))
      acc := concatenate(acc, separator);
      acc := concatenate(acc, %fip-current-element(state));
      %fip-advance!(state);
    end;
    acc
  end
end method;

