//! Sprint 39a — separate translation unit holding the default
//! `nod_user_main` stub.
//!
//! ## Why a dedicated `.rs` file (and not just `#[cfg]`-gated in `aot.rs`)
//!
//! When `nod-runtime` is built as a `staticlib` (`nod_runtime.lib`), each
//! `.rs` source file becomes one `.obj` member of the archive. The MSVC
//! linker `link.exe` pulls in archive members **on demand**: an object
//! file is only extracted from the `.lib` if one of its exported symbols
//! is required and not already defined.
//!
//! `nod_aot_main_wrapper` (in `aot.rs`'s translation unit) references
//! `nod_user_main` as `extern`. At AOT link time the driver passes the
//! user's `.obj` FIRST, then `nod_runtime.lib`. Two cases:
//!
//! 1. **User defined `nod_user_main`**: the linker resolves the symbol
//!    against the user's `.obj`. When it later needs `nod_aot_main_wrapper`,
//!    it pulls in `aot.obj` from the library — but `nod_user_main` is
//!    already satisfied, so `aot_user_main_stub.obj` is **never extracted**.
//!    No duplicate-symbol error. This is the normal Sprint 39a path.
//!
//! 2. **User did not define `nod_user_main`** (e.g. nod-runtime's own
//!    test binary, or a downstream Rust binary that synthetically calls
//!    `nod_aot_main_wrapper`): pulling in `aot.obj` triggers a search
//!    for `nod_user_main`; the linker finds it in
//!    `aot_user_main_stub.obj` and extracts that too. Wrapper resolves;
//!    no `LNK2019`.
//!
//! If the stub lived inside `aot.rs`, both definitions would always
//! travel together — case (1) would fail with `LNK2005: nod_user_main
//! already defined`. The translation-unit split is what makes the
//! "weak default" trick robust under MSVC's archive-member resolution.
//!
//! ## CGU coupling is the hidden fragility (fixed via per-package profile)
//!
//! The 1-file=1-`.obj` premise only holds if Cargo doesn't merge `.rs`
//! files into one CGU. The default 16-CGU layout can merge
//! `aot_user_main_stub.rs` into the same `.obj` as `aot.rs` (or any
//! other always-referenced module), at which point pulling
//! `nod_aot_main_wrapper` forces pulling the stub too — and the user
//! EXE's own `nod_user_main` collides. We pin nod-runtime to
//! `codegen-units = 1` in the workspace `Cargo.toml`'s per-package
//! profile, which means one CGU PER ARCHIVE MEMBER — every `.rs` file
//! ends up in its own `.obj` and the on-demand extraction works as
//! designed.
//!
//! ## When this file's symbol is unused (the default Sprint 39a flow)
//!
//! Compiling this file always produces an `.obj` and contributes it to
//! `nod_runtime.lib`. Cost is one stub function (~20 bytes of code) plus
//! one symbol table row — negligible. The archive-extraction rule
//! ensures the stub `.obj` is silently dropped at AOT link time when
//! the user supplies their own `nod_user_main`.

/// Default stub for `nod_user_main`. Used only when nothing else in the
/// final link defines this symbol — see the module-level doc for the
/// archive-extraction rule that makes this safe to ship in
/// `nod_runtime.lib`.
///
/// Returns `0` so a synthetic test of `nod_aot_main_wrapper` (which
/// just calls `nod_runtime_init` then forwards to `nod_user_main`)
/// observes a clean exit. Real AOT EXEs override this with the user's
/// renamed Dylan `main` body.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn nod_user_main() -> i64 {
    0
}
