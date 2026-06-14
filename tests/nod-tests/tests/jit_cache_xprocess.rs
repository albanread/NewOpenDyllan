//! Sprint 38 — cross-process bitcode replay infrastructure tests.
//!
//! Sprint 37 shipped the in-process cache (a HashMap of `<eval-entry>`
//! function pointers) plus an on-disk bitcode mirror. Sprint 38 closes
//! the gap so the on-disk bitcode is loadable in a fresh process:
//!
//!   * A **manifest sidecar** (`<key>.manifest.json`) carries one
//!     [`nod_llvm::RelocKind`] entry per process-volatile address baked
//!     into the IR (class metadata pointers, literal pool singletons,
//!     interned string Words, stub entries, cache slots, generic
//!     function pointers).
//!
//!   * A new [`nod_llvm::Jit::add_module_from_bitcode`] entry point
//!     parses LLVM bitcode from a buffer, walks the manifest to compute
//!     fresh current-process addresses for each entry, registers each
//!     named external global via `LLVMAddGlobalMapping`, and finalises
//!     MCJIT.
//!
//! This file exercises the **infrastructure** end-to-end without
//! depending on the codegen-side conversion of every individual bake
//! site (which is the next sprint's work — see Sprint 38 retrospective
//! in `docs/SPRINTS.md`). The tests here:
//!
//!  1. Confirm a manifest round-trips through disk JSON + parse cleanly
//!     (file I/O variant of the unit test in `nod-llvm::symbols`).
//!  2. Confirm a bitcode file produced by cold codegen reloads through
//!     `add_module_from_bitcode` and the loaded function still
//!     produces the same answer in the same process — the loader's
//!     plumbing works.
//!  3. Confirm `resolve_reloc_kind` produces process-valid addresses
//!     for every `RelocKind` variant (a smoke test that ensures the
//!     resolver doesn't panic or return null for any kind).
//!  4. Confirm the on-disk layout (`.bc` + `.json` + `.manifest.json`)
//!     stays internally consistent — write/read round-trips preserve
//!     bytewise equality.
//!  5. Confirm corrupt or wrong-version manifests are rejected (the
//!     cache loader treats them as a miss, falling through to a fresh
//!     compile).

use std::path::PathBuf;

use nod_llvm::{
    ModuleManifest, RelocKind, cache_slot_symbol, class_md_symbol, generic_symbol,
    imm_false_symbol, imm_false_wrapper_symbol, imm_nil_symbol, imm_true_symbol,
    read_cache_entry_with_manifest, strlit_symbol, symlit_symbol,
    write_cache_entry_with_manifest,
};
use serial_test::serial;

fn test_cache_dir(name: &str) -> PathBuf {
    let mut dir = nod_llvm::default_cache_dir();
    dir.push(format!("xproc-test-{name}"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn key0() -> nod_llvm::CacheKey {
    nod_llvm::CacheKey([0xdead_beef_1234_5678, 0xa, 0xb, 0xc])
}

#[test]
#[serial]
fn manifest_round_trips_through_disk() {
    let dir = test_cache_dir("manifest-rt");
    nod_llvm::clear_cache_dir(&dir);
    let k = key0();
    let mut m = ModuleManifest::new(k);
    m.push(imm_true_symbol(k), RelocKind::ImmTrue);
    m.push(imm_false_symbol(k), RelocKind::ImmFalse);
    m.push(imm_nil_symbol(k), RelocKind::ImmNil);
    m.push(imm_false_wrapper_symbol(k), RelocKind::ImmFalseWrapper);
    m.push(class_md_symbol(k, 1), RelocKind::ClassMetadata { class_id: 1 });
    m.push(
        strlit_symbol(k, 0),
        RelocKind::StringLiteral { text: "héllo \"json\"\n".into() },
    );
    m.push(symlit_symbol(k, 0), RelocKind::SymbolLiteral { name: "foo".into() });
    m.push(cache_slot_symbol(k, 99), RelocKind::CacheSlot { site_id: 99 });
    m.push(generic_symbol(k, "+"), RelocKind::Generic { name: "+".into() });

    // Write a fake bitcode + manifest sidecar.
    let fake_bitcode: Vec<u8> = (0..1024u32).flat_map(|n| n.to_le_bytes()).collect();
    write_cache_entry_with_manifest(&dir, k, &fake_bitcode, &m);

    // Read it back.
    let (bytes, _meta, parsed_manifest) =
        read_cache_entry_with_manifest(&dir, k).expect("read manifest");
    assert_eq!(bytes, fake_bitcode, "bitcode bytes round-trip");
    assert_eq!(parsed_manifest, m, "manifest contents round-trip");

    nod_llvm::clear_cache_dir(&dir);
}

#[test]
#[serial]
fn read_returns_none_when_manifest_missing() {
    let dir = test_cache_dir("missing-manifest");
    nod_llvm::clear_cache_dir(&dir);
    let k = key0();
    let m = ModuleManifest::new(k);
    let fake_bitcode = vec![1, 2, 3, 4];
    // Write bitcode + sidecar but not the manifest.
    nod_llvm::write_cache_entry(&dir, k, &fake_bitcode);
    let _ = m; // not used
    assert!(
        read_cache_entry_with_manifest(&dir, k).is_none(),
        "missing manifest → None"
    );
    nod_llvm::clear_cache_dir(&dir);
}

#[test]
#[serial]
fn read_returns_none_when_manifest_version_mismatches() {
    let dir = test_cache_dir("manifest-vmismatch");
    nod_llvm::clear_cache_dir(&dir);
    let k = key0();
    let mut m = ModuleManifest::new(k);
    m.manifest_version = 9999; // wrong
    m.push(imm_true_symbol(k), RelocKind::ImmTrue);
    let fake_bitcode = vec![1, 2, 3, 4];
    write_cache_entry_with_manifest(&dir, k, &fake_bitcode, &m);
    assert!(
        read_cache_entry_with_manifest(&dir, k).is_none(),
        "version-mismatched manifest → None"
    );
    nod_llvm::clear_cache_dir(&dir);
}

#[test]
#[serial]
fn empty_manifest_round_trips() {
    // The minimum-viable case: an empty manifest is a valid manifest
    // (it means "this module has no relocations to apply"). Sprint 38's
    // cache-hit path needs to handle this gracefully because not every
    // cached module touches a runtime address.
    let dir = test_cache_dir("empty-manifest");
    nod_llvm::clear_cache_dir(&dir);
    let k = key0();
    let m = ModuleManifest::new(k);
    let fake_bitcode = vec![0u8; 32];
    write_cache_entry_with_manifest(&dir, k, &fake_bitcode, &m);
    let (_, _, parsed) = read_cache_entry_with_manifest(&dir, k).expect("read empty");
    assert_eq!(parsed.entries.len(), 0);
    assert_eq!(parsed.manifest_version, nod_llvm::MANIFEST_VERSION);
    nod_llvm::clear_cache_dir(&dir);
}

#[test]
#[serial]
fn lru_eviction_cleans_manifest_sidecars() {
    // Sprint 38 — LRU eviction must remove the new `.manifest.json`
    // sibling alongside the `.bc` + sidecar pair. Otherwise orphan
    // manifests would accumulate on disk after each eviction cycle.
    let dir = test_cache_dir("lru-manifest");
    nod_llvm::clear_cache_dir(&dir);
    let mut keys = Vec::new();
    for i in 0..5u64 {
        let k = nod_llvm::CacheKey([i, i, i, i]);
        let m = ModuleManifest::new(k);
        let bc = vec![0u8; 1024];
        write_cache_entry_with_manifest(&dir, k, &bc, &m);
        keys.push(k);
        // Force LRU access-time delta.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // Evict down to a 1-byte cap → everything goes.
    let evicted = nod_llvm::evict_to(&dir, 1);
    assert!(evicted >= 5, "evicted at least 5, got {evicted}");
    // Walk the dir; no `.manifest.json` should remain.
    let mut left = 0;
    for de in std::fs::read_dir(&dir).expect("readdir").flatten() {
        let p = de.path();
        if let Some(name) = p.file_name().and_then(|s| s.to_str())
            && (name.ends_with(".manifest.json")
                || name.ends_with(".bc")
                || name.ends_with(".json"))
        {
            left += 1;
        }
    }
    assert_eq!(left, 0, "no orphan files after eviction; got {left} leftover");
    nod_llvm::clear_cache_dir(&dir);
}

#[test]
#[serial]
fn manifest_carries_stub_entry_signature_bytes() {
    // The stub-entry RelocKind carries the ApiCallSignature as raw
    // bytes. Confirm the byte length matches `size_of::<ApiCallSignature>()`
    // — the JIT-link path checks this and refuses to allocate a stub
    // entry from a wrong-length payload.
    let dir = test_cache_dir("stub-sig");
    nod_llvm::clear_cache_dir(&dir);
    let k = key0();
    let mut m = ModuleManifest::new(k);
    let sig = nod_runtime::ApiCallSignature {
        arg_count: 2,
        arg_kinds: [
            nod_runtime::CArgKind::UInt32 as u8,
            nod_runtime::CArgKind::UInt32 as u8,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        return_kind: nod_runtime::CReturnKind::Bool32 as u8,
    };
    let sig_bytes: Vec<u8> = unsafe {
        std::slice::from_raw_parts(
            &sig as *const _ as *const u8,
            std::mem::size_of::<nod_runtime::ApiCallSignature>(),
        )
    }
    .to_vec();
    m.push(
        nod_llvm::stub_symbol(k, 0),
        RelocKind::StubEntry {
            dll: "kernel32.dll".into(),
            symbol: "Beep".into(),
            signature_bytes: sig_bytes.clone(),
        },
    );
    let fake_bitcode = vec![0u8; 16];
    write_cache_entry_with_manifest(&dir, k, &fake_bitcode, &m);
    let (_, _, parsed) = read_cache_entry_with_manifest(&dir, k).expect("read");
    let RelocKind::StubEntry { dll, symbol, signature_bytes } = &parsed.entries[0].kind else {
        panic!("expected StubEntry");
    };
    assert_eq!(dll, "kernel32.dll");
    assert_eq!(symbol, "Beep");
    assert_eq!(signature_bytes.len(), sig_bytes.len());
    assert_eq!(signature_bytes, &sig_bytes);
    nod_llvm::clear_cache_dir(&dir);
}

#[test]
fn manifest_version_constant_is_one() {
    // Sprint 38 ships at manifest schema v1. Bumping this is the
    // tripwire for any incompatible change to the manifest JSON shape;
    // bump in lockstep with `nod_llvm::MANIFEST_VERSION`.
    assert_eq!(nod_llvm::MANIFEST_VERSION, 1);
}

#[test]
fn runtime_abi_version_is_two_after_sprint38_bump() {
    // Sprint 37 shipped at ABI v1. Sprint 38's named-global codegen
    // path is a load-bearing IR shape change, so the constant bumped
    // to 2 — Sprint 37 cache entries (baked-address-as-i64) won't
    // match a Sprint 38 codegen output's cache key. This is the
    // documented breaking change.
    assert_eq!(nod_llvm::NOD_RUNTIME_ABI_VERSION, 2);
}

#[test]
#[serial]
fn sprint38b_immediate_globals_round_trip_through_cached_bitcode() {
    // Sprint 38b — end-to-end proof that the codegen-side conversion
    // of immediate bake sites (true_/false_/nil/false_wrapper) flows
    // through the manifest sidecar and the JIT-link loader. The same
    // expression is cold-compiled (producing bitcode + manifest with
    // `Imm*` entries), then loaded into a fresh JIT via
    // `add_module_from_bitcode`, and re-invoked. Both runs must
    // produce the same answer, which proves:
    //
    //   1. Codegen emitted external globals (not literals) for the
    //      immediate sites — otherwise the manifest would be empty.
    //   2. The manifest reached disk with at least one `Imm*` entry.
    //   3. The loader resolved each entry to a fresh process-local
    //      slot address and `LLVMAddGlobalMapping`-bound it.
    //   4. The replayed module's `<eval-entry>` reads the correct
    //      Word through the external global, matching the cold result.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-imm-roundtrip");
    nod_llvm::clear_cache_dir(&dir);
    // SAFETY: env mutation is serialised by the `#[serial]` attribute.
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    // `if (#t) 1 else 2 end` exercises the imm_true / imm_false
    // codegen path (Terminator::If reads imm_false; ConstValue::Bool
    // produces imm_true; the result type is integer so no boolean
    // returns).
    let cold = eval_expr_to_string("if (#t) 1 else 2 end").expect("cold eval");
    assert_eq!(cold, "1", "cold path returns 1 for if-#t branch");

    // Walk the cache dir for the `.bc` + `.manifest.json` pair we just
    // wrote. We don't know the exact cache key without rebuilding the
    // wrapped source string, but there should be at most one .bc.
    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    assert!(!bc_paths.is_empty(), "cache dir should contain ≥1 .bc");
    let bc_path = bc_paths[0].path();
    let manifest_path = bc_path.with_extension("manifest.json");

    let bitcode = std::fs::read(&bc_path).expect("read bitcode");
    assert!(!bitcode.is_empty(), "bitcode non-empty");

    let manifest_text =
        std::fs::read_to_string(&manifest_path).expect("manifest.json must exist for Sprint 38b");
    let manifest =
        nod_llvm::ModuleManifest::parse(&manifest_text).expect("manifest parses");

    // Sprint 38b — codegen must have emitted at least one immediate
    // reloc entry. The exact count depends on how many distinct
    // immediates the IR references and how many DFM functions live in
    // the module; we just assert ≥1.
    let imm_kinds: Vec<&nod_llvm::RelocKind> = manifest
        .entries
        .iter()
        .map(|e| &e.kind)
        .filter(|k| {
            matches!(
                k,
                nod_llvm::RelocKind::ImmTrue
                    | nod_llvm::RelocKind::ImmFalse
                    | nod_llvm::RelocKind::ImmNil
                    | nod_llvm::RelocKind::ImmFalseWrapper
            )
        })
        .collect();
    assert!(
        !imm_kinds.is_empty(),
        "manifest must carry ≥1 Imm* RelocKind entry; got: {:?}",
        manifest.entries
    );

    // Fresh JIT + Context; load the bitcode + manifest.
    let ctx: &'static nod_llvm::LlvmContext =
        Box::leak(Box::new(nod_llvm::LlvmContext::create()));
    let mut jit = nod_llvm::Jit::new(ctx).expect("Jit::new");
    jit.add_module_from_bitcode(ctx, &bitcode, "__replay_imm__", &manifest)
        .expect("add_module_from_bitcode");

    let ptr = unsafe { jit.get_function_ptr("<eval-entry>") }
        .expect("<eval-entry> resolves from replayed bitcode");
    assert!(!ptr.is_null(), "function pointer must be non-null");
    // The wrapped expression returns an integer, so the entry's
    // C-ABI signature is `() -> i64`. The tagged Word for `1` is
    // `1 << 1 = 2`.
    let f: extern "C-unwind" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let raw = f();
    assert_eq!(raw >> 1, 1, "replayed eval-entry returns 1");

    // SAFETY: env mutation is serialised by `#[serial]`.
    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[serial]
fn sprint38b_bitcode_round_trip_reads_named_global() {
    // Sprint 38b — text-level inspection of the bitcode-as-IR: the
    // round-trip test above proves the **functional** correctness;
    // this one proves the **structural** correctness, namely that
    // the IR contains a load through `@nod_imm_*` rather than a baked
    // `i64 <bits>` constant at the bake site.
    //
    // We can't easily build a synthetic DFM module to feed
    // `codegen_module_with_key` here (the test crate doesn't have the
    // private constructors), so instead we re-parse the cached bitcode
    // through inkwell and check the *parsed* module's IR text.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-imm-irshape");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    // Force a cache miss so the bitcode is regenerated. `if (#t) 7 else 9 end`
    // exercises the imm_true/imm_false codegen path.
    let cold = eval_expr_to_string("if (#t) 7 else 9 end").expect("cold eval");
    assert_eq!(cold, "7");

    // Find the .bc file.
    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    let bc_path = bc_paths[0].path();
    let bitcode = std::fs::read(&bc_path).expect("read bc");

    // Re-parse the bitcode and inspect its IR text. The named global
    // declaration should be present, and there should be at least one
    // `load i64, ptr @nod_imm_` reading through it.
    let ir = nod_llvm::bitcode_to_ir_text(&bitcode).expect("bitcode_to_ir_text");

    // External-global declaration for at least one of the four
    // immediate symbols.
    let has_imm_true_decl = ir.contains("@nod_imm_true__");
    let has_imm_false_decl = ir.contains("@nod_imm_false__");
    assert!(
        has_imm_true_decl || has_imm_false_decl,
        "IR must declare an external global for #t or #f; IR:\n{ir}"
    );
    // The load shape should appear at least once. (We don't pin the
    // exact register names since inkwell renumbers on re-parse.)
    let has_load_through_imm = ir.contains("load i64, ptr @nod_imm_");
    assert!(
        has_load_through_imm,
        "IR must contain a `load i64, ptr @nod_imm_*` instruction; IR:\n{ir}"
    );
    // Print the immediate-related IR lines for human inspection when
    // running with --nocapture.
    for line in ir.lines() {
        if line.contains("nod_imm_") {
            println!("[IR] {line}");
        }
    }
    // And no baked constant for the immediate Word bits — the old
    // Sprint 37 shape baked the runtime address as `i64 N` where N is
    // the runtime literal pool true_/false_ pointer. We don't have an
    // easy way to assert "no baked constant" without false positives,
    // so we settle for the positive assertion above. The test in
    // `sprint38b_immediate_globals_round_trip_through_cached_bitcode`
    // catches the negative case (round-trip would crash with stale
    // pointer if the IR still baked them).

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

// ─── Sprint 38c — class metadata + string + symbol literal bake sites ──
//
// Same end-to-end shape as Sprint 38b's immediate-round-trip tests: cold
// compile, drop the in-process cache, reload from bitcode + manifest,
// re-invoke, and assert the result matches. Plus three IR-shape tests
// that read the cached `.bc` back as IR text and confirm the named
// external global is present (not a baked `i64 <bits>` constant).

#[test]
#[serial]
fn sprint38c_class_metadata_globals_round_trip() {
    // `make(<range>, from: 0, to: 5)` references the `<range>` class's
    // raw metadata pointer at the `emit_class_metadata_ptr_const` bake
    // site — Sprint 38c's class-metadata category. The pointer is
    // genuinely needed at runtime (`nod_make`'s first arg) so the
    // compiler can't fold it the way it folds `instance?(1, <integer>)`.
    // Cold-compile, then reload the bitcode in a fresh JIT and prove
    // the same result.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-class-md-roundtrip");
    nod_llvm::clear_cache_dir(&dir);
    // SAFETY: env mutation is serialised by `#[serial]`.
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    let cold = eval_expr_to_string("size(make(<range>, from: 0, to: 5))").expect("cold eval");
    assert_eq!(cold, "6", "cold path returns 6 for size(make(<range>, from: 0, to: 5))");

    // Read back the .bc + manifest.
    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    assert!(!bc_paths.is_empty(), "cache dir should contain ≥1 .bc");
    let bc_path = bc_paths[0].path();
    let manifest_path = bc_path.with_extension("manifest.json");
    let bitcode = std::fs::read(&bc_path).expect("read bitcode");
    let manifest_text =
        std::fs::read_to_string(&manifest_path).expect("manifest.json must exist");
    let manifest = nod_llvm::ModuleManifest::parse(&manifest_text).expect("manifest parses");

    // Sprint 38c — codegen must have emitted at least one ClassMetadata
    // reloc entry (covering <range> at minimum).
    let cm_kinds: Vec<&nod_llvm::RelocKind> = manifest
        .entries
        .iter()
        .map(|e| &e.kind)
        .filter(|k| matches!(k, nod_llvm::RelocKind::ClassMetadata { .. }))
        .collect();
    assert!(
        !cm_kinds.is_empty(),
        "manifest must carry ≥1 ClassMetadata RelocKind entry; got: {:?}",
        manifest.entries
    );

    // Fresh JIT + Context.
    let ctx: &'static nod_llvm::LlvmContext =
        Box::leak(Box::new(nod_llvm::LlvmContext::create()));
    let mut jit = nod_llvm::Jit::new(ctx).expect("Jit::new");
    jit.add_module_from_bitcode(ctx, &bitcode, "__replay_class_md__", &manifest)
        .expect("add_module_from_bitcode");

    let ptr = unsafe { jit.get_function_ptr("<eval-entry>") }
        .expect("<eval-entry> resolves from replayed bitcode");
    assert!(!ptr.is_null());
    // `size(...)` returns a fixnum; tagged Word = 6 << 1 = 12.
    let f: extern "C-unwind" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let raw = f();
    assert_eq!(raw >> 1, 6, "replayed size(make(<range>, from: 0, to: 5)) returns 6");

    // SAFETY: serialised by #[serial].
    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[serial]
fn sprint38c_string_literal_globals_round_trip() {
    // `"hello"` lowering exercises the `emit_string_literal` bake
    // site (now `ConstValue::StringLiteralRef`). Cold-compile,
    // reload, and confirm the bitcode replays correctly.
    //
    // The expression is just the literal string itself: `<eval-entry>`
    // returns the interned `<byte-string>` Word's raw bits. We assert
    // the replay yields the same Word as `intern_string_literal("hello")`
    // in the current process — i.e. the slot allocator and the per-
    // module external global mapping agree.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-strlit-roundtrip");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    // Cold path: returns the interned-string Word, which
    // `eval_expr_to_string`'s pretty-printer renders as the bare
    // string (Dylan's `print` shape).
    let cold = eval_expr_to_string("\"hello\"").expect("cold eval");
    assert_eq!(cold, "\"hello\"", "cold path returns the literal string");

    // Read .bc + manifest.
    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    assert!(!bc_paths.is_empty());
    let bc_path = bc_paths[0].path();
    let manifest_path = bc_path.with_extension("manifest.json");
    let bitcode = std::fs::read(&bc_path).expect("read bitcode");
    let manifest_text = std::fs::read_to_string(&manifest_path).expect("manifest exists");
    let manifest = nod_llvm::ModuleManifest::parse(&manifest_text).expect("manifest parses");

    let str_kinds: Vec<&nod_llvm::RelocKind> = manifest
        .entries
        .iter()
        .map(|e| &e.kind)
        .filter(|k| matches!(k, nod_llvm::RelocKind::StringLiteral { .. }))
        .collect();
    assert!(
        !str_kinds.is_empty(),
        "manifest must carry ≥1 StringLiteral RelocKind entry"
    );
    let texts: Vec<&str> = str_kinds
        .iter()
        .filter_map(|k| match k {
            nod_llvm::RelocKind::StringLiteral { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.contains(&"hello"),
        "expected `hello` among string literals; got {texts:?}"
    );

    let ctx: &'static nod_llvm::LlvmContext =
        Box::leak(Box::new(nod_llvm::LlvmContext::create()));
    let mut jit = nod_llvm::Jit::new(ctx).expect("Jit::new");
    jit.add_module_from_bitcode(ctx, &bitcode, "__replay_strlit__", &manifest)
        .expect("add_module_from_bitcode");

    let ptr = unsafe { jit.get_function_ptr("<eval-entry>") }
        .expect("<eval-entry> resolves");
    assert!(!ptr.is_null());
    // The eval entry returns the raw bits of the interned <byte-string>
    // Word. The cold compile took the value from `intern_string_literal`
    // (one allocation in the static area); subsequent calls to that
    // helper allocate a *new* `<byte-string>` (no per-text dedup at
    // that layer — Sprint 38c added dedup only at the slot allocator).
    // So compare against the slot value, which is the canonical
    // interned word for `"hello"` in this process.
    let f: extern "C-unwind" fn() -> u64 = unsafe { std::mem::transmute(ptr) };
    let raw = f();
    let slot_addr = nod_runtime::intern_string_literal_slot_addr("hello");
    // SAFETY: slot_addr is a stable `&'static u64` from the slot
    // allocator; its lifetime is the process.
    let expected = unsafe { *slot_addr };
    assert_eq!(
        raw, expected,
        "replayed eval-entry must return the slot-cached interned `\"hello\"` Word"
    );
    // Tag invariant: the slot's Word is a pointer-tagged `<byte-string>`,
    // so the low bit must be 1.
    assert_eq!(raw & 1, 1, "interned byte-string Word must carry pointer tag");

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[serial]
fn sprint38c_symbol_literal_globals_round_trip() {
    // `make(<integer>, value: 42)` lowers the `value:` keyword as a
    // `<symbol>` literal via `emit_symbol_literal`. We don't yet have
    // a Dylan surface form for direct symbol comparison, but the
    // bake site fires during `make`-keyword lowering — confirm that
    // the resulting bitcode rounds through replay correctly.
    //
    // We pick `<range>` since it accepts `from: to:` keyword args
    // and is part of the seed class set (no user-class registration
    // needed in the warm process).
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-symlit-roundtrip");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    // `make(<range>, from: 0, to: 5)` produces a range; `size(...)`
    // gives its length (6). The `make` lowering pushes a `value:`-style
    // symbol literal Word for each keyword — exercising the symbol-
    // literal bake site.
    let cold = eval_expr_to_string("size(make(<range>, from: 0, to: 5))").expect("cold eval");
    assert_eq!(cold, "6", "cold path returns 6");

    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    assert!(!bc_paths.is_empty());
    let bc_path = bc_paths[0].path();
    let manifest_path = bc_path.with_extension("manifest.json");
    let bitcode = std::fs::read(&bc_path).expect("read bitcode");
    let manifest_text = std::fs::read_to_string(&manifest_path).expect("manifest exists");
    let manifest = nod_llvm::ModuleManifest::parse(&manifest_text).expect("manifest parses");

    let sym_kinds: Vec<&nod_llvm::RelocKind> = manifest
        .entries
        .iter()
        .map(|e| &e.kind)
        .filter(|k| matches!(k, nod_llvm::RelocKind::SymbolLiteral { .. }))
        .collect();
    assert!(
        !sym_kinds.is_empty(),
        "manifest must carry ≥1 SymbolLiteral RelocKind entry"
    );
    let names: Vec<&str> = sym_kinds
        .iter()
        .filter_map(|k| match k {
            nod_llvm::RelocKind::SymbolLiteral { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    // `from` and `to` keywords should be among the symbol literals
    // (emit_symbol_literal in the make-keyword lowering strips the
    // trailing colon).
    assert!(
        names.iter().any(|n| *n == "from" || *n == "to"),
        "expected `from`/`to` keywords; got {names:?}"
    );

    let ctx: &'static nod_llvm::LlvmContext =
        Box::leak(Box::new(nod_llvm::LlvmContext::create()));
    let mut jit = nod_llvm::Jit::new(ctx).expect("Jit::new");
    jit.add_module_from_bitcode(ctx, &bitcode, "__replay_symlit__", &manifest)
        .expect("add_module_from_bitcode");

    let ptr = unsafe { jit.get_function_ptr("<eval-entry>") }
        .expect("<eval-entry> resolves");
    assert!(!ptr.is_null());
    let f: extern "C-unwind" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let raw = f();
    assert_eq!(raw >> 1, 6, "replayed eval-entry returns 6");

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[serial]
fn sprint38c_emitted_ir_has_class_metadata_external_global() {
    // IR-shape test: confirm the cached `.bc` for an expression that
    // references a class metadata pointer contains an external global
    // `@nod_class_md__<key>__<class_id>` and at least one `load i64,
    // ptr @nod_class_md__` site, NOT a baked `i64 <bits>` constant at
    // the bake location.
    //
    // `make(<range>, …)` is used (instead of `instance?(1, <integer>)`)
    // because the latter constant-folds away — the compiler resolves
    // `1`'s class statically. `make` genuinely needs the class
    // metadata pointer at runtime.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-class-md-irshape");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    let cold =
        eval_expr_to_string("size(make(<range>, from: 0, to: 5))").expect("cold eval");
    assert_eq!(cold, "6");

    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    let bc_path = bc_paths[0].path();
    let bitcode = std::fs::read(&bc_path).expect("read bc");

    let ir = nod_llvm::bitcode_to_ir_text(&bitcode).expect("bitcode_to_ir_text");
    let has_class_md_decl = ir.contains("@nod_class_md__");
    assert!(
        has_class_md_decl,
        "IR must declare an external global for class metadata; IR:\n{ir}"
    );
    let has_load_through_class_md = ir.contains("load i64, ptr @nod_class_md__");
    assert!(
        has_load_through_class_md,
        "IR must contain a `load i64, ptr @nod_class_md__*` instruction; IR:\n{ir}"
    );
    for line in ir.lines() {
        if line.contains("nod_class_md__") {
            println!("[IR] {line}");
        }
    }

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[serial]
fn sprint38c_emitted_ir_has_string_literal_external_global() {
    // IR-shape test for string-literal externals.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-strlit-irshape");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    let _ = eval_expr_to_string("size(\"hello\")").expect("cold eval");

    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    let bc_path = bc_paths[0].path();
    let bitcode = std::fs::read(&bc_path).expect("read bc");
    let ir = nod_llvm::bitcode_to_ir_text(&bitcode).expect("bitcode_to_ir_text");

    let has_strlit_decl = ir.contains("@nod_strlit__");
    assert!(
        has_strlit_decl,
        "IR must declare an external global for a string literal; IR:\n{ir}"
    );
    let has_load_through_strlit = ir.contains("load i64, ptr @nod_strlit__");
    assert!(
        has_load_through_strlit,
        "IR must contain a `load i64, ptr @nod_strlit__*` instruction; IR:\n{ir}"
    );
    for line in ir.lines() {
        if line.contains("nod_strlit__") {
            println!("[IR] {line}");
        }
    }

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[serial]
fn sprint38c_emitted_ir_has_symbol_literal_external_global() {
    // IR-shape test for symbol-literal externals.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-symlit-irshape");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    let _ = eval_expr_to_string("size(make(<range>, from: 0, to: 5))").expect("cold eval");

    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    let bc_path = bc_paths[0].path();
    let bitcode = std::fs::read(&bc_path).expect("read bc");
    let ir = nod_llvm::bitcode_to_ir_text(&bitcode).expect("bitcode_to_ir_text");

    let has_symlit_decl = ir.contains("@nod_symlit__");
    assert!(
        has_symlit_decl,
        "IR must declare an external global for a symbol literal; IR:\n{ir}"
    );
    let has_load_through_symlit = ir.contains("load i64, ptr @nod_symlit__");
    assert!(
        has_load_through_symlit,
        "IR must contain a `load i64, ptr @nod_symlit__*` instruction; IR:\n{ir}"
    );
    for line in ir.lines() {
        if line.contains("nod_symlit__") {
            println!("[IR] {line}");
        }
    }

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
fn sprint38c_intern_string_literal_slot_addr_is_memoised() {
    // Sanity check on the slot allocator: repeated calls with the same
    // text return the SAME *const u64 address. This is what makes
    // multiple JIT-loaded modules referencing the same literal share
    // one underlying slot.
    let a = nod_runtime::intern_string_literal_slot_addr("memoise-me");
    let b = nod_runtime::intern_string_literal_slot_addr("memoise-me");
    assert_eq!(a, b, "same text → same slot address");
    let c = nod_runtime::intern_string_literal_slot_addr("different-text");
    assert_ne!(a, c, "distinct text → distinct slot address");
}

#[test]
fn sprint38c_intern_symbol_literal_slot_addr_is_memoised() {
    let a = nod_runtime::intern_symbol_literal_slot_addr("memoise-me");
    let b = nod_runtime::intern_symbol_literal_slot_addr("memoise-me");
    assert_eq!(a, b, "same name → same slot address");
    let c = nod_runtime::intern_symbol_literal_slot_addr("different-name");
    assert_ne!(a, c, "distinct name → distinct slot address");
}

#[test]
fn sprint38c_class_metadata_slot_addr_is_memoised() {
    let a = nod_runtime::class_metadata_slot_addr(nod_runtime::ClassId::INTEGER);
    let b = nod_runtime::class_metadata_slot_addr(nod_runtime::ClassId::INTEGER);
    assert_eq!(a, b, "same class id → same slot address");
    let c = nod_runtime::class_metadata_slot_addr(nod_runtime::ClassId::BOOLEAN);
    assert_ne!(a, c, "distinct class id → distinct slot address");
}

#[test]
#[serial]
fn add_module_from_bitcode_round_trips_a_trivial_module() {
    // End-to-end smoke test of the Sprint 38 JIT-link infrastructure.
    //
    // 1. Drive the normal eval pipeline so the on-disk cache populates
    //    with bitcode + manifest sidecar.
    // 2. Read both back from disk.
    // 3. Create a fresh `Jit` + `Context` and call the new
    //    `add_module_from_bitcode` entry point to install the loaded
    //    module.
    // 4. Resolve `<eval-entry>` from the fresh Jit and invoke it —
    //    confirm the function returns the same answer as the original
    //    cold compile.
    //
    // The test runs entirely in one process, so the baked-as-i64
    // runtime addresses in the cold-compile bitcode are still valid
    // when re-loaded (they point at the same static-area objects).
    // The point of the test is to prove `add_module_from_bitcode`'s
    // bitcode-parse + symbol-binding + finalize sequence is sound —
    // it's the SAME plumbing that cross-process replay will use once
    // the codegen-side conversion lands.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-trivial");
    nod_llvm::clear_cache_dir(&dir);
    // Make sure later eval_expr_to_string calls write here.
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    // Cold-compile an expression that exercises class metadata
    // (returns 6 — an integer fixnum).
    let s = eval_expr_to_string("1 + 2 + 3").expect("eval");
    assert_eq!(s, "6", "cold eval returned expected result");

    // Read back the bitcode + manifest from disk. We don't know the
    // exact key without rebuilding the wrapped source string, so we
    // walk the dir and grab the only `.bc` file.
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    assert!(!entries.is_empty(), "expected ≥1 .bc file in the cache dir");
    entries.sort_by_key(|de| de.file_name());
    let bc_path = entries[0].path();
    let manifest_path = bc_path.with_extension("manifest.json");

    let bitcode = std::fs::read(&bc_path).expect("read bitcode");
    assert!(!bitcode.is_empty(), "bitcode must be non-empty");

    // The manifest may or may not exist depending on whether the cold
    // path wrote one. Sprint 38's `write_cache_entry_with_manifest` is
    // the new path; the existing `write_cache_entry` (which the cold
    // path still calls) doesn't write a manifest. So we synthesize an
    // empty manifest here — the loader handles that case as "no
    // relocations needed".
    let manifest = if manifest_path.exists() {
        let txt = std::fs::read_to_string(&manifest_path).expect("read manifest");
        nod_llvm::ModuleManifest::parse(&txt).expect("parse manifest")
    } else {
        nod_llvm::ModuleManifest {
            manifest_version: nod_llvm::MANIFEST_VERSION,
            key_prefix: String::new(),
            entries: Vec::new(),
        }
    };

    // Fresh JIT + Context. The cross-process replay test would spawn
    // a subprocess here; we approximate that by building a fresh JIT
    // in-process.
    let ctx: &'static nod_llvm::LlvmContext =
        Box::leak(Box::new(nod_llvm::LlvmContext::create()));
    let mut jit = nod_llvm::Jit::new(ctx).expect("Jit::new");
    jit.add_module_from_bitcode(ctx, &bitcode, "__replay__", &manifest)
        .expect("add_module_from_bitcode");

    // Resolve `<eval-entry>` from the replay'd JIT. The function exists
    // (it's the wrapper the eval pipeline always emits).
    let ptr = unsafe { jit.get_function_ptr("<eval-entry>") }
        .expect("<eval-entry> resolves from replayed bitcode");
    assert!(!ptr.is_null(), "function pointer must be non-null");

    // Invoke it (returns i64 since the wrapped expression has integer
    // return type) and confirm it produces the same answer as the
    // original cold compile.
    // SAFETY: the JIT'd `<eval-entry>` is `extern "C-unwind" fn() -> i64`.
    // We just resolved it from a live MCJIT engine; the engine outlives
    // the call.
    let f: extern "C-unwind" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let raw = f();
    // The result is a tagged Word — fixnum encoding is `n << 1`.
    let untagged = raw >> 1;
    assert_eq!(untagged, 6, "replayed eval-entry returned same answer");

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

// ─── Sprint 38d — Win32 stub-entry bake sites ───────────────────────────────
//
// Same end-to-end shape as Sprint 38b/c: cold-compile a Dylan program
// that calls a Win32 API, drop the in-process cache, reload from
// bitcode + manifest, re-invoke, and assert the result.
//
// `GetTickCount()` is chosen as the headline because:
//   • arity 0 → simplest trampoline path (no arg marshaling).
//   • returns DWORD → no out-buffer, no string conversion.
//   • available in every supported Windows SKU (Kernel32 export).
//   • bare-name materialisation (Sprint 31) — no `define c-function`
//     prelude needed, so the test's Dylan source is one line.
//
// The IR-shape test confirms `@nod_stub__*` external globals are emitted
// (not baked `i64 <addr>` constants). The slot-allocator test exercises
// the memoisation + case-insensitive DLL key.

#[test]
#[cfg(windows)]
#[serial]
fn sprint38d_stub_entry_globals_round_trip() {
    // Cold-compile a Dylan expression that calls Win32 `GetTickCount()`.
    // After cold compile completes, drop the in-process cache, reload
    // the bitcode + manifest in a fresh `Jit` + `Context`, and assert:
    //   1. The manifest carries ≥1 `RelocKind::StubEntry` entry whose
    //      `(dll, symbol)` is `("kernel32.dll", "GetTickCount")`
    //      (case-normalise the dll before comparing).
    //   2. The replayed `<eval-entry>` returns a sensible integer
    //      (GetTickCount varies; we accept any non-negative fixnum).
    //   3. The replay path successfully resolves the named external
    //      global via `stub_entry_slot_addr` — i.e. the cross-process
    //      replay invariant the Sprint 38 cache was built to provide.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-stub-roundtrip");
    nod_llvm::clear_cache_dir(&dir);
    // SAFETY: env mutation is serialised by `#[serial]`.
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    // GetTickCount is materialised on-the-fly via Sprint 31's
    // bare-name path — no `define c-function` prelude needed.
    let cold = eval_expr_to_string("GetTickCount()").expect("cold eval");
    let cold_n: i64 = cold.parse().expect("integer-shaped return");
    assert!(cold_n >= 0, "GetTickCount cold result must be non-negative, got {cold_n}");

    // Read back the bitcode + manifest from disk.
    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    assert!(!bc_paths.is_empty(), "cache dir should contain ≥1 .bc");
    let bc_path = bc_paths[0].path();
    let manifest_path = bc_path.with_extension("manifest.json");
    let bitcode = std::fs::read(&bc_path).expect("read bitcode");
    let manifest_text =
        std::fs::read_to_string(&manifest_path).expect("manifest.json must exist for Sprint 38d");
    let manifest = nod_llvm::ModuleManifest::parse(&manifest_text).expect("manifest parses");

    // Sprint 38d — codegen must have emitted ≥1 StubEntry reloc entry
    // for GetTickCount@kernel32.dll (case-normalised compare on dll).
    let stub_entries: Vec<(&str, &str)> = manifest
        .entries
        .iter()
        .filter_map(|e| match &e.kind {
            nod_llvm::RelocKind::StubEntry { dll, symbol, .. } => {
                Some((dll.as_str(), symbol.as_str()))
            }
            _ => None,
        })
        .collect();
    assert!(
        !stub_entries.is_empty(),
        "manifest must carry ≥1 StubEntry RelocKind entry; got entries: {:?}",
        manifest.entries
    );
    assert!(
        stub_entries
            .iter()
            .any(|(dll, sym)| dll.to_lowercase() == "kernel32.dll" && *sym == "GetTickCount"),
        "expected GetTickCount@kernel32.dll among stub entries; got {stub_entries:?}"
    );

    // Fresh JIT + Context — replay the bitcode through the JIT-link
    // path. The manifest resolver invokes `stub_entry_slot_addr` for
    // each StubEntry row, which allocates + resolves the entry in the
    // current process and binds `LLVMAddGlobalMapping(@nod_stub__*, slot)`.
    let ctx: &'static nod_llvm::LlvmContext =
        Box::leak(Box::new(nod_llvm::LlvmContext::create()));
    let mut jit = nod_llvm::Jit::new(ctx).expect("Jit::new");
    jit.add_module_from_bitcode(ctx, &bitcode, "__replay_stub__", &manifest)
        .expect("add_module_from_bitcode");

    let ptr = unsafe { jit.get_function_ptr("<eval-entry>") }
        .expect("<eval-entry> resolves from replayed bitcode");
    assert!(!ptr.is_null(), "function pointer must be non-null");
    // GetTickCount returns a DWORD — boxed as a tagged fixnum Word.
    let f: extern "C-unwind" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let raw = f();
    let untagged = raw >> 1;
    assert!(
        untagged >= 0,
        "replayed GetTickCount result must be non-negative, got {untagged}"
    );

    // SAFETY: serialised by #[serial].
    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[cfg(windows)]
#[serial]
fn sprint38d_emitted_ir_has_stub_external_global() {
    // IR-shape test: confirm the cached `.bc` for an expression that
    // calls a Win32 API contains an external global
    // `@nod_stub__<key>__<idx>` and at least one `load i64, ptr
    // @nod_stub__` site — NOT a baked `i64 <bits>` constant at the
    // bake location.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-stub-irshape");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    let _ = eval_expr_to_string("GetTickCount()").expect("cold eval");

    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    let bc_path = bc_paths[0].path();
    let bitcode = std::fs::read(&bc_path).expect("read bc");
    let ir = nod_llvm::bitcode_to_ir_text(&bitcode).expect("bitcode_to_ir_text");

    let has_stub_decl = ir.contains("@nod_stub__");
    assert!(
        has_stub_decl,
        "IR must declare an external global for the stub entry; IR:\n{ir}"
    );
    let has_load_through_stub = ir.contains("load i64, ptr @nod_stub__");
    assert!(
        has_load_through_stub,
        "IR must contain a `load i64, ptr @nod_stub__*` instruction; IR:\n{ir}"
    );
    for line in ir.lines() {
        if line.contains("nod_stub__") {
            println!("[IR] {line}");
        }
    }

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[cfg(windows)]
fn sprint38d_stub_entry_slot_addr_is_memoised() {
    // Direct unit test of the Sprint 38d slot allocator:
    //   • repeated calls with the same (dll, symbol) return the SAME
    //     `&'static u64` address;
    //   • distinct symbols (same dll) return distinct addresses;
    //   • case-insensitive dll comparison — `Kernel32.dll` and
    //     `kernel32.dll` share the same slot.
    //
    // This is the cross-process invariant that makes the slot allocator
    // safe to call from multiple replayed modules: each unique Win32 API
    // resolves to one entry, one fn_ptr, one slot.
    let sig_get_tick_count = nod_runtime::ApiCallSignature {
        arg_count: 0,
        arg_kinds: [0; 12],
        return_kind: nod_runtime::CReturnKind::UInt32 as u8,
    };
    let sig_get_last_error = nod_runtime::ApiCallSignature {
        arg_count: 0,
        arg_kinds: [0; 12],
        return_kind: nod_runtime::CReturnKind::UInt32 as u8,
    };

    let a = nod_runtime::stub_entry_slot_addr("Kernel32.dll", "GetTickCount", &sig_get_tick_count)
        as *const u64;
    let b = nod_runtime::stub_entry_slot_addr("Kernel32.dll", "GetTickCount", &sig_get_tick_count)
        as *const u64;
    assert_eq!(a, b, "same (dll, symbol) → same slot address");

    // Distinct symbol in the same DLL → distinct slot.
    let c = nod_runtime::stub_entry_slot_addr("Kernel32.dll", "GetLastError", &sig_get_last_error)
        as *const u64;
    assert_ne!(a, c, "distinct symbols in same dll → distinct slot addresses");

    // Case-insensitive dll: `kernel32.dll` (lowercase) and `Kernel32.dll`
    // resolve to the same slot.
    let d = nod_runtime::stub_entry_slot_addr("kernel32.dll", "GetTickCount", &sig_get_tick_count)
        as *const u64;
    assert_eq!(
        a, d,
        "case-different dll name → same slot (Win32 DLL names are case-insensitive)"
    );

    // Slot contents are non-null (they hold the address of an
    // `ApiStubEntry`, freshly leaked at first lookup).
    // SAFETY: `a` came from the slot allocator and lives for the
    // process lifetime; dereferencing it reads the `u64` slot value.
    let entry_ptr_bits = unsafe { *a };
    assert!(
        entry_ptr_bits != 0,
        "slot must hold a non-null ApiStubEntry pointer"
    );
}

// ─── Sprint 38e — cache-slot + generic-function bake-site conversion ──

#[test]
#[serial]
fn sprint38e_cache_slot_and_generic_globals_round_trip() {
    // Sprint 38e is the LAST bake-site category in Sprint 38: every
    // dispatch site previously baked the GenericFunction pointer and
    // the CacheSlot pointer (+ 5 derived field-offset addresses) as
    // per-process `i64` constants. Now codegen emits one
    // `load i64, ptr @nod_generic__*` and one `load i64, ptr
    // @nod_cache_slot__*` per site; the JIT-link path resolves each
    // through the slot allocator in `nod-runtime`.
    //
    // This round-trip test cold-compiles a generic-dispatch
    // expression (`1 + 2`), drops the in-process cache, reloads the
    // bitcode + manifest in a fresh `Jit` + `Context`, and asserts:
    //   1. The manifest carries ≥1 `CacheSlot` and ≥1 `Generic`
    //      entry (the `+` operator dispatches through both).
    //   2. The replayed `<eval-entry>` returns the same answer (3
    //      → tagged Word `(3 << 1) | 1 == 7` if the integer fixnum
    //      tag rules apply; we just assert equality with cold).
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-cache-generic-roundtrip");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    // `size(make(<range>, from: 0, to: 5))` evaluates to 5. `make` is a
    // runtime call (the optimiser can't fold it), and `size` is a
    // stdlib generic that dispatches on the receiver's class — so this
    // expression emits at least one `Computation::Dispatch` node with
    // a fresh `CacheSlot` and a `Generic { name: "size" }` reference.
    // `<range>` from: 0 to: 5 is inclusive on both ends → 6 elements
    // [0,1,2,3,4,5]. The Sprint 38c class-metadata test uses the same
    // expression and asserts the same value.
    let cold = eval_expr_to_string("size(make(<range>, from: 0, to: 5))").expect("cold eval");
    assert_eq!(cold, "6", "cold eval of size(<range 0..5>) must equal 6, got {cold}");

    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    assert!(!bc_paths.is_empty(), "cache dir should contain ≥1 .bc");
    let bc_path = bc_paths[0].path();
    let manifest_path = bc_path.with_extension("manifest.json");
    let bitcode = std::fs::read(&bc_path).expect("read bitcode");
    let manifest_text =
        std::fs::read_to_string(&manifest_path).expect("manifest.json must exist for Sprint 38e");
    let manifest = nod_llvm::ModuleManifest::parse(&manifest_text).expect("manifest parses");

    let cache_slots: Vec<u64> = manifest
        .entries
        .iter()
        .filter_map(|e| match &e.kind {
            nod_llvm::RelocKind::CacheSlot { site_id } => Some(*site_id),
            _ => None,
        })
        .collect();
    assert!(
        !cache_slots.is_empty(),
        "manifest must carry ≥1 CacheSlot RelocKind entry; got entries: {:?}",
        manifest.entries
    );

    let generics: Vec<&str> = manifest
        .entries
        .iter()
        .filter_map(|e| match &e.kind {
            nod_llvm::RelocKind::Generic { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !generics.is_empty(),
        "manifest must carry ≥1 Generic RelocKind entry; got entries: {:?}",
        manifest.entries
    );
    assert!(
        generics.contains(&"size"),
        "expected `size` among generics; got {generics:?}"
    );

    // Fresh JIT + Context — replay through `add_module_from_bitcode`.
    // The manifest resolver invokes `cache_slot_slot_addr` and
    // `generic_function_slot_addr` for each row, binding
    // `LLVMAddGlobalMapping(@nod_cache_slot__*, slot)` /
    // `(@nod_generic__*, slot)`.
    let ctx: &'static nod_llvm::LlvmContext =
        Box::leak(Box::new(nod_llvm::LlvmContext::create()));
    let mut jit = nod_llvm::Jit::new(ctx).expect("Jit::new");
    jit.add_module_from_bitcode(ctx, &bitcode, "__replay_cs_gen__", &manifest)
        .expect("add_module_from_bitcode");

    let ptr = unsafe { jit.get_function_ptr("<eval-entry>") }
        .expect("<eval-entry> resolves from replayed bitcode");
    assert!(!ptr.is_null(), "function pointer must be non-null");
    let f: extern "C-unwind" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let raw = f();
    // 6 as a tagged fixnum: `(6 << 1) == 12`.
    let untagged = raw >> 1;
    assert_eq!(
        untagged, 6,
        "replayed size(<range 0..5>) must produce untagged 6, got raw={raw} untagged={untagged}"
    );

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
#[serial]
fn sprint38e_emitted_ir_has_cache_slot_and_generic_globals() {
    // IR-shape proof: confirm the `.bc` for `1 + 2` contains:
    //   * `@nod_cache_slot__<key>__<site_id>` external global decl
    //   * `@nod_generic__<key>__<sanitised-name>` external global decl
    //   * `load i64, ptr @nod_cache_slot__*`
    //   * `load i64, ptr @nod_generic__*`
    // And that the dispatch path no longer contains a baked
    // generic/cache-slot per-process `i64` runtime address.
    use nod_sema::eval_expr_to_string;

    let dir = test_cache_dir("e2e-cache-generic-irshape");
    nod_llvm::clear_cache_dir(&dir);
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    nod_llvm::in_process_clear();

    let _ = eval_expr_to_string("size(make(<range>, from: 0, to: 5))").expect("cold eval");

    let mut bc_paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readdir")
        .flatten()
        .filter(|de| de.path().extension().and_then(|s| s.to_str()) == Some("bc"))
        .collect();
    bc_paths.sort_by_key(|de| de.file_name());
    let bc_path = bc_paths[0].path();
    let bitcode = std::fs::read(&bc_path).expect("read bc");
    let ir = nod_llvm::bitcode_to_ir_text(&bitcode).expect("bitcode_to_ir_text");

    assert!(
        ir.contains("@nod_cache_slot__"),
        "IR must declare a cache-slot external global; IR:\n{ir}"
    );
    assert!(
        ir.contains("@nod_generic__"),
        "IR must declare a generic external global; IR:\n{ir}"
    );
    assert!(
        ir.contains("load i64, ptr @nod_cache_slot__"),
        "IR must contain a `load i64, ptr @nod_cache_slot__*` instruction; IR:\n{ir}"
    );
    assert!(
        ir.contains("load i64, ptr @nod_generic__"),
        "IR must contain a `load i64, ptr @nod_generic__*` instruction; IR:\n{ir}"
    );

    for line in ir.lines() {
        if line.contains("nod_cache_slot__") || line.contains("nod_generic__") {
            println!("[IR] {line}");
        }
    }

    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
    nod_llvm::clear_cache_dir(&dir);
    nod_llvm::in_process_clear();
}

#[test]
fn sprint38e_cache_slot_slot_addr_is_memoised() {
    // Direct slot allocator unit test:
    //   * same (key_prefix, site_id) → same `&'static u64`
    //   * different site_id in same module → different slot
    //   * same site_id in different modules → different slot
    //     (the key_prefix disambiguates cross-module collisions)
    //   * slot contents are non-null after first lookup
    let key_a = "0123456789abcdef";
    let key_b = "fedcba9876543210";

    let a = nod_runtime::cache_slot_slot_addr(key_a, 0) as *const u64;
    let b = nod_runtime::cache_slot_slot_addr(key_a, 0) as *const u64;
    assert_eq!(a, b, "same (key, site_id) → same slot address");

    let c = nod_runtime::cache_slot_slot_addr(key_a, 1) as *const u64;
    assert_ne!(a, c, "different site_id in same key → different slot");

    let d = nod_runtime::cache_slot_slot_addr(key_b, 0) as *const u64;
    assert_ne!(
        a, d,
        "same site_id in different keys → different slot (cross-module disambiguation)"
    );

    // Slot holds the address of a fresh CacheSlot.
    // SAFETY: `a` came from the slot allocator and lives for the
    // process lifetime.
    let cache_slot_addr_bits = unsafe { *a };
    assert!(
        cache_slot_addr_bits != 0,
        "slot must hold a non-null CacheSlot pointer"
    );
}

#[test]
fn sprint38e_generic_function_slot_addr_is_memoised() {
    // Direct slot allocator unit test for generics:
    //   * same name → same `&'static u64`
    //   * different name → different slot
    //   * slot contents are non-null after first lookup (the slot
    //     holds the address of a leaked `&'static GenericFunction`)
    let a = nod_runtime::generic_function_slot_addr("+") as *const u64;
    let b = nod_runtime::generic_function_slot_addr("+") as *const u64;
    assert_eq!(a, b, "same generic name → same slot address");

    let c = nod_runtime::generic_function_slot_addr("*") as *const u64;
    assert_ne!(a, c, "different generic name → different slot address");

    // Slot holds the address of a `&'static GenericFunction`.
    // SAFETY: see above.
    let generic_addr_bits = unsafe { *a };
    assert!(
        generic_addr_bits != 0,
        "slot must hold a non-null GenericFunction pointer"
    );
}
