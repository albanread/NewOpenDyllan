Module: dylan-lexer
Precedence: c

// Sprint 52.6 — whole-file macro expansion driver (locus B at test level).
//
// Reads argv[1] as a path, collects the file's own `define macro`s into a
// table, and `expand-module-source` expands every macro call site to
// fixpoint — stripping the `define macro` forms and rewriting call sites —
// then prints the expanded source.
//
// The gate `tests/nod-tests/tests/macro_file_expand.rs` re-parses this
// expanded source with the Rust parser and asserts its AST matches Rust's
// own `parse → expand` of the original file (modulo the compile-time-only
// `define macro` items), proving the Dylan expander produces the same
// kernel-shaped AST the host lowers today — the locus-(B) verify-mode
// invariant, checked at the test level before the front-end rollout.

define function file-expand-main () => ()
  let path = %argv1();
  if (empty?(path))
    format-out("dylan-macro-file: missing input path\n");
  else
    let source = load-source-via-rope(path);
    if (empty?(source))
      format-out("dylan-macro-file: could not read %s\n", path);
    else
      let table    = collect-macro-defs(source);
      let expanded = expand-module-source(source, table, "42");
      format-out("%s\n", expanded);
    end;
  end;
end function file-expand-main;
