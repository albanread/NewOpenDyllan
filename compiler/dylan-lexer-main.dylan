// dylan-lexer-main.dylan — entry point for the `dump-dylan-tokens` subcommand.
//
// Compiled together with dylan-lexer.dylan as a two-file build:
//   nod-driver build dylan-lexer.dylan dylan-lexer-main.dylan -o dylan-lexer.exe
//
// Keeping main() here (rather than inside dylan-lexer.dylan) lets
// dylan-parser.dylan reuse the lexer as a library in its own two-file
// build without a duplicate-main conflict.
//
// Empty argv[1] → print a usage line to stdout and exit cleanly.
// Sprint 45a doesn't have a process-exit primitive that returns
// non-zero, so usage failures still exit 0; the driver layer can
// detect the empty-stdout case if it wants to.

define function main () => ()
  let path = %argv1();
  let mode = %argv2();
  if (empty?(path))
    format-out("dylan-lexer: missing input path\n");
  else
    let source = load-source-via-rope(path);
    if (empty?(source))
      format-out("dylan-lexer: could not read %s\n", path);
    else
      let tokens = lex(source);
      format-out("%s", dump-tokens(tokens, source));
      if (mode = "--gc-stats")
        %print-gc-stats();
      end;
    end;
  end;
end function main;

main();
