//! Sprint 39a — non-shelling smoke tests for `nod_llvm::aot::emit_object_file`.
//!
//! Unlike `aot_exe.rs` (which spawns `cargo run --bin nod-driver` and
//! `link.exe` — `#[ignore]`-only because not every CI host has MSVC),
//! these tests exercise the LLVM-only piece of the pipeline in-process.
//! Suitable for routine `cargo test --workspace` runs.

use nod_llvm::LlvmContext as Context;
use nod_llvm::OptimizationLevel;
use nod_llvm::aot::{emit_aot_entry_stubs, emit_object_file};
use nod_llvm::symbols::ModuleManifest;

/// Build a tiny module with `i64 @main() { ret i64 0 }`, post-process
/// it through `emit_aot_entry_stubs`, then emit a COFF object file.
/// Assert the file exists, is non-trivial, and begins with the expected
/// magic bytes for x86_64 Windows COFF (`\x64\x86`, little-endian
/// `IMAGE_FILE_MACHINE_AMD64`).
#[test]
fn emit_object_file_smoke_windows_coff() {
    let ctx = Context::create();
    let module = ctx.create_module("smoke");
    let i64_ty = ctx.i64_type();
    let main_ty = i64_ty.fn_type(&[], false);
    // No explicit linkage — inkwell defaults to External, which is what
    // we want here. Avoids dragging `inkwell::module::Linkage` into the
    // test crate's import surface.
    let user_main = module.add_function("main", main_ty, None);
    let bb = ctx.append_basic_block(user_main, "entry");
    let builder = ctx.create_builder();
    builder.position_at_end(bb);
    let zero = i64_ty.const_zero();
    builder.build_return(Some(&zero)).unwrap();

    let manifest = ModuleManifest::default();
    emit_aot_entry_stubs(&module, &manifest).expect("entry stub injection");

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nod-aot-obj-smoke-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("smoke.obj");
    emit_object_file(&module, &path, OptimizationLevel::None).expect("emit_object_file");

    let bytes = std::fs::read(&path).expect("read .obj");
    assert!(bytes.len() > 100, "expected non-trivial .obj, got {} bytes", bytes.len());

    // On Windows we expect IMAGE_FILE_MACHINE_AMD64 = 0x8664 little-endian
    // as the first two bytes. On non-Windows hosts the host default
    // triple is ELF/Mach-O — we just verify non-empty there.
    if cfg!(target_os = "windows") {
        assert_eq!(
            &bytes[..2], &[0x64, 0x86],
            "expected COFF AMD64 magic 0x8664 LE; got {:02x}{:02x}",
            bytes[0], bytes[1]
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
