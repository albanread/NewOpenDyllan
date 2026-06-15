Module: dylan
Author: NewOpenDylan stdlib

// ─── Abstract array / vector collection classes ───────────────────────────
//
// The DRM array/vector hierarchy, registered here as pure-Dylan
// `define class` forms — the AOT-safe route `<stream>`/`<string-stream>`
// already use (see streams.dylan). Stdlib classes mint FIRST_USER ids in
// the shared stdlib load and replay in the EXE via
// nod_aot_register_user_class WITHOUT perturbing the Rust seed band, so
// they cannot cause class-id drift.
//
// DRM hierarchy:
//
//   <array>        ⊂ <mutable-sequence>   (abstract)
//   <vector>       ⊂ <array>              (abstract)
//   <simple-vector>⊂ <vector>             (abstract)
//   <bit-vector>   ⊂ <vector>             (concrete — A2)
//   <byte-vector>  ⊂ <vector>             (abstract surface today)
//
// `<simple-object-vector>` remains a runtime seed class (id 9) whose
// concrete layout the GC and the SOV fast path already understand; the
// `instance?` lowering keeps an exact-id SOV fast path AND a CPL walk so
// `instance?(realSOV, <vector>)` and `instance?(bitvector, <vector>)`
// both answer #t. `<mutable-sequence>` is a runtime seed user-class
// (ensure_collections_registered); it is resolvable by name at stdlib
// load time, exactly as `<table>` / `<stretchy-vector>` are.

define abstract class <array> (<mutable-sequence>)
end class;

define abstract class <vector> (<array>)
end class;

define abstract class <simple-vector> (<vector>)
end class;

// `<byte-vector>` — abstract surface for now. A concrete representation
// (packed u8 words) lands alongside the bit-vector follow-up; today this
// just gives the class identity so `instance?(x, <byte-vector>)` and the
// CPL walk resolve.
define abstract class <byte-vector> (<vector>)
end class;

// `<bit-vector>` — concrete. Two slots:
//
//   - `%bit-vector-words`: a `<simple-object-vector>` of fixnum words;
//      bit i lives in word i / word-bits at bit (i mod word-bits). The
//      SOV is GC-scanned normally, so the words stay live.
//   - `%bit-vector-bit-size`: the logical bit count (a fixnum).
//
// `make(<bit-vector>, size:, fill:)` is intercepted in `lower_make` and
// redirected to the `nod_bit_vector_allocate` runtime primitive (mirrors
// the `<table>` → `nod_make_table` arm); a `define method make` would be
// dead code because `lower_make` intercepts at the call site.
define class <bit-vector> (<vector>)
  slot %bit-vector-words, init-keyword: words:;
  slot %bit-vector-bit-size :: <integer>, init-keyword: bit-size:;
end class;

// ─── <bit-vector> element / size generics over the primitives ─────────────
//
// These dispatch on `<bit-vector>` so they outrank the `<object>`
// rewrites of `size` / `element` / `element-setter`. The primitives
// (`%bit-vector-*`) are Rust shims in `nod-runtime::bitvectors`.

define method size (bv :: <bit-vector>) => (n :: <integer>)
  %bit-vector-size(bv)
end method;

define method element (bv :: <bit-vector>, i :: <integer>) => (bit :: <integer>)
  %bit-vector-ref(bv, i)
end method;

define method element-setter (value :: <integer>, bv :: <bit-vector>, i :: <integer>) => (value :: <integer>)
  %bit-vector-set(bv, i, value)
end method;

// Set bit `i` to 1; returns the bit-vector.
define method set-bits! (bv :: <bit-vector>, i :: <integer>) => (bv :: <bit-vector>)
  %bit-vector-set(bv, i, 1)
end method;

// Clear bit `i` to 0; returns the bit-vector.
define method unset-bits! (bv :: <bit-vector>, i :: <integer>) => (bv :: <bit-vector>)
  %bit-vector-set(bv, i, 0)
end method;

// Population count — number of 1 bits across the whole vector.
define method bit-count (bv :: <bit-vector>) => (n :: <integer>)
  %bit-vector-count(bv)
end method;
