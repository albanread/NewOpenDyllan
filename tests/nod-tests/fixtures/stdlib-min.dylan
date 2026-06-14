Module: stdlib-min

// Sprint 18 first cut at a Dylan-side macro library. The "real"
// `nod-dylan/stdlib.dylan` that auto-loads at compiler startup lands in
// Sprint 25; for now this fixture establishes the shape and exercises
// the Sprint 18 macro engine.
//
// Notable deviations from upstream Dylan:
//   - `for-range` takes the body as a single trailing expression
//     (Sprint 18's parser doesn't yet handle statement-position macros
//     with their own `end` keyword; the upstream `for (i from 1 to 10)
//     body end` shape needs Sprint 19+ statement-stream parsing).
//   - `unless-1` is named distinctly from the parser-hardcoded `unless`
//     so the macro engine isn't competing with `Expr::Unless`. The
//     hardcoded form's migration to a pure macro is Sprint 25.
//   - `cond` is not yet shipped (deferred — needs auxiliary `rule`
//     clauses inside `define macro`, or a flattening macro at the
//     pattern level; both are Sprint 19+).

define macro when
  { when (?cond:expression) ?body:body end } => { if (?cond) ?body else 0 end }
end macro;

define macro unless-1
  { unless-1 (?cond:expression) ?body:body end } => { if (~ ?cond) ?body else 0 end }
end macro;

define macro until-loop
  { until-loop (?cond:expression) ?body:body end } => { while (~ ?cond) ?body end }
end macro;

define macro for-range
  { for-range(?var:name, ?start:expression, ?end-val:expression, ?body:expression) }
    => { begin
           let ?var = ?start;
           while (?var <= ?end-val)
             ?body;
             ?var := ?var + 1
           end
         end }
end macro;
