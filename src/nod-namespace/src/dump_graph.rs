//! Graphviz dump for the library/module graph.

use std::fmt::Write as _;

use crate::graph::{Graph, ModuleRef};

pub fn dump_graph(g: &Graph) -> String {
    let mut out = String::new();
    out.push_str("digraph G {\n");
    out.push_str("  rankdir=LR;\n");
    out.push_str("  compound=true;\n");

    for (lib_idx, lib) in g.libraries().enumerate() {
        let lib_name = g.resolve(lib.name);
        writeln!(out, "  subgraph cluster_lib_{lib_idx} {{").unwrap();
        writeln!(out, "    label={};", quote(lib_name)).unwrap();
        writeln!(
            out,
            "    lib_{lib_idx} [shape=box, label={}];",
            quote(lib_name)
        )
        .unwrap();
        for mid in &lib.modules {
            let m = g.module(*mid);
            let mname = g.resolve(m.name);
            writeln!(
                out,
                "    mod_{} [shape=ellipse, label={}];",
                mid.0,
                quote(mname)
            )
            .unwrap();
        }
        out.push_str("  }\n");
    }

    for m in g.modules() {
        for u in &m.uses {
            if let ModuleRef::Resolved(target) = u.module {
                writeln!(out, "  mod_{} -> mod_{};", m.id.0, target.0).unwrap();
            }
        }
    }

    out.push_str("}\n");
    out
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lid::parse_lid_str;
    use std::path::PathBuf;

    #[test]
    fn empty_graph_is_well_formed() {
        let g = Graph::new();
        let s = dump_graph(&g);
        assert!(s.starts_with("digraph G {"));
        assert!(s.trim_end().ends_with('}'));
    }

    #[test]
    fn single_library() {
        let lid = parse_lid_str("library: foo\nfiles: a\n", PathBuf::from("x.lid"));
        let mut g = Graph::new();
        let lib = g.add_library_from_lid(&lid);
        g.add_module(lib, "internal");
        let s = dump_graph(&g);
        assert!(s.contains("cluster_lib_0"));
        assert!(s.contains("\"foo\""));
        assert!(s.contains("\"internal\""));
    }
}
