Module: dylan-lexer
Precedence: c

// Sprint 52.4 — substitution + hygiene parity unit driver.
//
// For a fixed set of (define-macro, call-site) cases: collect the def,
// match rule 0's pattern against the call, then `substitute-hyg` the
// template with the bindings under a PINNED hygiene nonce ("42"), and
// print the expansion text:
//
//   EXPAND <name> = <substituted + hygiene-renamed text>
//   NOMATCH                                  (if the pattern didn't match)
//
// The Rust gate `tests/nod-tests/tests/macro_expand.rs` runs the same
// cases through `nod_macro::substitute` (nonce 42) and asserts the
// whitespace-normalised expansions are byte-identical. Cases cover:
//   * substitution only, no binders (unless),
//   * a `let`-introduced binder (renamed everywhere),
//   * a `method (…)` param binder,
//   * the real stdlib `for-each` (the `%fip-state` binder renamed, the
//     `?var`/`?coll`/`?body` pattern vars NOT renamed).

define function run-expand (nm :: <byte-string>,
                            def-src :: <byte-string>,
                            call-src :: <byte-string>) => ()
  let defs = collect-macro-defs(def-src);
  if (size(defs) = 0)
    format-out("EXPAND %s = NODEF\n", nm);
  else
    // expand-call does multi-rule selection (first matching rule wins),
    // then hygienic substitution under the pinned nonce.
    let call-frags = tokens-to-fragments(lex-source-to-toks(call-src));
    let text = expand-call(defs[0], call-frags, "42");
    if (text)
      format-out("EXPAND %s = %s\n", nm, text);
    else
      format-out("EXPAND %s = NOMATCH\n", nm);
    end;
  end;
end function run-expand;

define function expand-main () => ()
  run-expand("unless",
             "define macro unless { unless ?cond:expression ?body:body end } => { if (~ ?cond) ?body else #f end } end macro;",
             "unless x (foo) end");
  run-expand("let-binder",
             "define macro lt { lt ?e:expression end } => { let tmp = ?e ; tmp end } end macro;",
             "lt (foo) end");
  run-expand("method-param",
             "define macro mm { mm ?e:expression end } => { method (q) q end } end macro;",
             "mm (z) end");
  run-expand("for-each",
             "define macro for-each { for-each (?var:name in ?coll:expression) ?body:body end } => { begin let %fip-state = %fip-init(?coll); until (%fip-finished?(%fip-state)) let ?var = %fip-current-element(%fip-state); ?body; %fip-advance!(%fip-state) end end } end macro;",
             "for-each (i in xs) (work) end");
  // Multi-rule selection — the 4-rule stdlib `cond`. A 1-pair call
  // selects rule 1; a 2-pair call selects rule 2.
  run-expand("cond-1arm",
             "define macro cond { cond ?t1:expression ?b1:expression otherwise ?d:expression end } => { if (?t1) ?b1 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression ?t4:expression ?b4:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 elseif (?t4) ?b4 else ?d end } end macro;",
             "cond (x) (y) otherwise (z) end");
  run-expand("cond-2arm",
             "define macro cond { cond ?t1:expression ?b1:expression otherwise ?d:expression end } => { if (?t1) ?b1 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression ?t4:expression ?b4:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 elseif (?t4) ?b4 else ?d end } end macro;",
             "cond (a) (b) (c) (d) otherwise (e) end");
end function expand-main;
