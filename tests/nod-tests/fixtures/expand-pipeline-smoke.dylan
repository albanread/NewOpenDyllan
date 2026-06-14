Module: expand-pipeline-smoke

// Sprint 52.6 — end-to-end exercise of the locus-(B) Dylan macro
// expander. `unless` is a stdlib macro. With NOD_EXPAND_WITH_DYLAN set,
// the Dylan front-end expands it BEFORE the AST wire (the shim's
// `dylan-expand-source`), the parser translates the resulting macro-free
// kernel source, and the program builds + runs — printing 42.
//
// Without the flag the same program still builds (the Rust expander runs
// in nod-sema instead); the gate `macro_pipeline.rs` asserts both paths
// produce 42, and that the flagged build's stderr shows the Dylan
// expander fired ("expand-with-dylan: expanded").

define function main () => ()
  unless (1 = 0)
    format-out("%d\n", 42)
  end;
end function main;
