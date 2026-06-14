//! Sprint 51b — opt-in static link of the Dylan-side lexer shim.
//!
//! If `compiler/dylan-lex-shim.lib.obj` exists (built
//! via `nod-driver build --library --project
//! compiler/dylan-lex-shim.prj -o <that path>`), link
//! it into the resulting `nod-driver` binary AND set the
//! `dylan_lex_shim_linked` cfg flag so `src/dylan_lex_jit.rs` knows it
//! can declare the externs.
//!
//! When the `.obj` is absent (fresh checkout, or you blew away
//! `compiler/`), the build still succeeds — the
//! `--lex-with-dylan` flag prints a clear "not statically linked" error
//! at run time and falls back to `nod_reader::lex_rust`. Bootstrap
//! sequence:
//!
//! ```
//! cargo build -p nod-driver               # no shim yet
//! ./target/debug/nod-driver.exe build --library --project \
//!     compiler/dylan-lex-shim.prj \
//!     -o compiler/dylan-lex-shim.lib.obj
//! cargo build -p nod-driver               # picks up the .obj
//! ./target/debug/nod-driver.exe --lex-with-dylan dump-tokens hello.dylan
//! ```
//!
//! The `.obj` is `cargo:rerun-if-changed`-tracked, so subsequent
//! rebuilds of the Dylan source can refresh it without a full clean.

use std::path::PathBuf;

fn main() {
    // Tell cargo we know about this cfg name, so it doesn't flag the
    // dependent `#[cfg(dylan_lex_shim_linked)]` blocks as unrecognised
    // under rustc's `unexpected_cfgs` lint.
    println!("cargo:rustc-check-cfg=cfg(dylan_lex_shim_linked)");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // `nod-driver` lives at `<workspace>/src/nod-driver`; jump up two
    // levels to reach the workspace root.
    let workspace = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("nod-driver should live at <workspace>/src/nod-driver");

    let shim_obj = workspace
        .join("compiler")
        .join("dylan-lex-shim.lib.obj");

    println!("cargo:rerun-if-changed={}", shim_obj.display());

    if !shim_obj.is_file() {
        println!(
            "cargo:warning=nod-driver: {} not found — `--lex-with-dylan` will fall back to the Rust lexer. \
             Run `nod-driver build --library --project compiler/dylan-lex-shim.prj \
             -o {}` first, then `cargo build -p nod-driver` to wire it in.",
            shim_obj.display(),
            shim_obj.display(),
        );
        return;
    }

    // `cargo:rustc-link-arg-bin` passes a raw arg to the linker only for
    // the `nod-driver` binary, NOT for tests or examples that wouldn't
    // know about `dylan-lex-collect`. The `.obj` filename is positional
    // on MSVC link.exe — it just gets concatenated into the OBJ list.
    println!(
        "cargo:rustc-link-arg-bin=nod-driver={}",
        shim_obj.display()
    );
    println!("cargo:rustc-cfg=dylan_lex_shim_linked");
}
