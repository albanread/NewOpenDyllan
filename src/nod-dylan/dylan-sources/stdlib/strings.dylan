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

