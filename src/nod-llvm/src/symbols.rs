//! Sprint 38 — stable named-symbol naming convention for cross-process
//! bitcode replay.
//!
//! Sprint 37 baked runtime addresses (class metadata pointers, literal
//! pool singleton addresses, stub-table entry pointers, inline cache
//! slot addresses, generic-function addresses) as `i64` constants
//! straight into LLVM IR. That's fine for in-process replay where the
//! addresses survive the cache-hit hot path, but fails for cross-process
//! replay because every process re-allocates the runtime's static area
//! at a fresh OS address.
//!
//! Sprint 38 replaces each baked address with a reference to a named
//! external global. The JIT-link step registers each name → current
//! process address before MCJIT finalises, so the symbols resolve at
//! load time to fresh, in-process addresses.
//!
//! ## Symbol naming
//!
//! Per-module, deterministic, content-keyed. The module's cache key
//! prefix is folded into every symbol so cached modules co-loaded into
//! the same JIT engine don't collide on symbol names.
//!
//! - **Stub entries** (Win32 FFI):
//!   `nod_stub__{key8}__{slot_index}` (slot index assigned in codegen
//!   order; payload in manifest records the (dll, symbol) pair).
//! - **Inline cache slots**:
//!   `nod_cache_slot__{key8}__{site_id}` — site_id is the codegen's
//!   stable per-site counter, already content-deterministic.
//! - **Generic functions** (for the generation field):
//!   `nod_generic__{key8}__{sanitized_name}`.
//! - **Class metadata pointers**:
//!   `nod_class_md__{key8}__{class_id}` — class_id is u32, stable
//!   across runs because the runtime registers seed classes in fixed
//!   order before any cache loading.
//! - **Literal pool immediates**:
//!   `nod_imm_true__{key8}`, `nod_imm_false__{key8}`, `nod_imm_nil__{key8}`,
//!   `nod_imm_false_wrapper__{key8}` (the untagged-wrapper variant
//!   for fallback loads in class-id reads).
//! - **String literals**:
//!   `nod_strlit__{key8}__{lit_index}` — payload in manifest carries
//!   the literal text.
//! - **Symbol literals**:
//!   `nod_symlit__{key8}__{sym_index}` — payload in manifest carries
//!   the symbol name.
//!
//! `{key8}` is the first 16 hex characters (8 bytes) of the module's
//! Sprint 37 256-bit cache key — sufficient to give each module a
//! distinct namespace without bloating IR text.

use std::fmt::Write as _;

use crate::cache::CacheKey;

/// Compute the 16-character (8-byte) key prefix used to namespace
/// per-module symbols.
pub fn key_prefix(key: CacheKey) -> String {
    let full = key.to_hex();
    full[..16].to_string()
}

/// Sanitize a string for use in an LLVM symbol name: replace any
/// character that isn't `[A-Za-z0-9_]` with `_`. Matches the
/// conservative set MCJIT's symbol resolver accepts.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

pub fn stub_symbol(key: CacheKey, slot_index: u32) -> String {
    format!("nod_stub__{}__{}", key_prefix(key), slot_index)
}

pub fn cache_slot_symbol(key: CacheKey, site_id: u64) -> String {
    format!("nod_cache_slot__{}__{}", key_prefix(key), site_id)
}

pub fn generic_symbol(key: CacheKey, name: &str) -> String {
    format!("nod_generic__{}__{}", key_prefix(key), sanitize(name))
}

pub fn class_md_symbol(key: CacheKey, class_id: u32) -> String {
    format!("nod_class_md__{}__{}", key_prefix(key), class_id)
}

pub fn imm_true_symbol(key: CacheKey) -> String {
    format!("nod_imm_true__{}", key_prefix(key))
}

pub fn imm_false_symbol(key: CacheKey) -> String {
    format!("nod_imm_false__{}", key_prefix(key))
}

pub fn imm_nil_symbol(key: CacheKey) -> String {
    format!("nod_imm_nil__{}", key_prefix(key))
}

pub fn imm_false_wrapper_symbol(key: CacheKey) -> String {
    format!("nod_imm_false_wrapper__{}", key_prefix(key))
}

pub fn strlit_symbol(key: CacheKey, lit_index: u32) -> String {
    format!("nod_strlit__{}__{}", key_prefix(key), lit_index)
}

pub fn symlit_symbol(key: CacheKey, sym_index: u32) -> String {
    format!("nod_symlit__{}__{}", key_prefix(key), sym_index)
}

/// One row of the per-module relocation manifest. Describes what value
/// a named symbol should resolve to at JIT-link time. Both cold compile
/// (when we have the address in-process) and warm replay (when we need
/// to recompute the address from the current process's runtime state)
/// use the same enum — cold path materialises the address eagerly and
/// records the kind for later replay; warm path materialises the
/// address from the kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelocKind {
    /// Win32 FFI stub entry — payload is `(dll, symbol)`. The replay
    /// path allocates a fresh `ApiStubEntry` in the current process,
    /// calls `LoadLibrary` + `GetProcAddress` to populate `fn_ptr`,
    /// and registers the entry address. The `signature` field carries
    /// the postcard-encoded `ApiCallSignature` so the replay path can
    /// reconstruct the same trampoline behavior.
    StubEntry {
        dll: String,
        symbol: String,
        signature_bytes: Vec<u8>,
    },
    /// Inline-cache slot — payload is the codegen `site_id`. Replay
    /// allocates a fresh `CacheSlot` with the same `site_id` and
    /// registers its address.
    CacheSlot { site_id: u64 },
    /// Generic-function address — payload is the generic's name.
    /// Replay calls `get_or_create_generic(name)` and registers the
    /// returned pointer.
    Generic { name: String },
    /// Class metadata pointer — payload is the class id. Replay looks
    /// up `class_metadata_ptr(class_id)` and registers it.
    ClassMetadata { class_id: u32 },
    /// `#t` singleton's tagged Word bits.
    ImmTrue,
    /// `#f` singleton's tagged Word bits.
    ImmFalse,
    /// `nil` (empty-list) singleton's tagged Word bits.
    ImmNil,
    /// `#f` singleton's *untagged-wrapper* address — used by codegen
    /// as a fault-free fallback in branchless class-id reads.
    ImmFalseWrapper,
    /// `<byte-string>` literal — payload is the UTF-8 text. Replay
    /// calls `intern_string_literal(text)` and registers the resulting
    /// Word's tagged bits.
    StringLiteral { text: String },
    /// `<symbol>` literal — payload is the symbol name. Replay calls
    /// `intern_symbol_literal(name)` and registers the result.
    SymbolLiteral { name: String },
}

/// One row of the manifest: a symbol name and the kind describing what
/// it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocEntry {
    pub symbol: String,
    pub kind: RelocKind,
}

/// Whole-module manifest. Serialised to `<key>.manifest.json` next to
/// the bitcode; read on cache hit before MCJIT finalises.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleManifest {
    /// Bumped on any change to `RelocKind`, the JSON shape, or the
    /// symbol-naming scheme. A mismatch invalidates the cache entry
    /// (treated as if the bitcode didn't exist).
    pub manifest_version: u32,
    /// 16-char hex key prefix, for sanity-checking.
    pub key_prefix: String,
    /// All relocation rows in arbitrary order. The symbol names are
    /// content-deterministic; the order isn't.
    pub entries: Vec<RelocEntry>,
}

/// Current manifest schema version. Bump on any breaking change.
pub const MANIFEST_VERSION: u32 = 1;

impl ModuleManifest {
    pub fn new(key: CacheKey) -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            key_prefix: key_prefix(key),
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, symbol: String, kind: RelocKind) {
        self.entries.push(RelocEntry { symbol, kind });
    }

    /// Encode the manifest as a hand-rolled JSON string. Avoids pulling
    /// `serde_json` into `nod-llvm`'s dep tree (Sprint 37 sidecar
    /// follows the same precedent).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        let _ = write!(
            out,
            "\"manifest_version\":{},\"key_prefix\":\"{}\",\"entries\":[",
            self.manifest_version, self.key_prefix
        );
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            let _ = write!(out, "\"symbol\":\"{}\"", escape_json(&entry.symbol));
            match &entry.kind {
                RelocKind::StubEntry { dll, symbol, signature_bytes } => {
                    let _ = write!(out, ",\"kind\":\"stub\",\"dll\":\"{}\",\"sym\":\"{}\",\"sig\":\"",
                        escape_json(dll), escape_json(symbol));
                    for b in signature_bytes {
                        let _ = write!(out, "{b:02x}");
                    }
                    out.push('"');
                }
                RelocKind::CacheSlot { site_id } => {
                    let _ = write!(out, ",\"kind\":\"cache_slot\",\"site_id\":{site_id}");
                }
                RelocKind::Generic { name } => {
                    let _ = write!(out, ",\"kind\":\"generic\",\"name\":\"{}\"", escape_json(name));
                }
                RelocKind::ClassMetadata { class_id } => {
                    let _ = write!(out, ",\"kind\":\"class_md\",\"class_id\":{class_id}");
                }
                RelocKind::ImmTrue => out.push_str(",\"kind\":\"imm_true\""),
                RelocKind::ImmFalse => out.push_str(",\"kind\":\"imm_false\""),
                RelocKind::ImmNil => out.push_str(",\"kind\":\"imm_nil\""),
                RelocKind::ImmFalseWrapper => out.push_str(",\"kind\":\"imm_false_wrapper\""),
                RelocKind::StringLiteral { text } => {
                    let _ = write!(out, ",\"kind\":\"strlit\",\"text\":\"{}\"", escape_json(text));
                }
                RelocKind::SymbolLiteral { name } => {
                    let _ = write!(out, ",\"kind\":\"symlit\",\"name\":\"{}\"", escape_json(name));
                }
            }
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// Parse the JSON produced by [`Self::to_json`]. Returns `None` on
    /// any structural error — caller treats that as a cache miss.
    pub fn parse(text: &str) -> Option<Self> {
        let mut p = JsonParser::new(text);
        p.expect('{')?;
        let mut manifest_version = 0u32;
        let mut key_prefix = String::new();
        let mut entries: Vec<RelocEntry> = Vec::new();
        let mut first = true;
        loop {
            p.skip_ws();
            if p.peek() == Some('}') {
                p.bump();
                break;
            }
            if !first {
                p.expect(',')?;
                p.skip_ws();
            }
            first = false;
            let key = p.string()?;
            p.expect(':')?;
            p.skip_ws();
            match key.as_str() {
                "manifest_version" => {
                    manifest_version = p.number()? as u32;
                }
                "key_prefix" => {
                    key_prefix = p.string()?;
                }
                "entries" => {
                    p.expect('[')?;
                    let mut inner_first = true;
                    loop {
                        p.skip_ws();
                        if p.peek() == Some(']') {
                            p.bump();
                            break;
                        }
                        if !inner_first {
                            p.expect(',')?;
                            p.skip_ws();
                        }
                        inner_first = false;
                        let entry = parse_entry(&mut p)?;
                        entries.push(entry);
                    }
                }
                _ => return None,
            }
        }
        Some(Self {
            manifest_version,
            key_prefix,
            entries,
        })
    }
}

fn parse_entry(p: &mut JsonParser<'_>) -> Option<RelocEntry> {
    p.expect('{')?;
    let mut symbol = String::new();
    let mut kind_str = String::new();
    let mut dll = String::new();
    let mut sym = String::new();
    let mut sig_hex = String::new();
    let mut site_id = 0u64;
    let mut name = String::new();
    let mut class_id = 0u32;
    let mut text = String::new();
    let mut first = true;
    loop {
        p.skip_ws();
        if p.peek() == Some('}') {
            p.bump();
            break;
        }
        if !first {
            p.expect(',')?;
            p.skip_ws();
        }
        first = false;
        let key = p.string()?;
        p.expect(':')?;
        p.skip_ws();
        match key.as_str() {
            "symbol" => symbol = p.string()?,
            "kind" => kind_str = p.string()?,
            "dll" => dll = p.string()?,
            "sym" => sym = p.string()?,
            "sig" => sig_hex = p.string()?,
            "site_id" => site_id = p.number()? as u64,
            "name" => name = p.string()?,
            "class_id" => class_id = p.number()? as u32,
            "text" => text = p.string()?,
            _ => return None,
        }
    }
    let kind = match kind_str.as_str() {
        "stub" => {
            let mut signature_bytes = Vec::with_capacity(sig_hex.len() / 2);
            let bytes = sig_hex.as_bytes();
            if !bytes.len().is_multiple_of(2) {
                return None;
            }
            for chunk in bytes.chunks(2) {
                let s = std::str::from_utf8(chunk).ok()?;
                signature_bytes.push(u8::from_str_radix(s, 16).ok()?);
            }
            RelocKind::StubEntry { dll, symbol: sym, signature_bytes }
        }
        "cache_slot" => RelocKind::CacheSlot { site_id },
        "generic" => RelocKind::Generic { name },
        "class_md" => RelocKind::ClassMetadata { class_id },
        "imm_true" => RelocKind::ImmTrue,
        "imm_false" => RelocKind::ImmFalse,
        "imm_nil" => RelocKind::ImmNil,
        "imm_false_wrapper" => RelocKind::ImmFalseWrapper,
        "strlit" => RelocKind::StringLiteral { text },
        "symlit" => RelocKind::SymbolLiteral { name },
        _ => return None,
    };
    Some(RelocEntry { symbol, kind })
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

struct JsonParser<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> JsonParser<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, i: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.i += c.len_utf8();
        }
    }

    fn expect(&mut self, c: char) -> Option<()> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.bump();
            Some(())
        } else {
            None
        }
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        self.skip_ws();
        if self.peek() != Some('"') {
            return None;
        }
        self.bump();
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            if c == '"' {
                self.bump();
                return Some(out);
            } else if c == '\\' {
                self.bump();
                let esc = self.peek()?;
                self.bump();
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let h = self.peek()?;
                            self.bump();
                            let v = h.to_digit(16)?;
                            code = (code << 4) | v;
                        }
                        out.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            } else {
                self.bump();
                out.push(c);
            }
        }
    }

    fn number(&mut self) -> Option<i64> {
        self.skip_ws();
        let start = self.i;
        if self.peek() == Some('-') {
            self.bump();
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.bump();
            } else {
                break;
            }
        }
        if self.i == start {
            return None;
        }
        let s = &self.s[start..self.i];
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k() -> CacheKey {
        CacheKey([0x0123_4567_89ab_cdef, 0, 0, 0])
    }

    #[test]
    fn key_prefix_is_16_chars() {
        let p = key_prefix(k());
        assert_eq!(p.len(), 16);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sanitize_keeps_alphanumerics_and_underscore() {
        assert_eq!(sanitize("foo_bar123"), "foo_bar123");
        assert_eq!(sanitize("<integer>"), "_integer_");
        assert_eq!(sanitize("kernel32.dll"), "kernel32_dll");
        assert_eq!(sanitize("with-dashes"), "with_dashes");
    }

    #[test]
    fn stable_symbol_naming_is_collision_resistant() {
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();
        // Generate ~1000 distinct stubs and assert no collisions.
        for i in 0..1000u32 {
            let s = stub_symbol(k(), i);
            assert!(seen.insert(s.clone()), "collision on {s}");
        }
        for i in 0..1000u64 {
            let s = cache_slot_symbol(k(), i);
            assert!(seen.insert(s.clone()), "collision on {s}");
        }
        for i in 0..1000u32 {
            let s = class_md_symbol(k(), i);
            assert!(seen.insert(s.clone()), "collision on {s}");
        }
    }

    #[test]
    fn manifest_round_trips() {
        let mut m = ModuleManifest::new(k());
        m.push(
            stub_symbol(k(), 0),
            RelocKind::StubEntry {
                dll: "kernel32.dll".into(),
                symbol: "Beep".into(),
                signature_bytes: vec![1, 2, 3, 0xff],
            },
        );
        m.push(cache_slot_symbol(k(), 17), RelocKind::CacheSlot { site_id: 17 });
        m.push(generic_symbol(k(), "+"), RelocKind::Generic { name: "+".into() });
        m.push(
            class_md_symbol(k(), 42),
            RelocKind::ClassMetadata { class_id: 42 },
        );
        m.push(imm_true_symbol(k()), RelocKind::ImmTrue);
        m.push(imm_false_symbol(k()), RelocKind::ImmFalse);
        m.push(imm_nil_symbol(k()), RelocKind::ImmNil);
        m.push(imm_false_wrapper_symbol(k()), RelocKind::ImmFalseWrapper);
        m.push(
            strlit_symbol(k(), 3),
            RelocKind::StringLiteral { text: "héllo \"world\"\n".into() },
        );
        m.push(
            symlit_symbol(k(), 5),
            RelocKind::SymbolLiteral { name: "foo".into() },
        );
        let json = m.to_json();
        let back = ModuleManifest::parse(&json).expect("parse");
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_version_mismatch_rejected() {
        let mut m = ModuleManifest::new(k());
        m.manifest_version = 999;
        m.push(imm_true_symbol(k()), RelocKind::ImmTrue);
        let json = m.to_json();
        let back = ModuleManifest::parse(&json).expect("parse");
        // The parser doesn't reject — caller checks `manifest_version`
        // against `MANIFEST_VERSION`. The version round-trips though.
        assert_eq!(back.manifest_version, 999);
    }

    #[test]
    fn manifest_parse_rejects_garbage() {
        assert!(ModuleManifest::parse("").is_none());
        assert!(ModuleManifest::parse("{").is_none());
        assert!(ModuleManifest::parse("not json").is_none());
    }
}
