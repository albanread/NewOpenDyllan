Module: dylan
Author: NewOpenDylan stdlib

// ─── DRM system class NAME registrations ──────────────────────────────────
//
// Pure-Dylan `define class` forms for the DRM threads / reflection /
// collection / condition classes the corpus references by name. This is the
// AOT-safe route `<stream>` (streams.dylan) and the array/vector hierarchy
// (arrays.dylan) already use: stdlib classes mint FIRST_USER ids in the
// shared stdlib load and replay in the EXE via nod_aot_register_user_class
// WITHOUT perturbing the Rust seed band, so they cannot cause class-id drift.
//
// These are class-NAME registrations ONLY — they give the class identity so
// that type annotations (`let l :: <lock> = …`), `make`-refs (`make(<lock>)`)
// and `instance?` resolve through the generic CPL-walk path. They deliberately
// carry NO slots and NO operations (acquire/release/spawn/…); those are real
// behaviour to be filled in later. A file that was blocked on one of these
// names now gets PAST the undefined-ident stage (it may then hit a different
// downstream blocker — e.g. a missing thread/lock operation — which is
// expected).
//
// Parents that ALREADY exist (do not re-register here):
//   <object>     — runtime seed
//   <vector>     — arrays.dylan (⊂ <array> ⊂ <mutable-sequence>)
//   <function>   — nod-runtime functions.rs (⊂ <object>)
//   <error>      — nod-runtime conditions.rs (⊂ <serious-condition> ⊂ <condition>)

// ─── Threads: synchronization + locks ─────────────────────────────────────
//
// DRM hierarchy (common-dylan / threads):
//
//   <synchronization>  ⊂ <object>            (abstract)
//   <lock>             ⊂ <synchronization>   (abstract)
//   <simple-lock>      ⊂ <lock>              (concrete — make(<simple-lock>))
//   <recursive-lock>   ⊂ <lock>              (concrete — make(<recursive-lock>))
//   <semaphore>        ⊂ <lock>              (concrete — make(<semaphore>))
//   <notification>     ⊂ <synchronization>   (concrete — make(<notification>, lock:))
//
// The corpus constructs <simple-lock>, <recursive-lock>, <semaphore> and
// <notification> directly, so they are concrete; <synchronization> and
// <lock> are the abstract bases consumers annotate against.

define abstract class <synchronization> (<object>)
end class;

define abstract class <lock> (<synchronization>)
end class;

define class <simple-lock> (<lock>)
end class;

define class <recursive-lock> (<lock>)
end class;

define class <semaphore> (<lock>)
end class;

define class <notification> (<synchronization>)
end class;

// ─── Threads: <thread> ────────────────────────────────────────────────────
//
//   <thread> ⊂ <object>   (concrete — make(<thread>, name:, function:))

define class <thread> (<object>)
end class;

// ─── Reflection: <generic-function> ───────────────────────────────────────
//
//   <generic-function> ⊂ <function>
//
// Lets `instance?(some-generic, <generic-function>)` resolve by name. The
// runtime currently tags callables as <function>; whether a given callable
// answers #t to <generic-function> specifically is a downstream concern —
// this just registers the class identity so the name resolves.

define class <generic-function> (<function>)
end class;

// ─── Collections: <float-vector> ──────────────────────────────────────────
//
//   <float-vector> ⊂ <vector>
//
// Some corpus files instead define <float-vector> locally as a `limited`
// vector constant; this stdlib class serves the files that reference the
// bare name as undefined. Concrete so `make(<float-vector>, size:, fill:)`
// resolves to a class identity.

define class <float-vector> (<vector>)
end class;

// ─── Conditions: <type-error> ─────────────────────────────────────────────
//
//   <type-error> ⊂ <error>
//
// DRM type error. <error>/<condition>/<serious-condition>/<warning>/
// <simple-error>/<simple-warning>/<simple-condition> are already registered
// (nod-runtime conditions.rs); only <type-error> is added here.

define class <type-error> (<error>)
end class;

// ══════════════════════════════════════════════════════════════════════════
// Batch 2 — next corpus undefined-ident CLASS blockers (slot-less markers).
//
// Same AOT-safe route as above: pure-Dylan `define class` NAME registrations
// minted in the shared stdlib load, replayed in the EXE via
// nod_aot_register_user_class WITHOUT perturbing the Rust seed band. NO slots,
// NO operations. They give the class identity so type annotations, `make`-refs
// and `instance?` resolve through the CPL-walk path. A file blocked on one of
// these names now gets PAST the undefined-ident stage (it may then hit a
// different downstream blocker — expected).
//
// Parents that ALREADY exist (runtime seed / earlier registration — do NOT
// re-register; just inherit from them):
//   <object> <condition> <error> <integer>  — runtime seed (classes.rs/conditions.rs)
//   <float>                                  — runtime seed (classes.rs)
//   <collection> <mutable-collection> <sequence> <mutable-sequence>
//   <explicit-key-collection> <stretchy-collection> <stretchy-vector> <table>
//                                            — nod-runtime collections.rs/tables.rs
//   <vector>                                 — arrays.dylan

// ─── Reflection: the type / metaobject classes ────────────────────────────
//
// DRM:  <type>      ⊂ <object>   (abstract)
//       <class>     ⊂ <type>
//       <singleton> ⊂ <type>
//
// LIMITATION: these are NAME markers only. The runtime's actual class objects
// and singletons are not (yet) instances of these Dylan classes, so
// `instance?(<integer>, <class>)` / `instance?(singleton(5), <singleton>)` will
// answer #f. Registering the names clears the undefined-ident blocker; full
// metaclass semantics are deferred.

define abstract class <type> (<object>)
end class;

define class <class> (<type>)
end class;

define class <singleton> (<type>)
end class;

// ─── Numbers: the DRM number tower ────────────────────────────────────────
//
// DRM:  <number>   ⊂ <object>    (abstract)
//       <complex>  ⊂ <number>    (abstract)
//       <real>     ⊂ <complex>   (abstract)
//       <rational> ⊂ <real>      (abstract)
//
// LIMITATION: <integer>/<single-float>/<double-float>/<float> are seeded by the
// runtime DIRECTLY under <object> (classes.rs), so they are NOT re-parented
// under <number>/<real> here — `instance?(5, <number>)` answers #f. These
// markers only clear the undefined-ident blocker for files that annotate or
// `make`-ref the abstract number classes.

define abstract class <number> (<object>)
end class;

define abstract class <complex> (<number>)
end class;

define abstract class <real> (<complex>)
end class;

define abstract class <rational> (<real>)
end class;

// <float> — DRM abstract float base (corpus annotates `<float>` directly, e.g.
// dylan/tests/numbers.dylan / regressions.dylan). The runtime seeds
// <single-float>/<double-float> DIRECTLY under <object> (classes.rs) and only
// uses "<float>" as a sema TYPE-ALIAS — there is no real <float> CLASS. Register
// it here as the abstract <real> subclass DRM specifies so the bare name
// resolves. LIMITATION: the seeded <single-float>/<double-float> are NOT
// re-parented under it, so `instance?(1.5, <float>)` answers #f (marker only).

define abstract class <float> (<real>)
end class;

// <byte> / <bit> — limited-integer markers. DRM-wise these are limited <integer>
// types; a marker subclass of the seeded <integer> is sufficient to clear the
// name. <extended-float> ⊂ <float> completes the DRM float family.

define class <byte> (<integer>)
end class;

define class <bit> (<integer>)
end class;

define class <extended-float> (<float>)
end class;

// ─── Conditions: <restart> and friends ────────────────────────────────────
//
// DRM:  <restart>                  ⊂ <condition>
//       <arithmetic-error>         ⊂ <error>
//       <arithmetic-overflow-error>⊂ <arithmetic-error>
//       <sealed-object-error>      ⊂ <error>
//
// <condition>/<error> and <simple-restart> are already registered (nod-runtime
// conditions.rs); <simple-restart> is NOT redefined here. <abort>/<simple-restart>
// are runtime-side restart concretes; the abstract <restart> base + the two
// arithmetic/sealed errors are the undefined-ident blockers added here.

define class <restart> (<condition>)
end class;

define abstract class <arithmetic-error> (<error>)
end class;

define class <arithmetic-overflow-error> (<arithmetic-error>)
end class;

define class <sealed-object-error> (<error>)
end class;

// ─── Conditions: io stream errors ─────────────────────────────────────────
//
// DRM io stream-condition family (common-dylan/tests/streams.dylan annotates
// all of these):
//   <stream-error>           ⊂ <error>
//   <end-of-stream-error>    ⊂ <stream-error>
//   <incomplete-read-error>  ⊂ <stream-error>
//   <incomplete-write-error> ⊂ <stream-error>
//
// Slot-less markers — the message/position/count slots are real io behaviour to
// be filled in with the io port. <stream> itself is registered in streams.dylan.

define class <stream-error> (<error>)
end class;

define class <end-of-stream-error> (<stream-error>)
end class;

define class <incomplete-read-error> (<stream-error>)
end class;

define class <incomplete-write-error> (<stream-error>)
end class;

// <test-input-stream> — the io test suite (common-dylan/tests/streams.dylan)
// references this stream subclass by name; it is a test-suite stream concrete,
// registered here as a marker to clear the bare-name ident. Parented under
// <object> (NOT <stream>): streams.dylan — where <stream> is registered — loads
// AFTER this file in STDLIB_FILES, so <stream> is not yet resolvable here, and
// the exact parent is immaterial for clearing the undefined-ident. NOTE: not a
// DRM class — if the real test-library definition is later loaded alongside it,
// this marker should be dropped to avoid a redefinition collision.

define class <test-input-stream> (<object>)
end class;

define class <test-output-stream> (<object>)
end class;

// ─── Collections: list / deque / set / table / stretchy markers ───────────
//
// DRM:  <list>                          ⊂ <sequence>            (abstract)
//       <deque>                         ⊂ <mutable-sequence>    (DRM: also
//                                         <stretchy-collection>; SI to dodge
//                                         the runtime's MI-bookkeeping risk,
//                                         same policy as <mutable-sequence>)
//       <object-deque>                  ⊂ <deque>
//       <set>                           ⊂ <mutable-collection>
//       <mutable-explicit-key-collection> ⊂ <explicit-key-collection>
//       <object-table>                  ⊂ <table>
//       <string-table>                  ⊂ <table>
//       <stretchy-sequence>             ⊂ <mutable-sequence>
//       <stretchy-object-vector>        ⊂ <stretchy-vector>
//       <single-float-vector>           ⊂ <vector>  (parallels <float-vector>)
//       <double-float-vector>           ⊂ <vector>
//
// LIMITATION: <pair>/<empty-list> are seeded directly under <object>, so they
// are NOT re-parented under <list>; `instance?(#(1,2), <list>)` answers #f.
// Marker only.

define abstract class <list> (<sequence>)
end class;

define class <deque> (<mutable-sequence>)
end class;

define class <object-deque> (<deque>)
end class;

define class <set> (<mutable-collection>)
end class;

define abstract class <mutable-explicit-key-collection> (<explicit-key-collection>)
end class;

define class <object-table> (<table>)
end class;

define class <string-table> (<table>)
end class;

define class <stretchy-sequence> (<mutable-sequence>)
end class;

define class <stretchy-object-vector> (<stretchy-vector>)
end class;

define class <single-float-vector> (<vector>)
end class;

define class <double-float-vector> (<vector>)
end class;
