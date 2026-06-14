//! Sprint 51c — Dylan-side parser side-load (verify mode).
//!
//! Same static-link pattern as `dylan_lex_jit.rs`. When the combined
//! shim `.obj` is linked, `dylan-parse-collect(source: <byte-string>)
//! => <integer>` is reachable as an `extern "C" fn(u64) -> i64`. The
//! return is the number of top-level `<ast-error-node>`s the
//! Dylan-side parser found.
//!
//! ## Wire contract
//!
//! `dylan-parse-collect` internally lexes + parses the source on the
//! Dylan side (same module, so `lex` is in-process). The return value
//! `n`:
//!
//!   * `n == 0` — Dylan parser accepted the source.
//!   * `n > 0`  — Dylan parser flagged `n` top-level error
//!     constituents. Nested errors propagate up to constituent level,
//!     so this counts the "the parser bailed somewhere" cases without
//!     requiring a full tree walk.
//!
//! ## Verify mode
//!
//! The [`verify`] entry point runs the Dylan parser and compares its
//! "accepted? yes/no" verdict against an "accepted" boolean the
//! caller supplies (from running the Rust parser). Disagreement is
//! the loud failure that surfaces a divergence between the two
//! parsers on real corpus inputs. Agreement is the silent OK that
//! proves the Dylan parser is being exercised in production.
//!
//! ## Future
//!
//! This module currently does verify-mode only. Replacement mode —
//! where the Dylan parser produces an actual AST that the Rust side
//! consumes — needs a tree-shaped wire format that lives in
//! `docs/DYLAN_AST_WIRE.md` and a `dylan-parse-emit` entry returning
//! a stretchy-vector of packed node records. That's Sprint 51d work;
//! the verify mode is the contract-stabilising step before then.

use std::sync::OnceLock;

use nod_reader::FileId;
use nod_runtime::Word;

#[cfg(dylan_lex_shim_linked)]
unsafe extern "C" {
    /// Sprint 51c — `define function dylan-parse-collect (source) =>
    /// (error-count :: <integer>)` from `dylan-lex-shim.dylan`. Word
    /// in (a `<byte-string>`), Word out (a boxed fixnum that decodes
    /// to the top-level error count).
    #[link_name = "dylan-parse-collect"]
    fn dylan_parse_collect(source: u64) -> u64;
}

#[cfg(not(dylan_lex_shim_linked))]
unsafe extern "C" fn dylan_parse_collect(_source: u64) -> u64 {
    unreachable!("dylan_lex_shim_linked is not set — `init` would have errored")
}

/// Tracks whether the `init` step in `dylan_lex_jit::init` has run
/// (which fires `nod_aot_resolve_relocs` for the whole combined shim).
/// We piggy-back on that: if the lexer's resolver has run, the
/// parser's relocations are also live (single resolver, single .obj).
static PARSE_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Record that the shim's resolver has fired. Called once from
/// `dylan_lex_jit::init` so the parser verify path can read a stable
/// "ready" signal without re-running the resolver.
pub fn mark_available() {
    let _ = PARSE_AVAILABLE.set(true);
}

/// Run the Dylan-side parser on `src` and compare its accept/reject
/// verdict against `rust_accepted`. Returns `Ok(())` if they agree,
/// `Err((dylan_errors, rust_accepted))` otherwise.
///
/// No-op (returns `Ok(())`) when the shim isn't linked or the
/// resolver hasn't run — the verify check is opt-in via
/// `--verify-parse`, so a failed shim init shouldn't block ordinary
/// builds.
pub fn verify(src: &str, _file_id: FileId, rust_accepted: bool) -> Result<(), VerifyMismatch> {
    if PARSE_AVAILABLE.get().is_none() {
        return Ok(());
    }

    // Build the Dylan `<byte-string>` exactly like the lex path does.
    let bytes = src.as_bytes();
    let len_word = Word::from_fixnum(bytes.len() as i64).expect("source under fixnum max");
    // SAFETY: `nod_byte_string_allocate` is the runtime's vetted
    // constructor. Same lifetime story as `dylan_lex_jit::lex`.
    let bs_raw = unsafe { nod_runtime::nod_byte_string_allocate(len_word.raw()) };
    for (i, &b) in bytes.iter().enumerate() {
        let byte_word = Word::from_fixnum(b as i64).expect("byte fits");
        let i_word = Word::from_fixnum(i as i64).expect("offset fits");
        // SAFETY: bs_raw was just allocated; single-threaded driver.
        unsafe {
            nod_runtime::nod_byte_string_element_setter(byte_word.raw(), bs_raw, i_word.raw());
        }
    }

    // SAFETY: bs_raw is a valid `<byte-string>`; the entry is the
    // statically-linked shim function. The function lexes + parses
    // internally and returns a fixnum-tagged integer count.
    let raw = unsafe { dylan_parse_collect(bs_raw) };
    let dylan_errors = Word::from_raw(raw)
        .as_fixnum()
        .expect("dylan-parse-collect returns fixnum") as usize;
    let dylan_accepted = dylan_errors == 0;

    if dylan_accepted == rust_accepted {
        Ok(())
    } else {
        Err(VerifyMismatch { rust_accepted, dylan_errors })
    }
}

/// Verdict diff between the Rust and Dylan parsers. Surfaced from
/// [`verify`] when the two disagree on whether the source parsed.
#[derive(Debug, Clone, Copy)]
pub struct VerifyMismatch {
    pub rust_accepted: bool,
    pub dylan_errors: usize,
}

impl std::fmt::Display for VerifyMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "parse-verify divergence: rust_accepted={}, dylan_errors={}",
            self.rust_accepted, self.dylan_errors
        )
    }
}

impl std::error::Error for VerifyMismatch {}
