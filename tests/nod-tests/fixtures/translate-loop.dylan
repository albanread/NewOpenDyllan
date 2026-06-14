Module: translate-loop

// Sprint 51e — `until` / `while` loops that FULLY translate via
// --parse-with-dylan (no nested binary ops, no macros), so the gate
// validates Statement::Until / Statement::While reconstruction.

define function drain (q) => ()
  until (done(q))
    consume(q)
  end;
end function;

define function spin (n) => ()
  while (active(n))
    tick(n)
  end;
end function;
