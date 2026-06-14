Module: dylan-lexer
Precedence: c

// Sprint 52.3 — pattern-matching parity unit driver.
//
// Runs a fixed set of (define-macro, call-site) cases through the
// Dylan-side macro engine: collect the def, take rule 0's pattern, build
// the call-site fragments, and `match-pattern`. For each case it prints a
// stable report:
//
//   CASE <name>
//   BIND <var> = <rendered fragments>     (one per binding, pattern order)
//   NOMATCH                               (if the pattern did not match)
//
// The Rust gate `tests/nod-tests/tests/macro_match.rs` runs the SAME
// cases through Rust's `nod_macro::match_pattern` (call fragments built
// with `nod_reader::build_fragments`) and asserts the bindings are
// identical (sorted, so HashMap order on the Rust side doesn't matter).
//
// The cases exercise every nod-macro PatternKind: expression, name,
// body (both end-delimited and keyword-delimited), variable,
// macro-arg, parameter-list, constraint, plus group patterns.

define function run-case (nm :: <byte-string>,
                          def-src :: <byte-string>,
                          call-src :: <byte-string>) => ()
  format-out("CASE %s\n", nm);
  let defs = collect-macro-defs(def-src);
  if (size(defs) = 0)
    format-out("NODEF\n");
  else
    let rule       = macro-def-rules(defs[0])[0];
    let pattern    = macro-rule-pattern(rule);
    let call-toks  = lex-source-to-toks(call-src);
    let call-frags = tokens-to-fragments(call-toks);
    let b = match-pattern(pattern, call-frags);
    if (~ b)
      format-out("NOMATCH\n");
    else
      let n = size(b);
      let i = 0;
      until (i = n)
        let e = b[i];
        format-out("BIND %s = %s\n",
                   binding-name(e), render-frags(binding-frags(e)));
        i := i + 1;
      end;
    end;
  end;
end function run-case;

define function match-main () => ()
  // Expression + body (end-delimited).
  run-case("unless",
           "define macro unless { unless ?cond:expression ?body:body end } => { if (~ ?cond) ?body else #f end } end macro;",
           "unless x (foo) end");
  run-case("when",
           "define macro when { when ?cond:expression ?body:body end } => { if (?cond) ?body else #f end } end macro;",
           "when y (g) end");
  // Group pattern with `name` + literal `in` + expression, then body.
  run-case("for-each",
           "define macro for-each { for-each (?var:name in ?coll:expression) ?body:body end } => { 1 } end macro;",
           "for-each (i in xs) (work) end");
  // Two `:body` vars split at the `cleanup` keyword delimiter.
  run-case("with-cleanup",
           "define macro with-cleanup { with-cleanup ?body:body cleanup ?cleanup:body end } => { 1 } end macro;",
           "with-cleanup (a) cleanup (b) end");
  // Multiple `:expression` vars + literal `otherwise`.
  run-case("cond",
           "define macro cond { cond ?t1:expression ?b1:expression otherwise ?d:expression end } => { 1 } end macro;",
           "cond (x) (y) otherwise (z) end");
  // name kind — identifier only.
  run-case("name",
           "define macro nm { nm ?x:name end } => { 1 } end macro;",
           "nm foo end");
  // variable kind — bare identifier (no `:: type`).
  run-case("variable",
           "define macro vv { vv ?x:variable end } => { 1 } end macro;",
           "vv a end");
  // parameter-list kind — a single paren group.
  run-case("parameter-list",
           "define macro pl { pl ?p:parameter-list end } => { 1 } end macro;",
           "pl (a, b) end");
  // macro-arg kind — aliases expression (one fragment).
  run-case("macro-arg",
           "define macro ma { ma ?x:macro-arg end } => { 1 } end macro;",
           "ma z end");
  // constraint kind — aliases expression (one fragment).
  run-case("constraint",
           "define macro co { co ?x:constraint end } => { 1 } end macro;",
           "co w end");
end function match-main;
