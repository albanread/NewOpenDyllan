//! Sprint 38f — on-disk replay wired into `eval_wrapped_source`.
//!
//! Sprint 38a–38e shipped the cross-process bitcode replay infrastructure
//! (manifest, slot allocators, `Jit::add_module_from_bitcode`). Sprint
//! 38f closes the integration gap: `eval_expr_to_string` now tries the
//! on-disk path before falling through to a cold compile.
//!
//! These tests assert the integration behaviour:
//!
//!   1. A round-trip through the disk cache produces the same answer in
//!      a fresh-in-process scenario (cleared in-process cache, populated
//!      on-disk cache). Distinguished from `jit_cache_xprocess.rs`'s
//!      infrastructure tests by exercising the **eval entry point** —
//!      i.e. the user-facing `eval_expr_to_string` API.
//!
//!   2. Each replay-success increments `record_disk_hit`; each
//!      sidecar-missing / ABI-mismatch decision falls through to the
//!      cold path via `record_disk_miss` instead. This is what proves
//!      Sprint 38f's wiring actually fires — pre-Sprint-38f the disk
//!      cache existed but was never consulted, so the headline ≥10×
//!      cross-process speedup never landed.
//!
//!   3. Dispatch + Win32 expressions round-trip alongside the trivial
//!      `1 + 2` case, exercising all of Sprint 38b/c/d/e's bake-site
//!      categories through the disk path.
//!
//!   4. A pure-unit test of the sidecar JSON encoder/decoder, matching
//!      `jit_cache_xprocess.rs::manifest_round_trips_through_disk`'s
//!      role for the manifest sidecar.

use std::path::PathBuf;

use nod_sema::eval_expr_to_string;
use nod_sema::sidecar::{
    PersistedBlock, PersistedFunction, PersistedHandler, PersistedMethod, REGISTRATIONS_ABI_VERSION,
    RegistrationSidecar,
};
use serial_test::serial;

fn test_cache_dir(name: &str) -> PathBuf {
    let mut dir = nod_llvm::default_cache_dir();
    dir.push(format!("xproc-eval-{name}"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Wrap the expression the same way `eval_expr_to_string` does so we
/// can compute the cache key the disk cache will key on. Mirrors the
/// `body_wrapped` shape in `nod_sema::eval_expr_to_string`.
fn wrapped_source_for(expr: &str) -> String {
    let trimmed = expr.trim();
    let body = if trimmed.starts_with("let ") || trimmed.starts_with("let\t") {
        trimmed.strip_suffix("end").map(str::trim_end).unwrap_or(trimmed)
    } else {
        trimmed
    };
    format!(
        "Module: __eval__\n\
         define function <eval-entry> ()\n  {body}\nend;\n"
    )
}

/// Reset all eval-relevant caching state. Tests rely on this to
/// observe disk-hit / cold-miss transitions in isolation.
fn reset_eval_caches(dir: &std::path::Path) {
    // SAFETY: env mutation is serialised by the `#[serial]` attribute.
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", dir) };
    nod_llvm::clear_cache_dir(dir);
    // Also clear `<key>.manifest.json` + `<key>.registrations.json`
    // siblings that `clear_cache_dir` ignores (its pattern is
    // `.bc` + `.json` only — the new sibling files don't match).
    if let Ok(rd) = std::fs::read_dir(dir) {
        for de in rd.flatten() {
            let p = de.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str())
                && (name.ends_with(".manifest.json")
                    || name.ends_with(".registrations.json"))
            {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    nod_llvm::in_process_clear();
    nod_llvm::reset_stats();
}

#[test]
#[serial]
fn sprint38f_disk_replay_round_trip_simple() {
    // Headline: a fresh-in-process eval, cleared in-process cache,
    // then a second eval. The second eval MUST hit disk (not re-run
    // the cold pipeline). Pre-Sprint-38f this test would fail because
    // `eval_wrapped_source` never read bitcode from disk.
    let dir = test_cache_dir("simple-rt");
    reset_eval_caches(&dir);

    // First call: cold compile, populates the on-disk cache.
    let cold = eval_expr_to_string("1 + 2").expect("cold eval");
    assert_eq!(cold, "3");
    let (dh0, dm0) = nod_llvm::disk_cache_stats();
    assert_eq!(
        dm0, 1,
        "first call should record one disk miss (no sidecars yet)"
    );
    assert_eq!(dh0, 0, "no disk hits before any cache files exist");

    // Clear the in-process cache so the second call is forced through
    // the disk path. The on-disk sidecars from the cold compile must
    // still be there.
    nod_llvm::in_process_clear();

    let warm = eval_expr_to_string("1 + 2").expect("warm eval");
    assert_eq!(warm, "3", "disk replay produces same answer");
    let (dh1, dm1) = nod_llvm::disk_cache_stats();
    assert_eq!(
        dh1, 1,
        "second call after in_process_clear should record a disk hit"
    );
    assert_eq!(dm1, dm0, "no new disk miss on the second call");

    // For the sample-payload requirement in the brief: locate the
    // registration sidecar file we just wrote and print it. Done as a
    // side-effect of the test so the commit message can paste it.
    if std::env::var("NOD_SPRINT38F_DUMP_REGS").is_ok() {
        for de in std::fs::read_dir(&dir).expect("readdir").flatten() {
            let p = de.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str())
                && name.ends_with(".registrations.json")
            {
                eprintln!("--- {name} ---");
                if let Ok(text) = std::fs::read_to_string(&p) {
                    eprintln!("{text}");
                }
            }
        }
    }
    // Confirm the wrapped source string keys into the same file the
    // disk cache wrote (sanity check on the wrapping logic).
    let key = nod_llvm::cache_key_for_dfm(&wrapped_source_for("1 + 2"));
    let regs_path = dir.join(format!("{}.registrations.json", key.to_hex()));
    assert!(
        regs_path.exists(),
        "expected registration sidecar at {regs_path:?}"
    );

    reset_eval_caches(&dir);
}

#[test]
#[serial]
fn sprint38f_disk_replay_round_trip_dispatch() {
    // `size(make(<range>, ...))` exercises Sprint 38c's class metadata
    // bake site, Sprint 38e's cache-slot and generic-pointer bake
    // sites, plus the regular runtime shim externs. All must
    // round-trip through the disk cache.
    let dir = test_cache_dir("dispatch-rt");
    reset_eval_caches(&dir);

    let cold = eval_expr_to_string("size(make(<range>, from: 0, to: 5))")
        .expect("cold dispatch eval");
    assert_eq!(cold, "6", "<range> from 0 to 5 has size 6 (inclusive)");

    nod_llvm::in_process_clear();
    let (dh0, _) = nod_llvm::disk_cache_stats();
    let warm = eval_expr_to_string("size(make(<range>, from: 0, to: 5))")
        .expect("disk dispatch eval");
    assert_eq!(warm, "6", "disk replay produces same dispatch result");
    let (dh1, _) = nod_llvm::disk_cache_stats();
    assert_eq!(
        dh1,
        dh0 + 1,
        "dispatch round-trip should produce one disk hit"
    );

    reset_eval_caches(&dir);
}

#[test]
#[serial]
fn sprint38f_disk_replay_round_trip_winapi() {
    // `GetTickCount()` exercises Sprint 38d's stub-entry bake site —
    // the manifest carries a `RelocKind::StubEntry`, the replay path
    // calls `stub_entry_slot_addr`, which eagerly LoadLibrary's
    // kernel32.dll and resolves GetProcAddress("GetTickCount"). This
    // is the test that proves `initialize_module_winffi` truly is
    // redundant on the disk path.
    //
    // GetTickCount's return value changes between calls (it's the
    // millisecond uptime), so we only check both calls succeeded and
    // returned a non-negative integer.
    let dir = test_cache_dir("winapi-rt");
    reset_eval_caches(&dir);

    let cold = eval_expr_to_string("GetTickCount()").expect("cold winapi eval");
    let cold_val: u64 = cold.parse().expect("cold result is integer");
    // GetTickCount on a running system is always positive; the value
    // doesn't matter past that.
    assert!(cold_val > 0, "cold GetTickCount returned {cold_val}");

    nod_llvm::in_process_clear();
    let (dh0, _) = nod_llvm::disk_cache_stats();
    let warm = eval_expr_to_string("GetTickCount()").expect("warm winapi eval");
    let warm_val: u64 = warm.parse().expect("warm result is integer");
    assert!(warm_val > 0, "warm GetTickCount returned {warm_val}");
    let (dh1, _) = nod_llvm::disk_cache_stats();
    assert_eq!(
        dh1,
        dh0 + 1,
        "winapi round-trip should produce one disk hit"
    );

    reset_eval_caches(&dir);
}

#[test]
#[serial]
fn sprint38f_disk_replay_abi_mismatch_falls_through() {
    // Pre-write a registration sidecar with a bogus abi_version under
    // a key that DOES match what `1 + 2` will produce. The on-disk
    // path must reject it, increment `record_disk_miss`, and fall
    // through to the cold compile. The cold compile then overwrites
    // the bad sidecar with a fresh one.
    let dir = test_cache_dir("abi-mismatch");
    reset_eval_caches(&dir);

    let key = nod_llvm::cache_key_for_dfm(&wrapped_source_for("1 + 2"));
    // Also pre-write a fake bitcode + manifest with the same key so
    // `read_cache_entry_with_manifest` doesn't fail FIRST (the disk
    // miss we want to assert is the registration-sidecar one).
    let fake_manifest = nod_llvm::ModuleManifest::new(key);
    nod_llvm::write_cache_entry_with_manifest(&dir, key, &[0u8; 64], &fake_manifest);
    // The bitcode is junk so even if the registration sidecar were
    // valid, `add_module_from_bitcode` would fail. Pre-write a bogus
    // registration sidecar that the ABI check rejects FIRST so we
    // never reach bitcode parsing.
    let bad_regs = RegistrationSidecar {
        abi_version: 9999, // wrong
        return_type_tag: 0,
        return_type_payload: 0,
        functions: vec![],
        methods: vec![],
        blocks: vec![],
        variables: vec![],
    };
    bad_regs.write(&dir, key);

    let result = eval_expr_to_string("1 + 2").expect("cold compile despite bad sidecar");
    assert_eq!(result, "3");
    let (dh, dm) = nod_llvm::disk_cache_stats();
    // Note: the bitcode + manifest pair we pre-wrote means
    // `read_cache_entry_with_manifest` returns `Some(...)`. The
    // registration sidecar then fails the ABI check, which records
    // exactly one disk miss. The cold compile path then writes a
    // fresh sidecar trio, but doesn't touch the disk-hit/miss
    // counters.
    assert_eq!(dm, 1, "ABI mismatch should record exactly one disk miss");
    assert_eq!(dh, 0, "ABI mismatch must not record a disk hit");

    reset_eval_caches(&dir);
}

#[test]
#[serial]
fn sprint38f_disk_replay_missing_sidecar_falls_through() {
    // Pre-write bitcode + manifest but NOT the registrations sidecar.
    // The disk path must miss on the registrations file specifically
    // (not crash, not silently fall through without counting).
    let dir = test_cache_dir("missing-regs");
    reset_eval_caches(&dir);

    let key = nod_llvm::cache_key_for_dfm(&wrapped_source_for("1 + 2"));
    let manifest = nod_llvm::ModuleManifest::new(key);
    nod_llvm::write_cache_entry_with_manifest(&dir, key, &[0u8; 64], &manifest);
    // Deliberately NOT writing a `<key>.registrations.json`.

    let result = eval_expr_to_string("1 + 2").expect("cold compile despite missing sidecar");
    assert_eq!(result, "3");
    let (dh, dm) = nod_llvm::disk_cache_stats();
    assert_eq!(
        dm, 1,
        "missing registrations sidecar should record one disk miss"
    );
    assert_eq!(dh, 0, "missing sidecar must not record a disk hit");

    reset_eval_caches(&dir);
}

#[test]
#[serial]
fn sprint38f_registrations_sidecar_round_trips() {
    // Pure-unit round-trip through encode/decode (no eval pipeline
    // involvement). Mirrors `manifest_round_trips_through_disk` for
    // the manifest sidecar. Catches schema-level regressions
    // independent of the eval integration.
    let dir = test_cache_dir("regs-unit");
    reset_eval_caches(&dir);

    let key = nod_llvm::CacheKey([1, 2, 3, 4]);
    let s = RegistrationSidecar {
        abi_version: REGISTRATIONS_ABI_VERSION,
        return_type_tag: 9, // Class(<some-id>)
        return_type_payload: 123,
        functions: vec![
            PersistedFunction {
                name: "foo".into(),
                arity: 2,
                is_closure: false,
                source_arity: 2,
            },
            PersistedFunction {
                name: "make-counter".into(),
                arity: 0,
                is_closure: true,
                source_arity: 0,
            },
        ],
        methods: vec![PersistedMethod {
            generic_name: "+".into(),
            specialisers: vec![1, 1],
            body_fn_name: "+$1$1".into(),
            param_count: 2,
        }],
        blocks: vec![PersistedBlock {
            block_id: 0x1234_5678,
            body_fn_name: "block-body".into(),
            cleanup_fn_name: Some("block-cleanup".into()),
            afterwards_fn_name: None,
            handlers: vec![PersistedHandler {
                class_id: 42,
                class_name: "<simple-error>".into(),
                body_fn_name: "block-handler".into(),
            }],
        }],
        variables: vec![],
    };

    s.write(&dir, key);
    let back = RegistrationSidecar::read(&dir, key).expect("read");
    assert_eq!(s, back);
    assert!(back.is_abi_compatible());

    reset_eval_caches(&dir);
}
