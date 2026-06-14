//! `nod-aot-stub` — the default `nod_user_main` stub, isolated in its own crate.
//!
//! ## Why a whole crate for one function
//!
//! When `nod-runtime` is built as a `staticlib` (`nod_runtime.lib`), the MSVC
//! linker pulls archive members ON DEMAND: an object is extracted only if one of
//! its symbols is required and not already defined. `nod_aot_main_wrapper`
//! (in nod-runtime) references `nod_user_main` as `extern`. At AOT link time the
//! driver passes the user's `.obj` FIRST:
//!
//! 1. **User defines `nod_user_main`** (every real AOT EXE — it's the renamed
//!    Dylan `main`): the symbol resolves against the user's `.obj`, so this
//!    crate's object is NEVER extracted. No `LNK2005`.
//! 2. **User does not** (nod-runtime's own test binary, which synthetically
//!    calls `nod_aot_main_wrapper`): the linker pulls this object to resolve
//!    `nod_user_main`. No `LNK2019`.
//!
//! The previous design kept the stub in nod-runtime's own `aot_user_main_stub.rs`
//! and relied on the "1 file = 1 object" premise — but Cargo's CGU partitioner
//! can MERGE that file into the same object as an always-pulled module (`aot.rs`
//! or a hot `std` monomorphization), forcing extraction → `LNK2005 nod_user_main`
//! intermittently (CGU-partition dependent; it broke whenever nod-runtime was
//! edited). A SEPARATE CRATE is compiled to its own object regardless of
//! nod-runtime's CGU layout, making the on-demand-extraction trick robust.

/// Default stub for `nod_user_main`. Returns `0` so a synthetic test of
/// `nod_aot_main_wrapper` observes a clean exit. Real AOT EXEs override this with
/// the user's renamed Dylan `main` body, so this object is dropped at link time.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn nod_user_main() -> i64 {
    0
}
