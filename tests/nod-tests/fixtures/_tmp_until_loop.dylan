Module: untilloop

define function sum-to-ten () => (<integer>)
  let i = 0;
  let s = 0;
  until (i >= 10)
    i := i + 1;
    s := s + i
  end;
  s
end function sum-to-ten;
