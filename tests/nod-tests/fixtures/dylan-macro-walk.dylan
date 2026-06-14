Module: dylan-lexer
Precedence: c

// Sprint 52.5 — module-walk expansion driver.
//
// For a fixed set of (define-macros, input-source) cases: collect the
// macro table from the def source, then `expand-module-source` the input
// — walking the fragment stream, expanding every macro call site
// (multi-rule selection), re-lexing each expansion, and recursing to
// fixpoint. Prints the rendered expanded source:
//
//   WALK <name> = <expanded source>
//
// The gate `tests/nod-tests/tests/macro_walk.rs` checks each line against
// a hand-verified expectation. The authoritative AST-level cross-check
// against the Rust expander lands in 52.6 (front-end integration +
// dump-expanded byte gate). Cases exercise: a call embedded in a larger
// stream (begin … end passthrough), no-macro passthrough, recursion to
// fixpoint (a macro expanding to another macro call), multi-rule
// selection inside the walk, and two sibling calls in one stream.

define function run-walk (nm :: <byte-string>,
                          def-src :: <byte-string>,
                          input-src :: <byte-string>) => ()
  let table = collect-macro-defs(def-src);
  let text  = expand-module-source(input-src, table, "42");
  format-out("WALK %s = %s\n", nm, text);
end function run-walk;

define constant $unless-def =
  "define macro unless { unless ?cond:expression ?body:body end } => { if (~ ?cond) ?body else #f end } end macro;";

define function walk-main () => ()
  // A call embedded in a begin … end — surrounding fragments pass through.
  run-walk("embedded", $unless-def, "begin unless x (b) end end");
  // No macro call — verbatim passthrough.
  run-walk("passthrough", $unless-def, "foo (bar)");
  // Recursion to fixpoint: `neg` expands to an `unless` call, which then
  // expands to `if`.
  run-walk("recursion",
           "define macro neg { neg ?x:expression end } => { unless ?x (1) end } end macro; define macro unless { unless ?cond:expression ?body:body end } => { if (~ ?cond) ?body else #f end } end macro;",
           "neg y end");
  // Multi-rule selection inside the walk (the 4-rule cond, 1-pair call).
  run-walk("cond-walk",
           "define macro cond { cond ?t1:expression ?b1:expression otherwise ?d:expression end } => { if (?t1) ?b1 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 else ?d end } end macro;",
           "cond (x) (y) otherwise (z) end");
  // Two sibling calls in one stream — both expand independently.
  run-walk("siblings", $unless-def, "unless a (p) end unless b (q) end");
  // A `define macro` form in the input is STRIPPED (compile-time only,
  // not losslessly renderable); the following call still expands.
  run-walk("strip-def",
           "define macro u { u ?c:expression ?b:body end } => { if (~ ?c) ?b end } end macro;",
           "define macro u { u ?c:expression ?b:body end } => { if (~ ?c) ?b end } end macro; u x (y) end");
  // Call-shaped macro `name(args)` (no `end`) — spans the name plus the
  // immediately-following paren group.
  run-walk("call-form",
           "define macro twice { twice(?x:expression) } => { ?x + ?x } end macro;",
           "twice(5)");
end function walk-main;
