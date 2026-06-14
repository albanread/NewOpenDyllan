//! Sprint 53.1 — `dump-sema` oracle gate.
//!
//! `nod_sema::dump_sema_for_file` serialises the sema *recording* model
//! (top-names, generics, classes, sealing) — the `SemaModel` Sprint 53
//! ports to Dylan — as deterministic text. This is the byte-identical
//! oracle the Dylan-computed model will be checked against.
//!
//! These tests run the dump in-process (the test binary doesn't link the
//! front-end shim, so they avoid the pre-existing `dump-sema`-under-shim
//! class-id-drift CLI crash) and assert the model is deterministic and
//! captures the expected facts for a class-heavy fixture.
//!
//! Run with:
//!   cargo test -p nod-tests --test sema_dump -- --nocapture

use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[test]
fn dump_sema_point_is_complete() {
    // NOTE: `dump_sema_for_file` registers the module's classes in the
    // process-global class registry, so it is NOT idempotent within one
    // process (a second call would hit `ClassRedefinitionNotSupported`).
    // The dump's determinism comes from the sorted tables, not from
    // re-running. Call once; the verify-mode (later) computes the two
    // models in separate processes for the same reason.
    let path = fixtures_dir().join("point.dylan");
    let a = nod_sema::dump_sema_for_file(&path).expect("dump-sema point.dylan");

    // The four model sections, in order.
    let secs = ["=== top-names ===", "=== generics ===", "=== classes ===", "=== sealing ==="];
    let mut last = 0;
    for sec in secs {
        let at = a.find(sec).unwrap_or_else(|| panic!("missing section {sec}:\n{a}"));
        assert!(at >= last, "sections out of order at {sec}:\n{a}");
        last = at;
    }

    // Class record: CPL via C3, slot layout with offsets + origins.
    for needle in [
        "class <user-point>",
        "  cpl [<user-point>, <object>]",
        "  slot x @8 setter=true origin=<user-point>",
        "  slot y @16 setter=true origin=<user-point>",
        // Auto-generated slot generics + the user function (with arity).
        "generic x",
        "generic y",
        "fn distance-squared arity=1",
    ] {
        assert!(a.contains(needle), "dump-sema missing {needle:?}:\n{a}");
    }
}
