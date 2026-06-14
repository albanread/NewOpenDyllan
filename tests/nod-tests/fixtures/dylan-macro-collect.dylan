Module: dylan-lexer
Precedence: c

// Sprint 52.2 — macro-collection unit driver.
//
// Reads argv[1] as a path, loads the source, runs the Dylan-side
// `collect-macro-defs` over it (lex → group-balance → extract every
// top-level `define macro … end macro` into a <macro-def>), and prints
// a stable corpus-exercise report:
//
//   COLLECTED <n>
//   MACRO <name> rules=<k>      (one line per def, definition order)
//
// The Rust gate (`tests/nod-tests/tests/macro_collect.rs`) builds this
// via `dylan-macro-collect.prj`, runs it on `stdlib.dylan` + the macro
// fixtures, and asserts the counts + names match Rust's `collect_macros`
// for the same input — the 52.2 parity gate.
//
// Bundled with `dylan-lexer.dylan` (lex(), load-source-via-rope(),
// <token> machinery) and `dylan-macro.dylan` (the engine). No parser
// needed — collection works on the token/fragment level, below the AST.

define function collect-main () => ()
  let path = %argv1();
  if (empty?(path))
    format-out("dylan-macro-collect: missing input path\n");
  else
    let source = load-source-via-rope(path);
    if (empty?(source))
      format-out("dylan-macro-collect: could not read %s\n", path);
    else
      let defs = collect-macro-defs(source);
      format-out("COLLECTED %d\n", size(defs));
      let n = size(defs);
      let i = 0;
      until (i = n)
        let d = defs[i];
        format-out("MACRO %s rules=%d\n",
                   macro-def-name(d), size(macro-def-rules(d)));
        i := i + 1;
      end;
    end;
  end;
end function collect-main;
