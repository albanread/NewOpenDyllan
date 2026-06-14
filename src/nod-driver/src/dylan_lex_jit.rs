//! Sprint 51b — Dylan-side lexer side-load via static linking.
//!
//! When `nod-driver`'s `build.rs` finds
//! `compiler/dylan-lex-shim.lib.obj` it links it into
//! the `nod-driver` binary AND sets the `dylan_lex_shim_linked` cfg. At
//! that point this module's externs resolve to the real Dylan-compiled
//! code; `--lex-with-dylan` installs [`lex`] as the
//! `nod_reader::set_lex_override` callback and the entire front-end
//! starts going through Dylan-compiled lex.
//!
//! Without the cfg (fresh checkout, `.obj` deleted), the externs are
//! never declared, [`init`] returns an error, and `--lex-with-dylan`
//! prints a clear "build the shim first" message and falls back to the
//! Rust lexer.
//!
//! The bridge is the wire format from `docs/DYLAN_TOKEN_WIRE.md` plus
//! one ABI agreement: `dylan-lex-collect(source: <byte-string>) =>
//! <stretchy-vector>` lowers to `extern "C" fn(u64) -> u64`, with the
//! source byte-string passed as a tagged Word and the return being a
//! tagged-pointer Word to a stretchy-vector of `3N` boxed fixnums —
//! `(kind, lo, hi)` per emitted token.
//!
//! ## Why this is simpler than the JIT path we tried first
//!
//! There is no JIT engine to spin up, no `register_methods` /
//! `register_variables` replay (those calls already ran at AOT build
//! time and are baked into the resolver), no LLVMAddGlobalMapping list
//! to audit against `nod-runtime` externs. The shim is just code that
//! is already linked into our process; we point at its symbols.

use std::sync::OnceLock;

use nod_reader::{FileId, Span, Token, TokenKind};
use nod_runtime::Word;

// ─── Externs from the statically-linked shim .obj ─────────────────────────
//
// `dylan-lex-collect` is the entry function defined in
// `compiler/dylan-lex-shim.dylan`. Its LLVM symbol keeps
// the source-language name verbatim (dashes and all) because we built
// the .obj in `AotShape::StaticLibrary` mode, which skips the
// `nod_user_main` rename. `nod_aot_resolve_relocs` is the resolver
// that the AOT pipeline injected into the same .obj; it must be called
// exactly once before any other Dylan-side function fires.

#[cfg(dylan_lex_shim_linked)]
unsafe extern "C" {
    /// Sprint 51b — the resolver inside the linked shim .obj. Wires up
    /// every relocation site the codegen pass emitted: class metadata
    /// addresses, stub-table entries, string-literal pointers, generic
    /// dispatch slots. Idempotent in spirit but must run before any
    /// shim function — calling a Dylan function whose body references
    /// an unresolved global derefs NULL.
    fn nod_aot_resolve_relocs();

    /// Sprint 51b — `define function dylan-lex-collect (source) => …`
    /// from `dylan-lex-shim.dylan`. Word in, Word out. The source Word
    /// must be a `<byte-string>` (built via
    /// `nod_byte_string_allocate` + per-byte
    /// `nod_byte_string_element_setter`); the return is a pointer to
    /// a `<stretchy-vector>` whose entries are `3N` boxed fixnums.
    #[link_name = "dylan-lex-collect"]
    fn dylan_lex_collect(source: u64) -> u64;
}

// Stub versions for the no-shim build — keep the rest of this module
// type-checking; `init` reports a clear error and `lex` falls back.
#[cfg(not(dylan_lex_shim_linked))]
unsafe extern "C" fn nod_aot_resolve_relocs() {
    unreachable!("dylan_lex_shim_linked is not set — `init` should have errored");
}

#[cfg(not(dylan_lex_shim_linked))]
unsafe extern "C" fn dylan_lex_collect(_source: u64) -> u64 {
    unreachable!("dylan_lex_shim_linked is not set — `init` should have errored");
}

/// Marker recording that [`init`] has run successfully. Set inside the
/// `OnceLock` so re-installs are no-ops and the no-shim build can't
/// accidentally fire the resolver.
static INIT_GUARD: OnceLock<()> = OnceLock::new();

/// Materialise the side-loaded lexer. Calls
/// `nod_aot_resolve_relocs()` once and records success in
/// [`INIT_GUARD`]. Subsequent calls observe the recorded state and
/// return `Ok(())` cheaply.
///
/// Returns `Err(message)` when the shim wasn't statically linked
/// (`dylan_lex_shim_linked` cfg unset, i.e. the `.obj` was missing at
/// `cargo build` time). Callers should treat the error as "fall back
/// to the Rust lexer".
pub fn init() -> Result<(), String> {
    #[cfg(not(dylan_lex_shim_linked))]
    {
        return Err(
            "dylan-lex-shim.lib.obj not statically linked into this nod-driver binary. \
             Build it first:\n  \
             ./target/debug/nod-driver.exe build --library \
             --project compiler/dylan-lex-shim.prj \
             -o compiler/dylan-lex-shim.lib.obj\n\
             then `cargo build -p nod-driver` to pick it up via build.rs."
                .to_string(),
        );
    }

    #[cfg(dylan_lex_shim_linked)]
    {
        // First-call path runs the resolver. Subsequent calls observe
        // the OnceLock's stored value and skip the resolver — calling
        // it twice is at-best wasteful and at-worst confuses the
        // generic-dispatch cache slots.
        if INIT_GUARD.get().is_some() {
            return Ok(());
        }
        // SAFETY: nod_aot_resolve_relocs is the codegen-emitted
        // resolver from the linked .obj. Its preconditions (nod_runtime
        // initialised, the runtime registries empty of conflicting
        // entries for the shim's classes) are met because nod-driver's
        // startup calls `nod_sema::stdlib::ensure_loaded` via earlier
        // entry points OR (in the bare `--lex-with-dylan` startup case)
        // by the resolver itself, which is structured to be the first
        // runtime-touching code.
        unsafe {
            nod_aot_resolve_relocs();
        }
        let _ = INIT_GUARD.set(());
        // Sprint 51c — the same resolver covered the parser's
        // relocations (single combined .obj), so the parse-verify
        // path is now safe to call too. Telling
        // `dylan_parse_check` directly keeps it from re-running the
        // resolver.
        crate::dylan_parse_check::mark_available();
        Ok(())
    }
}

/// `nod_reader::LexFn`-compatible entry point. Marshals `src.as_bytes()`
/// into a Dylan `<byte-string>` via the public runtime ABI, calls the
/// statically-linked `dylan-lex-collect`, walks the returned
/// `<stretchy-vector>` in strides of 3 (kind, lo, hi), and reconstructs
/// `Vec<Token>`.
///
/// Falls back to [`nod_reader::lex_rust`] if [`init`] hasn't recorded
/// success — both for the no-shim build (cfg unset) and for the case
/// where the override got installed without the resolver actually
/// running (defensive; should not happen in practice).
pub fn lex(src: &str, file_id: FileId) -> Vec<Token> {
    if INIT_GUARD.get().is_none() {
        return nod_reader::lex_rust(src, file_id);
    }

    // Step 1 — build a Dylan `<byte-string>` from `src.as_bytes()`.
    // `nod_byte_string_allocate` returns a freshly-allocated buffer of
    // the requested size; subsequent setters populate it byte by byte.
    let bytes = src.as_bytes();
    let len_word = Word::from_fixnum(bytes.len() as i64).expect("source under fixnum max");
    // SAFETY: `nod_byte_string_allocate` is the runtime's vetted
    // constructor.
    let bs_raw = unsafe { nod_runtime::nod_byte_string_allocate(len_word.raw()) };
    for (i, &b) in bytes.iter().enumerate() {
        let byte_word = Word::from_fixnum(b as i64).expect("byte fits");
        let i_word = Word::from_fixnum(i as i64).expect("offset fits");
        // SAFETY: `bs_raw` was just allocated and isn't reachable from
        // the GC's mutator yet (we're in single-threaded driver code
        // between two synchronous runtime calls).
        unsafe {
            nod_runtime::nod_byte_string_element_setter(byte_word.raw(), bs_raw, i_word.raw());
        }
    }

    // Step 2 — call `dylan-lex-collect(bs)`. Word in, Word out.
    // SAFETY: bs_raw is a valid `<byte-string>` pointer; the Dylan-side
    // function was statically linked into this binary at build time.
    let sv_raw = unsafe { dylan_lex_collect(bs_raw) };

    // Step 3 — walk the returned stretchy-vector in strides of 3.
    let size_word_raw = unsafe { nod_runtime::nod_stretchy_vector_size(sv_raw) };
    let size = Word::from_raw(size_word_raw)
        .as_fixnum()
        .expect("size is fixnum") as usize;
    debug_assert!(
        size.is_multiple_of(3),
        "dylan-lex-collect returned {size} ints — not a multiple of 3 (kind, lo, hi)"
    );

    let mut tokens = Vec::with_capacity(size / 3);
    let mut i = 0;
    while i + 2 < size {
        let kind_raw = unsafe {
            nod_runtime::nod_stretchy_vector_element(
                sv_raw,
                Word::from_fixnum(i as i64).unwrap().raw(),
            )
        };
        let lo_raw = unsafe {
            nod_runtime::nod_stretchy_vector_element(
                sv_raw,
                Word::from_fixnum((i + 1) as i64).unwrap().raw(),
            )
        };
        let hi_raw = unsafe {
            nod_runtime::nod_stretchy_vector_element(
                sv_raw,
                Word::from_fixnum((i + 2) as i64).unwrap().raw(),
            )
        };

        let kind_ord = Word::from_raw(kind_raw)
            .as_fixnum()
            .expect("kind is fixnum");
        let lo = Word::from_raw(lo_raw).as_fixnum().expect("lo is fixnum") as u32;
        let hi = Word::from_raw(hi_raw).as_fixnum().expect("hi is fixnum") as u32;
        let kind = token_kind_from_ordinal(kind_ord);
        tokens.push(Token { kind, span: Span::new(file_id, lo, hi) });
        i += 3;
    }

    tokens
}

/// Map the wire-format kind ordinal (`docs/DYLAN_TOKEN_WIRE.md` §3)
/// back to a [`TokenKind`]. Discriminants ARE the `#[repr(u8)]`
/// ordinals of `TokenKind`, but we go through an explicit match so a
/// future enum reshuffle fails loudly here rather than producing
/// silently-corrupt tokens via `transmute`.
fn token_kind_from_ordinal(n: i64) -> TokenKind {
    match n {
        0 => TokenKind::Ident,
        1 => TokenKind::KwDefine,
        2 => TokenKind::KwEnd,
        3 => TokenKind::KwOtherwise,
        4 => TokenKind::EscapedIdent,
        5 => TokenKind::HashTrue,
        6 => TokenKind::HashFalse,
        7 => TokenKind::HashLParen,
        8 => TokenKind::HashLBracket,
        9 => TokenKind::HashLBrace,
        10 => TokenKind::HashHash,
        11 => TokenKind::HashRest,
        12 => TokenKind::HashKey,
        13 => TokenKind::HashAllKeys,
        14 => TokenKind::HashNext,
        15 => TokenKind::HashIncludeMarker,
        16 => TokenKind::Symbol,
        17 => TokenKind::HashKeyword,
        18 => TokenKind::IntegerBin,
        19 => TokenKind::IntegerOct,
        20 => TokenKind::IntegerHex,
        21 => TokenKind::KeywordColon,
        22 => TokenKind::Integer,
        23 => TokenKind::Float,
        24 => TokenKind::Ratio,
        25 => TokenKind::String,
        26 => TokenKind::StringMulti,
        27 => TokenKind::StringRaw,
        28 => TokenKind::Char,
        29 => TokenKind::LParen,
        30 => TokenKind::RParen,
        31 => TokenKind::LBracket,
        32 => TokenKind::RBracket,
        33 => TokenKind::LBrace,
        34 => TokenKind::RBrace,
        35 => TokenKind::Comma,
        36 => TokenKind::Semicolon,
        37 => TokenKind::Dot,
        38 => TokenKind::Ellipsis,
        39 => TokenKind::Colon,
        40 => TokenKind::ColonColon,
        41 => TokenKind::ColonEqual,
        42 => TokenKind::Equal,
        43 => TokenKind::EqualEqual,
        44 => TokenKind::Arrow,
        45 => TokenKind::Tilde,
        46 => TokenKind::TildeEqual,
        47 => TokenKind::TildeEqualEqual,
        48 => TokenKind::Plus,
        49 => TokenKind::Minus,
        50 => TokenKind::Star,
        51 => TokenKind::Slash,
        52 => TokenKind::Caret,
        53 => TokenKind::Amp,
        54 => TokenKind::Bar,
        55 => TokenKind::Less,
        56 => TokenKind::Greater,
        57 => TokenKind::LessEqual,
        58 => TokenKind::GreaterEqual,
        59 => TokenKind::Query,
        60 => TokenKind::QueryQuery,
        61 => TokenKind::QueryEqual,
        62 => TokenKind::QueryAt,
        63 => TokenKind::Eof,
        64 => TokenKind::Invalid,
        other => panic!(
            "dylan_lex_jit: unrecognised token kind {other} from wire format \
             (extend docs/DYLAN_TOKEN_WIRE.md §3 and this match together)"
        ),
    }
}
