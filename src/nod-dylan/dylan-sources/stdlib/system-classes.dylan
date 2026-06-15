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
