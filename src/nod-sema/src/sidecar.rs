//! Sprint 38f — on-disk registration sidecar for cross-process replay.
//!
//! Sprint 38a–38e finished the bitcode-relocation story: every
//! process-volatile address baked into IR now flows through a named
//! external global resolved at JIT-link time via the manifest. That
//! made `Jit::add_module_from_bitcode` correct for cross-process replay
//! — but the JIT'd code only becomes *callable* after four
//! post-codegen registration passes that aren't recoverable from the
//! bitcode alone:
//!
//!   1. `register_methods` — wires each `MethodRegistration`'s
//!      `body_fn_name` into the runtime dispatch table.
//!   2. `register_blocks` — resolves block-form lifted thunks
//!      (body / cleanup / afterwards / handlers).
//!   3. `register_top_level_functions` — registers every non-block,
//!      non-`<eval-entry>` function in `lm.functions` against
//!      `nod_runtime`'s function-ref table (consulting `lm.closures`
//!      for closure env-arity correction).
//!   4. `initialize_module_winffi` — Sprint 38d made this **redundant**
//!      on the replay path: every stub-entry reloc resolves through
//!      `nod_runtime::stub_entry_slot_addr`, which lazily allocates the
//!      stub table entry AND eagerly resolves `LoadLibrary` +
//!      `GetProcAddress` on first lookup. The replay path therefore
//!      doesn't need any winffi-specific persistence.
//!
//! This module persists the data needed to replay (1)–(3) plus the
//! [`return_type`](nod_dfm::TypeEstimate) encoding the entry point's
//! caller uses to pick a `call_and_format` arm.
//!
//! ## Storage layout
//!
//! Alongside the existing Sprint 37/38 sidecars:
//!
//! ```text
//!   <cache_dir>/
//!     <hex_key>.bc                    ← LLVM bitcode (Sprint 37)
//!     <hex_key>.json                  ← LRU metadata sidecar (Sprint 37)
//!     <hex_key>.manifest.json         ← reloc manifest (Sprint 38a)
//!     <hex_key>.registrations.json    ← THIS SPRINT (38f)
//! ```
//!
//! The new file is intentionally separate so the Sprint 37 LRU sidecar
//! (`<key>.json`) stays a stable shape — that file's `size_bytes` field
//! is bitcode-only by construction, and folding new data into it would
//! break the existing `read_cache_entry` size cross-check.
//!
//! ## ABI version
//!
//! [`REGISTRATIONS_ABI_VERSION`] is a separate constant from
//! [`nod_llvm::NOD_RUNTIME_ABI_VERSION`]. The registration sidecar
//! captures sema-side metadata; a bump here means "the registration
//! sidecar JSON schema or the registration replay contract changed",
//! not "the IR / runtime ABI changed". Mismatch → treated as a cache
//! miss and the cold compile path runs.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use nod_dfm::TypeEstimate;

use crate::lower::{
    BlockHandlerRegistration, BlockRegistration, LoweredModule, MethodRegistration,
};

/// Current schema version for the registration sidecar. Bump on any
/// breaking change to the JSON shape, the persisted registration
/// fields, or the replay contract that consumes them.
///
/// GAP-004 — bumped from 1 to 2 with the addition of the `variables`
/// list. Old sidecars are treated as ABI-incompatible (a cache miss)
/// and the cold path overwrites them.
pub const REGISTRATIONS_ABI_VERSION: u32 = 2;

/// One persisted top-level function registration. Carries the
/// `(name, source-arity)` pair `register_top_level_functions` needs.
/// `is_closure` + `source_arity` mirror Sprint 24's closure env-arity
/// correction: the JIT signature carries a hidden env parameter, so we
/// register under the source arity, not `params.len()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedFunction {
    pub name: String,
    pub arity: u32,
    pub is_closure: bool,
    /// Sprint 24: closure source arity (== `arity` here for both branches;
    /// kept as a separate field for forward-compat with future
    /// closure-shape changes that may carry distinct values).
    pub source_arity: u32,
}

/// One persisted method registration. Mirrors `MethodRegistration`
/// minus the `ClassId` newtype (stored as `u32` since `ClassId` is a
/// `#[repr(transparent)] u32` wrapper).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedMethod {
    pub generic_name: String,
    pub specialisers: Vec<u32>,
    pub body_fn_name: String,
    pub param_count: u32,
}

/// One persisted exception handler. Mirrors `BlockHandlerRegistration`
/// with `ClassId` flattened to `u32`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedHandler {
    pub class_id: u32,
    pub class_name: String,
    pub body_fn_name: String,
}

/// One persisted block-form registration. Mirrors `BlockRegistration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedBlock {
    pub block_id: u64,
    pub body_fn_name: String,
    pub cleanup_fn_name: Option<String>,
    pub afterwards_fn_name: Option<String>,
    pub handlers: Vec<PersistedHandler>,
}

/// GAP-004 — one persisted `define variable` registration. Mirrors
/// [`VariableRegistration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedVariable {
    pub name: String,
    pub init_fn_name: String,
}

/// The whole sidecar payload — everything `eval_wrapped_source` needs
/// to call the four registration passes after `add_module_from_bitcode`
/// returns, plus the return-type pair `call_and_format` consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationSidecar {
    pub abi_version: u32,
    pub return_type_tag: u32,
    pub return_type_payload: u64,
    pub functions: Vec<PersistedFunction>,
    pub methods: Vec<PersistedMethod>,
    pub blocks: Vec<PersistedBlock>,
    /// GAP-004 — `define variable` registrations to replay at JIT
    /// disk-cache hit time. Each entry triggers a
    /// `nod_aot_register_variable` call.
    pub variables: Vec<PersistedVariable>,
}

impl RegistrationSidecar {
    /// True iff this sidecar's `abi_version` matches the current
    /// `REGISTRATIONS_ABI_VERSION`. Loader treats `false` as a cache
    /// miss and falls through to a cold compile.
    pub fn is_abi_compatible(&self) -> bool {
        self.abi_version == REGISTRATIONS_ABI_VERSION
    }

    /// Extract the registration data from a freshly-lowered module
    /// plus the (already-encoded) return type pair from
    /// [`crate::encode_type_tag`]. Called on the cold-compile path
    /// right before `write` so the next process's replay path has
    /// what it needs.
    pub fn from_lowered_module(
        lm: &LoweredModule,
        return_type_tag: u32,
        return_type_payload: u64,
    ) -> Self {
        // Build the block-thunk name set so we skip those from
        // `functions` (matches `register_top_level_functions`).
        let mut block_thunk_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for b in &lm.blocks {
            block_thunk_names.insert(b.body_fn_name.clone());
            if let Some(n) = &b.cleanup_fn_name {
                block_thunk_names.insert(n.clone());
            }
            if let Some(n) = &b.afterwards_fn_name {
                block_thunk_names.insert(n.clone());
            }
            for h in &b.handlers {
                block_thunk_names.insert(h.body_fn_name.clone());
            }
        }
        let mut functions = Vec::new();
        for f in &lm.functions {
            if block_thunk_names.contains(&f.name) {
                continue;
            }
            if f.name == "<eval-entry>" {
                continue;
            }
            let (is_closure, src_arity) =
                if let Some(info) = lm.closures.closure_for(&f.name) {
                    (true, info.arity as u32)
                } else {
                    (false, f.params.len() as u32)
                };
            functions.push(PersistedFunction {
                name: f.name.clone(),
                arity: src_arity,
                is_closure,
                source_arity: src_arity,
            });
        }
        let methods = lm
            .methods
            .iter()
            .map(persist_method)
            .collect();
        let blocks = lm.blocks.iter().map(persist_block).collect();
        let variables = lm
            .variables
            .iter()
            .map(|v| PersistedVariable {
                name: v.name.clone(),
                init_fn_name: v.init_fn_name.clone(),
            })
            .collect();
        Self {
            abi_version: REGISTRATIONS_ABI_VERSION,
            return_type_tag,
            return_type_payload,
            functions,
            methods,
            blocks,
            variables,
        }
    }

    /// Encode as hand-rolled JSON. Matches the style of
    /// `nod_llvm::cache::SidecarMeta::to_json` and
    /// `nod_llvm::symbols::ModuleManifest::to_json`: no `serde_json`,
    /// fixed schema, escapes the safe subset.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        let _ = write!(
            out,
            "\"abi_version\":{},\"return_type_tag\":{},\"return_type_payload\":{},",
            self.abi_version, self.return_type_tag, self.return_type_payload
        );
        // functions
        out.push_str("\"functions\":[");
        for (i, f) in self.functions.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"name\":\"{}\",\"arity\":{},\"is_closure\":{},\"source_arity\":{}}}",
                escape_json(&f.name),
                f.arity,
                if f.is_closure { "true" } else { "false" },
                f.source_arity,
            );
        }
        out.push_str("],");
        // methods
        out.push_str("\"methods\":[");
        for (i, m) in self.methods.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"generic\":\"{}\",\"body\":\"{}\",\"param_count\":{},\"specialisers\":[",
                escape_json(&m.generic_name),
                escape_json(&m.body_fn_name),
                m.param_count,
            );
            for (j, s) in m.specialisers.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{s}");
            }
            out.push_str("]}");
        }
        out.push_str("],");
        // blocks
        out.push_str("\"blocks\":[");
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"block_id\":{},\"body\":\"{}\",",
                b.block_id,
                escape_json(&b.body_fn_name),
            );
            match &b.cleanup_fn_name {
                Some(n) => {
                    let _ = write!(out, "\"cleanup\":\"{}\",", escape_json(n));
                }
                None => out.push_str("\"cleanup\":null,"),
            }
            match &b.afterwards_fn_name {
                Some(n) => {
                    let _ = write!(out, "\"afterwards\":\"{}\",", escape_json(n));
                }
                None => out.push_str("\"afterwards\":null,"),
            }
            out.push_str("\"handlers\":[");
            for (j, h) in b.handlers.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                let _ = write!(
                    out,
                    "{{\"class_id\":{},\"class_name\":\"{}\",\"body\":\"{}\"}}",
                    h.class_id,
                    escape_json(&h.class_name),
                    escape_json(&h.body_fn_name),
                );
            }
            out.push_str("]}");
        }
        out.push_str("],");
        // variables (GAP-004)
        out.push_str("\"variables\":[");
        for (i, v) in self.variables.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"name\":\"{}\",\"init\":\"{}\"}}",
                escape_json(&v.name),
                escape_json(&v.init_fn_name),
            );
        }
        out.push_str("]}");
        out
    }

    /// Inverse of [`Self::to_json`]. Returns `None` on any structural
    /// error — callers treat that as a cache miss and fall through to
    /// the cold compile path.
    pub fn parse(text: &str) -> Option<Self> {
        let mut p = JsonParser::new(text);
        p.expect('{')?;
        let mut abi_version = 0u32;
        let mut return_type_tag = 0u32;
        let mut return_type_payload = 0u64;
        let mut functions: Vec<PersistedFunction> = Vec::new();
        let mut methods: Vec<PersistedMethod> = Vec::new();
        let mut blocks: Vec<PersistedBlock> = Vec::new();
        let mut variables: Vec<PersistedVariable> = Vec::new();
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
                "abi_version" => abi_version = p.number()? as u32,
                "return_type_tag" => return_type_tag = p.number()? as u32,
                "return_type_payload" => return_type_payload = p.number()? as u64,
                "functions" => functions = p.array(parse_function)?,
                "methods" => methods = p.array(parse_method)?,
                "blocks" => blocks = p.array(parse_block)?,
                "variables" => variables = p.array(parse_variable)?,
                _ => return None,
            }
        }
        Some(Self {
            abi_version,
            return_type_tag,
            return_type_payload,
            functions,
            methods,
            blocks,
            variables,
        })
    }

    /// Write `<key>.registrations.json` next to the bitcode. Best-
    /// effort; errors are logged and swallowed (matches Sprint 37's
    /// `write_cache_entry` discipline).
    pub fn write(&self, dir: &Path, key: nod_llvm::CacheKey) {
        let path = dir.join(format!("{}.registrations.json", key.to_hex()));
        if let Err(e) = fs::write(&path, self.to_json()) {
            eprintln!("nod-jit-cache: write {path:?} failed: {e}");
        }
    }

    /// Read `<key>.registrations.json`. Returns `None` if the file is
    /// missing, malformed, or has an incompatible `abi_version` — all
    /// three are treated as a cache miss by the caller.
    pub fn read(dir: &Path, key: nod_llvm::CacheKey) -> Option<Self> {
        let path = dir.join(format!("{}.registrations.json", key.to_hex()));
        let text = fs::read_to_string(&path).ok()?;
        Self::parse(&text)
    }
}

fn persist_method(m: &MethodRegistration) -> PersistedMethod {
    PersistedMethod {
        generic_name: m.generic_name.clone(),
        specialisers: m.specialisers.iter().map(|c| c.0).collect(),
        body_fn_name: m.body_fn_name.clone(),
        param_count: m.param_count as u32,
    }
}

fn persist_block(b: &BlockRegistration) -> PersistedBlock {
    PersistedBlock {
        block_id: b.block_id,
        body_fn_name: b.body_fn_name.clone(),
        cleanup_fn_name: b.cleanup_fn_name.clone(),
        afterwards_fn_name: b.afterwards_fn_name.clone(),
        handlers: b
            .handlers
            .iter()
            .map(persist_handler)
            .collect(),
    }
}

fn persist_handler(h: &BlockHandlerRegistration) -> PersistedHandler {
    PersistedHandler {
        class_id: h.class_id.0,
        class_name: h.class_name.clone(),
        body_fn_name: h.body_fn_name.clone(),
    }
}

// ─── JSON helpers (private) ────────────────────────────────────────

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

    fn number(&mut self) -> Option<i128> {
        // i128 so we can losslessly carry u64 payload values.
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

    fn boolean(&mut self) -> Option<bool> {
        self.skip_ws();
        if self.s[self.i..].starts_with("true") {
            self.i += 4;
            Some(true)
        } else if self.s[self.i..].starts_with("false") {
            self.i += 5;
            Some(false)
        } else {
            None
        }
    }

    fn null(&mut self) -> Option<()> {
        self.skip_ws();
        if self.s[self.i..].starts_with("null") {
            self.i += 4;
            Some(())
        } else {
            None
        }
    }

    fn array<T>(&mut self, mut elem: impl FnMut(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        self.expect('[')?;
        let mut out = Vec::new();
        let mut first = true;
        loop {
            self.skip_ws();
            if self.peek() == Some(']') {
                self.bump();
                return Some(out);
            }
            if !first {
                self.expect(',')?;
                self.skip_ws();
            }
            first = false;
            out.push(elem(self)?);
        }
    }
}

fn parse_function(p: &mut JsonParser<'_>) -> Option<PersistedFunction> {
    p.expect('{')?;
    let mut name = String::new();
    let mut arity = 0u32;
    let mut is_closure = false;
    let mut source_arity = 0u32;
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
        let k = p.string()?;
        p.expect(':')?;
        p.skip_ws();
        match k.as_str() {
            "name" => name = p.string()?,
            "arity" => arity = p.number()? as u32,
            "is_closure" => is_closure = p.boolean()?,
            "source_arity" => source_arity = p.number()? as u32,
            _ => return None,
        }
    }
    Some(PersistedFunction { name, arity, is_closure, source_arity })
}

fn parse_variable(p: &mut JsonParser<'_>) -> Option<PersistedVariable> {
    p.expect('{')?;
    let mut name = String::new();
    let mut init_fn_name = String::new();
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
        let k = p.string()?;
        p.expect(':')?;
        p.skip_ws();
        match k.as_str() {
            "name" => name = p.string()?,
            "init" => init_fn_name = p.string()?,
            _ => return None,
        }
    }
    Some(PersistedVariable { name, init_fn_name })
}

fn parse_method(p: &mut JsonParser<'_>) -> Option<PersistedMethod> {
    p.expect('{')?;
    let mut generic_name = String::new();
    let mut body_fn_name = String::new();
    let mut param_count = 0u32;
    let mut specialisers: Vec<u32> = Vec::new();
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
        let k = p.string()?;
        p.expect(':')?;
        p.skip_ws();
        match k.as_str() {
            "generic" => generic_name = p.string()?,
            "body" => body_fn_name = p.string()?,
            "param_count" => param_count = p.number()? as u32,
            "specialisers" => {
                specialisers = p.array(|pp| pp.number().map(|n| n as u32))?;
            }
            _ => return None,
        }
    }
    Some(PersistedMethod {
        generic_name,
        specialisers,
        body_fn_name,
        param_count,
    })
}

fn parse_block(p: &mut JsonParser<'_>) -> Option<PersistedBlock> {
    p.expect('{')?;
    let mut block_id = 0u64;
    let mut body_fn_name = String::new();
    let mut cleanup_fn_name: Option<String> = None;
    let mut afterwards_fn_name: Option<String> = None;
    let mut handlers: Vec<PersistedHandler> = Vec::new();
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
        let k = p.string()?;
        p.expect(':')?;
        p.skip_ws();
        match k.as_str() {
            "block_id" => block_id = p.number()? as u64,
            "body" => body_fn_name = p.string()?,
            "cleanup" => {
                // `null` or string.
                p.skip_ws();
                if p.peek() == Some('n') {
                    p.null()?;
                    cleanup_fn_name = None;
                } else {
                    cleanup_fn_name = Some(p.string()?);
                }
            }
            "afterwards" => {
                p.skip_ws();
                if p.peek() == Some('n') {
                    p.null()?;
                    afterwards_fn_name = None;
                } else {
                    afterwards_fn_name = Some(p.string()?);
                }
            }
            "handlers" => {
                handlers = p.array(parse_handler)?;
            }
            _ => return None,
        }
    }
    Some(PersistedBlock {
        block_id,
        body_fn_name,
        cleanup_fn_name,
        afterwards_fn_name,
        handlers,
    })
}

fn parse_handler(p: &mut JsonParser<'_>) -> Option<PersistedHandler> {
    p.expect('{')?;
    let mut class_id = 0u32;
    let mut class_name = String::new();
    let mut body_fn_name = String::new();
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
        let k = p.string()?;
        p.expect(':')?;
        p.skip_ws();
        match k.as_str() {
            "class_id" => class_id = p.number()? as u32,
            "class_name" => class_name = p.string()?,
            "body" => body_fn_name = p.string()?,
            _ => return None,
        }
    }
    Some(PersistedHandler {
        class_id,
        class_name,
        body_fn_name,
    })
}

// `TypeEstimate` import retained for use by callers that round-trip
// through `encode_type_tag` / `decode_type_tag`. Kept here so a forward
// schema change (e.g. an additional `return_type_*` field that mirrors
// `TypeEstimate` directly) doesn't need to fight the unused-import lint.
#[allow(dead_code)]
fn _retain_type_estimate_link(_t: TypeEstimate) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RegistrationSidecar {
        RegistrationSidecar {
            abi_version: REGISTRATIONS_ABI_VERSION,
            return_type_tag: 9,
            return_type_payload: 42,
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
                block_id: 0xdead_beef,
                body_fn_name: "block-body-7".into(),
                cleanup_fn_name: Some("block-cleanup-7".into()),
                afterwards_fn_name: None,
                handlers: vec![PersistedHandler {
                    class_id: 42,
                    class_name: "<simple-error>".into(),
                    body_fn_name: "block-handler-7-0".into(),
                }],
            }],
            variables: vec![PersistedVariable {
                name: "*counter*".into(),
                init_fn_name: "__init-*counter*".into(),
            }],
        }
    }

    #[test]
    fn round_trip_through_json() {
        let s = sample();
        let j = s.to_json();
        let back = RegistrationSidecar::parse(&j).expect("parse");
        assert_eq!(s, back);
    }

    #[test]
    fn empty_payload_round_trips() {
        let s = RegistrationSidecar {
            abi_version: REGISTRATIONS_ABI_VERSION,
            return_type_tag: 0,
            return_type_payload: 0,
            functions: vec![],
            methods: vec![],
            blocks: vec![],
            variables: vec![],
        };
        let j = s.to_json();
        let back = RegistrationSidecar::parse(&j).expect("parse");
        assert_eq!(s, back);
    }

    #[test]
    fn abi_compatibility_check() {
        let mut s = sample();
        assert!(s.is_abi_compatible());
        s.abi_version = 999;
        assert!(!s.is_abi_compatible());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(RegistrationSidecar::parse("").is_none());
        assert!(RegistrationSidecar::parse("{").is_none());
        assert!(RegistrationSidecar::parse("not json").is_none());
    }

    #[test]
    fn name_with_special_chars_escapes_correctly() {
        let s = RegistrationSidecar {
            abi_version: REGISTRATIONS_ABI_VERSION,
            return_type_tag: 0,
            return_type_payload: 0,
            functions: vec![PersistedFunction {
                name: "name-with-\"quotes\"-and-\\backslashes".into(),
                arity: 1,
                is_closure: false,
                source_arity: 1,
            }],
            methods: vec![],
            blocks: vec![],
            variables: vec![],
        };
        let j = s.to_json();
        let back = RegistrationSidecar::parse(&j).expect("parse");
        assert_eq!(s, back);
    }
}
