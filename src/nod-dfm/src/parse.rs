//! Indented DFM dump **parser** — the exact inverse of [`crate::format`].
//!
//! [`parse_dfm_module`] reads the textual dump produced by
//! [`crate::format::format_dfm_module`] back into `Vec<Function>`, such that
//!
//! ```text
//! format_dfm_module(&parse_dfm_module(&format_dfm_module(&fns), &r)?)
//!     == format_dfm_module(&fns)        // byte-for-byte
//! ```
//!
//! This unblocks making the Dylan-side lowering load-bearing: a Dylan stage
//! can emit the dump as its wire format and the Rust back-end can read it
//! back into the IR.
//!
//! ## Design
//!
//! The dump is strictly line-oriented and **indentation-classified**
//! (see `fmt_function` in `format.rs`):
//!
//! * column 0, `fn ` … `:`  — function header (`fmt_function` L52-60)
//! * 2-space indent          — block header (`fmt_function` L61-74)
//! * 4-space indent          — computation (`fmt_computation` L82-83) or terminator (`fmt_terminator` L312-313)
//! * blank line              — function separator (`format_dfm_module` L24-29)
//!
//! A 4-space line is a *computation* iff its content starts with `t<digit>`
//! (every computation renders `t{dst}: {type} = …`); otherwise it is a
//! *terminator* (`Return` / `If` / `Jump`).
//!
//! ## Known lossy points
//!
//! The dump is not a fully faithful serialization of the IR; a few fields
//! are reconstructed with placeholders. None of them affect round-trip
//! identity, because the formatter never prints the lost information:
//!
//! * **`SealedDirectCall::fallback_chain` contents.** The formatter prints
//!   only `chain={len}` (`fmt_computation` L238-242), not the chain symbols.
//!   We rebuild a `Vec<String>` of `len` empty strings — the length round-
//!   trips, the contents do not.
//! * **Bare `<class>` / `<singleton>` ids.** `TypeEstimate::name()` prints
//!   `<class>` and `<singleton>` with no payload, while `type_label()`
//!   prints `<class:N>` / `<singleton:0xHEX>`. As of B-i, params, returns,
//!   block-params, and the `Dispatch`/`SealedDirectCall` `dst` all render
//!   via `type_label`, so their ids are LOSSLESS; the remaining computation
//!   `dst`s still use `name()` and lose the id (parsing back as
//!   `Class(0)` / `Singleton(0)`) — invisible to the round-trip, since
//!   `name()` drops the id anyway. A Dylan wire dump emits class types BY
//!   NAME (`<class:<idler>>`); `parse_type` resolves the non-numeric payload
//!   through `resolve_class`.
//! * **`ClassCheck::UserClass` / `TypeEstimate::Class` ids generally.** The
//!   class label resolves through the caller-supplied `resolve_class`; when
//!   it returns `None` we store `0`. `ClassCheck::name()` prints the name,
//!   not the id, so this never shows up in the round-trip either.
//! * **`Block::id` / `Function::id` values.** The dump prints labels, never
//!   numeric block/function ids (`block_label` L351-356 looks up by id but
//!   emits the label). We assign ids sequentially in parse order and build
//!   a per-function label→id map for `If`/`Jump` target resolution.
//! * **`Function::span`.** Not in the dump; set to a dummy
//!   `Span { file_id: FileId(0), lo: 0, hi: 0 }`.
//! * **`Temporary` table order.** Rebuilt from every def site (params,
//!   block-params, each computation `dst`) sorted by id; `temp_type` is a
//!   lookup, so order is immaterial to the dump.

use nod_reader::{FileId, Span};

use crate::ir::{
    Block, BlockId, ClassCheck, Computation, ConstValue, Function, FunctionId, PrimOp, SlotTypeKind,
    TempId, Temporary, Terminator, TypeEstimate,
};

/// Parse a DFM module dump (the output of
/// [`crate::format::format_dfm_module`]) back into `Vec<Function>`.
///
/// `resolve_class` maps a class label (e.g. `"<my-class>"`) to a runtime
/// `ClassId` for `TypeCheck`'s `ClassCheck::UserClass`. It may return
/// `None`, in which case `0` is stored (the id is not part of the dump, so
/// this does not affect round-tripping — see the module docs).
///
/// Returns a descriptive `Err(String)` on malformed input.
pub fn parse_dfm_module(
    text: &str,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<Vec<Function>, String> {
    // Split the module into per-function chunks. Functions are separated by
    // a single blank line (`format_dfm_module` L24-29 pushes one '\n'
    // *between* functions, on top of each function's own trailing newlines).
    // We treat any wholly-blank line as a separator and drop empty chunks so
    // a trailing newline doesn't manufacture a spurious empty function.
    //
    // We carry line numbers for diagnostics.
    let mut functions = Vec::new();
    let mut current: Vec<(usize, &str)> = Vec::new();
    let mut next_fn_id: u32 = 0;

    for (idx, raw_line) in text.lines().enumerate() {
        if raw_line.trim().is_empty() {
            // Blank line: end of the current function chunk (if any).
            if !current.is_empty() {
                functions.push(parse_function(
                    &current,
                    FunctionId(next_fn_id),
                    resolve_class,
                )?);
                next_fn_id += 1;
                current.clear();
            }
        } else {
            current.push((idx + 1, raw_line));
        }
    }
    if !current.is_empty() {
        functions.push(parse_function(&current, FunctionId(next_fn_id), resolve_class)?);
    }

    Ok(functions)
}

/// A computation/terminator line, classified by leading indentation.
enum BodyLine<'a> {
    /// 2-space indent: a block header (already indentation-stripped).
    BlockHeader(usize, &'a str),
    /// 4-space indent: a computation or terminator (already stripped).
    Stmt(usize, &'a str),
}

/// Parse one function chunk (its header line plus all block/stmt lines,
/// no blank lines) into a [`Function`].
fn parse_function(
    lines: &[(usize, &str)],
    id: FunctionId,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<Function, String> {
    let (&(hdr_lno, hdr), body) = lines
        .split_first()
        .ok_or_else(|| "internal: empty function chunk".to_string())?;

    let (name, params_src, return_type) = parse_function_header(hdr_lno, hdr, resolve_class)?;

    // Classify each body line by its indentation. `fmt_function` emits block
    // headers at 2 spaces (L61-62) and `fmt_computation`/`fmt_terminator`
    // emit at 4 spaces (L83 / L313).
    let mut body_lines: Vec<BodyLine> = Vec::with_capacity(body.len());
    for &(lno, line) in body {
        if let Some(rest) = line.strip_prefix("    ") {
            body_lines.push(BodyLine::Stmt(lno, rest));
        } else if let Some(rest) = line.strip_prefix("  ") {
            body_lines.push(BodyLine::BlockHeader(lno, rest));
        } else {
            return Err(format!(
                "line {lno}: unexpected indentation in function `{name}`: {line:?}"
            ));
        }
    }

    // First pass: collect block headers in order so we can build a
    // label→BlockId map *before* parsing terminators (a forward `Jump`/`If`
    // may reference a block defined later). Block ids are assigned
    // sequentially by appearance order (the dump carries no numeric ids).
    //
    // We also accumulate temp definitions (params, block-params, computation
    // dsts) into `temps` as we go, then build the `Temporary` table at the
    // end.
    let mut temps: Vec<Temporary> = Vec::new();

    // Function params come from the header (`fmt_function` L54-59).
    let params = parse_temp_decls(hdr_lno, params_src, &mut temps, resolve_class)?;

    // Split body_lines into per-block groups: each group is one BlockHeader
    // followed by its Stmts (computations + one terminator).
    struct RawBlock<'a> {
        lno: usize,
        label: String,
        params: Vec<TempId>,
        stmts: Vec<(usize, &'a str)>,
    }
    let mut raw_blocks: Vec<RawBlock> = Vec::new();
    for bl in &body_lines {
        match *bl {
            BodyLine::BlockHeader(lno, hdr) => {
                let (label, bparams) = parse_block_header(lno, hdr, &mut temps, resolve_class)?;
                raw_blocks.push(RawBlock {
                    lno,
                    label,
                    params: bparams,
                    stmts: Vec::new(),
                });
            }
            BodyLine::Stmt(lno, s) => {
                let blk = raw_blocks.last_mut().ok_or_else(|| {
                    format!("line {lno}: statement before any block header: {s:?}")
                })?;
                blk.stmts.push((lno, s));
            }
        }
    }

    if raw_blocks.is_empty() {
        return Err(format!(
            "line {hdr_lno}: function `{name}` has no blocks"
        ));
    }

    // Build the label→BlockId map (block ids sequential by appearance).
    let mut label_to_id: Vec<(String, BlockId)> = Vec::with_capacity(raw_blocks.len());
    for (i, rb) in raw_blocks.iter().enumerate() {
        let bid = BlockId(i as u32);
        if label_to_id.iter().any(|(l, _)| *l == rb.label) {
            return Err(format!(
                "line {}: duplicate block label `{}`",
                rb.lno, rb.label
            ));
        }
        label_to_id.push((rb.label.clone(), bid));
    }
    let resolve_label = |lno: usize, label: &str| -> Result<BlockId, String> {
        label_to_id
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, id)| *id)
            .ok_or_else(|| format!("line {lno}: jump/branch to unknown block `{label}`"))
    };

    // Second pass: parse each block's computations and terminator.
    let mut blocks: Vec<Block> = Vec::with_capacity(raw_blocks.len());
    for (i, rb) in raw_blocks.iter().enumerate() {
        let bid = BlockId(i as u32);
        let (term_stmt, comp_stmts) = rb.stmts.split_last().ok_or_else(|| {
            format!(
                "line {}: block `{}` has no terminator",
                rb.lno, rb.label
            )
        })?;

        let mut computations = Vec::with_capacity(comp_stmts.len());
        for &(lno, s) in comp_stmts {
            // A computation always renders `t{dst}: …`; a terminator never
            // does (`Return`/`If`/`Jump`). Guard against a stray terminator
            // in the middle of a block.
            if !starts_with_temp(s) {
                return Err(format!(
                    "line {lno}: expected a computation (t…) but found terminator-shaped line: {s:?}"
                ));
            }
            computations.push(parse_computation(lno, s, &mut temps, resolve_class)?);
        }

        // The terminator is the last stmt; it must NOT be temp-shaped.
        let &(term_lno, term_src) = term_stmt;
        if starts_with_temp(term_src) {
            return Err(format!(
                "line {term_lno}: block `{}` ends with a computation, not a terminator: {term_src:?}",
                rb.label
            ));
        }
        let terminator = parse_terminator(term_lno, term_src, &resolve_label)?;

        blocks.push(Block {
            id: bid,
            label: rb.label.clone(),
            params: rb.params.clone(),
            computations,
            terminator,
        });
    }

    // Build the Temporary table. Multiple def sites can't legally collide
    // (SSA), but the same id may have been pushed once per appearance only
    // at its def site, so we de-dup by id keeping the first (the def-site
    // type). Sort by id for a stable table; `temp_type` is a lookup so order
    // is cosmetic.
    temps.sort_by_key(|t| t.id.0);
    temps.dedup_by_key(|t| t.id.0);

    let entry = blocks[0].id; // entry is the first block (`fmt_function` L61).

    Ok(Function {
        id,
        name,
        params,
        entry,
        blocks,
        temps,
        return_type,
        span: Span {
            file_id: FileId(0),
            lo: 0,
            hi: 0,
        },
    })
}

/// Parse the function header line (no indentation), inverse of
/// `fmt_function` L52-60: `fn {name} ({params}) -> {rettype}:`.
///
/// Returns `(name, params_src, return_type)` where `params_src` is the raw
/// (possibly empty) `t..: <..>, ..` substring between `(` and `)`.
///
/// A Dylan-side lowering wire dump emits a `define method` body's name with the
/// specialiser classes BY NAME (`g$<idler>_<integer>`); it can't know the
/// host-assigned class ids at lowering time. We resolve those `$<class>`
/// suffixes to the numeric scheme (`g$1082_1`) via `resolve_class` here — the
/// same seam/precedent as `ClassMetadataPtr`-by-name in `parse_const`. A numeric
/// suffix passes through unchanged (the human dump round-trips identically).
fn parse_function_header<'a>(
    lno: usize,
    line: &'a str,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<(String, &'a str, TypeEstimate), String> {
    // `format!("fn {} (", name)` — note the literal " (" after the name,
    // emitted even with zero params, then `) -> ` and a trailing ':'.
    let after_fn = line
        .strip_prefix("fn ")
        .ok_or_else(|| format!("line {lno}: function header must start with `fn `: {line:?}"))?;

    // The name ends at the first " (" — labels/types never contain " (".
    let name_end = after_fn
        .find(" (")
        .ok_or_else(|| format!("line {lno}: malformed function header (no ` (`): {line:?}"))?;
    let name = resolve_method_name_suffix(lno, &after_fn[..name_end], resolve_class)?;
    let rest = &after_fn[name_end + 2..]; // skip " ("

    // Params run up to the literal ") -> " (param list contains no ')').
    let params_end = rest
        .find(") -> ")
        .ok_or_else(|| format!("line {lno}: malformed function header (no `) -> `): {line:?}"))?;
    let params_src = &rest[..params_end];
    let after_params = &rest[params_end + 5..]; // skip ") -> "

    // Return type is everything up to the trailing ':'.
    let rty_src = after_params.strip_suffix(':').ok_or_else(|| {
        format!("line {lno}: function header must end with ':': {line:?}")
    })?;
    let return_type = parse_type(lno, rty_src, resolve_class)?;

    Ok((name, params_src, return_type))
}

/// Resolve a `define method` body name's by-name specialiser suffix to the
/// numeric scheme. A Dylan-side lowering dump names a method body
/// `g$<idler>_<integer>` (specialisers by class NAME — it can't know the ids at
/// lowering time); the Rust lowering names it `g$1082_1` (numeric `ClassId`s,
/// `lower_method_item` ~L3567). We split on the FIRST `$` into
/// `(generic, suffix)`, then resolve each `_`-separated suffix token: a `<…>`
/// token (a class label, which by construction contains no `_`) is mapped
/// through `resolve_class` to its id; a numeric token passes through unchanged
/// (so the human dump round-trips); any other token also passes through (a
/// non-method `$` name — e.g. a lifted closure body — is left intact). An
/// unresolvable `<…>` token is a hard error (we never silently emit a wrong
/// callee/header). A name with no `$` is returned verbatim.
fn resolve_method_name_suffix(
    lno: usize,
    name: &str,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<String, String> {
    let Some((generic, suffix)) = name.split_once('$') else {
        return Ok(name.to_string());
    };
    let mut out = String::with_capacity(name.len());
    out.push_str(generic);
    out.push('$');
    for (i, tok) in suffix.split('_').enumerate() {
        if i > 0 {
            out.push('_');
        }
        // A `<…>` token is a class label emitted by name; resolve it. Anything
        // else (a numeric id, or a non-class token) passes through unchanged.
        if tok.starts_with('<') && tok.ends_with('>') {
            let id = resolve_class(tok).ok_or_else(|| {
                format!("line {lno}: unresolved class name in method body name suffix: {tok:?}")
            })?;
            out.push_str(&id.to_string());
        } else {
            out.push_str(tok);
        }
    }
    Ok(out)
}

/// Parse a block header (2-space-stripped), inverse of `fmt_function`
/// L61-74: `{label}` or `{label}(t{id}: {type}, ...)` then `:`.
/// Records any block-param temps into `temps`.
fn parse_block_header(
    lno: usize,
    line: &str,
    temps: &mut Vec<Temporary>,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<(String, Vec<TempId>), String> {
    let body = line
        .strip_suffix(':')
        .ok_or_else(|| format!("line {lno}: block header must end with ':': {line:?}"))?;

    // Block labels are alphanumeric+underscore (entry/then0/loop_header0/…),
    // never containing '(' or ':' — so a '(' unambiguously opens the param
    // list (`fmt_function` L64-72).
    if let Some(open) = body.find('(') {
        let label = body[..open].to_string();
        let inner = body[open + 1..].strip_suffix(')').ok_or_else(|| {
            format!("line {lno}: block param list missing closing ')': {line:?}")
        })?;
        let params = parse_temp_decls(lno, inner, temps, resolve_class)?;
        Ok((label, params))
    } else {
        Ok((body.to_string(), Vec::new()))
    }
}

/// Parse a comma-separated `t{id}: {type}` declaration list (function params
/// or block params), recording each into `temps`. Empty input yields an
/// empty `Vec`. Inverse of the loops in `fmt_function` L54-59 / L66-71.
fn parse_temp_decls(
    lno: usize,
    src: &str,
    temps: &mut Vec<Temporary>,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<Vec<TempId>, String> {
    if src.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for decl in src.split(", ") {
        let (id, ty) = parse_temp_decl(lno, decl, resolve_class)?;
        record_temp(temps, id, ty);
        ids.push(id);
    }
    Ok(ids)
}

/// Parse a single `t{id}: {type}` declaration. Inverse of
/// `write!(out, "t{}: {}", p.0, type_label(ty))`.
fn parse_temp_decl(
    lno: usize,
    decl: &str,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<(TempId, TypeEstimate), String> {
    let (lhs, rhs) = decl
        .split_once(": ")
        .ok_or_else(|| format!("line {lno}: malformed temp decl (no `: `): {decl:?}"))?;
    let id = parse_temp_ref(lno, lhs)?;
    let ty = parse_type(lno, rhs, resolve_class)?;
    Ok((id, ty))
}

/// Parse a `t{digits}` temp reference into a [`TempId`]. Inverse of the
/// ubiquitous `write!(out, "t{}", x.0)`.
fn parse_temp_ref(lno: usize, tok: &str) -> Result<TempId, String> {
    let digits = tok
        .strip_prefix('t')
        .ok_or_else(|| format!("line {lno}: expected a temp `t…`, got {tok:?}"))?;
    let n: u32 = digits
        .parse()
        .map_err(|_| format!("line {lno}: bad temp id in {tok:?}"))?;
    Ok(TempId(n))
}

/// Does the (indentation-stripped) statement begin with `t<digit>`? That is
/// the discriminator between a computation (`t{dst}: …`) and a terminator.
fn starts_with_temp(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('t')) && matches!(chars.next(), Some(c) if c.is_ascii_digit())
}

/// Record a temp's def-site type. SSA guarantees a single def per id; if the
/// same id appears twice we keep both (deduped later, first-wins).
fn record_temp(temps: &mut Vec<Temporary>, id: TempId, ty: TypeEstimate) {
    temps.push(Temporary {
        id,
        type_estimate: ty,
    });
}

// ---------------------------------------------------------------------------
// TypeEstimate
// ---------------------------------------------------------------------------

/// Parse a type label, inverse of BOTH `TypeEstimate::name()` (ir.rs
/// L491-505) and `type_label()` (format.rs L8-14).
///
/// `name()` renders `Class(_)`→`<class>` and `Singleton(_)`→`<singleton>`
/// (id dropped); `type_label()` renders `<class:N>` / `<singleton:0xHEX>`.
/// We accept all four shapes. Bare `<class>`/`<singleton>` parse to
/// `Class(0)`/`Singleton(0)` — see module-level "Known lossy points".
///
/// B-i: params/returns/block-params are now formatted with `type_label`, so a
/// class-typed temp dumps `<class:N>` (id present, LOSSLESS) — the Rust human
/// dump uses a numeric `N`. A Dylan-side lowering wire dump, which can't know
/// the host-assigned ids at lowering time, instead emits the class BY NAME
/// (`<class:<idler>>`); we resolve the non-numeric payload through
/// `resolve_class` at the reconstruction seam (the same precedent as
/// `ClassMetadataPtr`-by-name in `parse_const` and the method-name suffix in
/// `parse_function_header`). The encoding nests one `<…>` inside the other:
/// `strip_prefix("<class:")` then a single `strip_suffix('>')` yields the
/// payload `<idler>` (the class label, which still carries its own `>`).
fn parse_type(
    lno: usize,
    s: &str,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<TypeEstimate, String> {
    let ty = match s {
        "<top>" => TypeEstimate::Top,
        "<bottom>" => TypeEstimate::Bottom,
        "<integer>" => TypeEstimate::Integer,
        "<single-float>" => TypeEstimate::SingleFloat,
        "<double-float>" => TypeEstimate::DoubleFloat,
        "<character>" => TypeEstimate::Character,
        "<boolean>" => TypeEstimate::Boolean,
        "<string>" => TypeEstimate::String,
        "<unit>" => TypeEstimate::Unit,
        "<class>" => TypeEstimate::Class(0),
        "<singleton>" => TypeEstimate::Singleton(0),
        _ => {
            if let Some(rest) = s.strip_prefix("<class:") {
                let n = rest.strip_suffix('>').ok_or_else(|| {
                    format!("line {lno}: malformed <class:…> type: {s:?}")
                })?;
                // Numeric payload → the human/Rust dump (id round-trips as-is).
                // Non-numeric payload → a Dylan wire dump's class NAME (e.g.
                // `<idler>`), resolved at the seam.
                let id: u32 = match n.parse::<u32>() {
                    Ok(id) => id,
                    Err(_) => resolve_class(n).ok_or_else(|| {
                        format!("line {lno}: unresolved class name in type {s:?}")
                    })?,
                };
                TypeEstimate::Class(id)
            } else if let Some(rest) = s.strip_prefix("<singleton:") {
                let n = rest.strip_suffix('>').ok_or_else(|| {
                    format!("line {lno}: malformed <singleton:…> type: {s:?}")
                })?;
                let bits = parse_hex_u64(n).ok_or_else(|| {
                    format!("line {lno}: bad singleton bits in type {s:?}")
                })?;
                TypeEstimate::Singleton(bits)
            } else {
                return Err(format!("line {lno}: unknown type label {s:?}"));
            }
        }
    };
    Ok(ty)
}

// ---------------------------------------------------------------------------
// Computations
// ---------------------------------------------------------------------------

/// Parse a computation line (4-space-stripped), inverse of `fmt_computation`
/// (format.rs L82-246). Records the `dst` temp's type into `temps`.
fn parse_computation(
    lno: usize,
    s: &str,
    temps: &mut Vec<Temporary>,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<Computation, String> {
    // Every computation is `t{dst}: {type} = {VARIANT} …`. Split off the
    // `t{dst}: {type} = ` prefix first.
    let (lhs, after_eq) = s
        .split_once(" = ")
        .ok_or_else(|| format!("line {lno}: computation missing ` = `: {s:?}"))?;
    let (dst, dst_ty) = parse_temp_decl(lno, lhs, resolve_class)?;
    record_temp(temps, dst, dst_ty);

    // The variant keyword is the first whitespace-delimited token of the RHS
    // (`Const`, `PrimOp`, …) — except `Const`/`PrimOp`/`DirectCall`/… all
    // begin with a distinct capitalized word.
    let (kw, args_part) = split_first_word(after_eq);

    match kw {
        // `Const ` then a ConstValue (`fmt_computation` L85-89).
        "Const" => {
            let value = parse_const(lno, args_part, resolve_class)?;
            Ok(Computation::Const { dst, value })
        }
        // `PrimOp {Op}` then zero-or-more ` t{id}` (L90-102).
        "PrimOp" => {
            let (op_name, rest) = split_first_word(args_part);
            let op = parse_primop(lno, op_name)?;
            let args = parse_space_temp_list(lno, rest)?;
            Ok(Computation::PrimOp { dst, op, args })
        }
        // `DirectCall {callee}(args)` + optional safepoint + optional
        // ` [no_alloc]` (L103-127).
        "DirectCall" => {
            let (callee, rest) = split_callee_and_args(lno, args_part)?;
            let (arg_ids, after_args) = parse_paren_temp_list(lno, rest)?;
            let (safepoint_roots, after_sp) = parse_safepoint_suffix(lno, after_args)?;
            let is_no_alloc = parse_no_alloc_suffix(lno, after_sp)?;
            Ok(Computation::DirectCall {
                dst,
                callee: callee.to_string(),
                args: arg_ids,
                safepoint_roots,
                is_no_alloc,
            })
        }
        // `Call t{callee}(args)` + optional safepoint (L128-145).
        "Call" => {
            // The callee is a temp ref `t{id}`, then `(args)`.
            let open = args_part
                .find('(')
                .ok_or_else(|| format!("line {lno}: Call missing '(': {s:?}"))?;
            let callee = parse_temp_ref(lno, args_part[..open].trim())?;
            let (arg_ids, after_args) = parse_paren_temp_list(lno, &args_part[open..])?;
            let (safepoint_roots, after_sp) = parse_safepoint_suffix(lno, after_args)?;
            ensure_empty(lno, after_sp, "Call")?;
            Ok(Computation::Call {
                dst,
                callee,
                args: arg_ids,
                safepoint_roots,
            })
        }
        // `TypeCheck t{value} {classlabel}` (L146-155).
        "TypeCheck" => {
            let (val_tok, class_tok) = split_first_word(args_part);
            let value = parse_temp_ref(lno, val_tok)?;
            if class_tok.is_empty() {
                return Err(format!("line {lno}: TypeCheck missing class label: {s:?}"));
            }
            let class = parse_class_check(class_tok, resolve_class);
            Ok(Computation::TypeCheck { dst, value, class })
        }
        // `WriteBarrier t{slot} := t{value}` (L156-165).
        "WriteBarrier" => {
            let (slot_tok, rest) = split_first_word(args_part);
            let slot = parse_temp_ref(lno, slot_tok)?;
            let value_tok = rest
                .strip_prefix(":= ")
                .ok_or_else(|| format!("line {lno}: WriteBarrier missing ':= ': {s:?}"))?;
            let value = parse_temp_ref(lno, value_tok)?;
            Ok(Computation::WriteBarrier { dst, slot, value })
        }
        // `LoadSlot t{inst} @{offset} [{kind:?}]` (L166-176).
        "LoadSlot" => {
            let (inst_tok, rest) = split_first_word(args_part);
            let instance = parse_temp_ref(lno, inst_tok)?;
            let (off_tok, kind_tok) = split_first_word(rest);
            let offset = parse_offset(lno, off_tok)?;
            let slot_type = parse_slot_kind_bracketed(lno, kind_tok)?;
            Ok(Computation::LoadSlot {
                dst,
                instance,
                offset,
                slot_type,
            })
        }
        // `StoreSlot t{inst} @{offset} := t{value} [{kind:?}]` (L177-188).
        "StoreSlot" => {
            let (inst_tok, rest) = split_first_word(args_part);
            let instance = parse_temp_ref(lno, inst_tok)?;
            let (off_tok, rest2) = split_first_word(rest);
            let offset = parse_offset(lno, off_tok)?;
            // rest2 == ":= t{value} [{kind}]"
            let rest3 = rest2
                .strip_prefix(":= ")
                .ok_or_else(|| format!("line {lno}: StoreSlot missing ':= ': {s:?}"))?;
            let (val_tok, kind_tok) = split_first_word(rest3);
            let value = parse_temp_ref(lno, val_tok)?;
            let slot_type = parse_slot_kind_bracketed(lno, kind_tok)?;
            Ok(Computation::StoreSlot {
                dst,
                instance,
                offset,
                value,
                slot_type,
            })
        }
        // `Dispatch {generic}(args)` + optional safepoint (L189-206).
        "Dispatch" => {
            let (generic, rest) = split_callee_and_args(lno, args_part)?;
            let (arg_ids, after_args) = parse_paren_temp_list(lno, rest)?;
            let (safepoint_roots, after_sp) = parse_safepoint_suffix(lno, after_args)?;
            ensure_empty(lno, after_sp, "Dispatch")?;
            Ok(Computation::Dispatch {
                dst,
                generic_name: generic.to_string(),
                args: arg_ids,
                safepoint_roots,
            })
        }
        // `SealedDirectCall {method}(args)` + optional safepoint +
        // optional ` [no_alloc]` + `  ; sealed-direct on `{generic}`
        // (chain={N})` (L207-244).
        "SealedDirectCall" => {
            let (method, rest) = split_callee_and_args(lno, args_part)?;
            let (arg_ids, after_args) = parse_paren_temp_list(lno, rest)?;
            let (safepoint_roots, after_sp) = parse_safepoint_suffix(lno, after_args)?;
            let (is_no_alloc, after_na) = parse_no_alloc_prefix(after_sp);
            let (generic_name, chain_len) = parse_sealed_comment(lno, after_na)?;
            // Lossy: only the chain *length* is in the dump; rebuild a
            // Vec of empty placeholders so `fallback_chain.len()` matches.
            let fallback_chain = vec![String::new(); chain_len];
            Ok(Computation::SealedDirectCall {
                dst,
                method: method.to_string(),
                fallback_chain,
                generic_name,
                args: arg_ids,
                safepoint_roots,
                is_no_alloc,
            })
        }
        other => Err(format!(
            "line {lno}: unknown computation variant `{other}`: {s:?}"
        )),
    }
}

/// Split `"Word rest…"` into `("Word", "rest…")`. If there's no space, the
/// whole input is the word and the rest is empty.
fn split_first_word(s: &str) -> (&str, &str) {
    match s.find(' ') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    }
}

/// Split `"{callee}(args)…"` into `("{callee}", "(args)…")`. The callee is a
/// name (DirectCall/Dispatch/SealedDirectCall) terminated by the first '('.
/// Callee names can contain `<>-$%` and digits but never '(' (it opens the
/// arg list).
fn split_callee_and_args(lno: usize, s: &str) -> Result<(&str, &str), String> {
    let open = s
        .find('(')
        .ok_or_else(|| format!("line {lno}: call missing '(': {s:?}"))?;
    Ok((&s[..open], &s[open..]))
}

/// Parse a parenthesized, comma-separated temp list `"(t.., t..)rest"`,
/// returning the ids and the unconsumed remainder after the ')'. Inverse of
/// the `(`…args…`)` loops in `fmt_computation` (e.g. L111-117).
fn parse_paren_temp_list(lno: usize, s: &str) -> Result<(Vec<TempId>, &str), String> {
    let inner_start = s
        .strip_prefix('(')
        .ok_or_else(|| format!("line {lno}: expected '(' starting arg list: {s:?}"))?;
    let close = inner_start
        .find(')')
        .ok_or_else(|| format!("line {lno}: arg list missing ')': {s:?}"))?;
    let inner = &inner_start[..close];
    let rest = &inner_start[close + 1..];
    let ids = if inner.is_empty() {
        Vec::new()
    } else {
        inner
            .split(", ")
            .map(|t| parse_temp_ref(lno, t))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok((ids, rest))
}

/// Parse a space-separated ` t.. t..` temp list (PrimOp args, L98-100). The
/// input may begin with a leading space (the formatter writes `" t{}"`).
fn parse_space_temp_list(lno: usize, s: &str) -> Result<Vec<TempId>, String> {
    let mut ids = Vec::new();
    for tok in s.split_whitespace() {
        ids.push(parse_temp_ref(lno, tok)?);
    }
    Ok(ids)
}

/// Parse an optional ` safepoint=[t.., ...]` suffix (`fmt_safepoint`,
/// format.rs L248-260), returning the roots and the remainder after the ']'.
/// An empty roots list is never *emitted* (L249-251), so absence ⇒ `Vec::new`.
fn parse_safepoint_suffix(lno: usize, s: &str) -> Result<(Vec<TempId>, &str), String> {
    // `fmt_safepoint` writes `  safepoint=[` (two leading spaces, L252).
    let Some(rest) = s.strip_prefix("  safepoint=[") else {
        return Ok((Vec::new(), s));
    };
    let close = rest
        .find(']')
        .ok_or_else(|| format!("line {lno}: safepoint list missing ']': {s:?}"))?;
    let inner = &rest[..close];
    let after = &rest[close + 1..];
    let ids = if inner.is_empty() {
        Vec::new()
    } else {
        inner
            .split(", ")
            .map(|t| parse_temp_ref(lno, t))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok((ids, after))
}

/// Parse a trailing ` [no_alloc]` flag (DirectCall L123-125). Returns whether
/// present and errors if there's other unexpected trailing text.
fn parse_no_alloc_suffix(lno: usize, s: &str) -> Result<bool, String> {
    if s.is_empty() {
        Ok(false)
    } else if s == " [no_alloc]" {
        Ok(true)
    } else {
        Err(format!(
            "line {lno}: unexpected trailing text after DirectCall: {s:?}"
        ))
    }
}

/// Parse an optional leading ` [no_alloc]` (SealedDirectCall L232-234, which
/// is followed by the `  ; sealed-direct …` comment). Returns the flag and
/// the remainder.
fn parse_no_alloc_prefix(s: &str) -> (bool, &str) {
    match s.strip_prefix(" [no_alloc]") {
        Some(rest) => (true, rest),
        None => (false, s),
    }
}

/// Parse the trailing SealedDirectCall comment, inverse of L238-242:
/// `  ; sealed-direct on `{generic}` (chain={N})`. Returns
/// `(generic_name, chain_len)`.
fn parse_sealed_comment(lno: usize, s: &str) -> Result<(String, usize), String> {
    let rest = s.strip_prefix("  ; sealed-direct on `").ok_or_else(|| {
        format!("line {lno}: SealedDirectCall missing trailing comment: {s:?}")
    })?;
    // generic name is delimited by the closing backtick.
    let tick = rest
        .find('`')
        .ok_or_else(|| format!("line {lno}: SealedDirectCall comment missing closing '`': {s:?}"))?;
    let generic = rest[..tick].to_string();
    let tail = &rest[tick + 1..];
    let num = tail
        .strip_prefix(" (chain=")
        .and_then(|t| t.strip_suffix(')'))
        .ok_or_else(|| format!("line {lno}: SealedDirectCall malformed `(chain=N)`: {s:?}"))?;
    let chain_len: usize = num
        .parse()
        .map_err(|_| format!("line {lno}: bad chain length in {s:?}"))?;
    Ok((generic, chain_len))
}

/// Error unless `s` is empty (used to reject trailing junk after variants
/// with no further suffixes).
fn ensure_empty(lno: usize, s: &str, what: &str) -> Result<(), String> {
    if s.is_empty() {
        Ok(())
    } else {
        Err(format!("line {lno}: unexpected trailing text after {what}: {s:?}"))
    }
}

/// Parse a PrimOp name, inverse of `PrimOp::name()` (ir.rs L391-420).
fn parse_primop(lno: usize, name: &str) -> Result<PrimOp, String> {
    let op = match name {
        "AddInt" => PrimOp::AddInt,
        "SubInt" => PrimOp::SubInt,
        "MulInt" => PrimOp::MulInt,
        "DivInt" => PrimOp::DivInt,
        "ModInt" => PrimOp::ModInt,
        "RemInt" => PrimOp::RemInt,
        "NegInt" => PrimOp::NegInt,
        "AddFloat" => PrimOp::AddFloat,
        "SubFloat" => PrimOp::SubFloat,
        "MulFloat" => PrimOp::MulFloat,
        "DivFloat" => PrimOp::DivFloat,
        "NegFloat" => PrimOp::NegFloat,
        "IntToFloat" => PrimOp::IntToFloat,
        "StripTag" => PrimOp::StripTag,
        "EqInt" => PrimOp::EqInt,
        "NeInt" => PrimOp::NeInt,
        "LtInt" => PrimOp::LtInt,
        "GtInt" => PrimOp::GtInt,
        "LeInt" => PrimOp::LeInt,
        "GeInt" => PrimOp::GeInt,
        "EqFloat" => PrimOp::EqFloat,
        "LtFloat" => PrimOp::LtFloat,
        "GtFloat" => PrimOp::GtFloat,
        "LeFloat" => PrimOp::LeFloat,
        "GeFloat" => PrimOp::GeFloat,
        "BoolAnd" => PrimOp::BoolAnd,
        "BoolOr" => PrimOp::BoolOr,
        "BoolNot" => PrimOp::BoolNot,
        other => return Err(format!("line {lno}: unknown PrimOp `{other}`")),
    };
    Ok(op)
}

/// Parse a `ClassCheck` from its label, inverse of `ClassCheck::name()`
/// (ir.rs L273-285). Unknown labels become `UserClass` (id via
/// `resolve_class`, else 0 — see module "Known lossy points"). `name()`
/// prints the *name*, so the id is invisible to the round-trip.
fn parse_class_check(label: &str, resolve_class: &dyn Fn(&str) -> Option<u32>) -> ClassCheck {
    match label {
        "<integer>" => ClassCheck::Integer,
        "<boolean>" => ClassCheck::Boolean,
        "<byte-string>" => ClassCheck::String,
        "<symbol>" => ClassCheck::Symbol,
        "<simple-object-vector>" => ClassCheck::Vector,
        "<character>" => ClassCheck::Character,
        "<empty-list>" => ClassCheck::EmptyList,
        // NOTE: `ClassCheck::Unsupported { name }` also renders via `name`
        // (a `&'static str`), so its dump is indistinguishable from a
        // `UserClass` with the same label. We can't reconstruct a
        // `&'static str` from runtime text, so any non-builtin label maps
        // to `UserClass`. This is round-trip-safe because both variants
        // print identically through `name()`.
        _ => ClassCheck::UserClass {
            id: resolve_class(label).unwrap_or(0),
            name: label.to_string(),
        },
    }
}

/// Parse `@{offset}` (a `usize`), inverse of `write!(… "@{}", offset)`.
fn parse_offset(lno: usize, tok: &str) -> Result<usize, String> {
    let n = tok
        .strip_prefix('@')
        .ok_or_else(|| format!("line {lno}: expected '@offset', got {tok:?}"))?;
    n.parse()
        .map_err(|_| format!("line {lno}: bad slot offset {tok:?}"))
}

/// Parse `[{SlotTypeKind:?}]`, inverse of `write!(… "[{:?}]", slot_type)`.
/// The `Debug` of `SlotTypeKind` is `Integer` / `Object` (ir.rs L230-234).
fn parse_slot_kind_bracketed(lno: usize, tok: &str) -> Result<SlotTypeKind, String> {
    let inner = tok
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .ok_or_else(|| format!("line {lno}: expected '[kind]', got {tok:?}"))?;
    match inner {
        "Integer" => Ok(SlotTypeKind::Integer),
        "Object" => Ok(SlotTypeKind::Object),
        other => Err(format!("line {lno}: unknown slot kind `{other}`")),
    }
}

// ---------------------------------------------------------------------------
// ConstValue
// ---------------------------------------------------------------------------

/// Parse a `ConstValue`, inverse of `fmt_const` (format.rs L262-310).
///
/// `resolve_class` is used for `ClassMetadataPtr`: the human format prints a
/// numeric class id, but a Dylan-side lowering wire dump emits the class NAME
/// (it can't know the host-assigned id at lowering time) — a non-numeric id
/// payload is resolved through `resolve_class`. The id round-trips as-is when
/// numeric, so this does not disturb `format`→`parse`→`format` identity.
fn parse_const(
    lno: usize,
    s: &str,
    resolve_class: &dyn Fn(&str) -> Option<u32>,
) -> Result<ConstValue, String> {
    // `Unit` is the only payload-less form (L279-281).
    if s == "Unit" {
        return Ok(ConstValue::Unit);
    }
    // Everything else is `Tag(payload)`; split on the first '('.
    let open = s
        .find('(')
        .ok_or_else(|| format!("line {lno}: malformed Const value {s:?}"))?;
    let tag = &s[..open];
    let inner = s[open + 1..]
        .strip_suffix(')')
        .ok_or_else(|| format!("line {lno}: Const value missing ')': {s:?}"))?;

    let v = match tag {
        // Integer(i128) — L264-266.
        "Integer" => {
            let i: i128 = inner
                .parse()
                .map_err(|_| format!("line {lno}: bad Integer literal {inner:?}"))?;
            ConstValue::Integer(i)
        }
        // Float(f64) printed via `{:?}` — L267-269. Rust's Debug emits
        // `1.0`, `1e300`, `inf`, `-inf`, `NaN`; `f64::from_str` reads them.
        "Float" => {
            let f: f64 = inner
                .parse()
                .map_err(|_| format!("line {lno}: bad Float literal {inner:?}"))?;
            ConstValue::Float(f)
        }
        // Bool(true|false) — L270-272.
        "Bool" => {
            let b = match inner {
                "true" => true,
                "false" => false,
                other => return Err(format!("line {lno}: bad Bool literal {other:?}")),
            };
            ConstValue::Bool(b)
        }
        // String("…") — Rust `{:?}`-escaped (L273-275). Strip quotes, unescape.
        "String" => ConstValue::String(parse_quoted_string(lno, inner)?),
        // Char('c') — Rust char `{:?}` (L276-278).
        "Char" => ConstValue::Char(parse_quoted_char(lno, inner)?),
        // WordBits(0xHEX) — `{:#x}` (L282-284).
        "WordBits" => {
            let bits = parse_hex_prefixed_u64(inner)
                .ok_or_else(|| format!("line {lno}: bad WordBits literal {inner:?}"))?;
            ConstValue::WordBits(bits)
        }
        // ClassMetadataPtr(N, tagged=BOOL) — L285-287. N is numeric in the human
        // format; a Dylan wire dump emits the class NAME, resolved here.
        "ClassMetadataPtr" => {
            let (id_part, tag_part) = inner
                .split_once(", ")
                .ok_or_else(|| format!("line {lno}: malformed ClassMetadataPtr {inner:?}"))?;
            let class_id: u32 = match id_part.parse::<u32>() {
                Ok(n) => n,
                Err(_) => resolve_class(id_part).ok_or_else(|| {
                    format!("line {lno}: unresolved class name in ClassMetadataPtr: {id_part:?}")
                })?,
            };
            let tagged = parse_tagged_eq(lno, tag_part)?;
            ConstValue::ClassMetadataPtr { class_id, tagged }
        }
        // StringLiteralRef("…") — L288-290.
        "StringLiteralRef" => ConstValue::StringLiteralRef(parse_quoted_string(lno, inner)?),
        // SymbolLiteralRef("…") — L291-293.
        "SymbolLiteralRef" => ConstValue::SymbolLiteralRef(parse_quoted_string(lno, inner)?),
        // StubEntryRef("dll", "symbol", sig=HEXBYTES) — L294-308.
        "StubEntryRef" => parse_stub_entry(lno, inner)?,
        other => return Err(format!("line {lno}: unknown Const tag `{other}`: {s:?}")),
    };
    Ok(v)
}

/// Parse `tagged=true|false`.
fn parse_tagged_eq(lno: usize, s: &str) -> Result<bool, String> {
    match s {
        "tagged=true" => Ok(true),
        "tagged=false" => Ok(false),
        other => Err(format!("line {lno}: malformed `tagged=…`: {other:?}")),
    }
}

/// Parse a `StubEntryRef` payload: `"dll", "symbol", sig=HEXBYTES`
/// (format.rs L294-308). The dll/symbol are Rust `{:?}`-escaped; the
/// signature is lowercase hex byte pairs.
fn parse_stub_entry(lno: usize, inner: &str) -> Result<ConstValue, String> {
    // The two quoted strings may contain escaped commas/quotes, so we can't
    // naively split on ", ". Parse the first quoted string, then expect
    // ", ", then the second, then ", sig=".
    let (dll, after_dll) = parse_leading_quoted_string(lno, inner)?;
    let after_dll = after_dll
        .strip_prefix(", ")
        .ok_or_else(|| format!("line {lno}: StubEntryRef missing ', ' after dll: {inner:?}"))?;
    let (symbol, after_sym) = parse_leading_quoted_string(lno, after_dll)?;
    let sig_hex = after_sym
        .strip_prefix(", sig=")
        .ok_or_else(|| format!("line {lno}: StubEntryRef missing ', sig=': {inner:?}"))?;
    let signature_bytes = parse_hex_bytes(sig_hex)
        .ok_or_else(|| format!("line {lno}: StubEntryRef bad sig hex {sig_hex:?}"))?;
    Ok(ConstValue::StubEntryRef {
        dll,
        symbol,
        signature_bytes,
    })
}

// ---------------------------------------------------------------------------
// Terminators
// ---------------------------------------------------------------------------

/// Parse a terminator line (4-space-stripped), inverse of `fmt_terminator`
/// (format.rs L312-349). `resolve_label` maps a block label to its id.
fn parse_terminator(
    lno: usize,
    s: &str,
    resolve_label: &dyn Fn(usize, &str) -> Result<BlockId, String>,
) -> Result<Terminator, String> {
    let (kw, rest) = split_first_word(s);
    match kw {
        // `Return` / `Return t{v}` (L315-319).
        "Return" => {
            if rest.is_empty() {
                Ok(Terminator::Return { value: None })
            } else {
                let v = parse_temp_ref(lno, rest)?;
                Ok(Terminator::Return { value: Some(v) })
            }
        }
        // `If t{cond} {then_label} {else_label}` (L320-333).
        "If" => {
            let (cond_tok, labels) = split_first_word(rest);
            let cond = parse_temp_ref(lno, cond_tok)?;
            let (then_lbl, else_lbl) = split_first_word(labels);
            if then_lbl.is_empty() || else_lbl.is_empty() || else_lbl.contains(' ') {
                return Err(format!("line {lno}: malformed If terminator: {s:?}"));
            }
            let then_block = resolve_label(lno, then_lbl)?;
            let else_block = resolve_label(lno, else_lbl)?;
            Ok(Terminator::If {
                cond,
                then_block,
                else_block,
            })
        }
        // `Jump {target}` or `Jump {target}(t.., ...)` (L334-347).
        "Jump" => {
            // The target label is alphanumeric+underscore and is followed
            // either by end-of-line or by '(' opening the arg list.
            let (target_label, args) = match rest.find('(') {
                Some(open) => {
                    let label = &rest[..open];
                    let (ids, after) = parse_paren_temp_list(lno, &rest[open..])?;
                    ensure_empty(lno, after, "Jump")?;
                    (label, ids)
                }
                None => (rest, Vec::new()),
            };
            if target_label.is_empty() {
                return Err(format!("line {lno}: Jump missing target label: {s:?}"));
            }
            let target = resolve_label(lno, target_label)?;
            Ok(Terminator::Jump { target, args })
        }
        other => Err(format!("line {lno}: unknown terminator `{other}`: {s:?}")),
    }
}

// ---------------------------------------------------------------------------
// Numeric helpers
// ---------------------------------------------------------------------------

/// Parse a `0x`-prefixed lowercase hex `u64` (`{:#x}` output, e.g.
/// `WordBits(0x…)`, `<singleton:0x…>`).
fn parse_hex_prefixed_u64(s: &str) -> Option<u64> {
    let hex = s.strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

/// Parse hex bits that may or may not carry a `0x` prefix. `type_label`
/// renders `<singleton:{bits:#x}>` (always `0x`-prefixed), so this is the
/// path used by `parse_type`; kept lenient for safety.
fn parse_hex_u64(s: &str) -> Option<u64> {
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        u64::from_str_radix(s, 16).ok()
    }
}

/// Parse a contiguous lowercase-hex byte string (`{b:02x}` per byte, no
/// separators) into a `Vec<u8>`. Inverse of `fmt_const`'s StubEntryRef sig
/// loop (format.rs L304-306).
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// String/char unescaping (inverse of Rust's `{:?}` for str/char)
// ---------------------------------------------------------------------------

/// Parse a `"…"`-quoted, Rust-`{:?}`-escaped string (the whole token,
/// including the surrounding quotes). Errors if not quoted.
fn parse_quoted_string(lno: usize, tok: &str) -> Result<String, String> {
    let (s, rest) = parse_leading_quoted_string(lno, tok)?;
    if !rest.is_empty() {
        return Err(format!(
            "line {lno}: unexpected trailing text after string literal: {rest:?}"
        ));
    }
    Ok(s)
}

/// Parse a leading `"…"` string literal off the front of `tok`, returning the
/// decoded `String` and the remaining (unconsumed) suffix. Honours Rust's
/// `Debug` escaping so an escaped `\"` inside the literal does not terminate
/// it. This is the inverse of `format!("{s:?}")` for `&str` — see the escape
/// table verified empirically: `\n \t \r \\ \" \0` and `\u{HEX}`; printable
/// Unicode (incl. non-ASCII) passes through literally.
fn parse_leading_quoted_string(lno: usize, tok: &str) -> Result<(String, &str), String> {
    let mut chars = tok.char_indices();
    match chars.next() {
        Some((_, '"')) => {}
        _ => return Err(format!("line {lno}: expected '\"' starting string: {tok:?}")),
    }
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                // End of the literal; the remainder starts after this quote.
                let rest = &tok[i + 1..];
                return Ok((out, rest));
            }
            '\\' => {
                let (_, esc) = chars
                    .next()
                    .ok_or_else(|| format!("line {lno}: dangling '\\' in string {tok:?}"))?;
                match esc {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '\'' => out.push('\''), // not emitted for strings, accepted anyway
                    '0' => out.push('\0'),
                    'u' => {
                        // `\u{HEX}` — consume `{`, hex digits, `}`.
                        let cp = parse_unicode_escape(lno, &mut chars, tok)?;
                        out.push(cp);
                    }
                    other => {
                        return Err(format!(
                            "line {lno}: unknown string escape `\\{other}` in {tok:?}"
                        ));
                    }
                }
            }
            other => out.push(other),
        }
    }
    Err(format!("line {lno}: unterminated string literal {tok:?}"))
}

/// Parse a `'c'`-quoted, Rust-`{:?}`-escaped char literal (the whole token).
/// Inverse of `format!("{c:?}")` for `char`: escapes `\n \t \r \\ \' \0` and
/// `\u{HEX}`; `"` is NOT escaped for chars (printed bare as `'"'`).
fn parse_quoted_char(lno: usize, tok: &str) -> Result<char, String> {
    let inner = tok
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .ok_or_else(|| format!("line {lno}: expected '…' char literal: {tok:?}"))?;
    let mut chars = inner.char_indices().map(|(_, c)| c).peekable();
    // Re-wrap as a char_indices iterator over `inner` for the unicode helper.
    let first = chars
        .next()
        .ok_or_else(|| format!("line {lno}: empty char literal {tok:?}"))?;
    let result = if first == '\\' {
        let esc = chars
            .next()
            .ok_or_else(|| format!("line {lno}: dangling '\\' in char {tok:?}"))?;
        match esc {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"', // not emitted, accepted anyway
            '0' => '\0',
            'u' => {
                // Consume `{HEX}` from the remaining chars.
                let mut peekable = chars.by_ref();
                parse_unicode_escape_chars(lno, &mut peekable, tok)?
            }
            other => {
                return Err(format!(
                    "line {lno}: unknown char escape `\\{other}` in {tok:?}"
                ));
            }
        }
    } else {
        first
    };
    // Ensure nothing remains (a well-formed char literal is exactly one char).
    if chars.next().is_some() {
        return Err(format!(
            "line {lno}: char literal has trailing characters: {tok:?}"
        ));
    }
    Ok(result)
}

/// Consume a `{HEX}` after a `\u`, from a `CharIndices` iterator (string
/// path). Returns the decoded `char`.
fn parse_unicode_escape(
    lno: usize,
    chars: &mut std::str::CharIndices,
    tok: &str,
) -> Result<char, String> {
    // Expect '{'.
    match chars.next() {
        Some((_, '{')) => {}
        _ => return Err(format!("line {lno}: `\\u` not followed by '{{' in {tok:?}")),
    }
    let mut hex = String::new();
    loop {
        let (_, c) = chars
            .next()
            .ok_or_else(|| format!("line {lno}: unterminated `\\u{{…}}` in {tok:?}"))?;
        if c == '}' {
            break;
        }
        hex.push(c);
    }
    decode_unicode_hex(lno, &hex, tok)
}

/// Same as [`parse_unicode_escape`] but for a plain `char` iterator (the
/// char-literal path doesn't carry byte indices).
fn parse_unicode_escape_chars(
    lno: usize,
    chars: &mut impl Iterator<Item = char>,
    tok: &str,
) -> Result<char, String> {
    match chars.next() {
        Some('{') => {}
        _ => return Err(format!("line {lno}: `\\u` not followed by '{{' in {tok:?}")),
    }
    let mut hex = String::new();
    loop {
        let c = chars
            .next()
            .ok_or_else(|| format!("line {lno}: unterminated `\\u{{…}}` in {tok:?}"))?;
        if c == '}' {
            break;
        }
        hex.push(c);
    }
    decode_unicode_hex(lno, &hex, tok)
}

/// Decode the hex digits inside a `\u{…}` escape into a `char`.
fn decode_unicode_hex(lno: usize, hex: &str, tok: &str) -> Result<char, String> {
    let cp = u32::from_str_radix(hex, 16)
        .map_err(|_| format!("line {lno}: bad `\\u{{{hex}}}` hex in {tok:?}"))?;
    char::from_u32(cp).ok_or_else(|| format!("line {lno}: invalid scalar U+{hex} in {tok:?}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::format_dfm_module;

    /// Convenience: a resolver that always yields id 0 (the dump doesn't
    /// carry class ids, so any value round-trips).
    fn r0(_: &str) -> Option<u32> {
        Some(0)
    }

    /// Round-trip assertion: parse(format(fns)) re-formats to the same bytes.
    fn assert_roundtrip(fns: &[Function]) {
        let s = format_dfm_module(fns);
        let back = parse_dfm_module(&s, &r0)
            .unwrap_or_else(|e| panic!("parse failed: {e}\n--- input ---\n{s}"));
        let s2 = format_dfm_module(&back);
        assert_eq!(s2, s, "round-trip mismatch\n--- orig ---\n{s}\n--- back ---\n{s2}");
    }

    fn temp(id: u32, ty: TypeEstimate) -> Temporary {
        Temporary {
            id: TempId(id),
            type_estimate: ty,
        }
    }

    fn dummy_span() -> Span {
        Span {
            file_id: FileId(0),
            lo: 0,
            hi: 0,
        }
    }

    /// Function 1: exercises every TypeEstimate variant (params + block
    /// params + a Const dst), and the Return-with-value terminator.
    fn fn_types() -> Function {
        // Cover every TypeEstimate. As of B-i, params/block-params render via
        // `type_label`, so Class(5)/Singleton(0xdeadbeef) print LOSSLESSLY as
        // `<class:5>` / `<singleton:0xdeadbeef>` and round-trip with their ids
        // intact (the numeric `<class:N>` parse path, no resolver needed).
        let tys = [
            TypeEstimate::Top,
            TypeEstimate::Bottom,
            TypeEstimate::Integer,
            TypeEstimate::SingleFloat,
            TypeEstimate::DoubleFloat,
            TypeEstimate::Character,
            TypeEstimate::Boolean,
            TypeEstimate::String,
            TypeEstimate::Unit,
            TypeEstimate::Class(5),
            TypeEstimate::Singleton(0xdead_beef),
        ];
        let mut temps = Vec::new();
        let mut params = Vec::new();
        for (i, ty) in tys.iter().enumerate() {
            temps.push(temp(i as u32, *ty));
            params.push(TempId(i as u32));
        }
        // A Const result temp.
        let dst = TempId(100);
        temps.push(temp(100, TypeEstimate::Integer));

        Function {
            id: FunctionId(0),
            name: "all-types".to_string(),
            params,
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![Computation::Const {
                    dst,
                    value: ConstValue::Integer(42),
                }],
                terminator: Terminator::Return { value: Some(dst) },
            }],
            temps,
            return_type: TypeEstimate::Integer,
            span: dummy_span(),
        }
    }

    /// Function 2: exercises every Computation variant and the
    /// Dispatch/SealedDirectCall `type_label`-rendered dsts (Class(7),
    /// Singleton) so the `<class:N>` / `<singleton:0xN>` parse path runs.
    fn fn_computations() -> Function {
        // temps: define each dst with a concrete type.
        let mut temps = vec![
            temp(0, TypeEstimate::Top),       // param a
            temp(1, TypeEstimate::Integer),   // param b
            temp(2, TypeEstimate::Integer),   // Const Integer
            temp(3, TypeEstimate::String),    // Const String (escaped)
            temp(4, TypeEstimate::DoubleFloat), // Const Float
            temp(5, TypeEstimate::Boolean),   // Const Bool
            temp(6, TypeEstimate::Character), // Const Char
            temp(7, TypeEstimate::Unit),      // Const Unit
            temp(8, TypeEstimate::Top),       // Const WordBits
            temp(9, TypeEstimate::Top),       // Const ClassMetadataPtr
            temp(10, TypeEstimate::String),   // Const StringLiteralRef
            temp(11, TypeEstimate::Top),      // Const SymbolLiteralRef
            temp(12, TypeEstimate::Top),      // Const StubEntryRef
            temp(13, TypeEstimate::Integer),  // PrimOp
            temp(14, TypeEstimate::Top),      // DirectCall no safepoint, no_alloc
            temp(15, TypeEstimate::Top),      // DirectCall with safepoint
            temp(16, TypeEstimate::Top),      // Call with safepoint
            temp(17, TypeEstimate::Boolean),  // TypeCheck builtin
            temp(18, TypeEstimate::Boolean),  // TypeCheck UserClass
            temp(19, TypeEstimate::Unit),     // WriteBarrier
            temp(20, TypeEstimate::Integer),  // LoadSlot Integer
            temp(21, TypeEstimate::Top),      // LoadSlot Object
            temp(22, TypeEstimate::Integer),  // StoreSlot Integer
            temp(23, TypeEstimate::Top),      // StoreSlot Object
            temp(24, TypeEstimate::Class(7)), // Dispatch (type_label)
            temp(25, TypeEstimate::Singleton(0x1234)), // SealedDirectCall (type_label)
        ];
        // a slot-pointer temp + a value temp reused across barrier/slot ops.
        temps.push(temp(30, TypeEstimate::Top));
        temps.push(temp(31, TypeEstimate::Top));

        let computations = vec![
            Computation::Const {
                dst: TempId(2),
                value: ConstValue::Integer(-7),
            },
            Computation::Const {
                dst: TempId(3),
                // Escaped string: newline, tab, quotes, backslash, NUL,
                // a control char (\u{1b}), and a non-ASCII + astral char.
                value: ConstValue::String("a\nb\t\"c\"\\d\0e\u{1b}f\u{e9}g😀".to_string()),
            },
            Computation::Const {
                dst: TempId(4),
                // A non-round f64 that isn't an approximation of a known
                // math constant (clippy::approx_constant rejects e.g. PI);
                // exercises Rust's `{:?}` float formatting + `from_str`.
                value: ConstValue::Float(2.5009765625),
            },
            Computation::Const {
                dst: TempId(5),
                value: ConstValue::Bool(true),
            },
            Computation::Const {
                dst: TempId(6),
                value: ConstValue::Char('\n'),
            },
            Computation::Const {
                dst: TempId(7),
                value: ConstValue::Unit,
            },
            Computation::Const {
                dst: TempId(8),
                value: ConstValue::WordBits(0xdead_beef_0000_0001),
            },
            Computation::Const {
                dst: TempId(9),
                value: ConstValue::ClassMetadataPtr {
                    class_id: 12,
                    tagged: true,
                },
            },
            Computation::Const {
                dst: TempId(10),
                value: ConstValue::StringLiteralRef("hi\nthere".to_string()),
            },
            Computation::Const {
                dst: TempId(11),
                value: ConstValue::SymbolLiteralRef("my-sym".to_string()),
            },
            Computation::Const {
                dst: TempId(12),
                value: ConstValue::StubEntryRef {
                    dll: "kernel32.dll".to_string(),
                    symbol: "GetTickCount".to_string(),
                    signature_bytes: vec![0x00, 0x01, 0xfe, 0xff, 0x7f],
                },
            },
            Computation::PrimOp {
                dst: TempId(13),
                op: PrimOp::AddInt,
                args: vec![TempId(1), TempId(2)],
            },
            // DirectCall: no safepoint, with [no_alloc].
            Computation::DirectCall {
                dst: TempId(14),
                callee: "%fixnum-add".to_string(),
                args: vec![TempId(1), TempId(2)],
                safepoint_roots: vec![],
                is_no_alloc: true,
            },
            // DirectCall: with safepoint, no [no_alloc].
            Computation::DirectCall {
                dst: TempId(15),
                callee: "user-func".to_string(),
                args: vec![TempId(0)],
                safepoint_roots: vec![TempId(0), TempId(3)],
                is_no_alloc: false,
            },
            // Call: computed callee, with safepoint.
            Computation::Call {
                dst: TempId(16),
                callee: TempId(0),
                args: vec![TempId(1)],
                safepoint_roots: vec![TempId(3)],
            },
            // TypeCheck builtin.
            Computation::TypeCheck {
                dst: TempId(17),
                value: TempId(0),
                class: ClassCheck::Integer,
            },
            // TypeCheck UserClass.
            Computation::TypeCheck {
                dst: TempId(18),
                value: TempId(0),
                class: ClassCheck::UserClass {
                    id: 99,
                    name: "<my-class>".to_string(),
                },
            },
            // WriteBarrier.
            Computation::WriteBarrier {
                dst: TempId(19),
                slot: TempId(30),
                value: TempId(31),
            },
            // LoadSlot Integer + Object.
            Computation::LoadSlot {
                dst: TempId(20),
                instance: TempId(30),
                offset: 8,
                slot_type: SlotTypeKind::Integer,
            },
            Computation::LoadSlot {
                dst: TempId(21),
                instance: TempId(30),
                offset: 16,
                slot_type: SlotTypeKind::Object,
            },
            // StoreSlot Integer + Object.
            Computation::StoreSlot {
                dst: TempId(22),
                instance: TempId(30),
                offset: 8,
                value: TempId(31),
                slot_type: SlotTypeKind::Integer,
            },
            Computation::StoreSlot {
                dst: TempId(23),
                instance: TempId(30),
                offset: 24,
                value: TempId(31),
                slot_type: SlotTypeKind::Object,
            },
            // Dispatch with safepoint (dst type_label = <class:7>).
            Computation::Dispatch {
                dst: TempId(24),
                generic_name: "size".to_string(),
                args: vec![TempId(0)],
                safepoint_roots: vec![TempId(0)],
            },
            // SealedDirectCall with a 2-long chain, safepoint, no_alloc.
            Computation::SealedDirectCall {
                dst: TempId(25),
                method: "draw$1".to_string(),
                fallback_chain: vec!["m2".to_string(), "m3".to_string()],
                generic_name: "draw".to_string(),
                args: vec![TempId(0)],
                safepoint_roots: vec![TempId(0)],
                is_no_alloc: true,
            },
        ];

        Function {
            id: FunctionId(0),
            name: "all-computations".to_string(),
            params: vec![TempId(0), TempId(1)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations,
                terminator: Terminator::Return { value: None },
            }],
            temps,
            return_type: TypeEstimate::Unit,
            span: dummy_span(),
        }
    }

    /// Function 3: multi-block control flow exercising If, Jump (with and
    /// without args), block params, and a forward branch.
    fn fn_control_flow() -> Function {
        let temps = vec![
            temp(0, TypeEstimate::Boolean), // param: cond
            temp(1, TypeEstimate::Integer), // then value
            temp(2, TypeEstimate::Integer), // else value
            temp(3, TypeEstimate::Integer), // join block param
        ];
        let blocks = vec![
            // entry: If cond then0 else0  (forward refs to later blocks)
            Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![],
                terminator: Terminator::If {
                    cond: TempId(0),
                    then_block: BlockId(1),
                    else_block: BlockId(2),
                },
            },
            // then0: const 1; Jump join0(t1)
            Block {
                id: BlockId(1),
                label: "then0".to_string(),
                params: vec![],
                computations: vec![Computation::Const {
                    dst: TempId(1),
                    value: ConstValue::Integer(1),
                }],
                terminator: Terminator::Jump {
                    target: BlockId(3),
                    args: vec![TempId(1)],
                },
            },
            // else0: const 2; Jump join0(t2)
            Block {
                id: BlockId(2),
                label: "else0".to_string(),
                params: vec![],
                computations: vec![Computation::Const {
                    dst: TempId(2),
                    value: ConstValue::Integer(2),
                }],
                terminator: Terminator::Jump {
                    target: BlockId(3),
                    args: vec![TempId(2)],
                },
            },
            // join0(t3: <integer>): Return t3   — block params path
            Block {
                id: BlockId(3),
                label: "join0".to_string(),
                params: vec![TempId(3)],
                computations: vec![],
                terminator: Terminator::Return {
                    value: Some(TempId(3)),
                },
            },
        ];
        Function {
            id: FunctionId(0),
            name: "<user-point>-getter-x".to_string(), // name with <>/-
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks,
            temps,
            return_type: TypeEstimate::Integer,
            span: dummy_span(),
        }
    }

    /// Function 4: a no-param, no-arg Jump (Jump without arg list) plus an
    /// empty-arg DirectCall to verify the `()` empty-list path.
    fn fn_edge_cases() -> Function {
        let temps = vec![temp(0, TypeEstimate::Top)];
        let blocks = vec![
            Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![Computation::DirectCall {
                    dst: TempId(0),
                    callee: "thunk".to_string(),
                    args: vec![],
                    safepoint_roots: vec![],
                    is_no_alloc: false,
                }],
                terminator: Terminator::Jump {
                    target: BlockId(1),
                    args: vec![],
                },
            },
            Block {
                id: BlockId(1),
                label: "loop_exit0".to_string(),
                params: vec![],
                computations: vec![],
                terminator: Terminator::Return { value: None },
            },
        ];
        Function {
            id: FunctionId(0),
            name: "run-task$1082_1".to_string(), // name with $, digits, _
            params: vec![],
            entry: BlockId(0),
            blocks,
            temps,
            return_type: TypeEstimate::Unit,
            span: dummy_span(),
        }
    }

    #[test]
    fn roundtrip_types() {
        assert_roundtrip(&[fn_types()]);
    }

    /// A Dylan-side lowering wire dump emits `ClassMetadataPtr` by class NAME
    /// (it can't know the host-assigned id at lowering time); the reconstruction
    /// resolves it via `resolve_class`. (The human format uses a numeric id,
    /// which round-trips unchanged — see `roundtrip_computations`.)
    #[test]
    fn classmetadataptr_by_name_resolves() {
        let text = "fn mk () -> <top>:\n  \
                    entry:\n    \
                    t0: <top> = Const ClassMetadataPtr(<my-class>, tagged=false)\n    \
                    Return t0\n";
        let resolver = |name: &str| if name == "<my-class>" { Some(4242) } else { None };
        let fns = parse_dfm_module(text, &resolver).unwrap();
        match &fns[0].blocks[0].computations[0] {
            Computation::Const {
                value: ConstValue::ClassMetadataPtr { class_id, tagged },
                ..
            } => {
                assert_eq!(*class_id, 4242);
                assert!(!tagged);
            }
            other => panic!("expected ClassMetadataPtr, got {other:?}"),
        }
        // An unresolvable name is a descriptive error, not a panic.
        assert!(parse_dfm_module(text, &|_| None).is_err());
    }

    /// A Dylan-side lowering dump names a `define method` body with its
    /// specialiser classes BY NAME (`g$<idler>_<integer>`); the reconstruction
    /// resolves each `$<class>` suffix to the numeric `ClassId` scheme
    /// (`g$1082_1`) via `resolve_class`. A numeric suffix round-trips unchanged
    /// (see `roundtrip_edge_cases`'s `run-task$1082_1`).
    #[test]
    fn method_body_name_suffix_resolves() {
        let text = "fn run-task$<idler>_<integer> () -> <top>:\n  \
                    entry:\n    \
                    Return\n";
        let resolver = |name: &str| match name {
            "<idler>" => Some(1082),
            "<integer>" => Some(1),
            _ => None,
        };
        let fns = parse_dfm_module(text, &resolver).unwrap();
        assert_eq!(fns[0].name, "run-task$1082_1");

        // A single-specialiser method has no trailing `_` (e.g. `rope-size$N`).
        let text1 = "fn rope-size$<rope> () -> <top>:\n  entry:\n    Return\n";
        let fns1 = parse_dfm_module(text1, &|n: &str| {
            if n == "<rope>" { Some(2001) } else { None }
        })
        .unwrap();
        assert_eq!(fns1[0].name, "rope-size$2001");

        // An unannotated specialiser is `<object>` -> id 0; an already-numeric
        // suffix round-trips unchanged.
        let text2 = "fn f$1082_1 () -> <top>:\n  entry:\n    Return\n";
        let fns2 = parse_dfm_module(text2, &|_| None).unwrap();
        assert_eq!(fns2[0].name, "f$1082_1");

        // An unresolvable `<class>` suffix is a descriptive error, not a panic.
        assert!(parse_dfm_module(text, &|_| None).is_err());
    }

    /// B-i: a class-typed param/return/block-param now dumps `<class:N>`. The
    /// Rust dump uses a numeric `N` (round-trips unchanged); a Dylan wire dump
    /// emits the class BY NAME (`<class:<idler>>`), which `parse_type` resolves
    /// through `resolve_class`. After resolution the temp reformats (via
    /// `type_label`) to the numeric `<class:N>` form — exactly matching the Rust
    /// dump, which is the whole point of B-i (lossless class-typed params for
    /// SEALED dispatch resolution in the `--lower-with-dylan` flip).
    #[test]
    fn class_type_by_name_resolves() {
        let text = "fn step (t0: <class:<idler>>) -> <class:<task>>:\n  \
                    entry:\n    \
                    Return t0\n";
        let resolver = |name: &str| match name {
            "<idler>" => Some(1082),
            "<task>" => Some(7),
            _ => None,
        };
        let fns = parse_dfm_module(text, &resolver).unwrap();
        // The class-by-name payloads resolved to their ids.
        assert_eq!(fns[0].temp_type(TempId(0)), TypeEstimate::Class(1082));
        assert_eq!(fns[0].return_type, TypeEstimate::Class(7));
        // Reformat (via `type_label`) yields the numeric `<class:N>` form that
        // matches the Rust dump byte-for-byte.
        let reformatted = format_dfm_module(&fns);
        assert_eq!(
            reformatted,
            "fn step (t0: <class:1082>) -> <class:7>:\n  entry:\n    Return t0\n"
        );

        // A block-param carries a class-by-name too.
        let bp_text = "fn f () -> <unit>:\n  \
                       entry:\n    \
                       Jump join0\n  \
                       join0(t0: <class:<idler>>):\n    \
                       Return\n";
        let bp = parse_dfm_module(bp_text, &resolver).unwrap();
        assert_eq!(bp[0].temp_type(TempId(0)), TypeEstimate::Class(1082));

        // A numeric payload (the human/Rust dump) round-trips without a resolver.
        let num_text = "fn step (t0: <class:1082>) -> <top>:\n  entry:\n    Return t0\n";
        let num = parse_dfm_module(num_text, &|_| None).unwrap();
        assert_eq!(num[0].temp_type(TempId(0)), TypeEstimate::Class(1082));

        // An unresolvable class NAME is a descriptive error, not a panic.
        assert!(parse_dfm_module(text, &|_| None).is_err());
    }

    #[test]
    fn roundtrip_computations() {
        assert_roundtrip(&[fn_computations()]);
    }

    #[test]
    fn roundtrip_control_flow() {
        assert_roundtrip(&[fn_control_flow()]);
    }

    #[test]
    fn roundtrip_edge_cases() {
        assert_roundtrip(&[fn_edge_cases()]);
    }

    /// Multi-function module: all four functions concatenated, exercising the
    /// blank-line function separator.
    #[test]
    fn roundtrip_multi_function_module() {
        let fns = vec![
            fn_types(),
            fn_computations(),
            fn_control_flow(),
            fn_edge_cases(),
        ];
        assert_roundtrip(&fns);
    }

    /// Spot-check structural reconstruction (not just byte round-trip):
    /// function/block ids assigned in order, entry = first block, branch
    /// labels resolved to the right ids.
    #[test]
    fn structural_ids_and_branch_resolution() {
        let s = format_dfm_module(&[fn_control_flow()]);
        let back = parse_dfm_module(&s, &r0).unwrap();
        assert_eq!(back.len(), 1);
        let f = &back[0];
        assert_eq!(f.id, FunctionId(0));
        assert_eq!(f.entry, BlockId(0));
        assert_eq!(f.blocks.len(), 4);
        // Labels assigned ids in appearance order.
        assert_eq!(f.blocks[0].label, "entry");
        assert_eq!(f.blocks[3].label, "join0");
        // entry's `If` resolves then0→BlockId(1), else0→BlockId(2).
        match &f.blocks[0].terminator {
            Terminator::If {
                then_block,
                else_block,
                ..
            } => {
                assert_eq!(*then_block, BlockId(1));
                assert_eq!(*else_block, BlockId(2));
            }
            other => panic!("expected If, got {other:?}"),
        }
        // join0 has one block param of type <integer>.
        assert_eq!(f.blocks[3].params, vec![TempId(3)]);
        assert_eq!(f.temp_type(TempId(3)), TypeEstimate::Integer);
    }

    /// Multi-function module id assignment: ids are 0,1,2,… by parse order.
    #[test]
    fn structural_function_ids_sequential() {
        let fns = vec![fn_types(), fn_computations(), fn_edge_cases()];
        let s = format_dfm_module(&fns);
        let back = parse_dfm_module(&s, &r0).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].id, FunctionId(0));
        assert_eq!(back[1].id, FunctionId(1));
        assert_eq!(back[2].id, FunctionId(2));
        assert_eq!(back[0].name, "all-types");
        assert_eq!(back[2].name, "run-task$1082_1");
    }

    /// Malformed input returns Err, not a panic.
    #[test]
    fn malformed_inputs_error() {
        assert!(parse_dfm_module("fn foo:", &r0).is_err()); // no ` (`
        assert!(parse_dfm_module("fn foo () -> <integer>", &r0).is_err()); // no trailing ':'
        assert!(parse_dfm_module("not a function", &r0).is_err());
        // block stmt before any block header
        assert!(parse_dfm_module("fn f () -> <unit>:\n    Return\n", &r0).is_err());
        // unknown computation variant
        assert!(
            parse_dfm_module(
                "fn f () -> <unit>:\n  entry:\n    t0: <top> = Bogus\n    Return\n",
                &r0
            )
            .is_err()
        );
        // jump to unknown block
        assert!(
            parse_dfm_module(
                "fn f () -> <unit>:\n  entry:\n    Jump nowhere\n",
                &r0
            )
            .is_err()
        );
    }

    /// Build a one-block function returning Unit whose single Const carries
    /// `value`. Used to round-trip individual ConstValue edge cases.
    fn const_fn(value: ConstValue) -> Function {
        Function {
            id: FunctionId(0),
            name: "k".to_string(),
            params: vec![],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![Computation::Const {
                    dst: TempId(0),
                    value,
                }],
                terminator: Terminator::Return { value: None },
            }],
            temps: vec![temp(0, TypeEstimate::Top)],
            return_type: TypeEstimate::Unit,
            span: dummy_span(),
        }
    }

    /// Escaping corner cases that the formatter can actually emit, isolated
    /// per-Const so a failure pinpoints the offending shape.
    #[test]
    fn roundtrip_const_escaping_corners() {
        let cases = vec![
            // String with a literal single-quote (NOT escaped by `{:?}` for
            // strings) alongside an escaped double-quote and a backslash.
            ConstValue::String("it's a \"test\" \\ end".to_string()),
            // String with CR/LF/TAB/NUL and a DEL (0x7f) + low control char.
            ConstValue::String("\r\n\t\0\u{7f}\u{1}".to_string()),
            // Char: a literal double-quote (printed bare as '"' for chars).
            ConstValue::Char('"'),
            // Char: an escaped single-quote.
            ConstValue::Char('\''),
            // Char: an astral-plane scalar.
            ConstValue::Char('😀'),
            // Char: a control char rendered as \u{..}.
            ConstValue::Char('\u{1b}'),
            // Float edge cases that `{:?}` renders specially.
            ConstValue::Float(1.0),
            ConstValue::Float(-0.0),
            ConstValue::Float(1e300),
            ConstValue::Float(0.1),
            // i128 extremes.
            ConstValue::Integer(i128::MIN),
            ConstValue::Integer(i128::MAX),
            // WordBits zero and full-width.
            ConstValue::WordBits(0),
            ConstValue::WordBits(u64::MAX),
            // ClassMetadataPtr both tag states.
            ConstValue::ClassMetadataPtr { class_id: 0, tagged: false },
            // Empty string + empty symbol.
            ConstValue::StringLiteralRef(String::new()),
            ConstValue::SymbolLiteralRef(String::new()),
            // StubEntryRef whose dll/symbol contain escapable characters,
            // exercising the quote-aware split in `parse_stub_entry`, plus an
            // empty signature.
            ConstValue::StubEntryRef {
                dll: "weird\\name\".dll".to_string(),
                symbol: "sym, with comma".to_string(),
                signature_bytes: vec![],
            },
        ];
        for value in cases {
            assert_roundtrip(&[const_fn(value)]);
        }
    }
}
