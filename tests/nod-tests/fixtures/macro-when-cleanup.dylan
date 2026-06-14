Module: test

define function test-when (x) => (result)
  when (x > 3)
    42
  end
end function;

define function test-with-cleanup (x) => (result)
  with-cleanup
    x + 1
  cleanup
    x - 1
  end
end function;
