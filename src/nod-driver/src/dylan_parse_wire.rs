//! Sprint 51d — Dylan AST wire format reader.
//!
//! Contract: `docs/DYLAN_AST_WIRE.md`. The Dylan-side
//! `dylan-parse-emit` produces a `<stretchy-vector>` of `4N` fixnums;
//! each 4-int record is `(kind, span_lo, span_hi, subtree_size)` and
//! children pack pre-order after their parent.
//!
//! v1 produces a [`DylanAst`] tree — a Rust mirror of the Dylan
//! parser's output, NOT yet a `nod_reader::ast::Module`. The
//! `dump-dylan-ast` subcommand uses this tree to print a textual
//! representation. Sprint 51e converts the mirror tree into the
//! canonical `ast::Module` so `--parse-with-dylan` can replace
//! `parse_module` outright.

use nod_runtime::Word;

#[cfg(dylan_lex_shim_linked)]
unsafe extern "C" {
    /// Sprint 51d — `define function dylan-parse-emit (source) =>
    /// (records :: <stretchy-vector>)` from `dylan-lex-shim.dylan`.
    /// Word in (a `<byte-string>`), Word out (a tagged-pointer to a
    /// `<stretchy-vector>` of `4N` boxed fixnums).
    #[link_name = "dylan-parse-emit"]
    fn dylan_parse_emit(source: u64) -> u64;
}

#[cfg(not(dylan_lex_shim_linked))]
unsafe extern "C" fn dylan_parse_emit(_source: u64) -> u64 {
    unreachable!("dylan_lex_shim_linked is not set")
}

#[cfg(dylan_lex_shim_linked)]
unsafe extern "C" {
    /// Sprint 52.6 — `define function dylan-expand-source (source,
    /// stdlib-source) => (expanded :: <byte-string>)` from
    /// `dylan-lex-shim.dylan`. Expands every macro call in `source` to
    /// fixpoint (using the stdlib macros from `stdlib-source` plus the
    /// file's own `define macro`s), strips the define-macro forms, and
    /// returns the expanded source (preamble preserved). Word in (two
    /// `<byte-string>`s), Word out (a `<byte-string>`).
    #[link_name = "dylan-expand-source"]
    fn dylan_expand_source(source: u64, stdlib_source: u64) -> u64;
}

#[cfg(not(dylan_lex_shim_linked))]
unsafe extern "C" fn dylan_expand_source(_source: u64, _stdlib: u64) -> u64 {
    unreachable!("dylan_lex_shim_linked is not set")
}

/// Allocate a Dylan `<byte-string>` from Rust bytes and return its Word.
fn alloc_dylan_byte_string(bytes: &[u8]) -> Result<u64, String> {
    let len_word =
        Word::from_fixnum(bytes.len() as i64).map_err(|_| "source longer than fixnum range".to_string())?;
    // SAFETY: nod_byte_string_allocate is the vetted constructor.
    let bs_raw = unsafe { nod_runtime::nod_byte_string_allocate(len_word.raw()) };
    for (i, &b) in bytes.iter().enumerate() {
        let byte_word = Word::from_fixnum(b as i64).expect("byte fits");
        let i_word = Word::from_fixnum(i as i64).expect("offset fits");
        // SAFETY: bs_raw just allocated, single-threaded.
        unsafe {
            nod_runtime::nod_byte_string_element_setter(byte_word.raw(), bs_raw, i_word.raw());
        }
    }
    Ok(bs_raw)
}

/// Read a Dylan `<byte-string>` Word back into a Rust `String`.
fn read_dylan_byte_string(bs_raw: u64) -> Result<String, String> {
    let size = Word::from_raw(unsafe { nod_runtime::nod_byte_string_size(bs_raw) })
        .as_fixnum()
        .ok_or_else(|| "byte-string size not a fixnum".to_string())? as usize;
    let mut bytes = Vec::with_capacity(size);
    for i in 0..size {
        let i_word = Word::from_fixnum(i as i64).expect("offset fits");
        let b = Word::from_raw(unsafe { nod_runtime::nod_byte_string_element(bs_raw, i_word.raw()) })
            .as_fixnum()
            .ok_or_else(|| "byte-string element not a fixnum".to_string())? as u8;
        bytes.push(b);
    }
    String::from_utf8(bytes).map_err(|e| format!("expanded source is not valid UTF-8: {e}"))
}

/// Sprint 52.6 (locus B) — expand `src`'s macro calls Dylan-side, before
/// the AST-wire emit, by calling the statically-linked `dylan-expand-source`
/// shim entry. `stdlib_src` is the stdlib macro source the Dylan side
/// collects stdlib `define macro`s from (pass `""` to use only the file's
/// own macros). Returns the expanded (macro-free) source on success.
pub fn expand_source_via_shim(src: &str, stdlib_src: &str) -> Result<String, String> {
    let src_bs = alloc_dylan_byte_string(src.as_bytes())?;
    let stdlib_bs = alloc_dylan_byte_string(stdlib_src.as_bytes())?;
    // SAFETY: both Words are live <byte-string>s; the entry is the
    // statically-linked expander.
    let out_bs = unsafe { dylan_expand_source(src_bs, stdlib_bs) };
    read_dylan_byte_string(out_bs)
}

#[cfg(dylan_lex_shim_linked)]
unsafe extern "C" {
    /// Sprint 54b — `define function dylan-sema-emit (source) => (model-text
    /// :: <byte-string>)` from `dylan-lex-shim.dylan`. Lexes + parses
    /// (honouring `Precedence: c`) then runs the Dylan-side sema recording
    /// walk (`collect-top-names`) and returns its four-section model dump,
    /// byte-identical to `nod_sema::format_sema_model`. Word in (a
    /// `<byte-string>`), Word out (a `<byte-string>`).
    #[link_name = "dylan-sema-emit"]
    fn dylan_sema_emit(source: u64) -> u64;
}

#[cfg(not(dylan_lex_shim_linked))]
unsafe extern "C" fn dylan_sema_emit(_source: u64) -> u64 {
    unreachable!("dylan_lex_shim_linked is not set")
}

/// Sprint 54b — run the Dylan-side sema recording walk in-process via the
/// statically-linked `dylan-sema-emit` shim entry, returning its model dump
/// (the `=== top-names ===` … `=== sealing ===` text). Caller must have run
/// [`crate::dylan_lex_jit::init`] first (it fires the one-time
/// `nod_aot_resolve_relocs`). Used by `dump-dylan-sema` and the
/// `--sema-with-dylan` verify path.
pub fn sema_emit_via_shim(src: &str) -> Result<String, String> {
    let src_bs = alloc_dylan_byte_string(src.as_bytes())?;
    // SAFETY: src_bs is a live <byte-string>; the entry is the statically-
    // linked sema walk, called after init()'s nod_aot_resolve_relocs.
    let out_bs = unsafe { dylan_sema_emit(src_bs) };
    read_dylan_byte_string(out_bs)
}

#[cfg(dylan_lex_shim_linked)]
unsafe extern "C" {
    /// Sprint 55 Phase 0 — `define function dylan-lower-emit (source) =>
    /// (dfm-text :: <byte-string>)` from `dylan-lower.dylan`. Lexes + parses
    /// then runs the Dylan-side AST→DFM lowering (Phase-0 straight-line
    /// subset) and returns the `dump-dfm` text, byte-identical to
    /// `nod_dfm::format_dfm_module`. Returns "" for any module outside the
    /// Phase-0 subset (so the host keeps it on the Rust path). Word in (a
    /// `<byte-string>`), Word out (a `<byte-string>`).
    #[link_name = "dylan-lower-emit"]
    fn dylan_lower_emit(source: u64) -> u64;
}

#[cfg(not(dylan_lex_shim_linked))]
unsafe extern "C" fn dylan_lower_emit(_source: u64) -> u64 {
    unreachable!("dylan_lex_shim_linked is not set")
}

/// Sprint 55 Phase 0 — run the Dylan-side AST→DFM lowering in-process via the
/// statically-linked `dylan-lower-emit` shim entry, returning its `dump-dfm`
/// text (empty string when the module is outside the Phase-0 subset). Caller
/// must have run [`crate::dylan_lex_jit::init`] first. Used by
/// `dump-dylan-dfm` (the `dump-dfm` byte-match gate for the lowering port).
pub fn lower_emit_via_shim(src: &str) -> Result<String, String> {
    let src_bs = alloc_dylan_byte_string(src.as_bytes())?;
    // SAFETY: src_bs is a live <byte-string>; the entry is the statically-
    // linked Dylan lowering, called after init()'s nod_aot_resolve_relocs.
    let out_bs = unsafe { dylan_lower_emit(src_bs) };
    read_dylan_byte_string(out_bs)
}

/// AST kind codes — must match `docs/DYLAN_AST_WIRE.md` §3 and the
/// `$ast-kind-*` constants in `dylan-lex-shim.dylan`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(u8)]
pub enum Kind {
    Body = 0,
    DefineFunction = 1,
    Call = 2,
    VariableRef = 3,
    StringLit = 4,
    IntegerLit = 5,
    BinaryOp = 6,
    Error = 7,
    DefineClass = 8,
    DefineMethod = 9,
    DefineGeneric = 10,
    Statement = 11,
    StatementClause = 12,
    LocalDecl = 13,
    SlotSpec = 14,
    DotCall = 15,
    Subscript = 16,
    UnaryOp = 17,
    KwArg = 18,
    ParenList = 19,
    BoolLit = 20,
    CharLit = 21,
    SymbolLit = 22,
    FloatLit = 23,
    RatioLit = 24,
    ParamList = 25,
    ReturnSpec = 26,
    DefName = 27,
    Param = 28,
    VarMarker = 29,
    ReturnValue = 30,
    SlotAlloc = 31,
    SlotInitKw = 32,
    SlotRequired = 33,
    SlotType = 34,
    SlotInit = 35,
    HashLit = 36,
    DefineBinding = 37,
    Modifier = 38,
}

impl Kind {
    fn from_i64(n: i64) -> Option<Kind> {
        Some(match n {
            0 => Kind::Body,
            1 => Kind::DefineFunction,
            2 => Kind::Call,
            3 => Kind::VariableRef,
            4 => Kind::StringLit,
            5 => Kind::IntegerLit,
            6 => Kind::BinaryOp,
            7 => Kind::Error,
            8 => Kind::DefineClass,
            9 => Kind::DefineMethod,
            10 => Kind::DefineGeneric,
            11 => Kind::Statement,
            12 => Kind::StatementClause,
            13 => Kind::LocalDecl,
            14 => Kind::SlotSpec,
            15 => Kind::DotCall,
            16 => Kind::Subscript,
            17 => Kind::UnaryOp,
            18 => Kind::KwArg,
            19 => Kind::ParenList,
            20 => Kind::BoolLit,
            21 => Kind::CharLit,
            22 => Kind::SymbolLit,
            23 => Kind::FloatLit,
            24 => Kind::RatioLit,
            25 => Kind::ParamList,
            26 => Kind::ReturnSpec,
            27 => Kind::DefName,
            28 => Kind::Param,
            29 => Kind::VarMarker,
            30 => Kind::ReturnValue,
            31 => Kind::SlotAlloc,
            32 => Kind::SlotInitKw,
            33 => Kind::SlotRequired,
            34 => Kind::SlotType,
            35 => Kind::SlotInit,
            36 => Kind::HashLit,
            37 => Kind::DefineBinding,
            38 => Kind::Modifier,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Body => "Body",
            Kind::DefineFunction => "DefineFunction",
            Kind::Call => "Call",
            Kind::VariableRef => "VariableRef",
            Kind::StringLit => "StringLit",
            Kind::IntegerLit => "IntegerLit",
            Kind::BinaryOp => "BinaryOp",
            Kind::Error => "Error",
            Kind::DefineClass => "DefineClass",
            Kind::DefineMethod => "DefineMethod",
            Kind::DefineGeneric => "DefineGeneric",
            Kind::Statement => "Statement",
            Kind::StatementClause => "StatementClause",
            Kind::LocalDecl => "LocalDecl",
            Kind::SlotSpec => "SlotSpec",
            Kind::DotCall => "DotCall",
            Kind::Subscript => "Subscript",
            Kind::UnaryOp => "UnaryOp",
            Kind::KwArg => "KwArg",
            Kind::ParenList => "ParenList",
            Kind::BoolLit => "BoolLit",
            Kind::CharLit => "CharLit",
            Kind::SymbolLit => "SymbolLit",
            Kind::FloatLit => "FloatLit",
            Kind::RatioLit => "RatioLit",
            Kind::ParamList => "ParamList",
            Kind::ReturnSpec => "ReturnSpec",
            Kind::DefName => "DefName",
            Kind::Param => "Param",
            Kind::VarMarker => "VarMarker",
            Kind::ReturnValue => "ReturnValue",
            Kind::SlotAlloc => "SlotAlloc",
            Kind::SlotInitKw => "SlotInitKw",
            Kind::SlotRequired => "SlotRequired",
            Kind::SlotType => "SlotType",
            Kind::SlotInit => "SlotInit",
            Kind::HashLit => "HashLit",
            Kind::DefineBinding => "DefineBinding",
            Kind::Modifier => "Modifier",
        }
    }
}

/// One AST node as decoded from the wire. Children are in source
/// (pre-)order. Spans are byte offsets into the source the caller
/// passed to [`parse_to_tree`].
#[derive(Debug, Clone)]
pub struct DylanAst {
    pub kind: Kind,
    pub span_lo: u32,
    pub span_hi: u32,
    pub children: Vec<DylanAst>,
}

/// Parse `src` through the statically-linked Dylan parser and decode
/// the wire-format result. Returns `Err` if the shim isn't linked
/// (caller should fall back to the Rust parser).
pub fn parse_to_tree(src: &str) -> Result<DylanAst, String> {
    #[cfg(not(dylan_lex_shim_linked))]
    {
        let _ = src;
        return Err("dylan-lex-shim not statically linked".to_string());
    }

    #[cfg(dylan_lex_shim_linked)]
    {
        // Build a Dylan `<byte-string>` from the source bytes (same
        // marshalling as the lex + verify-parse paths).
        let bytes = src.as_bytes();
        let len_word = Word::from_fixnum(bytes.len() as i64)
            .map_err(|_| "source longer than fixnum range".to_string())?;
        // SAFETY: nod_byte_string_allocate is the vetted constructor.
        let bs_raw = unsafe { nod_runtime::nod_byte_string_allocate(len_word.raw()) };
        for (i, &b) in bytes.iter().enumerate() {
            let byte_word = Word::from_fixnum(b as i64).expect("byte fits");
            let i_word = Word::from_fixnum(i as i64).expect("offset fits");
            // SAFETY: bs_raw just allocated, single-threaded.
            unsafe {
                nod_runtime::nod_byte_string_element_setter(
                    byte_word.raw(),
                    bs_raw,
                    i_word.raw(),
                );
            }
        }

        // SAFETY: bs_raw is a live <byte-string>; the entry is the
        // statically-linked emitter.
        let sv_raw = unsafe { dylan_parse_emit(bs_raw) };

        // Read the stretchy-vector into a flat Vec<i64>.
        let size_word_raw = unsafe { nod_runtime::nod_stretchy_vector_size(sv_raw) };
        let size = Word::from_raw(size_word_raw)
            .as_fixnum()
            .ok_or_else(|| "vector size not a fixnum".to_string())? as usize;
        if !size.is_multiple_of(4) {
            return Err(format!(
                "dylan-parse-emit produced {size} ints, not a multiple of 4"
            ));
        }
        let mut flat = Vec::with_capacity(size);
        for i in 0..size {
            let elem_raw = unsafe {
                nod_runtime::nod_stretchy_vector_element(
                    sv_raw,
                    Word::from_fixnum(i as i64).unwrap().raw(),
                )
            };
            let n = Word::from_raw(elem_raw)
                .as_fixnum()
                .ok_or_else(|| format!("record element [{i}] is not a fixnum"))?;
            flat.push(n);
        }

        // Recursive descent over the flat records.
        let (tree, consumed) = decode_record(&flat, 0)?;
        if consumed != flat.len() / 4 {
            return Err(format!(
                "wire format consumed {consumed} records of {} — trailing data",
                flat.len() / 4
            ));
        }
        Ok(tree)
    }
}

#[cfg(dylan_lex_shim_linked)]
fn decode_record(flat: &[i64], rec_idx: usize) -> Result<(DylanAst, usize), String> {
    let base = rec_idx * 4;
    if base + 3 >= flat.len() {
        return Err(format!("record {rec_idx} out of bounds"));
    }
    let kind_n = flat[base];
    let span_lo = flat[base + 1];
    let span_hi = flat[base + 2];
    let subtree_size = flat[base + 3] as usize;
    if subtree_size == 0 {
        return Err(format!("record {rec_idx} has zero subtree_size"));
    }
    let kind = Kind::from_i64(kind_n).ok_or_else(|| {
        format!("record {rec_idx}: unknown kind ordinal {kind_n}")
    })?;
    let mut children = Vec::new();
    let mut child_idx = rec_idx + 1;
    let end_idx = rec_idx + subtree_size;
    while child_idx < end_idx {
        let (child, consumed) = decode_record(flat, child_idx)?;
        child_idx += consumed;
        children.push(child);
    }
    if child_idx != end_idx {
        return Err(format!(
            "record {rec_idx} subtree_size mismatch: claimed {subtree_size}, walked {}",
            child_idx - rec_idx
        ));
    }
    Ok((
        DylanAst {
            kind,
            span_lo: span_lo as u32,
            span_hi: span_hi as u32,
            children,
        },
        subtree_size,
    ))
}

/// Render a [`DylanAst`] as an indented Lisp-y tree. Spans resolve
/// against `src` for leaf payloads.
pub fn format_tree(node: &DylanAst, src: &str) -> String {
    let mut out = String::new();
    format_node(node, src, 0, &mut out);
    out
}

fn format_node(node: &DylanAst, src: &str, depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push('(');
    out.push_str(node.kind.name());
    out.push_str(&format!(" {}..{}", node.span_lo, node.span_hi));
    // Leaf payload preview from the span.
    if matches!(
        node.kind,
        Kind::VariableRef
            | Kind::StringLit
            | Kind::IntegerLit
            | Kind::BoolLit
            | Kind::CharLit
            | Kind::SymbolLit
            | Kind::FloatLit
            | Kind::RatioLit
            | Kind::DefName
    ) {
        let lo = node.span_lo as usize;
        let hi = node.span_hi as usize;
        if lo <= hi && hi <= src.len() {
            out.push_str(&format!(" {:?}", &src[lo..hi]));
        }
    }
    if node.children.is_empty() {
        out.push(')');
        out.push('\n');
    } else {
        out.push('\n');
        for c in &node.children {
            format_node(c, src, depth + 1, out);
        }
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push(')');
        out.push('\n');
    }
}
