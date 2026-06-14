Module: hello

// Minimal valid Dylan program used as the smallest oracle-corpus
// fixture for Sprint 45 (the Dylan-in-Dylan lexer). Sprint 45a's
// dump-dylan-tokens subcommand exercises the end-to-end path against
// this file; the stub lex returns `[<eof-token>]` so the expected
// stdout for 45a is exactly `1:1-1:1  EOF` + newline. Sprint 45b's
// real lex function will produce the full token stream over this
// fixture and 45d's oracle test diffs against nod-reader on it.

define function main () => ()
  format-out("hello\n");
end function main;
