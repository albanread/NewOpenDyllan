Module: macros-unless

define macro unless
  { unless ?cond:expression ?body:expression end } => { if (~ ?cond) ?body else 0 end }
end macro;

define function test () => (<integer>)
  unless (1 = 0) 42 end
end function test;
