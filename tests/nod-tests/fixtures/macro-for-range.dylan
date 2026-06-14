Module: macro-for-range

// Sprint 18 headline: a Dylan-side `for-range` macro that expands into
// `let i = …; while (i <= …) body; i := i + 1 end`, then JITs and runs.
//
// Surface shape: `for-range(var, start, end, body-expr)` — a call-form
// macro that takes the body as a single trailing expression. The
// upstream `for (i from 1 to 10) body end` statement-shape lives at
// the boundary of Sprint 19's statement-macro parsing work.
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

define function sum-to-ten () => (<integer>)
  let total = 0;
  for-range(i, 1, 10, (total := total + i));
  total
end function sum-to-ten;
