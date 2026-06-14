//! `nod-namespace` — LID/`dylan-package.json` parsers and library/module graph.
//!
//! Sprint 05 deliverable. See `specs/05-library-module-graph.md`.

pub mod dump_graph;
pub mod graph;
pub mod lid;
pub mod package_json;

pub use dump_graph::dump_graph;
pub use graph::{
    Binding, BindingId, BindingKind, Graph, Library, LibraryId, LibraryRef, LibraryUse, Module,
    ModuleId, ModuleRef, ModuleUse, Symbol, SymbolInterner,
};
pub use lid::{Diagnostic, Lid, TargetType, load_lid_chain, parse_lid, parse_lid_str};
pub use package_json::{Package, PackageDep, parse_package_json, parse_package_json_str};
