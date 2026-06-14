// Sprint 37 — JIT cache sample. ~80 helper functions whose JIT compile
// cost dominates the eval; the entry expression in jit_cache_sample.dylan
// invokes the deepest helpers.
define function s00 (x :: <integer>) => (<integer>) x + 1 end;
define function s01 (x :: <integer>) => (<integer>) x + s00(x) end;
define function s02 (x :: <integer>) => (<integer>) x + s01(x) end;
define function s03 (x :: <integer>) => (<integer>) x + s02(x) end;
define function s04 (x :: <integer>) => (<integer>) x + s03(x) end;
define function s05 (x :: <integer>) => (<integer>) x + s04(x) end;
define function s06 (x :: <integer>) => (<integer>) x + s05(x) end;
define function s07 (x :: <integer>) => (<integer>) x + s06(x) end;
define function s08 (x :: <integer>) => (<integer>) x + s07(x) end;
define function s09 (x :: <integer>) => (<integer>) x + s08(x) end;
define function s10 (x :: <integer>) => (<integer>) x + s09(x) end;
define function s11 (x :: <integer>) => (<integer>) x + s10(x) end;
define function s12 (x :: <integer>) => (<integer>) x + s11(x) end;
define function s13 (x :: <integer>) => (<integer>) x + s12(x) end;
define function s14 (x :: <integer>) => (<integer>) x + s13(x) end;
define function s15 (x :: <integer>) => (<integer>) x + s14(x) end;
define function s16 (x :: <integer>) => (<integer>) x + s15(x) end;
define function s17 (x :: <integer>) => (<integer>) x + s16(x) end;
define function s18 (x :: <integer>) => (<integer>) x + s17(x) end;
define function s19 (x :: <integer>) => (<integer>) x + s18(x) end;
define function s20 (x :: <integer>) => (<integer>) x + s19(x) end;
define function s21 (x :: <integer>) => (<integer>) x + s20(x) end;
define function s22 (x :: <integer>) => (<integer>) x + s21(x) end;
define function s23 (x :: <integer>) => (<integer>) x + s22(x) end;
define function s24 (x :: <integer>) => (<integer>) x + s23(x) end;
define function s25 (x :: <integer>) => (<integer>) x + s24(x) end;
define function s26 (x :: <integer>) => (<integer>) x + s25(x) end;
define function s27 (x :: <integer>) => (<integer>) x + s26(x) end;
define function s28 (x :: <integer>) => (<integer>) x + s27(x) end;
define function s29 (x :: <integer>) => (<integer>) x + s28(x) end;
define function s30 (x :: <integer>) => (<integer>) x + s29(x) end;
define function s31 (x :: <integer>) => (<integer>) x + s30(x) end;
define function s32 (x :: <integer>) => (<integer>) x + s31(x) end;
define function s33 (x :: <integer>) => (<integer>) x + s32(x) end;
define function s34 (x :: <integer>) => (<integer>) x + s33(x) end;
