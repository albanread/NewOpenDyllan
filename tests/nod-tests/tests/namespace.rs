//! Sprint 05 namespace integration tests — driven from real LID fixtures
//! under `opendylan-tests/sources/`.

use std::path::{Path, PathBuf};

use nod_namespace::{
    Graph, TargetType, dump_graph, load_lid_chain, parse_lid, parse_package_json,
};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("opendylan-tests").is_dir())
        .expect("opendylan-tests dir not found")
        .join("opendylan-tests")
        .join("sources")
}

#[test]
fn lid_tiny_thread_test() {
    let path = fixtures_root()
        .join("app")
        .join("thread-test")
        .join("thread-test.lid");
    let lid = parse_lid(&path).expect("parse thread-test.lid");
    assert_eq!(lid.library.as_deref(), Some("thread-test"));
    assert_eq!(lid.executable.as_deref(), Some("thread-test"));
    assert_eq!(lid.files, vec!["thread-test"]);
    assert!(lid.diagnostics.is_empty(), "{:?}", lid.diagnostics);
}

#[test]
fn lid_gctest() {
    let path = fixtures_root()
        .join("app")
        .join("gctest")
        .join("gctest.lid");
    let lid = parse_lid(&path).expect("parse gctest.lid");
    assert_eq!(lid.library.as_deref(), Some("gctest"));
    assert_eq!(lid.target_type, Some(TargetType::Executable));
    assert_eq!(lid.start_function.as_deref(), Some("main"));
    assert_eq!(lid.major_version, Some(1));
    assert_eq!(lid.minor_version, Some(0));
    assert_eq!(lid.files, vec!["library", "module", "gctest"]);
}

#[test]
fn lid_kernel_dylan() {
    let path = fixtures_root().join("dylan").join("dylan.lid");
    let lid = parse_lid(&path).expect("parse dylan.lid");
    assert_eq!(lid.library.as_deref(), Some("dylan"));
    assert_eq!(lid.target_type, Some(TargetType::Dll));
    assert_eq!(lid.files.len(), 91, "kernel LID should have 91 files");
    assert_eq!(lid.files.first().map(String::as_str), Some("dfmc-boot"));
    assert_eq!(lid.files.last().map(String::as_str), Some("dylan-spy"));
    assert!(lid.platforms.iter().any(|p| p == "x86_64-linux"));
}

#[test]
fn lid_chain_dylan_win32() {
    let path = fixtures_root().join("dylan").join("dylan-win32.lid");
    let merged = load_lid_chain(&path).expect("load dylan-win32 chain");
    // Child supplied Executable; parent supplied Files; merge should give
    // Executable from child and Files from parent (child has none).
    assert_eq!(merged.executable.as_deref(), Some("DxDYLAN"));
    assert_eq!(merged.library.as_deref(), Some("dylan"));
    assert_eq!(merged.files.len(), 91);
    assert!(merged.platforms.iter().any(|p| p == "x86-win32"));
    // Parent platforms also retained (list-valued merge is a union).
    assert!(merged.platforms.iter().any(|p| p == "x86_64-linux"));
    // Child fields shadow parent for single-valued.
    assert_eq!(merged.base_address.as_deref(), Some("0x66E00000"));
}

#[test]
fn package_json_upstream() {
    // upstream fixture
    let path = Path::new(r"E:\opendylan\dylan-package.json");
    if !path.exists() {
        // Skip on machines without the upstream tree checked out.
        return;
    }
    let pkg = parse_package_json(path).expect("parse dylan-package.json");
    assert_eq!(pkg.name, "opendylan");
    assert!(!pkg.version.is_empty());
    assert!(!pkg.dependencies.is_empty());
    assert!(
        pkg.dependencies.iter().any(|d| d.name == "command-line-parser"),
        "expected at least one known dep",
    );
}

#[test]
fn graph_build_from_kernel() {
    let path = fixtures_root().join("dylan").join("dylan.lid");
    let lid = parse_lid(&path).expect("parse dylan.lid");
    let mut g = Graph::new();
    let lib_id = g.add_library_from_lid(&lid);
    let lib = g.library(lib_id);
    assert_eq!(lib.files.len(), 91);
    let dump = dump_graph(&g);
    assert!(dump.starts_with("digraph"));
    assert!(dump.contains("\"dylan\""));
}
