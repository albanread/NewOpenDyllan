//! STEP 1 of the lasting class-id-stability fix: guard that the name->id
//! pin table (`nod_runtime::class_pins`) stays COMPLETE and consistent.
//!
//! Library classes (runtime-seeded `ensure_*` + stdlib `define class`) get
//! their ids from the pin table by NAME, so adding/removing/reordering a
//! stdlib class never renumbers another class (the root cause of the
//! recurring AOT/shim class-id drift). This test fails the moment a library
//! class is registered that is NOT pinned — telling the author to append one
//! row to `src/nod-runtime/src/class_pins.rs` at `PIN_CEILING`.

use std::path::PathBuf;

#[test]
fn class_pins_complete_and_consistent() {
    use nod_runtime::ClassId;

    // Trigger the FULL compile-time class registration (runtime-seeded
    // `ensure_*` classes + the stdlib `define class`es), exactly as a real
    // `dump-dfm` compile does. A trivial file registers no user classes, so
    // every FIRST_USER-band class seen below is a LIBRARY class.
    let probe: PathBuf = std::env::temp_dir().join("nod_class_pin_probe.dylan");
    std::fs::write(&probe, "define function main () => () end function;\n")
        .expect("write probe file");
    let _ = nod_sema::dump_dfm_for_file(&probe);

    // Collect every registered class in the library band [FIRST_USER, FIRST_SHIM).
    let mut registered: Vec<(String, u32)> = Vec::new();
    nod_runtime::for_each_class(|md| {
        let id = md.id.0;
        if id >= ClassId::FIRST_USER && id < ClassId::FIRST_SHIM {
            registered.push((md.name.clone(), id));
        }
    });
    assert!(
        !registered.is_empty(),
        "no library classes registered — probe compile did not run"
    );

    // 1. Every registered library class is pinned at exactly its registered id.
    let mut missing: Vec<String> = Vec::new();
    for (name, id) in &registered {
        match nod_runtime::class_pins::pinned_id(name) {
            Some(pid) if pid == *id => {}
            Some(pid) => panic!(
                "class `{name}` registered at id {id} but pinned at {pid} in class_pins.rs \
                 — pins must never be renumbered"
            ),
            None => missing.push(format!("{name} (registered id {id})")),
        }
    }
    assert!(
        missing.is_empty(),
        "library class(es) registered but NOT in CLASS_PINS — append each at the next free id \
         (PIN_CEILING) in src/nod-runtime/src/class_pins.rs and bump PIN_CEILING: {missing:?}"
    );

    // 2. PIN_CEILING is exactly one past the highest pinned id (so the user
    //    band starts cleanly above the library band).
    let max_pin = nod_runtime::class_pins::CLASS_PINS
        .iter()
        .map(|(_, i)| *i)
        .max()
        .expect("non-empty pin table");
    assert_eq!(
        nod_runtime::class_pins::PIN_CEILING,
        max_pin + 1,
        "PIN_CEILING must be (max pinned id) + 1"
    );

    // 3. Pinned ids are unique and inside the library band.
    let ids: Vec<u32> = nod_runtime::class_pins::CLASS_PINS.iter().map(|(_, i)| *i).collect();
    let mut sorted = ids.clone();
    let n = sorted.len();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), n, "duplicate pinned ids in CLASS_PINS");
    for &id in &ids {
        assert!(
            id >= ClassId::FIRST_USER && id < ClassId::FIRST_SHIM,
            "pinned id {id} outside the library band [{}, {})",
            ClassId::FIRST_USER,
            ClassId::FIRST_SHIM
        );
    }

    // 4. Pin names are unique.
    let mut names: Vec<&str> = nod_runtime::class_pins::CLASS_PINS.iter().map(|(n, _)| *n).collect();
    let nn = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), nn, "duplicate class names in CLASS_PINS");
}
