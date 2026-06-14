Module: whentest

define macro when
  { when(?cond:expression, ?body:expression) } => { if (?cond) ?body else 0 end }
end macro;

define function t-true () => (<integer>)
  when((1 = 1), 42)
end function t-true;

define function t-false () => (<integer>)
  when((1 = 0), 42)
end function t-false;
