Module: dylan
Precedence: c
Author: NewOpenDylan stdlib (Sprint 20b)

// ╔══════════════════════════════════════════════════════════════════════════╗
// ║ STDLIB + MACRO BOUNDARY POLICY                                           ║
// ║                                                                          ║
// ║ This file is the DEFAULT home for new stdlib API AND for new control-    ║
// ║ flow surface forms (case, cond, select, while, for, with-*, when, etc.). ║
// ║                                                                          ║
// ║   • docs/STDLIB_BOUNDARY.md — new API belongs in Dylan, not Rust.        ║
// ║     Pre-flight: write the Dylan version first.                           ║
// ║                                                                          ║
// ║   • docs/MACRO_BOUNDARY.md — new control-flow shapes belong here as      ║
// ║     `define macro`, not as hardcoded Expr::* / Statement::* variants     ║
// ║     in nod-reader::ast. The frozen kernel list (If, Begin, Let, Method,  ║
// ║     definitional items, Block-with-cleanup) is the only legitimate set. ║
// ║                                                                          ║
// ║   • docs/UPSTREAM_OPENDYLAN.md — when porting code from Open Dylan       ║
// ║     rather than writing fresh, preserve attribution per that workflow.   ║
// ╚══════════════════════════════════════════════════════════════════════════╝

// ── stdlib.dylan — collection ops, FIP wrappers, for-each macro ───────────
//
// Sprint 20b: this file is auto-loaded by `nod_sema::stdlib::ensure_loaded()`
// before user code lowers. Every `define function` here is rewritten to
// `define method <name> (params... :: <object>)` by the loader so that
// user code's call-site `Dispatch` IR resolves through the process-global
// dispatch table (cross-module symbol linkage is deferred — see
// DEFERRED.md → Sprint 20b residue).
//
// What lives here:
//   * `size(c)` — collection size, via `%collection-size`.
//   * `concatenate(c1, c2)` — binary concat, via `%collection-concatenate`.
//   * `for-each` macro — sugar over the FIP primitives.
//   * `nod-stdlib-marker` — sentinel used by tests to confirm the loader
//     parsed + JIT'd the stdlib.
//
// Deferred (DEFERRED.md → "Sprint 20b residue — full collections in
// Dylan"):
//   * `reduce`, `map`, `do` as Dylan functions accepting a first-class
//     function argument. Requires the function-Word ABI (Sprint 21
//     sub-goal) to thread function references through the JIT call shape.
//     The Rust-side `collection_reduce`/`collection_map`/`collection_do`
//     stay in `nod_runtime::collections` until then; the FIP primitives
//     wired in this sprint make the migration mechanical when first-
//     class functions land.
//   * Slot-accessor-based FIP (`define sealed method
//     forward-iteration-protocol …` per concrete class). Requires the
//     `%`-prefix lexer carve-out for `<iteration-state>`'s slot names
//     and slot-accessor generation for pre-registered Rust classes. Both
//     are Sprint 21 follow-ups; the Rust-side `forward_iteration_protocol`
//     + the `%fip-*` primitives here cover the protocol surface in the
//     meantime.

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
    let upper = if (b >= 97 & b <= 122)  // 'a'..'z'
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
    let lower = if (b >= 65 & b <= 90)   // 'A'..'Z'
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
  b >= 48 & b <= 57           // '0'..'9'
end function;

define function ascii-hex-digit? (b :: <integer>) => (yes? :: <boolean>)
  (b >= 48 & b <= 57)         // '0'..'9'
    | (b >= 65 & b <= 70)     // 'A'..'F'
    | (b >= 97 & b <= 102)    // 'a'..'f'
end function;

define function ascii-bin-digit? (b :: <integer>) => (yes? :: <boolean>)
  b = 48 | b = 49             // '0' | '1'
end function;

define function ascii-oct-digit? (b :: <integer>) => (yes? :: <boolean>)
  b >= 48 & b <= 55           // '0'..'7'
end function;

define function ascii-uppercase? (b :: <integer>) => (yes? :: <boolean>)
  b >= 65 & b <= 90           // 'A'..'Z'
end function;

define function ascii-lowercase? (b :: <integer>) => (yes? :: <boolean>)
  b >= 97 & b <= 122          // 'a'..'z'
end function;

define function ascii-alpha? (b :: <integer>) => (yes? :: <boolean>)
  (b >= 65 & b <= 90) | (b >= 97 & b <= 122)
end function;

define function ascii-alphanumeric? (b :: <integer>) => (yes? :: <boolean>)
  (b >= 48 & b <= 57) | (b >= 65 & b <= 90) | (b >= 97 & b <= 122)
end function;

// Standard ASCII whitespace: space, tab, LF, CR, FF. Matches the C
// `isspace` set minus VT (which Dylan source never uses meaningfully).
define function ascii-whitespace? (b :: <integer>) => (yes? :: <boolean>)
  b = 32 | b = 9 | b = 10 | b = 13 | b = 12
end function;

// ─── for-each macro ────────────────────────────────────────────────────────
//
// Sugar over the FIP primitives. Expands to a `let state = %fip-init(c);
// until (%fip-finished?(state)) ... %fip-advance!(state) end` loop. The
// `?var:name` binding is rebound on each iteration to the current
// element.
//
// Sprint 25: the body-shaped surface `for-each (x in c) body end` is
// recognised by the parser now (see `Expr::MacroCall` + the
// `known_macros` plumbing in `nod-reader/src/parser.rs`). Sprint 20b
// shipped the macro definition but couldn't call it from a separate
// file because the parser didn't know body-shaped macro syntax.

define macro for-each
  { for-each (?var:name in ?coll:expression) ?body:body end }
    => { begin
           let %fip-state = %fip-init(?coll);
           until (%fip-finished?(%fip-state))
             let ?var = %fip-current-element(%fip-state);
             ?body;
             %fip-advance!(%fip-state)
           end
         end }
end macro;

// ─── unless macro ──────────────────────────────────────────────────────────
//
// Sprint 25: retired the hardcoded `Expr::Unless` AST variant. The
// parser now treats `unless (cond) body end` as a body-shaped macro
// call (because `unless` is in the parser's known-macro set, seeded
// from this stdlib), and the rule below expands it to `if (~ cond)
// body end`. Identical compile-time output to the old hardcoded
// lowering — `if` remains the kernel primitive.

define macro unless
  { unless ?cond:expression ?body:body end }
    => { if (~ ?cond) ?body else #f end }
end macro;

// ─── when macro ────────────────────────────────────────────────────────────
//
// One-armed conditional — the natural partner to `unless`.
// `when` fires the body when the condition is true; `unless` fires it
// when the condition is false. Both expand to `if` with an `else #f`
// so the return type is always well-defined even when the body is
// not taken.
//
//   when (condition) body end
//   ⟹  if (condition) body else #f end
//
// Like `unless`, the condition is an `expression` constraint so the
// parser wraps it in a paren group before fragment-matching begins.

define macro when
  { when ?cond:expression ?body:body end }
    => { if (?cond) ?body else #f end }
end macro;

// ─── cond macro ────────────────────────────────────────────────────────────
//
// Sprint 49b: multi-arm conditional, lowers to nested `if/elseif/else`.
// The Common-Lisp shape, adapted to Dylan's macro engine. Each clause
// is `(test) (body)` — a paren-wrapped condition followed by a
// paren-wrapped body expression. The final clause uses the
// `otherwise` keyword as the default. Example:
//
//   cond
//     (x < 0)   ("negative")
//     (x = 0)   ("zero")
//     (x > 0)   ("positive")
//     otherwise ("unreachable")
//   end
//
// Expands to a straight `if/elseif/else` chain — the kernel
// primitive. No new AST variants; this is purely stdlib sugar.
//
// **Shape constraint.** Each test and body is a single
// `:expression` fragment, which means one token, identifier, literal,
// or grouped form (parens / brackets / braces). Multi-token bodies
// MUST be wrapped: `(foo(x) + 1)` not `foo(x) + 1`. The paren tax
// is the price of admission until the macro engine grows `*`
// repetition (Sprint 49c-ish) — at that point the wrapping can be
// dropped per clause and N-arm support stops being arity-bounded.
//
// **Arity cap.** This rule set supports 1 through 4 test/body pairs
// + `otherwise`. Beyond 4 arms, nest a second `cond` inside the
// `otherwise` clause. The cap is purely the number of fixed rules
// written below — extend by appending more rules in lockstep.

define macro cond
  // 1 test/body pair + otherwise.
  { cond ?t1:expression ?b1:expression otherwise ?d:expression end }
    => { if (?t1) ?b1 else ?d end }
  // 2 pairs + otherwise.
  { cond ?t1:expression ?b1:expression
         ?t2:expression ?b2:expression
         otherwise ?d:expression end }
    => { if (?t1) ?b1 elseif (?t2) ?b2 else ?d end }
  // 3 pairs + otherwise.
  { cond ?t1:expression ?b1:expression
         ?t2:expression ?b2:expression
         ?t3:expression ?b3:expression
         otherwise ?d:expression end }
    => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 else ?d end }
  // 4 pairs + otherwise.
  { cond ?t1:expression ?b1:expression
         ?t2:expression ?b2:expression
         ?t3:expression ?b3:expression
         ?t4:expression ?b4:expression
         otherwise ?d:expression end }
    => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 elseif (?t4) ?b4 else ?d end }
end macro;

// ─── with-cleanup macro ────────────────────────────────────────────────────
//
// Resource-management sugar over `block / cleanup / end`.  The cleanup
// arm is guaranteed to run whether the body exits normally or via a
// non-local exit (NLX) — see `Statement::Block` + `CleanupGuard` in
// the runtime.
//
//   with-cleanup
//     body
//   cleanup
//     cleanup-body
//   end
//   ⟹  block () body cleanup cleanup-body end
//
// The two `:body` variables split at the `cleanup` delimiter keyword.
// This works because the body matcher now uses delimiter-aware greedy
// matching (forward scan for the next Literal in the pattern) rather
// than the old trailing-count-only approach.

define macro with-cleanup
  { with-cleanup ?body:body cleanup ?cleanup:body end }
    => { block () ?body cleanup ?cleanup end }
end macro;

// ─── Sprint 32: closure → C callback pointer ──────────────────────────────
//
// `as-wndproc-callback(cb)` and `as-wndenumproc-callback(cb)` register
// a Dylan closure as a Win32-callable function pointer for the named
// signature, returning a `<c-pointer>` value (fixnum-tagged raw
// address — the FFI ABI Sprint 28 adopted for `<c-pointer>` values).
//
// Sprint 32 ships two signatures: `WNDPROC` (window procedure, used by
// `RegisterClass(W)`) and `WNDENUMPROC` (passed to `EnumWindows`).
// Later sprints add TIMERPROC, THREADPROC, DLGPROC, hook procs, etc.
// A unified `as-c-callback(cb, signature-symbol)` form is deferred
// until `select` lowers.
//
// Registrations are leak-by-design in Sprint 32 — the pool of 32
// slots per signature is allocated once and never freed. A later
// sprint adds release semantics.

define function as-wndproc-callback (closure) => (ptr)
  %register-wndproc(closure)
end function;

define function as-wndenumproc-callback (closure) => (ptr)
  %register-wndenumproc(closure)
end function;

// ─── Sprint 34: <c-struct> field accessors ────────────────────────────────
//
// One getter and one setter per field of every seed struct registered in
// `nod-runtime/src/structs.rs`. Each accessor lowers to a `%struct-get-*`
// / `%struct-set-*` primitive call that reads or writes typed bytes at
// the field's offset in the struct's payload buffer.
//
// The accessors are hand-generated for the Sprint 34 seed structs;
// Sprint 35+ adds a `define c-struct` Dylan-side parser surface that
// emits these automatically.
//
// Setter calling convention: `f(obj) := v` lowers to `f-setter(obj, v)`
// for unary getters (Sprint 12 shape — see `nod-sema/src/lower.rs`
// around line 4493). We accept `(obj, v)` and forward to the primitive
// in primitive-arg order `(value, struct, offset)`.

// ── <point> (8 bytes: LONG x @ 0, LONG y @ 4) ─────────────────────────────

define function point-x (p) => (n)
  %struct-get-i32(p, 0)
end function;

define function point-x-setter (p, v) => (v)
  %struct-set-i32(v, p, 0)
end function;

define function point-y (p) => (n)
  %struct-get-i32(p, 4)
end function;

define function point-y-setter (p, v) => (v)
  %struct-set-i32(v, p, 4)
end function;

// ── <rect> (16 bytes: LONG left @ 0, top @ 4, right @ 8, bottom @ 12) ──────

define function rect-left (r) => (n)
  %struct-get-i32(r, 0)
end function;

define function rect-left-setter (r, v) => (v)
  %struct-set-i32(v, r, 0)
end function;

define function rect-top (r) => (n)
  %struct-get-i32(r, 4)
end function;

define function rect-top-setter (r, v) => (v)
  %struct-set-i32(v, r, 4)
end function;

define function rect-right (r) => (n)
  %struct-get-i32(r, 8)
end function;

define function rect-right-setter (r, v) => (v)
  %struct-set-i32(v, r, 8)
end function;

define function rect-bottom (r) => (n)
  %struct-get-i32(r, 12)
end function;

define function rect-bottom-setter (r, v) => (v)
  %struct-set-i32(v, r, 12)
end function;

// ── <size> (8 bytes: LONG cx @ 0, LONG cy @ 4) ────────────────────────────

define function size-cx (s) => (n)
  %struct-get-i32(s, 0)
end function;

define function size-cx-setter (s, v) => (v)
  %struct-set-i32(v, s, 0)
end function;

define function size-cy (s) => (n)
  %struct-get-i32(s, 4)
end function;

define function size-cy-setter (s, v) => (v)
  %struct-set-i32(v, s, 4)
end function;

// ── <filetime> (8 bytes: DWORD dwLowDateTime @ 0, dwHighDateTime @ 4) ─────

define function filetime-low (f) => (n)
  %struct-get-u32(f, 0)
end function;

define function filetime-low-setter (f, v) => (v)
  %struct-set-u32(v, f, 0)
end function;

define function filetime-high (f) => (n)
  %struct-get-u32(f, 4)
end function;

define function filetime-high-setter (f, v) => (v)
  %struct-set-u32(v, f, 4)
end function;

// ── <systemtime> (16 bytes: WORD wYear @ 0, wMonth @ 2, …) ─────────────────

define function systemtime-year (s) => (n)
  %struct-get-u16(s, 0)
end function;

define function systemtime-year-setter (s, v) => (v)
  %struct-set-u16(v, s, 0)
end function;

define function systemtime-month (s) => (n)
  %struct-get-u16(s, 2)
end function;

define function systemtime-month-setter (s, v) => (v)
  %struct-set-u16(v, s, 2)
end function;

define function systemtime-day-of-week (s) => (n)
  %struct-get-u16(s, 4)
end function;

define function systemtime-day-of-week-setter (s, v) => (v)
  %struct-set-u16(v, s, 4)
end function;

define function systemtime-day (s) => (n)
  %struct-get-u16(s, 6)
end function;

define function systemtime-day-setter (s, v) => (v)
  %struct-set-u16(v, s, 6)
end function;

define function systemtime-hour (s) => (n)
  %struct-get-u16(s, 8)
end function;

define function systemtime-hour-setter (s, v) => (v)
  %struct-set-u16(v, s, 8)
end function;

define function systemtime-minute (s) => (n)
  %struct-get-u16(s, 10)
end function;

define function systemtime-minute-setter (s, v) => (v)
  %struct-set-u16(v, s, 10)
end function;

define function systemtime-second (s) => (n)
  %struct-get-u16(s, 12)
end function;

define function systemtime-second-setter (s, v) => (v)
  %struct-set-u16(v, s, 12)
end function;

define function systemtime-milliseconds (s) => (n)
  %struct-get-u16(s, 14)
end function;

define function systemtime-milliseconds-setter (s, v) => (v)
  %struct-set-u16(v, s, 14)
end function;

// ── <msg> (48 bytes — see structs.rs MSG_FIELDS for layout) ──────────────

define function msg-hwnd (m) => (n)
  %struct-get-pointer(m, 0)
end function;

define function msg-hwnd-setter (m, v) => (v)
  %struct-set-pointer(v, m, 0)
end function;

define function msg-message (m) => (n)
  %struct-get-u32(m, 8)
end function;

define function msg-message-setter (m, v) => (v)
  %struct-set-u32(v, m, 8)
end function;

define function msg-wparam (m) => (n)
  %struct-get-u64(m, 16)
end function;

define function msg-wparam-setter (m, v) => (v)
  %struct-set-u64(v, m, 16)
end function;

define function msg-lparam (m) => (n)
  %struct-get-i64(m, 24)
end function;

define function msg-lparam-setter (m, v) => (v)
  %struct-set-i64(v, m, 24)
end function;

define function msg-time (m) => (n)
  %struct-get-u32(m, 32)
end function;

define function msg-time-setter (m, v) => (v)
  %struct-set-u32(v, m, 32)
end function;

// MSG.pt is a nested POINT; Sprint 34 surfaces flat-offset accessors.
// Sprint 35+ adds dotted notation (`msg.pt.x`).
define function msg-pt-x (m) => (n)
  %struct-get-i32(m, 36)
end function;

define function msg-pt-x-setter (m, v) => (v)
  %struct-set-i32(v, m, 36)
end function;

define function msg-pt-y (m) => (n)
  %struct-get-i32(m, 40)
end function;

define function msg-pt-y-setter (m, v) => (v)
  %struct-set-i32(v, m, 40)
end function;

define function msg-lprivate (m) => (n)
  %struct-get-u32(m, 44)
end function;

define function msg-lprivate-setter (m, v) => (v)
  %struct-set-u32(v, m, 44)
end function;

// ─── Sprint 36: <wndclassexw> field accessors ─────────────────────────────
//
// WNDCLASSEXW carries the window-class registration shape that
// `RegisterClassExW` writes. Sprint 36's `nod-register-window-class`
// helper builds this struct in Rust (because the `lpszClassName`
// field needs a process-lifetime wide-string buffer that's awkward
// to express in pure Dylan); these accessors are here for callers
// that build their own WNDCLASSEXW. The size accessor is the most
// commonly used — calling code must store `sizeof(WNDCLASSEXW) = 80`
// in `cbSize` before passing the struct to `RegisterClassExW`.

define function wndclassexw-cbSize (w) => (n)
  %struct-get-u32(w, 0)
end function;

define function wndclassexw-cbSize-setter (w, v) => (v)
  %struct-set-u32(v, w, 0)
end function;

define function wndclassexw-style (w) => (n)
  %struct-get-u32(w, 4)
end function;

define function wndclassexw-style-setter (w, v) => (v)
  %struct-set-u32(v, w, 4)
end function;

define function wndclassexw-lpfnWndProc (w) => (n)
  %struct-get-pointer(w, 8)
end function;

define function wndclassexw-lpfnWndProc-setter (w, v) => (v)
  %struct-set-pointer(v, w, 8)
end function;

define function wndclassexw-hInstance (w) => (n)
  %struct-get-pointer(w, 24)
end function;

define function wndclassexw-hInstance-setter (w, v) => (v)
  %struct-set-pointer(v, w, 24)
end function;

define function wndclassexw-lpszClassName (w) => (n)
  %struct-get-pointer(w, 64)
end function;

define function wndclassexw-lpszClassName-setter (w, v) => (v)
  %struct-set-pointer(v, w, 64)
end function;

// ─── Sprint 36: <paintstruct> field accessors ─────────────────────────────
//
// PAINTSTRUCT is populated by `BeginPaint(hwnd, &ps)`. The IDE-shell
// WNDPROC typically only reads `hdc` and the inline `rcPaint` —
// we expose those plus a couple of the flag fields. The 32-byte
// `rgbReserved` tail is OS scratch; we don't expose it.

define function paintstruct-hdc (p) => (n)
  %struct-get-pointer(p, 0)
end function;

define function paintstruct-hdc-setter (p, v) => (v)
  %struct-set-pointer(v, p, 0)
end function;

define function paintstruct-fErase (p) => (n)
  %struct-get-i32(p, 8)
end function;

define function paintstruct-fErase-setter (p, v) => (v)
  %struct-set-i32(v, p, 8)
end function;

define function paintstruct-rc-left (p) => (n)
  %struct-get-i32(p, 16)
end function;

define function paintstruct-rc-left-setter (p, v) => (v)
  %struct-set-i32(v, p, 16)
end function;

define function paintstruct-rc-top (p) => (n)
  %struct-get-i32(p, 20)
end function;

define function paintstruct-rc-top-setter (p, v) => (v)
  %struct-set-i32(v, p, 20)
end function;

define function paintstruct-rc-right (p) => (n)
  %struct-get-i32(p, 24)
end function;

define function paintstruct-rc-right-setter (p, v) => (v)
  %struct-set-i32(v, p, 24)
end function;

define function paintstruct-rc-bottom (p) => (n)
  %struct-get-i32(p, 28)
end function;

define function paintstruct-rc-bottom-setter (p, v) => (v)
  %struct-set-i32(v, p, 28)
end function;

// ─── GAP-001 / Sprint 45-prerequisite: <stream> + <string-stream> ─────────
//
// First Dylan-side stream surface. `<stream>` is the abstract base —
// real consumers (write-byte / write-string / as-byte-string) dispatch
// on it. `<string-stream>` is the only concrete subclass today: a
// write-only accumulator that builds a fresh `<byte-string>` from a
// sequence of `write-*` calls.
//
// Use case driving this: Sprint 45's Dylan-in-Dylan lexer wants a
// `print-token(token, source, stream :: <stream>)` generic so each
// token class renders itself to a stream. Previously the lexer
// faked it with a `print-token-to-string` helper returning a
// fresh byte-string per token; `dump-tokens` then concatenated
// every token's slice. That's `O(N²)` allocation. With the stream,
// `dump-tokens` allocates ONE stream, every token appends into it,
// one final `as-byte-string` materialises the dump.
//
// Future subclasses (planned, not in this commit):
//   <file-stream>       — write to a real file handle
//   <console-stream>    — wraps `format-out` for line-buffered I/O
//   <input-stream>      — read-side; lexer-on-stream eventually
//
// Storage: <string-stream> uses a <stretchy-vector> of integer
// bytes. The byte-string materialisation copies them out at the
// end via `%byte-string-element-setter` over a freshly-allocated
// byte-string of the right size. The intermediate stretchy-vector
// is GC-reclaimed once the materialised byte-string outlives it.
//
// Generic-dispatch shape: every method here dispatches on the stream
// argument. `<string-stream>` is the only concrete class today, but
// the discipline means a future `<file-stream>` slots in with no
// changes to consumer code.

define class <stream> (<object>)
end class;

define class <string-stream> (<stream>)
  slot string-stream-bytes :: <stretchy-vector>, init-keyword: bytes:;
end class;

// Convenience constructor: a fresh empty string-stream.
define function make-string-stream () => (s :: <string-stream>)
  make(<string-stream>, bytes: %make-stretchy-vector(64))
end function;

// Append one byte (0..255 integer) to the stream.
define generic write-byte (stream :: <stream>, b :: <integer>) => ();

define method write-byte (stream :: <string-stream>, b :: <integer>) => ()
  %stretchy-vector-push(string-stream-bytes(stream), b);
end method;

// Append every byte of `s` to the stream.
define generic write-string (stream :: <stream>, s :: <byte-string>) => ();

define method write-string (stream :: <string-stream>, s :: <byte-string>) => ()
  let v = string-stream-bytes(stream);
  let n = %byte-string-size(s);
  let i = 0;
  until (i = n)
    %stretchy-vector-push(v, %byte-string-element(s, i));
    i := i + 1;
  end;
end method;

// Materialise the accumulated bytes as a fresh `<byte-string>`. The
// stream itself is unchanged — successive calls return successively
// longer strings if more bytes have been written between them.
define generic as-byte-string (stream :: <stream>) => (s :: <byte-string>);

define method as-byte-string (stream :: <string-stream>) => (s :: <byte-string>)
  let v = string-stream-bytes(stream);
  let n = %stretchy-vector-size(v);
  let out = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    %byte-string-element-setter(%stretchy-vector-element(v, i), out, i);
    i := i + 1;
  end;
  out
end method;
