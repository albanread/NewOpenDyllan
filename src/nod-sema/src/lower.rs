//! AST → DFM lowering.
//!
//! # Macro boundary policy
//!
//! The match arms in this file that handle `Expr::*` / `Statement::*`
//! variants ARE the second temptation surface for hardcoded
//! control-flow drift (the first being `nod-reader::ast`). Before
//! adding another arm for a new control-flow form, read
//! `docs/MACRO_BOUNDARY.md`. New surface forms should be `define
//! macro` in `stdlib/*.dylan` and expanded
//! by `nod-macro` before this file ever sees them.
//!
//! The remaining hardcoded forms here (`If`, `Begin`, `Let`,
//! `Method`/`LocalMethod`, definitional items, `Statement::Block`)
//! are the frozen kernel list per the policy. `Statement::While`,
//! `Until`, `For`, and `Expr::Case` are retirement candidates per
//! the macro-boundary porting plan (Wave 2).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

/// Sprint 44 — process-global counter for `__anon-method-NNNN`
/// synthetic names. Previously local to `LiftState`, which reset to 0
/// on every `lower_module_full` call; that's fine for single-file
/// builds (the stdlib merge silently dedups its `__anon-method-0`
/// against the user's), but multi-file user builds need monotonically-
/// increasing names across files so `merge_modules`'s "first writer
/// wins" closure-registry merge doesn't drop one closure's metadata
/// into the bucket of another closure with the same numeric suffix.
///
/// Tests that want deterministic per-test names can call
/// `_reset_anon_method_counter_for_tests()`.
static ANON_METHOD_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Test helper: reset the process-global anon-method counter to 0.
/// Useful for tests that snapshot lowering output and need stable
/// `__anon-method-N` indices. Production builds should never call
/// this — Sprint 44's multi-file path relies on monotone allocation.
pub fn _reset_anon_method_counter_for_tests() {
    ANON_METHOD_COUNTER.store(0, Ordering::SeqCst);
}

use nod_dfm::{
    Block, BlockId, ClassCheck, Computation, ConstValue, Function, FunctionId, PrimOp,
    SlotTypeKind, TempId, Temporary, Terminator, TypeEstimate,
};
use nod_reader::{
    BinOp, Binder, Expr, ForClause, Item, Module, Param, ReturnSig, Span, Statement, UnOp,
};
use nod_runtime::{
    ClassId, ClassMetadata, SlotDefault, SlotInfo, SlotType, Word, class_metadata_for,
    class_metadata_ptr, find_class_id_by_name, find_class_id_by_name_excluding_shim_band,
    register_mi_user_class, register_simple_user_class,
};

use crate::c3::{C3Error, c3_linearise};

/// Sprint 51e — class-name resolution as a USER program sees it.
///
/// Front-end-shim classes (`ClassId::FIRST_SHIM..`) get registered in a
/// host process when a statically-linked shim's AOT resolver fires
/// (e.g. parsing with `--parse-with-dylan`). They are the compiler
/// front-end's private classes and must NOT shadow same-named USER
/// classes, nor block a user program that defines (say) its own
/// `<token>`. So while lowering a USER module (the shim id band is OFF)
/// we resolve names through [`find_class_id_by_name_excluding_shim_band`],
/// which skips the shim band. While lowering the SHIM's OWN source (band
/// ON), those classes ARE the program's classes, so we fall back to the
/// unfiltered [`find_class_id_by_name`]. See `ClassId::FIRST_SHIM`.
fn resolve_class_id_by_name(name: &str) -> Option<ClassId> {
    if nod_runtime::shim_class_band_active() {
        find_class_id_by_name(name)
    } else {
        find_class_id_by_name_excluding_shim_band(name)
    }
}

type LocalEnv = HashMap<String, TempId>;

/// Sprint 15: structured outcomes of the redefinition-refusal pass.
/// Surfaced via `LoweringError` so the driver can display the
/// diagnostic with span context.
#[derive(Clone, Debug)]
pub enum SealingViolation {
    /// `define class <Sub> (<Sealed>)` where `<Sealed>` was sealed by
    /// a prior compilation unit ("another library" in Sprint 15's
    /// simulated-cross-library scope).
    SealedClassExtendedAcrossBoundary { sealed_parent: String, child: String },
    /// `add-method` against a generic whose `sealed` flag is set
    /// from a prior compilation unit.
    SealedGenericClosed { generic: String },
}

impl std::fmt::Display for SealingViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SealingViolation::SealedClassExtendedAcrossBoundary {
                sealed_parent,
                child,
            } => write!(
                f,
                "sealed-class-extended-across-boundary: `{child}` cannot extend `{sealed_parent}` — sealed classes are closed against subclassing across library boundaries (Sprint 15 single-library scope)"
            ),
            SealingViolation::SealedGenericClosed { generic } => write!(
                f,
                "sealed-generic-closed: cannot add methods to `{generic}` — sealed against further additions (Sprint 15 single-library scope)"
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub enum LoweringError {
    Unsupported { span: Span, message: String },
    UndefinedIdent { span: Span, name: String },
    TypeMismatch { span: Span, message: String },
    /// Integer literal doesn't fit in the fixnum range
    /// (`[FIXNUM_MIN, FIXNUM_MAX]` = 63-bit signed).
    IntegerOverflow { span: Span, value: i128 },
    /// Re-defining an existing class. Sprint 12 refuses class
    /// redefinition; Sprint 28+ adds lazy migration.
    ClassRedefinitionNotSupported { span: Span, class_name: String },
    /// `class:` / `each-subclass:` / `virtual:` slots — Sprint 12 only
    /// supports `instance:` allocation.
    UnsupportedSlotAllocation { span: Span, class_name: String, slot_name: String, allocation: String },
    /// The class's parent reference doesn't resolve to a known class.
    UnknownSuperclass { span: Span, class_name: String, super_name: String },
    /// Sprint 14: C3 linearisation failed — two parents impose
    /// inconsistent orders on shared ancestors.
    InconsistentInheritance { span: Span, class_name: String, detail: String },
    /// Sprint 14: two parents independently define a slot with the same
    /// name. Inheriting the same slot from a shared ancestor (diamond)
    /// is fine; defining the same slot name in two unrelated parents
    /// is an MI conflict the programmer must resolve.
    SlotConflict {
        span: Span,
        class_name: String,
        slot_name: String,
        first_origin: String,
        second_origin: String,
    },
    /// Sprint 15: a redefinition that would break a sealing assumption.
    /// Single-library Sprint 15 scope: cross-library extension is
    /// "simulated" as "another lowering call after the class is
    /// sealed". Per-method violations are surfaced before any
    /// runtime mutation runs.
    SealingViolation { span: Span, violation: SealingViolation },
}

impl std::fmt::Display for LoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoweringError::Unsupported { span, message } => {
                write!(f, "unsupported [{:?}]: {message}", span)
            }
            LoweringError::UndefinedIdent { span, name } => {
                write!(f, "undefined ident `{name}` [{:?}]", span)
            }
            LoweringError::TypeMismatch { span, message } => {
                write!(f, "type mismatch [{:?}]: {message}", span)
            }
            LoweringError::IntegerOverflow { span, value } => write!(
                f,
                "integer overflow [{:?}]: literal {value} out of fixnum range \
                 (<big-integer> / <double-integer> not yet supported)",
                span
            ),
            LoweringError::ClassRedefinitionNotSupported { span, class_name } => write!(
                f,
                "class redefinition refused [{:?}]: `{class_name}` already exists; Sprint 12 forbids redefinition",
                span
            ),
            LoweringError::UnsupportedSlotAllocation { span, class_name, slot_name, allocation } => write!(
                f,
                "slot allocation `{allocation}` not supported [{:?}]: in `{class_name}` slot `{slot_name}` (only `instance:` is supported in Sprint 12)",
                span
            ),
            LoweringError::UnknownSuperclass { span, class_name, super_name } => write!(
                f,
                "unknown superclass `{super_name}` [{:?}]: in `define class {class_name}`",
                span
            ),
            LoweringError::InconsistentInheritance { span, class_name, detail } => write!(
                f,
                "inconsistent inheritance [{:?}]: in `define class {class_name}`: {detail}",
                span
            ),
            LoweringError::SlotConflict {
                span,
                class_name,
                slot_name,
                first_origin,
                second_origin,
            } => write!(
                f,
                "slot conflict [{:?}]: `{class_name}` inherits slot `{slot_name}` from two unrelated parents (`{first_origin}` and `{second_origin}`); rename one slot to disambiguate",
                span
            ),
            LoweringError::SealingViolation { span, violation } => write!(f, "{violation} [{:?}]", span),
        }
    }
}

impl LoweringError {
    pub fn span(&self) -> Span {
        match self {
            LoweringError::Unsupported { span, .. }
            | LoweringError::UndefinedIdent { span, .. }
            | LoweringError::TypeMismatch { span, .. }
            | LoweringError::IntegerOverflow { span, .. }
            | LoweringError::ClassRedefinitionNotSupported { span, .. }
            | LoweringError::UnsupportedSlotAllocation { span, .. }
            | LoweringError::UnknownSuperclass { span, .. }
            | LoweringError::InconsistentInheritance { span, .. }
            | LoweringError::SlotConflict { span, .. }
            | LoweringError::SealingViolation { span, .. } => *span,
        }
    }
}

/// A method registration captured during lowering and applied to the
/// runtime dispatch table after JIT compilation. The driver / JIT glue
/// resolves `body_fn_name` to a JIT'd function pointer, then calls
/// `nod_runtime::add_method_full` with the full specialiser list.
///
/// Sprint 13 carries one `ClassId` per required parameter
/// (`specialisers`); the legacy `receiver_class` field is kept as a
/// convenience accessor for callers that only need the first
/// position.
#[derive(Clone, Debug)]
pub struct MethodRegistration {
    pub generic_name: String,
    pub specialisers: Vec<ClassId>,
    pub body_fn_name: String,
    pub param_count: usize,
}

impl MethodRegistration {
    /// First-parameter specialiser. Sprint 12 callers used this as
    /// "the receiver class"; Sprint 13's multi-arg dispatch reads the
    /// full vector.
    pub fn receiver_class(&self) -> ClassId {
        self.specialisers.first().copied().unwrap_or(ClassId::OBJECT)
    }
}

/// Sprint 20b: a `%`-prefixed primitive callee lowers to a `DirectCall`
/// against a `nod_*` runtime extern. The lowerer recognises the leading
/// `%` and routes through `LOWER_PRIMITIVE_TABLE` below; the codegen
/// layer (`nod-llvm/src/codegen.rs::emit_direct_call`) honours the
/// `%`-prefix and emits the matching extern declaration.
///
/// Each entry: `(dylan-name, runtime-symbol, arity, return-type)`.
///
/// **Naming convention** — every primitive name starts with `%`. The
/// runtime symbol is the `nod_*` C-ABI shim. Arity is the parameter
/// count; the return type is the Dylan-side `TypeEstimate`.
///
/// Primitives wired here are intentionally low-level — they bridge
/// Dylan source to the existing Sprint 20 runtime API. Higher-level
/// generics (`size`, `concatenate`, `for-each`) live in
/// `stdlib/*.dylan` and call these.
const LOWER_PRIMITIVE_TABLE: &[(&str, &str, usize, TypeEstimate)] = &[
    // Fail-fast: signal a <simple-error> with the given <byte-string> as
    // message. Does not return — the runtime's nod_error path raises
    // a Rust panic that the Sprint 45g crash dumper catches, exits 99,
    // and prints GC + safepoint state. Used by the in-flight Dylan
    // parser to crash at the closest point to a syntax problem (rather
    // than building a partial AST with inline error nodes that fail
    // later, far from the originating site). Declared TypeEstimate::Top
    // because the type system doesn't model never-returns yet.
    ("%error", "nod_error", 1, TypeEstimate::Top),
    // Sprint 47 — multi-value return secondary-values buffer. See
    // `docs/COMPILER_GAPS.md` GAP-003 and
    // `src/nod-runtime/src/values.rs`. The SBCL-style discipline:
    // `values(a, b, c)` lowers as `%values-set!(0, b); %values-set!(1, c)`
    // then returns `a` through the ordinary ABI; the multi-binder `let
    // (x, y, z) = …` form calls `%values-clear()` before evaluating the
    // RHS so polluted state from earlier calls doesn't leak in, binds
    // `x` to the RHS's normal return, then reads `y` from `%values-get(0)`
    // and `z` from `%values-get(1)`. `%values-count` is exposed for
    // completeness (rarely used directly from Dylan; the receiver-side
    // lowering relies on `%values-get` returning `#f` for missing extras).
    ("%values-clear", "nod_values_clear", 0, TypeEstimate::Top),
    ("%values-set!", "nod_values_set", 2, TypeEstimate::Top),
    ("%values-get", "nod_values_get", 1, TypeEstimate::Top),
    ("%values-count", "nod_values_count", 0, TypeEstimate::Integer),
    // Collection-class primitives (Sprint 20b — wraps the Rust Sprint 20 API).
    ("%collection-size", "nod_collection_size", 1, TypeEstimate::Integer),
    ("%collection-concatenate", "nod_collection_concatenate", 2, TypeEstimate::Top),
    // <range> field accessors.
    ("%range-from", "nod_range_from", 1, TypeEstimate::Integer),
    ("%range-to", "nod_range_to", 1, TypeEstimate::Integer),
    ("%range-by", "nod_range_by", 1, TypeEstimate::Integer),
    // <simple-object-vector> primitives.
    ("%vector-size", "nod_sov_size", 1, TypeEstimate::Integer),
    ("%vector-element", "nod_sov_element", 2, TypeEstimate::Top),
    ("%vector-element-setter", "nod_sov_element_setter", 3, TypeEstimate::Top),
    // <stretchy-vector> primitives.
    ("%stretchy-vector-size", "nod_stretchy_vector_size", 1, TypeEstimate::Integer),
    ("%stretchy-vector-element", "nod_stretchy_vector_element", 2, TypeEstimate::Top),
    (
        "%stretchy-vector-element-setter",
        "nod_stretchy_vector_element_setter",
        3,
        TypeEstimate::Top,
    ),
    ("%stretchy-vector-push", "nod_stretchy_vector_push", 2, TypeEstimate::Top),
    // Collection-classes lever (Part A2) — <bit-vector> primitives. The
    // `make(<bit-vector>, …)` allocation is a `lower_make` redirect (see
    // below); these surface element/size/count to the Dylan generics in
    // `stdlib/arrays.dylan`.
    ("%bit-vector-ref", "nod_bit_vector_ref", 2, TypeEstimate::Integer),
    ("%bit-vector-set", "nod_bit_vector_set", 3, TypeEstimate::Top),
    ("%bit-vector-size", "nod_bit_vector_size", 1, TypeEstimate::Integer),
    ("%bit-vector-count", "nod_bit_vector_count", 1, TypeEstimate::Integer),
    // Word-level bitwise integer primitives (logand/logior/logxor/lognot/
    // ash) over fixnums — the building blocks for bit-vector ops.
    ("%logand", "nod_logand", 2, TypeEstimate::Integer),
    ("%logior", "nod_logior", 2, TypeEstimate::Integer),
    ("%logxor", "nod_logxor", 2, TypeEstimate::Integer),
    ("%lognot", "nod_lognot", 1, TypeEstimate::Integer),
    ("%ash", "nod_ash", 2, TypeEstimate::Integer),
    // FIP primitives — drive the existing Rust iteration state.
    ("%fip-init", "nod_fip_init", 1, TypeEstimate::Top),
    ("%fip-finished?", "nod_fip_finished_p", 1, TypeEstimate::Boolean),
    ("%fip-current-element", "nod_fip_current_element", 1, TypeEstimate::Top),
    ("%fip-advance!", "nod_fip_advance", 1, TypeEstimate::Top),
    // Allocators — for tests that exercise <range> and <stretchy-vector>
    // from Dylan source without going through `make(<range>, …)` keyword
    // dispatch.
    ("%make-range", "nod_make_range", 3, TypeEstimate::Top),
    ("%make-stretchy-vector", "nod_make_stretchy_vector", 1, TypeEstimate::Top),
    // Sprint 21: first-class function dispatch primitives.
    // Sprint 26: extended to arities 0 and 3..=5 so closures and
    // env-bound function-Refs can be called cleanly without packing
    // args into a `<simple-object-vector>` for `nod_apply`.
    ("%funcall0", "nod_funcall0", 1, TypeEstimate::Top),
    ("%funcall1", "nod_funcall1", 2, TypeEstimate::Top),
    ("%funcall2", "nod_funcall2", 3, TypeEstimate::Top),
    ("%funcall3", "nod_funcall3", 4, TypeEstimate::Top),
    ("%funcall4", "nod_funcall4", 5, TypeEstimate::Top),
    ("%funcall5", "nod_funcall5", 6, TypeEstimate::Top),
    ("%apply", "nod_apply", 2, TypeEstimate::Top),
    // Sprint 21: allocate a zero-filled `<simple-object-vector>` of the
    // given length. Mirrors `collection_map`'s allocator path.
    ("%make-sov", "nod_make_sov_len", 1, TypeEstimate::Top),
    // Sprint 24: closures — `<cell>` and `<environment>` primitives.
    // `%make-cell(v) -> <cell>`. Allocate a one-slot box.
    ("%make-cell", "nod_make_cell", 1, TypeEstimate::Top),
    // `%cell-get(c) -> <object>`. Load the cell's value slot.
    ("%cell-get", "nod_cell_get", 1, TypeEstimate::Top),
    // `%cell-set!(v, c) -> v`. Store through the GC write barrier.
    ("%cell-set!", "nod_cell_set", 2, TypeEstimate::Top),
    // `%env-cell(env, idx) -> <cell>`. Read a cell pointer out of an
    // environment by index. The caller follows up with `%cell-get` /
    // `%cell-set!` to actually read/write the captured variable.
    ("%env-cell", "nod_env_cell", 2, TypeEstimate::Top),
    // `%make-environment(cells_vec) -> <environment>`. Wrap a pre-built
    // SOV of cell-Words into an environment record.
    ("%make-environment", "nod_make_environment", 1, TypeEstimate::Top),
    // `%make-closure(name, arity, env) -> <function>`. Allocate a fresh
    // closure `<function>` Word in the moveable heap whose body is the
    // already-registered `name` symbol and whose env-ptr slot points at
    // `env`. The lowerer emits this at every closure-creation site that
    // captures at least one variable.
    ("%make-closure", "nod_make_closure", 3, TypeEstimate::Top),
    // Sprint 42a — <byte-string> primitives. Minimum surface (allocate,
    // size, byte-read, byte-write, bulk-copy); all higher-level ops
    // (`concatenate`, `copy-sequence`, `subsequence`, `starts-with?`,
    // `ends-with?`, `find-substring`, `as-uppercase`, `as-lowercase`,
    // `empty?`) live in `stdlib.dylan` and call these.
    ("%byte-string-allocate", "nod_byte_string_allocate", 1, TypeEstimate::Top),
    ("%byte-string-size", "nod_byte_string_size", 1, TypeEstimate::Integer),
    ("%byte-string-element", "nod_byte_string_element", 2, TypeEstimate::Integer),
    ("%byte-string-element-setter", "nod_byte_string_element_setter", 3, TypeEstimate::Integer),
    ("%byte-string-copy!", "nod_byte_string_copy_bytes", 5, TypeEstimate::Integer),
    // `<character>` ↔ `<integer>` code-point conversion. A char lowers to
    // a raw i32 code (not a tagged Word); these bridge it to/from a
    // first-class fixnum `<integer>`. The codegen boundary sign-extends
    // the i32 char arg to the i64 ABI for `%char-code` and truncates the
    // i64 result back to i32 for `%code-char` (see `temp_val_as_word` /
    // the char-conv path in nod-llvm). Together they let stdlib express
    // `as(<integer>, ch)` / `as(<character>, code)` and route every char
    // predicate through the existing `ascii-*` integer helpers.
    ("%char-code", "nod_char_code", 1, TypeEstimate::Integer),
    ("%code-char", "nod_code_char", 1, TypeEstimate::Character),
    // Sprint 55 — the Dylan-side lowering (shim) calls these to classify a
    // non-local callee as a generic (-> Dispatch) vs a plain function
    // (-> DirectCall), and a param type as a class (-> <class>) vs not.
    // Host-side they're no-ops (no user code calls them).
    ("%is-generic?", "nod_is_generic_defined", 1, TypeEstimate::Boolean),
    ("%is-class?", "nod_is_class_defined", 1, TypeEstimate::Boolean),
    // Sprint 22 — <table> + hashing.
    ("%make-table", "nod_make_table", 1, TypeEstimate::Top),
    ("%table-size", "nod_table_size", 1, TypeEstimate::Integer),
    ("%table-element", "nod_table_element", 2, TypeEstimate::Top),
    ("%table-element-or-default", "nod_table_element_or_default", 3, TypeEstimate::Top),
    ("%table-element-setter", "nod_table_element_setter", 3, TypeEstimate::Top),
    ("%table-remove-key", "nod_table_remove_key", 2, TypeEstimate::Top),
    ("%table-keys", "nod_table_keys", 1, TypeEstimate::Top),
    ("%table-values", "nod_table_values", 1, TypeEstimate::Top),
    ("%object-hash", "nod_object_hash", 1, TypeEstimate::Integer),
    ("%object-equal?", "nod_object_equal_p", 2, TypeEstimate::Boolean),
    ("%subtype?", "nod_subtype_p", 2, TypeEstimate::Boolean),
    // Sprint 32 — closure → C function pointer trampolines. Each
    // primitive takes a `<function>` Word and returns a fixnum-tagged
    // `<c-pointer>` Word whose payload is the trampoline address Win32
    // can call through the standard Win64 ABI.
    ("%register-wndproc", "nod_register_wndproc", 1, TypeEstimate::Top),
    ("%register-wndenumproc", "nod_register_wndenumproc", 1, TypeEstimate::Top),
    // Sprint 34 — <c-struct> field accessors. Get primitives return an
    // <integer>; set primitives return the value Word (Dylan setter
    // convention). The offset arg is a fixnum literal baked into the
    // stdlib accessor.
    ("%struct-get-i32", "nod_struct_get_i32", 2, TypeEstimate::Integer),
    ("%struct-set-i32", "nod_struct_set_i32", 3, TypeEstimate::Integer),
    ("%struct-get-i64", "nod_struct_get_i64", 2, TypeEstimate::Integer),
    ("%struct-set-i64", "nod_struct_set_i64", 3, TypeEstimate::Integer),
    ("%struct-get-u16", "nod_struct_get_u16", 2, TypeEstimate::Integer),
    ("%struct-set-u16", "nod_struct_set_u16", 3, TypeEstimate::Integer),
    ("%struct-get-u32", "nod_struct_get_u32", 2, TypeEstimate::Integer),
    ("%struct-set-u32", "nod_struct_set_u32", 3, TypeEstimate::Integer),
    ("%struct-get-u64", "nod_struct_get_u64", 2, TypeEstimate::Integer),
    ("%struct-set-u64", "nod_struct_set_u64", 3, TypeEstimate::Integer),
    ("%struct-get-pointer", "nod_struct_get_pointer", 2, TypeEstimate::Integer),
    ("%struct-set-pointer", "nod_struct_set_pointer", 3, TypeEstimate::Integer),
    // Sprint 35 — COM shim: DXGI / D3D11 / D2D / DirectWrite primitives.
    // All return a fixnum-tagged opaque handle (or 0 on error). Sprint 35
    // uses integer-encoded floats throughout (color channels as
    // 0..=255, coordinates as integer pixels) — see
    // `nod-runtime::com_shim` module docs for the deviation rationale.
    ("%com-release", "nod_com_release", 1, TypeEstimate::Integer),
    ("%com-registry-len", "nod_com_registry_len", 0, TypeEstimate::Integer),
    ("%com-last-hresult", "nod_com_last_hresult", 0, TypeEstimate::Integer),
    ("%com-clear-last-hresult", "nod_com_clear_last_hresult", 0, TypeEstimate::Integer),
    ("%dxgi-create-factory", "nod_dxgi_create_factory", 0, TypeEstimate::Integer),
    ("%dxgi-device-from-d3d-device", "nod_dxgi_device_from_d3d_device", 1, TypeEstimate::Integer),
    ("%dxgi-create-surface-from-texture", "nod_dxgi_create_surface_from_texture", 1, TypeEstimate::Integer),
    ("%d3d11-create-device", "nod_d3d11_create_device", 0, TypeEstimate::Integer),
    ("%d3d11-get-immediate-context", "nod_d3d11_get_immediate_context", 1, TypeEstimate::Integer),
    ("%d3d11-create-texture-2d", "nod_d3d11_create_texture_2d", 4, TypeEstimate::Integer),
    ("%d3d11-copy-to-staging-and-map", "nod_d3d11_copy_to_staging_and_map", 5, TypeEstimate::Integer),
    ("%d3d11-last-staging-handle", "nod_d3d11_last_staging_handle", 0, TypeEstimate::Integer),
    ("%d3d11-last-mapped-row-pitch", "nod_d3d11_last_mapped_row_pitch", 0, TypeEstimate::Integer),
    ("%d3d11-unmap", "nod_d3d11_unmap", 2, TypeEstimate::Integer),
    ("%d2d-create-factory", "nod_d2d_create_factory", 0, TypeEstimate::Integer),
    ("%d2d-create-device", "nod_d2d_create_device", 2, TypeEstimate::Integer),
    ("%d2d-create-device-context", "nod_d2d_create_device_context", 1, TypeEstimate::Integer),
    ("%d2d-create-bitmap-for-target", "nod_d2d_create_bitmap_for_target", 2, TypeEstimate::Integer),
    ("%d2d-set-target", "nod_d2d_set_target", 2, TypeEstimate::Integer),
    ("%d2d-begin-draw", "nod_d2d_begin_draw", 1, TypeEstimate::Integer),
    ("%d2d-end-draw", "nod_d2d_end_draw", 1, TypeEstimate::Integer),
    ("%d2d-clear", "nod_d2d_clear", 5, TypeEstimate::Integer),
    ("%d2d-set-transform-identity", "nod_d2d_set_transform_identity", 1, TypeEstimate::Integer),
    ("%d2d-create-solid-color-brush", "nod_d2d_create_solid_color_brush", 5, TypeEstimate::Integer),
    ("%d2d-draw-text-layout", "nod_d2d_draw_text_layout", 5, TypeEstimate::Integer),
    ("%d2d-draw-rectangle", "nod_d2d_draw_rectangle", 7, TypeEstimate::Integer),
    ("%d2d-fill-rectangle", "nod_d2d_fill_rectangle", 6, TypeEstimate::Integer),
    ("%dwrite-create-factory", "nod_dwrite_create_factory", 0, TypeEstimate::Integer),
    ("%dwrite-create-text-format", "nod_dwrite_create_text_format", 4, TypeEstimate::Integer),
    ("%dwrite-create-text-layout", "nod_dwrite_create_text_layout", 5, TypeEstimate::Integer),
    ("%dwrite-get-layout-metrics", "nod_dwrite_get_layout_metrics", 1, TypeEstimate::Integer),
    ("%dwrite-hit-test-position", "nod_dwrite_hit_test_text_position", 3, TypeEstimate::Integer),
    ("%dwrite-hit-test-point", "nod_dwrite_hit_test_point", 3, TypeEstimate::Integer),
    ("%dwrite-set-drawing-effect", "nod_dwrite_set_drawing_effect", 4, TypeEstimate::Integer),
    ("%dwrite-set-line-spacing", "nod_dwrite_set_line_spacing", 3, TypeEstimate::Integer),
    ("%count-non-zero-red", "nod_count_non_zero_red", 4, TypeEstimate::Integer),
    // Sprint 36 — HWND-bound swap chain + IDE-shell window-class primitives.
    // All return fixnum-tagged handles, atoms, or HRESULT-encoded results;
    // float marshaling is deferred (Sprint 37+).
    ("%dxgi-factory-from-d3d-device", "nod_dxgi_factory_from_d3d_device", 1, TypeEstimate::Integer),
    ("%dxgi-create-swap-chain-for-hwnd", "nod_dxgi_create_swap_chain_for_hwnd", 5, TypeEstimate::Integer),
    ("%d2d-create-bitmap-from-swap-chain", "nod_d2d_create_bitmap_from_swap_chain", 2, TypeEstimate::Integer),
    ("%dxgi-swap-chain-present", "nod_dxgi_swap_chain_present", 1, TypeEstimate::Integer),
    ("%dxgi-swap-chain-resize-buffers", "nod_dxgi_swap_chain_resize_buffers", 3, TypeEstimate::Integer),
    ("%register-window-class", "nod_register_window_class", 2, TypeEstimate::Integer),
    ("%create-message-only-window", "nod_create_message_only_window", 1, TypeEstimate::Integer),
    ("%create-hidden-window", "nod_create_hidden_window", 1, TypeEstimate::Integer),
    ("%destroy-window", "nod_destroy_window", 1, TypeEstimate::Integer),
    ("%post-message", "nod_post_message", 4, TypeEstimate::Integer),
    ("%pump-one-message", "nod_pump_one_message", 1, TypeEstimate::Integer),
    // Sprint 41a — blocking Win32 message loop. Arity-0, returns the
    // fixnum-tagged WPARAM of the WM_QUIT message (typically the value
    // a WNDPROC's WM_DESTROY handler passed to `PostQuitMessage`).
    ("%run-message-loop", "nod_run_message_loop", 0, TypeEstimate::Integer),
    ("%def-window-proc", "nod_def_window_proc", 4, TypeEstimate::Integer),
    // Sprint 41b — IDE source-viewer primitives. Both return either a
    // fresh `<byte-string>` Word or the `nil` immediate, so the type
    // estimate has to be `Top` (a union that includes neither
    // `<integer>` nor a unique class). Dylan-side callers branch on
    // `result = nil` to surface "no file" / "no arg" cases.
    ("%read-file", "nod_read_file_to_string", 1, TypeEstimate::Top),
    ("%argv1", "nod_get_argv1", 0, TypeEstimate::Top),
    ("%argv2", "nod_get_argv2", 0, TypeEstimate::Top),
    ("%print-gc-stats", "nod_print_gc_stats", 0, TypeEstimate::Top),
    // Sprint 41b — LOWORD/HIWORD extraction for WM_SIZE `lparam` unpack.
    // Both take a fixnum value and return a fixnum. Future sprints
    // should replace with general bitwise primitives.
    ("%lo-word", "nod_lo_word", 1, TypeEstimate::Integer),
    ("%hi-word", "nod_hi_word", 1, TypeEstimate::Integer),
    // Sprint 41c — scrollbar primitives. `%set-scroll-info` takes
    // (hwnd, nbar, n-min, n-max, n-page, n-pos, redraw); `%get-scroll-pos`
    // takes (hwnd, nbar). Both return fixnum-tagged integers.
    ("%set-scroll-info", "nod_set_scroll_info", 7, TypeEstimate::Integer),
    ("%get-scroll-pos", "nod_get_scroll_pos", 2, TypeEstimate::Integer),
    // Sprint 41e — File → Open. Wraps Win32 `GetOpenFileNameW` plus the
    // 88-byte `OPENFILENAMEW` struct in a single shim that returns the
    // chosen path as a `<byte-string>` (or `nil` if the user cancelled).
    // Arity-1: takes the owner HWND as a fixnum. Return is a string-or-
    // nil union, so the type estimate has to be `Top`.
    ("%show-open-file-dialog", "nod_show_open_file_dialog", 1, TypeEstimate::Top),
    // Sprint 41g — File → Save / Save As. `%write-file` takes
    // (path, content) — both `<byte-string>` Words — and returns
    // fixnum 1 on success / 0 on I/O error. `%show-save-file-dialog`
    // mirrors `%show-open-file-dialog` exactly but calls
    // `GetSaveFileNameW` with OFN_OVERWRITEPROMPT.
    ("%write-file", "nod_write_file_from_string", 2, TypeEstimate::Integer),
    ("%show-save-file-dialog", "nod_show_save_file_dialog", 1, TypeEstimate::Top),
    // Sprint 41g's `%load-recent`, `%add-recent`, `%basename` primitives
    // (and Sprint 41c's `%count-newlines` / 41d's `%max-line-chars`) are
    // retired — Sprint 42a Phase E moved all of them into pure Dylan
    // in `tests/nod-tests/fixtures/nod-ide.dylan`, built on the
    // byte-string ops (`size`, `element`, `concatenate`, `copy-sequence`,
    // `=`) plus the `%read-file` / `%write-file` primitives.
];

fn lookup_primitive(name: &str) -> Option<(&'static str, usize, TypeEstimate)> {
    LOWER_PRIMITIVE_TABLE
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map(|(_, sym, ar, ty)| (*sym, *ar, *ty))
}

/// Sprint 21: a Dylan-source operator name (`+`, `-`, `*`, `=`, `<`,
/// `>`) used as a first-class function reference (`\+` etc.) has a
/// fixed runtime-shim arity. The shims live in `nod-runtime::functions`
/// and are pre-registered in the function-ref registry by
/// `ensure_operator_shims_registered`.
fn operator_arity(name: &str) -> Option<usize> {
    match name {
        "+" | "-" | "*" | "=" | "<" | ">" => Some(2),
        // First-class value forms of the equality / identity / instance?
        // operators. Inline forms (`3 == 3`, `instance?(x, <c>)`) lower
        // specially; these arities let a bareword / `\op` lower to
        // `nod_make_function_ref`, resolving to the matching Rust shim
        // registered in `ensure_operator_shims_registered`.
        "==" | "~=" | "~==" | "instance?" => Some(2),
        _ => None,
    }
}

/// Sprint 16: the five `<pair>` / `<list>` builtins. Each lowers to a
/// synthetic `%pair*` / `%nil` / `%empty?` callee that codegen turns
/// into a call into the matching `nod_runtime` shim.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum ListBuiltin {
    /// `pair(head, tail) -> <pair>`.
    Pair,
    /// `head(p :: <pair>) -> <object>`.
    Head,
    /// `tail(p :: <pair>) -> <object>`.
    Tail,
    /// `empty?(p) -> <boolean>`. Identity test against `nil`.
    EmptyP,
    /// `nil() -> <empty-list>`. Returns the pinned empty-list singleton.
    Nil,
}

impl ListBuiltin {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "pair" => Some(ListBuiltin::Pair),
            "head" => Some(ListBuiltin::Head),
            "tail" => Some(ListBuiltin::Tail),
            "empty?" => Some(ListBuiltin::EmptyP),
            "nil" => Some(ListBuiltin::Nil),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            ListBuiltin::Pair => "pair",
            ListBuiltin::Head => "head",
            ListBuiltin::Tail => "tail",
            ListBuiltin::EmptyP => "empty?",
            ListBuiltin::Nil => "nil",
        }
    }

    fn arity(self) -> usize {
        match self {
            ListBuiltin::Pair => 2,
            ListBuiltin::Head | ListBuiltin::Tail | ListBuiltin::EmptyP => 1,
            ListBuiltin::Nil => 0,
        }
    }

    /// Synthetic callee symbol carried in the DFM `DirectCall` and
    /// recognised by the codegen layer. Each one maps to a `nod_runtime`
    /// extern shim with a fixed ABI.
    fn callee_symbol(self) -> &'static str {
        match self {
            ListBuiltin::Pair => "%pair-alloc",
            ListBuiltin::Head => "%pair-head",
            ListBuiltin::Tail => "%pair-tail",
            ListBuiltin::EmptyP => "%empty?",
            ListBuiltin::Nil => "%nil",
        }
    }
}

/// Aggregated output of `lower_module_full`. Sprint 12 carries class
/// and method registrations alongside the lowered function list so
/// the JIT-glue (in `nod-sema::lib`) can install them. Sprint 15
/// adds the per-library sealing facts captured during lowering so
/// the dispatch resolver, `dump_sealed`, and the JIT-time installer
/// can read them.
#[derive(Default, Clone, Debug)]
pub struct LoweredModule {
    pub functions: Vec<Function>,
    pub methods: Vec<MethodRegistration>,
    /// Sprint 15 sealing facts collected from the parsed modifiers
    /// and `define sealed domain` declarations.
    pub sealing: crate::optimise::SealingFacts,
    /// Sprint 15 dispatch resolution log — one entry per `Dispatch`
    /// node the resolver inspected. Stored for `dump_dispatch`
    /// annotations and as a diagnostic aid; not load-bearing for
    /// codegen.
    pub resolutions: Vec<crate::optimise::DispatchResolution>,
    /// Sprint 19: every `block` form encountered during lowering.
    /// Post-JIT the glue (`register_blocks`) resolves the lifted thunk
    /// names to function pointers and registers them with the runtime
    /// (`nod_runtime::register_block_fns`).
    pub blocks: Vec<BlockRegistration>,
    /// Sprint 24: closure metadata produced by `lift_anonymous_methods`.
    /// The `register_top_level_functions` glue consults this to
    /// register closure bodies under their *source* arity (the body's
    /// JIT signature carries a hidden env parameter on top).
    pub closures: ClosureRegistry,
    /// Sprint 27: every `define c-function` we encountered during
    /// lowering. The driver / FFI glue (Sprint 28+) consults this to
    /// emit the per-module API stub table; Sprint 27 just recorded
    /// the metadata. Sprint 28 adds the parsed marshaling signature
    /// to each binding.
    pub c_functions: Vec<CFunctionBinding>,
    /// Sprint 28: deduplicated stub-table for this module. One entry
    /// per unique `(dll, symbol)` pair referenced by the module's
    /// `define c-function`s. The driver-side glue (`eval_expr_to_string`)
    /// builds the runtime [`nod_runtime::ApiStubTable`] from these
    /// specs and calls `nod_runtime::initialize_stub_table` BEFORE
    /// any JIT-emitted code runs. The `entry_ptr` field is patched
    /// in-place by lowering once the static-area entries exist.
    pub c_function_stub_table: Vec<CFunctionStubEntry>,
    /// Sprint 27: non-fatal diagnostics. Sprint 27 surfaces these
    /// for `define c-function` declarations whose target symbol is
    /// not present in the embedded `nod-winapi` index. The driver
    /// prints them; they don't block compilation.
    pub warnings: Vec<LoweringWarning>,
    /// Sprint 40a — every `define class` registered during lowering,
    /// in declaration / registration order. Used by the AOT pipeline
    /// (`compile_file_for_aot` → `build_aot_registrations`) to emit
    /// `nod_aot_register_user_class` calls inside the EXE's startup
    /// resolver. The JIT path ignores this field — it registers user
    /// classes inline as `register_class` runs in `lower_module_full`.
    pub user_classes: Vec<UserClassRegistration>,
    /// GAP-004 — every `define variable` lowered by this module. The
    /// AOT pipeline emits one `nod_aot_register_variable` call per
    /// entry inside the EXE's startup resolver, AFTER class / method /
    /// block / function registration (variable init expressions can
    /// reference any of those). The JIT path drives the same set
    /// through `register_variables` once the engine is materialised.
    pub variables: Vec<VariableRegistration>,
    /// Sprint 53 — the sema *recording* outputs, captured so the model
    /// can be serialised (`dump-sema`) and byte-compared against the
    /// Dylan-computed model. `top_names` + `generics` were previously
    /// computed and discarded; classes (`user_classes`) and `sealing`
    /// already lived here. Together these four are the `SemaModel` the
    /// sprint ports to Dylan. Lowering does not read these back —
    /// they're a recording snapshot, not an input (the structural
    /// `lower_with_model` split that enforces that is a later step).
    pub top_names: TopNames,
    /// Sprint 53 — generic-function names (sorted, deterministic) the
    /// recording walk collected.
    pub generics: Vec<String>,
}

/// Sprint 54 — the sema *recording* model: the authoritative record of what a
/// module declares (the four `dump-sema` sections), separated from DFM/CFG
/// construction. [`analyse_module`] produces it; the DFM construction in
/// [`lower_module_full`] consumes it (54a). Sprint 54c flips the *producer* to
/// the Dylan walk via the sema wire — the host then reconstructs a `SemaModel`
/// off the wire instead of recomputing it here.
///
/// `classes` carries `ClassId`s (process-global, assigned by registration) so
/// lowering can resolve class references; the dump and the wire deliberately
/// key on names, not ids (see [`format_sema_model`] / `DYLAN_SEMA_WIRE.md`).
#[derive(Clone, Debug)]
pub struct SemaModel {
    pub top_names: TopNames,
    /// Sorted, deterministic — the dump / wire form.
    pub generics: Vec<String>,
    /// Registration order; each entry carries its name, `ClassId`, and layout.
    pub classes: Vec<UserClassRegistration>,
    pub sealing: crate::optimise::SealingFacts,
}

impl SemaModel {
    /// The `name -> ClassId` map lowering uses to resolve class references,
    /// rebuilt from `classes`.
    pub fn user_class_map(&self) -> HashMap<String, ClassId> {
        self.classes
            .iter()
            .map(|r| (r.name.clone(), r.class_id))
            .collect()
    }
}

impl LoweredModule {
    /// Sprint 54 — a `SemaModel` view of the recording outputs captured on
    /// this `LoweredModule` (the four `dump-sema` sections). Used by
    /// [`format_sema_model`] / `dump-sema`.
    pub fn sema_model(&self) -> SemaModel {
        SemaModel {
            top_names: self.top_names.clone(),
            generics: self.generics.clone(),
            classes: self.user_classes.clone(),
            sealing: self.sealing.clone(),
        }
    }
}

/// GAP-004 — one `define variable` registration: the variable's source
/// name and the symbol name of its codegen-emitted `__init-<name>`
/// thunk (a zero-arg `extern "C-unwind" fn() -> u64` returning the
/// initial Dylan Word).
#[derive(Clone, Debug)]
pub struct VariableRegistration {
    pub name: String,
    pub init_fn_name: String,
}

/// Sprint 27: information captured for a single `define c-function`
/// declaration. Carries the DLL provenance + the c-side identifier;
/// Sprint 28 adds the marshaling signature + the index into the
/// per-module stub table. Sprint 31 adds `source` so callers can tell
/// user-written declarations apart from bindings the JIT materialized
/// from the embedded Win32 index on the fly.
#[derive(Clone, Debug)]
pub struct CFunctionBinding {
    pub dylan_name: String,
    pub c_name: String,
    pub library: String,
    pub span: Span,
    /// `true` when the symbol was found in the embedded
    /// `nod-winapi` index at compile time. `false` means the user
    /// declared a custom DLL/symbol the DB doesn't know about — we
    /// warn but continue.
    pub resolved_in_db: bool,
    /// Sprint 28: marshaling signature derived from the param /
    /// return c-type annotations. `None` when the declaration uses a
    /// c-type outside the Sprint 28 supported set; calls then surface
    /// a deferral diagnostic.
    pub signature: Option<nod_runtime::ApiCallSignature>,
    /// Sprint 31: provenance of this binding. `UserCFunction` for any
    /// explicit `define c-function` in user source (or stdlib);
    /// `JitMaterialized` when the lowerer synthesized the binding from
    /// the embedded `nod-winapi` index because a bare-name call site
    /// referenced a Win32 export the user hadn't declared.
    pub source: BindingSource,
}

/// Sprint 31: where a [`CFunctionBinding`] came from. User declarations
/// always win — if a name is declared explicitly anywhere in the module,
/// the JIT-materialization path declines to synthesize a binding for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingSource {
    /// `define c-function` written in user Dylan source (or in the
    /// future, stdlib). The default for every Sprint 27 / 28 / 30
    /// declaration.
    UserCFunction,
    /// Synthesized on the fly by Sprint 31's bare-name lookup hook.
    /// The binding never appears in the source; the lowerer fabricated
    /// it from the embedded `nod-winapi` index because a call site
    /// referenced a name the user hadn't declared.
    JitMaterialized,
}

/// Sprint 28: one resolved stub-table entry for the module being
/// lowered. The runtime-side [`nod_runtime::ApiStubTable`] is built
/// from these specs at JIT-finalize time. The per-call lowering bakes
/// the entry's static-area pointer (recovered from `entry_ptr`) into
/// the call site as an `i64` constant.
#[derive(Clone, Debug)]
pub struct CFunctionStubEntry {
    pub dll: String,
    pub symbol: String,
    pub signature: nod_runtime::ApiCallSignature,
    /// Pointer to the static-area [`nod_runtime::ApiStubEntry`] this
    /// resolved to. Populated once the per-module table is built.
    /// Until then this is null and per-call codegen would emit a 0
    /// constant; we always allocate the table BEFORE lowering call
    /// sites, so callers never observe `None` here.
    pub entry_ptr: u64,
}

/// Sprint 27: non-fatal sema diagnostic.
#[derive(Clone, Debug)]
pub enum LoweringWarning {
    /// `define c-function NAME` references a (library, c-name) pair
    /// not present in the embedded `nod-winapi` index. Sprint 27
    /// accepts the declaration anyway — the user may target a
    /// custom DLL. Sprint 28's call-site lowering will error at
    /// runtime if the LoadLibrary / GetProcAddress fails.
    CFunctionNotInDb {
        span: Span,
        name: String,
        library: String,
        c_name: String,
    },
}

impl std::fmt::Display for LoweringWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoweringWarning::CFunctionNotInDb { name, library, c_name, span } => write!(
                f,
                "warning: `define c-function {name}` (library: \"{library}\", c-name: \"{c_name}\") \
                 not in windows_api database; will fail at runtime if the DLL doesn't export it [{:?}]",
                span
            ),
        }
    }
}

/// Sprint 31: a synthesized c-function binding the lowerer fabricated
/// from the embedded Win32 index because a bare-name call site
/// referenced a name the user hadn't declared. Pre-stub-table working
/// state — once we've allocated the table this turns into both a
/// `CFunctionBinding` (for introspection) and a `CFunctionCallInfo`
/// (for per-call lowering).
#[derive(Clone, Debug)]
struct MaterializedBindingSpec {
    dylan_name: String,
    c_name: String,
    library: String,
    span: Span,
    signature: nod_runtime::ApiCallSignature,
    /// Index into `c_function_specs` where the stub-table slot lives.
    spec_idx: usize,
}

/// Sprint 31: outcome of [`try_jit_materialize_winapi`] for a single
/// bare-name candidate. Distinguishes "found and fully supported",
/// "found but signature unsupported" (so we can surface a helpful
/// diagnostic), and "not in the index at all" (so we fall through to
/// the existing unknown-ident path).
#[derive(Clone, Debug)]
enum MaterializationOutcome {
    Materialized {
        c_name: String,
        library: String,
        signature: nod_runtime::ApiCallSignature,
    },
    UnsupportedSignature {
        /// The DLL the matched function lives in. Surfaced for future
        /// diagnostic improvements (currently consumed only via the
        /// `reason` string).
        #[allow(dead_code)]
        c_name: String,
        #[allow(dead_code)]
        library: String,
        reason: String,
    },
    NotFound,
}

/// Sprint 31: DLL priority order for cross-DLL name collisions. Kernel
/// wins over user / gdi / advapi / shell / comctl; any other DLL falls
/// to alphabetical fallback. The list is small; a linear scan beats
/// pulling in a `phf` for six strings.
const WINAPI_DLL_PRIORITY: &[&str] = &[
    "kernel32.dll",
    "user32.dll",
    "gdi32.dll",
    "advapi32.dll",
    "shell32.dll",
    "comctl32.dll",
];

fn winapi_dll_priority(dll: &str) -> usize {
    WINAPI_DLL_PRIORITY
        .iter()
        .position(|&p| p == dll)
        .unwrap_or(WINAPI_DLL_PRIORITY.len())
}

/// Sprint 31: try to materialize a [`MaterializedBindingSpec`] for the
/// bare-name `name`. Default A/W disambiguation prefers W; if the
/// literal name already ends in `A` or `W` (or neither variant exists)
/// we use it as-is. Cross-DLL ambiguity is broken by
/// [`WINAPI_DLL_PRIORITY`]. Functions whose param / return types fall
/// outside Sprint 28-30's marshaling set return
/// [`MaterializationOutcome::UnsupportedSignature`] so the caller can
/// surface a helpful diagnostic instead of "unknown identifier".
fn try_jit_materialize_winapi(name: &str) -> MaterializationOutcome {
    // Pull candidates by name. We need to enumerate every DLL the name
    // lives in to apply the priority order, so we scan `functions()`
    // once rather than relying on the convenience accessor (which only
    // surfaces the first match).
    let try_one = |candidate_name: &str| -> Vec<&'static nod_winapi::FunctionInfo> {
        nod_winapi::functions()
            .iter()
            .filter(|f| f.name == candidate_name)
            .collect()
    };

    // A/W default: if the user wrote a bare name with no A/W suffix,
    // prefer the W variant (modern Unicode-correct).
    let try_order: Vec<String> = if name.ends_with('A') || name.ends_with('W') {
        vec![name.to_string()]
    } else {
        vec![format!("{name}W"), name.to_string()]
    };
    let mut candidates: Vec<&'static nod_winapi::FunctionInfo> = Vec::new();
    let mut resolved_via: String = String::new();
    for n in &try_order {
        let hits = try_one(n);
        if !hits.is_empty() {
            candidates = hits;
            resolved_via = n.clone();
            break;
        }
    }
    if candidates.is_empty() {
        return MaterializationOutcome::NotFound;
    }
    // Cross-DLL priority. Stable secondary key on dll name keeps the
    // pick deterministic when two non-priority DLLs tie.
    candidates.sort_by(|a, b| {
        winapi_dll_priority(&a.dll)
            .cmp(&winapi_dll_priority(&b.dll))
            .then_with(|| a.dll.cmp(&b.dll))
    });
    let chosen = candidates[0];

    match build_signature_from_function_info(chosen) {
        Ok(sig) => MaterializationOutcome::Materialized {
            c_name: resolved_via,
            library: chosen.dll.clone(),
            signature: sig,
        },
        Err(reason) => MaterializationOutcome::UnsupportedSignature {
            c_name: chosen.name.clone(),
            library: chosen.dll.clone(),
            reason,
        },
    }
}

/// Sprint 31: derive a Sprint 28/30 marshaling signature from a
/// [`nod_winapi::FunctionInfo`]. Returns `Err(reason)` if any param /
/// return type uses a category Sprint 28-30 can't marshal yet
/// (struct-by-value, function-pointer callback, opaque
/// pointer-to-pointer, …).
fn build_signature_from_function_info(
    info: &nod_winapi::FunctionInfo,
) -> Result<nod_runtime::ApiCallSignature, String> {
    if info.params.len() > 12 {
        return Err(format!(
            "arity {} exceeds Sprint 36b cap of 12",
            info.params.len()
        ));
    }
    let mut arg_kinds = [nod_runtime::CArgKind::Void as u8; 12];
    for (i, p) in info.params.iter().enumerate() {
        let kind = c_arg_kind_from_type_ref(&p.type_ref).map_err(|why| {
            format!(
                "parameter #{} ({}) has unsupported type: {}",
                i + 1,
                p.name.as_deref().unwrap_or("?"),
                why
            )
        })?;
        arg_kinds[i] = kind as u8;
    }
    let return_kind = c_return_kind_from_type_ref(&info.return_type)
        .map_err(|why| format!("return type has unsupported shape: {why}"))?;
    Ok(nod_runtime::ApiCallSignature {
        arg_count: info.params.len() as u8,
        arg_kinds,
        return_kind: return_kind as u8,
    })
}

/// Sprint 31: map a [`nod_winapi::TypeRef`] to a [`nod_runtime::CArgKind`].
/// Mirrors the Dylan-name table in `nod_runtime::CArgKind::from_c_type_name`
/// but works on the structured TypeRef enum directly so the JIT
/// materializer doesn't have to stringify-then-parse.
fn c_arg_kind_from_type_ref(t: &nod_winapi::TypeRef) -> Result<nod_runtime::CArgKind, String> {
    use nod_runtime::CArgKind;
    use nod_winapi::TypeRef as T;
    Ok(match t {
        T::I8 => CArgKind::Int8,
        T::U8 => CArgKind::UInt8,
        T::I16 => CArgKind::Int16,
        T::U16 => CArgKind::UInt16,
        T::I32 => CArgKind::Int32,
        T::U32 => CArgKind::UInt32,
        T::I64 => CArgKind::Int64,
        T::U64 => CArgKind::UInt64,
        T::Bool32 => CArgKind::Bool32,
        T::Handle => CArgKind::Handle,
        T::NarrowString => CArgKind::NarrowString,
        T::WideString => CArgKind::WideString,
        T::Pointer { pointee_type_ref } => match pointee_type_ref {
            // Opaque `*mut void` and one-level pointers to primitive
            // scalars marshal as a raw pointer; the Dylan side passes a
            // fixnum 0 (NULL) or a tagged-pointer word in.
            None => CArgKind::Pointer,
            Some(inner) => match inner.as_ref() {
                T::I8 | T::U8 | T::I16 | T::U16 | T::I32 | T::U32 | T::I64 | T::U64
                | T::Handle | T::Pointer { .. } => CArgKind::Pointer,
                // Pointers to enums / aliases / strings reduce to
                // raw `void*` for Sprint 31's purposes — callers can
                // still pass NULL or a raw word.
                T::Enum { .. } | T::Alias { .. } => CArgKind::Pointer,
                T::Bool32 => CArgKind::Pointer,
                T::NarrowString | T::WideString => CArgKind::Pointer,
                T::Void => CArgKind::Pointer,
            },
        },
        T::Enum { base } => c_arg_kind_from_type_ref(base)?,
        T::Alias { base, .. } => c_arg_kind_from_type_ref(base)?,
        T::Void => return Err("void as parameter type".to_string()),
    })
}

/// Sprint 31: companion to [`c_arg_kind_from_type_ref`] for return
/// types. Returns the matching [`nod_runtime::CReturnKind`].
fn c_return_kind_from_type_ref(t: &nod_winapi::TypeRef) -> Result<nod_runtime::CReturnKind, String> {
    use nod_runtime::CReturnKind;
    use nod_winapi::TypeRef as T;
    Ok(match t {
        T::Void => CReturnKind::Void,
        T::I8 | T::I16 | T::I32 => CReturnKind::Int32,
        T::U8 | T::U16 | T::U32 => CReturnKind::UInt32,
        T::I64 => CReturnKind::Int64,
        T::U64 => CReturnKind::UInt64,
        T::Bool32 => CReturnKind::Bool32,
        T::Handle => CReturnKind::Handle,
        T::NarrowString => CReturnKind::NarrowString,
        T::WideString => CReturnKind::WideString,
        T::Pointer { .. } => CReturnKind::Pointer,
        T::Enum { base } => c_return_kind_from_type_ref(base)?,
        T::Alias { base, .. } => c_return_kind_from_type_ref(base)?,
    })
}

/// Sprint 38d — bytewise-encode an [`nod_runtime::ApiCallSignature`] for
/// carriage in the DFM IR + the manifest sidecar. `ApiCallSignature` is
/// `#[repr(C)] Copy` so a `transmute`-equivalent `copy_nonoverlapping`
/// is well-defined; the inverse happens in
/// `nod_llvm::jit::resolve_reloc_kind` on the warm-replay path.
fn signature_to_bytes(sig: &nod_runtime::ApiCallSignature) -> Vec<u8> {
    let n = std::mem::size_of::<nod_runtime::ApiCallSignature>();
    let mut bytes = vec![0u8; n];
    // SAFETY: `ApiCallSignature` is `#[repr(C)] Copy` (struct of bytes
    // and a u8 array — no padding hazards on x86-64). The destination
    // slice has the exact same length.
    unsafe {
        std::ptr::copy_nonoverlapping(
            sig as *const nod_runtime::ApiCallSignature as *const u8,
            bytes.as_mut_ptr(),
            n,
        );
    }
    bytes
}

/// Sprint 31: walk a module's call sites collecting bare-name callees
/// that are *candidates* for JIT materialization — i.e. names that
/// aren't user-declared c-functions, aren't user-defined functions,
/// aren't generics, aren't classes, and aren't reserved builtins like
/// `make` / `instance?` / etc. The caller then tries each candidate
/// against the embedded Win32 index.
fn collect_bare_call_candidates(
    m: &Module,
    user_declared_c_names: &HashSet<String>,
    top_names: &TopNames,
    generics: &HashSet<String>,
    user_classes: &HashMap<String, ClassId>,
    out: &mut Vec<(String, nod_reader::Span)>,
) {
    for item in &m.items {
        match item {
            Item::DefineFunction { body, .. } | Item::DefineMethod { body, .. } => {
                for s in body {
                    walk_stmt_for_candidates(
                        s,
                        user_declared_c_names,
                        top_names,
                        generics,
                        user_classes,
                        out,
                    );
                }
            }
            Item::DefineConstant { value, .. } | Item::DefineVariable { value, .. } => {
                walk_expr_for_candidates(
                    value,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
            Item::Expr(e) => walk_expr_for_candidates(
                e,
                user_declared_c_names,
                top_names,
                generics,
                user_classes,
                out,
            ),
            _ => {}
        }
    }
}

fn walk_stmt_for_candidates(
    s: &Statement,
    user_declared_c_names: &HashSet<String>,
    top_names: &TopNames,
    generics: &HashSet<String>,
    user_classes: &HashMap<String, ClassId>,
    out: &mut Vec<(String, nod_reader::Span)>,
) {
    match s {
        Statement::Expr(e) => walk_expr_for_candidates(
            e,
            user_declared_c_names,
            top_names,
            generics,
            user_classes,
            out,
        ),
        Statement::Let { value, .. } => walk_expr_for_candidates(
            value,
            user_declared_c_names,
            top_names,
            generics,
            user_classes,
            out,
        ),
        Statement::Local { .. } => {}
        Statement::For { body, finally_, .. } => {
            for s in body {
                walk_stmt_for_candidates(
                    s,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
            for s in finally_ {
                walk_stmt_for_candidates(
                    s,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
        }
        Statement::While { cond, body, .. } | Statement::Until { cond, body, .. } => {
            walk_expr_for_candidates(
                cond,
                user_declared_c_names,
                top_names,
                generics,
                user_classes,
                out,
            );
            for s in body {
                walk_stmt_for_candidates(
                    s,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
        }
        Statement::Block { body, cleanup, afterwards, .. } => {
            for s in body {
                walk_stmt_for_candidates(
                    s,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
            for s in cleanup {
                walk_stmt_for_candidates(
                    s,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
            for s in afterwards {
                walk_stmt_for_candidates(
                    s,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
        }
    }
}

fn walk_expr_for_candidates(
    e: &Expr,
    user_declared_c_names: &HashSet<String>,
    top_names: &TopNames,
    generics: &HashSet<String>,
    user_classes: &HashMap<String, ClassId>,
    out: &mut Vec<(String, nod_reader::Span)>,
) {
    match e {
        Expr::Call { callee, args, span } => {
            if let Expr::Ident(_, name) = callee.as_ref()
                && is_winapi_candidate_name(
                    name,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                )
            {
                out.push((name.clone(), *span));
            }
            walk_expr_for_candidates(
                callee,
                user_declared_c_names,
                top_names,
                generics,
                user_classes,
                out,
            );
            for a in args {
                walk_expr_for_candidates(
                    a,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            walk_expr_for_candidates(
                lhs,
                user_declared_c_names,
                top_names,
                generics,
                user_classes,
                out,
            );
            walk_expr_for_candidates(
                rhs,
                user_declared_c_names,
                top_names,
                generics,
                user_classes,
                out,
            );
        }
        Expr::UnOp { operand, .. } => walk_expr_for_candidates(
            operand,
            user_declared_c_names,
            top_names,
            generics,
            user_classes,
            out,
        ),
        Expr::Paren { inner, .. } => walk_expr_for_candidates(
            inner,
            user_declared_c_names,
            top_names,
            generics,
            user_classes,
            out,
        ),
        Expr::If { cond, then_, else_, .. } => {
            walk_expr_for_candidates(
                cond,
                user_declared_c_names,
                top_names,
                generics,
                user_classes,
                out,
            );
            walk_expr_for_candidates(
                then_,
                user_declared_c_names,
                top_names,
                generics,
                user_classes,
                out,
            );
            if let Some(eb) = else_ {
                walk_expr_for_candidates(
                    eb,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
        }
        Expr::Begin { body, .. } => {
            for e in body {
                walk_expr_for_candidates(
                    e,
                    user_declared_c_names,
                    top_names,
                    generics,
                    user_classes,
                    out,
                );
            }
        }
        _ => {}
    }
}

/// Sprint 31: callee-name filter. A name is a winapi-candidate iff it
/// looks like a Win32 export (capital-letter start, all ASCII letters
/// / digits, no Dylan-style `<...>` / hyphenated tokens, and not a
/// known intrinsic / Dylan symbol).
fn is_winapi_candidate_name(
    name: &str,
    user_declared_c_names: &HashSet<String>,
    top_names: &TopNames,
    generics: &HashSet<String>,
    user_classes: &HashMap<String, ClassId>,
) -> bool {
    if user_declared_c_names.contains(name) {
        return false;
    }
    if top_names.contains(name) {
        return false;
    }
    if generics.contains(name) {
        return false;
    }
    if user_classes.contains_key(name) {
        return false;
    }
    if nod_runtime::is_generic_defined(name) {
        return false;
    }
    // Reserved Dylan-side identifiers. We could enumerate from a single
    // table, but the explicit allowlist of Win32-shape names below is
    // a stronger filter — if a name doesn't match that shape we never
    // bother the index.
    if !looks_like_win32_export(name) {
        return false;
    }
    true
}

/// Sprint 31: shape filter for Win32 exports. Must:
///   * Be at least 3 characters long
///   * Contain only ASCII letters and digits (no `-`, `_`, `<`, `>`, `?`, `!`)
///   * Contain at least one uppercase ASCII letter somewhere (so e.g.
///     `print`, `read`, `format` don't trigger a 13000-entry index
///     scan — every real Win32 export has at least one uppercase
///     letter, including the lowercase-prefixed ones like `lstrlenW`
///     and `wsprintfW`).
///
/// This keeps Dylan-side names like `print`, `+`, `<my-class>`, `id?`
/// out of the candidate set while admitting the unusual lowercase-
/// prefixed Win32 exports (`lstrlenA/W`, `wsprintf*`, `wnsprintf*`).
fn looks_like_win32_export(name: &str) -> bool {
    if name.len() < 3 {
        return false;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Must start with a letter (no leading digits — those aren't Dylan
    // identifiers anyway, but belt-and-braces).
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    // Must contain at least one uppercase letter somewhere. Every real
    // Win32 export does; ordinary Dylan identifiers don't.
    name.chars().any(|c| c.is_ascii_uppercase())
}

/// Sprint 19: one lifted-thunk set per `block` form in the source. The
/// names refer to top-level functions present in `LoweredModule::functions`
/// (each emitted with the canonical 8-captured-locals C ABI; handlers
/// take an additional leading `condition` arg).
#[derive(Clone, Debug)]
pub struct BlockRegistration {
    /// Deterministic id derived from `(parent_name, thunk_seq)` at
    /// lowering time. Baked into the call site as a `WordBits` constant.
    pub block_id: u64,
    pub body_fn_name: String,
    pub cleanup_fn_name: Option<String>,
    pub afterwards_fn_name: Option<String>,
    /// One entry per `exception` clause (source order).
    pub handlers: Vec<BlockHandlerRegistration>,
}

#[derive(Clone, Debug)]
pub struct BlockHandlerRegistration {
    pub class_id: ClassId,
    pub class_name: String,
    pub body_fn_name: String,
}

/// Sprint 40a — a single `define class` registration captured during
/// lowering. The JIT path doesn't need this (it calls
/// `nod_runtime::register_simple_user_class` / `register_mi_user_class`
/// inline as `register_class` runs); the AOT path serialises this shape
/// into the EXE's startup so a fresh process can replay the same
/// registrations with the same class IDs in the same order.
///
/// All offsets / CPL / slot_origin entries are already fully resolved
/// by the lowering pass (mirroring what `register_user_class_metadata`
/// pins into the static area on the JIT side). The EXE-side shim
/// (`nod_aot_register_user_class`) reconstructs a `UserClassSpec` from
/// these fields and calls `register_user_class_metadata` directly.
///
/// # Class-id determinism
///
/// The JIT/compiler process allocated `class_id` via
/// `allocate_user_class_id()` in monotonic order. The EXE-side
/// `nod_aot_resolve_relocs` calls `nod_aot_register_user_class` in the
/// SAME order this `Vec` was populated, so the EXE's
/// `allocate_user_class_id` produces the exact same sequence of IDs.
/// The shim asserts the returned id matches `class_id` and panics on
/// drift — a panic here would be a codegen bug, not a user error.
#[derive(Clone, Debug)]
pub struct UserClassRegistration {
    pub name: String,
    pub class_id: ClassId,
    /// Direct supers in declaration order. Empty for `<object>` (which
    /// the AOT path never emits — it's a seed class). For user classes
    /// with no explicit super list, this is `[<object>]` per Dylan
    /// convention (matching `register_class`'s default).
    pub parents: Vec<ClassId>,
    /// Full C3-linearised class precedence list including self at
    /// index 0.
    pub cpl: Vec<ClassId>,
    /// All slots (own + inherited) in layout order, with offsets +
    /// init-keyword strings + type kinds already populated.
    pub slots: Vec<SlotInfo>,
    /// For each `slots[i]`, the class id that introduced that slot
    /// (`class_id` for own slots; some ancestor's id for inherited).
    pub slot_origin: Vec<ClassId>,
    pub own_slot_count: usize,
    pub inherited_slot_count: usize,
}

pub fn lower_module(m: &Module) -> Result<Vec<Function>, Vec<LoweringError>> {
    lower_module_full(m).map(|lm| lm.functions)
}

/// Sprint 54 — the recording phase. Walks the (already macro-expanded,
/// anonymous-method-lifted) module and produces its [`SemaModel`]: registers
/// every `define class` (a runtime side effect that assigns `ClassId`s),
/// flips sealed flags, collects the top-level names + generic names, and
/// computes (but does NOT install) the sealing facts. No DFM/CFG is built
/// here — that is the DFM construction in [`lower_module_full`] (the named
/// `lower_with_model` extraction is a later 54a step). Returns `Err` if any
/// class fails to register.
///
/// Callers must have run the `ensure_*_registered` seed-registrations and the
/// anonymous-method lift pre-pass first (so `__anon-method-N` are present and
/// seed/runtime classes resolve) — `lower_module_full` does both before it
/// calls this.
pub fn analyse_module(m: &Module) -> Result<SemaModel, Vec<LoweringError>> {
    let (user_class_registrations, user_classes) = register_module_classes(m)?;

    // Phase 2: top-level function names (incl. auto-accessor names) +
    // generic names (sorted for a deterministic dump / wire).
    let top_names = collect_top_level_names(m, &user_classes);
    let mut generics: Vec<String> = collect_generic_names(m).into_iter().collect();
    generics.sort();

    // Sealing facts — computation only. Installation (a global side effect)
    // stays in `lower_module_full` at its historical point, just before
    // dispatch resolution, to preserve behavior.
    let sealing = crate::optimise::collect_sealing_facts(&m.items, &user_classes);

    Ok(SemaModel {
        top_names,
        generics,
        classes: user_class_registrations,
        sealing,
    })
}

/// Sprint 54c — the load-bearing variant of [`analyse_module`]. Registers the
/// module's classes (the runtime mechanism that assigns `ClassId`s — kept in
/// Rust because ids are process-global), then takes the rest of the recording
/// (`top_names` / `generics` / `sealing`) from a Dylan-produced model **dump**
/// (`dump-sema` text, emitted in-process by the `dylan-sema-emit` shim) rather
/// than recomputing it in Rust. Class references inside the dump resolve
/// against the just-registered classes (so registration must precede the
/// parse — it does). This is what makes the Dylan sema authoritative for the
/// back-end under `--sema-with-dylan`; gated `dump-dfm` byte-identical against
/// the all-Rust path.
pub fn analyse_module_from_dump(m: &Module, dump: &str) -> Result<SemaModel, String> {
    // Sprint 56a-CONSUME — under the combined front-end flag
    // (`--frontend-with-dylan` ⇒ `NOD_FRONTEND_WITH_DYLAN=1`), the Dylan class
    // derivation becomes LOAD-BEARING: install the module's classes FROM the
    // dump's lossless `=== classes ===` records (`install_dylan_classes`)
    // instead of re-deriving them in Rust (`register_module_classes`). The
    // byte-match (56a-WIRE) already proves the records equal the Rust
    // derivation, so the installed registry is identical. MI (or any case the
    // install can't faithfully reconstruct) returns `Err` → fall back to the
    // Rust derivation rather than install a wrong class.
    let consume = std::env::var("NOD_FRONTEND_WITH_DYLAN").as_deref() == Ok("1");

    let dylan_classes = parse_sema_classes(dump)?;

    let (classes, consumed) = if consume {
        match install_dylan_classes(&dylan_classes) {
            Ok(installed) => (installed, true),
            Err(e) => {
                // Faithful reconstruction failed (MI / unknown name / drift) —
                // fall back to the Rust derivation. This keeps the consume
                // safe: a partial/wrong install is never used. The fall-back
                // re-runs the full Rust registration + verify path below.
                eprintln!(
                    "frontend-with-dylan: install_dylan_classes bailed ({e}); \
                     falling back to register_module_classes"
                );
                let (classes, _user_classes) = register_module_classes(m).map_err(|errs| {
                    format!(
                        "frontend-with-dylan: {} class-registration error(s) on fallback",
                        errs.len()
                    )
                })?;
                (classes, false)
            }
        }
    } else {
        let (classes, _user_classes) = register_module_classes(m).map_err(|errs| {
            format!(
                "sema-with-dylan: {} class-registration error(s) before model parse",
                errs.len()
            )
        })?;
        (classes, false)
    };

    // Classes are now registered (ids assigned); the dump's `Class(<name>)` /
    // `sealed-domain` references resolve through the class table.
    let (top_names, generics, sealing) = parse_sema_dump(dump)?;

    if consumed {
        // Sprint 56a-CONSUME — `install_dylan_classes` does NOT flip sealed
        // flags (it uses the explicit-shape runtime entry directly). Replay the
        // sealed-flag flip that `register_module_classes` Phase 1c performed,
        // driven by the dump's `=== sealing ===` facts: `sealed-class` →
        // `mark_sealed()` on the installed class metadata; `sealed-generic` →
        // `mark_sealed()` on the generic. This MUST happen after all classes are
        // installed (so in-library subclassing of a sealed class is allowed),
        // exactly as Phase 1c runs after registration.
        for name in &sealing.sealed_classes {
            if let Some(id) = resolve_class_id_by_name(name) {
                let p = class_metadata_ptr(id);
                if !p.is_null() {
                    // SAFETY: static-area metadata.
                    unsafe { (*p).mark_sealed() };
                }
            }
        }
        for name in &sealing.sealed_generics {
            nod_runtime::get_or_create_generic(name).mark_sealed();
        }
        // When CONSUMING, the dump's class records ARE the source of truth, so
        // `verify_dylan_classes` would compare Dylan-to-Dylan (vacuous) — skip
        // it.
    } else {
        // Sprint 56a (verify-only) — make the Dylan class derivation a CHECKED
        // input on the load-bearing path: the dump's `=== classes ===` section
        // (parents / CPL / slot layout, all by name) must match the host's
        // registration. A divergence is a Dylan-vs-Rust class-derivation bug;
        // fail loudly rather than silently trust Rust. The host still owns
        // `ClassId` allocation, so this verifies the *derivation* — the
        // precondition for retiring `register_module_classes`. It promotes the
        // offline 53.3/53.4 byte-match oracle to a live invariant on
        // `--sema-with-dylan` (kept EXACTLY as-is for the non-consume path).
        verify_dylan_classes(&classes, &dylan_classes)
            .map_err(|e| format!("sema-with-dylan: {e}"))?;
    }

    Ok(SemaModel {
        top_names,
        generics,
        classes,
        sealing,
    })
}

/// Sprint 54c — Phase 1 of analysis, shared by [`analyse_module`] (Rust
/// recording) and [`analyse_module_from_dump`] (Dylan recording): register
/// every `define class` (assigning `ClassId`s + capturing AOT metadata) and
/// flip sealed flags. Returns the registrations (declaration order) and the
/// `name -> ClassId` map. `Err` if any class fails to register.
fn register_module_classes(
    m: &Module,
) -> Result<(Vec<UserClassRegistration>, HashMap<String, ClassId>), Vec<LoweringError>> {
    let mut errors: Vec<LoweringError> = Vec::new();
    let mut user_classes: HashMap<String, ClassId> = HashMap::new();
    let mut user_class_registrations: Vec<UserClassRegistration> = Vec::new();

    // Phase 1a: walk define-class items and register metadata. The sealing
    // flag flip is deferred to Phase 1c so subclassing a sealed class WITHIN
    // THIS analysis pass is allowed (in-library subclassing — spec 15 §6).
    for item in &m.items {
        if let Item::DefineClass { name, supers, slots, span, .. } = item {
            match register_class(name, supers, slots, *span) {
                Ok(id) => {
                    user_classes.insert(name.clone(), id);
                    // Snapshot the freshly-registered metadata for the AOT
                    // pipeline, read from the canonical static-area entry so
                    // offsets / CPL / slot_origin match what lowering resolves
                    // through the class table — no parallel computation.
                    let md_ptr = class_metadata_ptr(id);
                    if !md_ptr.is_null() {
                        // SAFETY: pointer is to static-area metadata
                        // (process-lived); we just registered it.
                        let md = unsafe { &*md_ptr };
                        user_class_registrations.push(UserClassRegistration {
                            name: md.name.clone(),
                            class_id: id,
                            parents: md.parents.clone(),
                            cpl: md.cpl.clone(),
                            slots: md.slots.clone(),
                            slot_origin: md.slot_origin.clone(),
                            own_slot_count: md.own_slot_count,
                            inherited_slot_count: md.inherited_slot_count,
                        });
                    }
                }
                Err(e) => errors.push(e),
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    // Phase 1c: flip sealed flags on classes + generics bearing the `sealed`
    // modifier, AFTER every class in this pass is registered (so the
    // cross-library refusal in `register_class` doesn't fire in-library).
    for item in &m.items {
        if let Item::DefineClass { name, modifiers, .. } = item
            && modifiers.contains(&nod_reader::Modifier::Sealed)
            && let Some(&id) = user_classes.get(name)
        {
            let p = class_metadata_ptr(id);
            if !p.is_null() {
                // SAFETY: static-area metadata.
                unsafe { (*p).mark_sealed() };
            }
        }
        if let Item::DefineGeneric { name, modifiers, .. } = item
            && modifiers.contains(&nod_reader::Modifier::Sealed)
        {
            let g = nod_runtime::get_or_create_generic(name);
            g.mark_sealed();
        }
    }

    Ok((user_class_registrations, user_classes))
}

/// Sprint 56a-CONSUME — the INVERSE of [`slot_type_label`]: reconstruct a
/// [`SlotType`] from the canonical `type=` label the `=== classes ===` dump
/// carries. Scalar labels map back to their `SlotType` variant exactly as
/// `slot_type_from_expr` produces them (so the round-trip
/// `slot_type_from_expr → slot_type_label → slot_type_from_label` is the
/// identity on every label the emitter can print). A class NAME (any token not
/// in the scalar bucket) resolves to `SlotType::Class(id)` via
/// [`resolve_class_id_by_name`] — the class must already be registered (its
/// type-name appears in the dump only because it was declared earlier in the
/// module, and classes install in declaration order), so an unknown name is an
/// `Err`.
fn slot_type_from_label(label: &str) -> Result<SlotType, String> {
    Ok(match label {
        "<integer>" => SlotType::Integer,
        "<double-float>" => SlotType::DoubleFloat,
        "<boolean>" => SlotType::Boolean,
        "<character>" => SlotType::Character,
        "<string>" => SlotType::String,
        "<symbol>" => SlotType::Symbol,
        "<vector>" => SlotType::Vector,
        "<object>" => SlotType::Object,
        "<top>" => SlotType::Top,
        // Anything else is a class-typed slot rendered BY NAME — resolve it.
        // NOTE: the scalar arms above match what `slot_type_label` emits;
        // `<object>` → `Object` and `<top>` → `Top` are distinct variants
        // (both pointer-shaped, identical to the GC scanner), reproducing the
        // exact `SlotType` `slot_type_from_expr` would have built.
        name => SlotType::Class(resolve_class_id_by_name(name).ok_or_else(|| {
            format!("install-dylan-classes: unknown class `{name}` in slot type label")
        })?),
    })
}

/// Sprint 56a-CONSUME — the INVERSE of [`slot_default_tag`]: reconstruct a
/// [`SlotDefault`] from the GAP-009 tag text the dump carries
/// (`unbound`/`true`/`false`/`nil`/`value:<bits>`). The boolean/nil immediates
/// are resolved from THIS process's live literal pool (matching how the AOT
/// registrar re-resolves tags 2/3/4, aot.rs ~982-986) — never baked bits —
/// while `value:<bits>` is a process-stable raw `Word` (fixnums; tag 1).
fn slot_default_from_tag(tag: &str) -> Result<SlotDefault, String> {
    let imm = nod_runtime::literal_pool_immediates();
    Ok(match tag {
        "unbound" => SlotDefault::Unbound,
        "true" => SlotDefault::Value(imm.true_),
        "false" => SlotDefault::Value(imm.false_),
        "nil" => SlotDefault::Value(imm.nil),
        other => {
            let bits = other.strip_prefix("value:").ok_or_else(|| {
                format!("install-dylan-classes: malformed default tag `{other}`")
            })?;
            let raw: u64 = bits.parse().map_err(|_| {
                format!("install-dylan-classes: malformed default value bits `{bits}`")
            })?;
            SlotDefault::Value(nod_runtime::Word::from_raw(raw))
        }
    })
}

/// Sprint 56a-CONSUME — INSTALL the module's classes FROM the Dylan-derived
/// `=== classes ===` records (the lossless dump parsed by [`parse_sema_classes`])
/// instead of re-deriving them in Rust via [`register_module_classes`]. This is
/// the load-bearing flip: under `--frontend-with-dylan` the Dylan class
/// derivation becomes the source of truth for the runtime registry, retiring the
/// last Rust front-end class logic on that path.
///
/// Classes install in dump DECLARATION ORDER (= the order
/// [`register_module_classes`] walked `m.items`), which is exactly what the AOT
/// replay (`nod_aot_register_user_class`) expects — `allocate_user_class_id` is
/// monotonic from the same seed, so the minted ids equal the Rust-derived ids
/// (the byte-match already proved the records equal the Rust derivation).
/// Parents/ancestors and class-typed slot origins resolve by NAME against
/// classes registered earlier in this very loop (plus the pre-existing
/// `<object>` / scalar / seed classes); an unknown name is an `Err`.
///
/// SCOPE: single-inheritance only. A class with `parents.len() > 1` (MI) is
/// reconstructed faithfully here too (the dump carries the merged slot list +
/// per-slot origin + C3 CPL), but the corpus has ZERO real MI, and the merge /
/// offset-patch invariants are subtle — so MI returns `Err` to force the caller
/// to fall back to `register_module_classes` rather than install a wrong class.
///
/// Returns the registrations shaped exactly like [`register_module_classes`]'s
/// output (declaration order), so the caller's `SemaModel.classes` is identical
/// whether the host derived them or the Dylan walk did.
fn install_dylan_classes(
    classes: &[ParsedSemaClass],
) -> Result<Vec<UserClassRegistration>, String> {
    let self_sentinel = ClassId(u32::MAX);
    let mut out: Vec<UserClassRegistration> = Vec::with_capacity(classes.len());
    // The id the FIRST class in this module mints — captured on the first
    // iteration. Subsequent classes must mint exactly `base + decl_idx`
    // (monotonic from the same seed). We anchor on the observed first id rather
    // than a hard `FIRST_USER` constant because seed-registration (run before
    // this loop on BOTH host + EXE) may have already minted user-band ids, so
    // the module's first class is not necessarily at `FIRST_USER`.
    let mut base_id: Option<u32> = None;

    for (decl_idx, c) in classes.iter().enumerate() {
        // Resolve direct parents by name (registered earlier in this loop, or a
        // pre-existing seed/scalar class such as `<object>`).
        let parents: Vec<ClassId> = c
            .parents
            .iter()
            .map(|n| {
                resolve_class_id_by_name(n).ok_or_else(|| {
                    format!(
                        "install-dylan-classes: class `{}` has unknown parent `{n}`",
                        c.name
                    )
                })
            })
            .collect::<Result<_, _>>()?;

        // SCOPE GUARD — SI only. Bail (Err) on MI so the caller falls back to
        // the Rust derivation rather than risk a wrong slot-merge / offset.
        if parents.len() > 1 {
            return Err(format!(
                "install-dylan-classes: class `{}` is multiple-inheritance \
                 (parents.len()={}); MI is out of scope — falling back to Rust",
                c.name,
                parents.len()
            ));
        }

        // Reconstruct the C3 CPL by name. The first entry is SELF (the class
        // isn't registered yet, so its name can't resolve) → use the
        // `ClassId(u32::MAX)` self-sentinel that `register_mi_user_class`
        // patches to the freshly minted id (mirrors `register_class`).
        let mut cpl: Vec<ClassId> = Vec::with_capacity(c.cpl.len());
        for (i, n) in c.cpl.iter().enumerate() {
            if i == 0 {
                // cpl[0] is the class itself.
                if *n != c.name {
                    return Err(format!(
                        "install-dylan-classes: class `{}` cpl[0] is `{n}`, expected self",
                        c.name
                    ));
                }
                cpl.push(self_sentinel);
            } else {
                cpl.push(resolve_class_id_by_name(n).ok_or_else(|| {
                    format!(
                        "install-dylan-classes: class `{}` has unknown CPL ancestor `{n}`",
                        c.name
                    )
                })?);
            }
        }

        // Reconstruct the full (own + inherited) slot list with offsets, types,
        // init-keywords, required flags and defaults straight from the dump —
        // the INVERSES of the WIRE helpers. Own-slot origins (origin == self
        // name) become the self-sentinel; inherited-slot origins resolve to the
        // ancestor's id by name.
        let mut slots: Vec<SlotInfo> = Vec::with_capacity(c.slots.len());
        let mut slot_origin: Vec<ClassId> = Vec::with_capacity(c.slots.len());
        let mut own_slot_count = 0usize;
        let mut inherited_slot_count = 0usize;
        for s in &c.slots {
            let type_kind = slot_type_from_label(&s.type_kind)
                .map_err(|e| format!("class `{}` slot `{}`: {e}", c.name, s.name))?;
            let default_init = slot_default_from_tag(&s.default_tag)
                .map_err(|e| format!("class `{}` slot `{}`: {e}", c.name, s.name))?;
            slots.push(SlotInfo {
                name: s.name.clone(),
                offset: s.offset,
                type_kind,
                init_keyword: s.init_keyword.clone(),
                required_init_keyword: s.required_init_keyword,
                default_init,
                has_setter: s.has_setter,
            });
            if s.origin == c.name {
                slot_origin.push(self_sentinel);
                own_slot_count += 1;
            } else {
                let origin_id = resolve_class_id_by_name(&s.origin).ok_or_else(|| {
                    format!(
                        "install-dylan-classes: class `{}` slot `{}` has unknown origin `{}`",
                        c.name, s.name, s.origin
                    )
                })?;
                slot_origin.push(origin_id);
                inherited_slot_count += 1;
            }
        }

        // Install via the explicit-shape runtime entry (the SAME one
        // `register_class`'s MI branch + `nod_aot_register_user_class` build a
        // `UserClassSpec` for): it mints the id, pins the metadata, and patches
        // the `cpl[0]` + own-slot-origin sentinels to the minted id. The SI
        // shape (`parents.len() == 1`) is identical to what
        // `register_simple_user_class` would have produced — same merged slots,
        // same cpl, same origins.
        let (id, _md_ptr) = nod_runtime::register_mi_user_class(
            &c.name,
            parents.clone(),
            cpl.clone(),
            slots.clone(),
            slot_origin.clone(),
            own_slot_count,
            inherited_slot_count,
        );

        // B3 — the canonical-order invariant, asserted at compile time (the
        // compile-time analogue of the AOT drift assert, aot.rs:1088). Ids must
        // be MONOTONIC in declaration order from the same seed: the first class
        // anchors `base_id`, each subsequent class must mint exactly
        // `base_id + decl_idx`. The host and EXE both seed-register the same
        // classes before this loop, so this catches an install order that
        // diverged from `register_module_classes`'s `m.items` walk.
        let base = *base_id.get_or_insert(id.0);
        let expected = base + decl_idx as u32;
        if id.0 != expected {
            return Err(format!(
                "install-dylan-classes: class-id drift — class `{}` (decl #{decl_idx}) \
                 minted id {} but expected {expected} (monotonic from base {base}). The \
                 install order diverged from register_module_classes' walk.",
                c.name, id.0
            ));
        }

        // Register direct-subclass links so the dispatch resolver can enumerate
        // bounded subclass sets when a parent is sealed (mirrors
        // `register_class`'s `register_direct_subclass` calls).
        for &parent_id in &parents {
            register_direct_subclass(parent_id, id);
        }

        // Re-read the now-patched metadata (cpl[0] + own-slot origins point at
        // `id`) so the captured `UserClassRegistration` is byte-identical to
        // what `register_module_classes` snapshots from the static area.
        let md_ptr = class_metadata_ptr(id);
        if md_ptr.is_null() {
            return Err(format!(
                "install-dylan-classes: class `{}` registered but metadata is null",
                c.name
            ));
        }
        // SAFETY: static-area metadata, just registered, process-lived.
        let md = unsafe { &*md_ptr };
        out.push(UserClassRegistration {
            name: md.name.clone(),
            class_id: id,
            parents: md.parents.clone(),
            cpl: md.cpl.clone(),
            slots: md.slots.clone(),
            slot_origin: md.slot_origin.clone(),
            own_slot_count: md.own_slot_count,
            inherited_slot_count: md.inherited_slot_count,
        });
    }

    // Opt-in diagnostic (env-gated, silent by default) — confirms the consume
    // ran non-vacuously and prints the installed class ids for drift debugging.
    if std::env::var("NOD_DEBUG_INSTALL_CLASSES").as_deref() == Ok("1") {
        eprintln!(
            "[install_dylan_classes] installed {} class(es): {:?}",
            out.len(),
            out.iter().map(|c| (c.name.as_str(), c.class_id.0)).collect::<Vec<_>>()
        );
    }
    Ok(out)
}

pub fn lower_module_full(m: &Module) -> Result<LoweredModule, Vec<LoweringError>> {
    lower_module_full_inner(m, None, None)
}

/// Sprint 54c — lower a module whose recording `SemaModel` was produced
/// EXTERNALLY (e.g. by the Dylan sema walk via [`analyse_module_from_dump`]),
/// rather than recomputed by [`analyse_module`]. The injected model's classes
/// must already be registered (the producer does that — ids must be live for
/// DFM construction). This is the load-bearing seam: under `--sema-with-dylan`
/// the host builds the model from the Dylan dump and feeds it here.
pub fn lower_module_full_with_model(
    m: &Module,
    model: SemaModel,
) -> Result<LoweredModule, Vec<LoweringError>> {
    lower_module_full_inner(m, Some(model), None)
}

/// Sprint 55 — the load-bearing LOWERING flip (the analogue of 54c's sema
/// flip). When `dfm_dump` is `Some(non-empty)`, the Phase-3/4 Rust lowering
/// output is REPLACED by the functions reconstructed from that dump (the
/// Dylan-side `dylan-lower-emit` text, under `--lower-with-dylan`), and the
/// SAME back-end passes (narrow / resolve-dispatch / safepoint-roots) then run
/// on the Dylan-produced DFM. `injected_model` selects the sema source exactly
/// as in [`lower_module_full_with_model`]; the two flips compose. An empty
/// `dfm_dump` (the Dylan lowering bailed) leaves the Rust lowering in place.
pub fn lower_module_full_choice(
    m: &Module,
    injected_model: Option<SemaModel>,
    dfm_dump: Option<&str>,
) -> Result<LoweredModule, Vec<LoweringError>> {
    lower_module_full_inner(m, injected_model, dfm_dump)
}

fn lower_module_full_inner(
    m: &Module,
    injected_model: Option<SemaModel>,
    dfm_dump: Option<&str>,
) -> Result<LoweredModule, Vec<LoweringError>> {
    // Sprint 19: ensure the seed condition classes are registered
    // before lowering starts so `<error>` / `<simple-error>` / etc.
    // resolve via `find_class_id_by_name` during exception-clause
    // lowering. Idempotent — repeated calls are cheap.
    nod_runtime::ensure_conditions_registered();
    // Sprint 21: ensure the `<function>` / `<wrong-number-of-arguments-error>`
    // classes + operator shim registrations are alive before lowering
    // touches `\name` / anonymous-method expressions.
    nod_runtime::ensure_functions_registered();
    // Sprint 24: ensure `<cell>` and `<environment>` are registered
    // before any closure-creation site lowers. The runtime exports
    // `nod_make_cell` / `nod_cell_get` / … as `extern "C-unwind"` symbols
    // already; this just lights up the class table.
    nod_runtime::ensure_closures_registered();
    // Sprint 27: ensure FFI c-type classes (`<c-bool>`, `<c-dword>`,
    // …) are registered before any `define c-function` declaration
    // tries to validate its parameter / return type annotations
    // against the class table.
    nod_runtime::ensure_c_types_registered();
    // Sprint 54 on-ramp — class-id-drift fix (host/shim registration
    // conflict). `nod_runtime_init` (the AOT/shim path) registers the
    // float c-types (`<c-float>` / `<c-double>`) and `<c-ffi-error>`
    // EAGERLY right here, after `ensure_c_types_registered`. The host
    // JIT/eval path used to defer them — float types to first use, and
    // `<c-ffi-error>` to the `define c-function` pre-pass further down —
    // so they landed AFTER the stdlib's `<stream>` / `<string-stream>`
    // `define class`es instead of before. That 3-class divergence (2
    // float + 1 c-ffi-error) pushed the host's `<stream>` id 3 below the
    // id the shim baked assuming the eager order, tripping
    // `nod_aot_register_user_class`'s drift assert when the shim's
    // resolver ran inside the host. Registering them here — in the
    // canonical order — makes the host and AOT/shim paths assign
    // identical user-band ids. Idempotent: the later calls become no-ops.
    nod_runtime::ensure_float_types_registered();
    nod_runtime::ensure_c_ffi_error_registered();

    // Sprint 21 pre-pass: rewrite every `Expr::Method` in expression
    // position to a synthetic `Expr::Ident(__anon-method-NNNN)` and
    // emit a matching `Item::DefineFunction` at the top level. The
    // normal lowering path then handles the lifted thunks as ordinary
    // top-level functions and the call sites as ordinary `\name`
    // references.
    let mut m_owned: Module = m.clone();
    let (closure_registry, lift_errors) = lift_anonymous_methods(&mut m_owned);
    if !lift_errors.is_empty() {
        return Err(lift_errors);
    }
    let m: &Module = &m_owned;

    // Sprint 54a — recording phase. `analyse_module` registers classes, flips
    // sealed flags, and collects top-names / generics / sealing into a
    // `SemaModel`; the DFM construction below consumes it. Rebuild the exact
    // locals the rest of this function already uses, so Phase 3/4 are
    // unchanged. Sprint 54c: when a model was injected (the Dylan walk produced
    // it, off the `dylan-sema-emit` shim, under `--sema-with-dylan`), consume
    // THAT instead of recomputing in Rust — its classes are already registered.
    let model = match injected_model {
        Some(model) => model,
        None => analyse_module(m)?,
    };
    let user_classes: HashMap<String, ClassId> = model.user_class_map();
    let user_class_registrations: Vec<UserClassRegistration> = model.classes.clone();
    let top_names: TopNames = model.top_names.clone();
    // Lowering consults generics as a set; the dump wants the sorted vec.
    let generics: HashSet<String> = model.generics.iter().cloned().collect();
    let generics_for_dump: Vec<String> = model.generics.clone();
    // Sealing facts are computed in `analyse_module`; install happens below at
    // the historical point (before dispatch resolution).
    let sealing: crate::optimise::SealingFacts = model.sealing.clone();
    // Phase 4 reuses this accumulator for per-item lowering errors; Phase 1's
    // class-registration errors are handled inside `analyse_module`.
    let mut errors: Vec<LoweringError> = Vec::new();

    let mut out: Vec<Function> = Vec::new();
    let mut methods: Vec<MethodRegistration> = Vec::new();
    // Sprint 27: every `define c-function` declaration we encounter
    // is recorded here. The driver / FFI lowerer (Sprint 28+) reads
    // these to populate the per-module API stub table.
    let mut c_functions: Vec<CFunctionBinding> = Vec::new();
    // Sprint 27: non-fatal diagnostics — currently just
    // `c-function not in windows_api database`.
    let mut warnings: Vec<LoweringWarning> = Vec::new();
    // GAP-004: per-`define variable` registrations. The AOT pipeline
    // emits one `nod_aot_register_variable` call per entry in source
    // order; the JIT path drives the same set through
    // `register_variables`.
    let mut variable_registrations: Vec<VariableRegistration> = Vec::new();
    // Sprint 19: a single `LiftSink` carries the FunctionId counter and
    // any per-`block` lifted thunks the lowerer synthesises. Both the
    // Phase 3 slot accessors and the Phase 4 user-item lowering allocate
    // ids through it.
    let mut lift_sink = LiftSink::default();
    let alloc_id = |sink: &mut LiftSink| sink.alloc_fn_id();

    // Phase 3: emit auto-generated slot accessors for every user class.
    //
    // For each slot in a class's merged layout:
    //   * If the slot was introduced by THIS class (`slot_origin == self`),
    //     emit the canonical `<C>-getter-x` / `<C>-setter-x` and register
    //     them as methods on the slot's generic (`x` / `x-setter`).
    //   * Else if the slot is inherited from an ancestor AND its offset
    //     in this class differs from the offset it had in the defining
    //     class's own layout, emit an override accessor that bakes the
    //     new offset, and register it as an additional method on the
    //     slot's generic specialised to this class. The Sprint 13
    //     dispatcher picks the override when the receiver is an instance
    //     of this class.
    //   * If the slot is inherited and the offset matches the parent's
    //     ("fixed-offset" case), no override is needed — the parent's
    //     accessor method already handles the receiver via inheritance.
    for item in &m.items {
        let Item::DefineClass { name, slots, .. } = item else {
            continue;
        };
        let Some(&class_id) = user_classes.get(name) else {
            continue;
        };
        let md_ptr = nod_runtime::class_metadata_ptr(class_id);
        if md_ptr.is_null() {
            continue;
        }
        // SAFETY: registered above; static-area lifetime.
        let metadata = unsafe { &*md_ptr };
        for (idx, slot) in metadata.slots.iter().enumerate() {
            let origin = metadata.slot_origin[idx];
            if origin == class_id {
                // Own slot — emit canonical accessors + register methods.
                let getter_name = format!("{}-getter-{}", name, slot.name);
                if !module_defines_function(m, &getter_name) {
                    out.push(build_slot_getter(
                        alloc_id(&mut lift_sink),
                        &getter_name,
                        slot.offset,
                        slot_type_to_dfm_kind(slot.type_kind),
                        slot_type_to_estimate(slot.type_kind),
                    ));
                    methods.push(MethodRegistration {
                        generic_name: slot.name.clone(),
                        specialisers: vec![class_id],
                        body_fn_name: getter_name,
                        param_count: 1,
                    });
                }
                if slot.has_setter {
                    let setter_name = format!("{}-setter-{}", name, slot.name);
                    if !module_defines_function(m, &setter_name) {
                        out.push(build_slot_setter(
                            alloc_id(&mut lift_sink),
                            &setter_name,
                            slot.offset,
                            slot_type_to_dfm_kind(slot.type_kind),
                        ));
                        methods.push(MethodRegistration {
                            generic_name: format!("{}-setter", slot.name),
                            specialisers: vec![class_id, ClassId::OBJECT],
                            body_fn_name: setter_name,
                            param_count: 2,
                        });
                    }
                }
                let _ = slots;
            } else {
                // Inherited slot — generate an override iff the offset
                // shifts vs. the slot's defining class's own layout.
                let origin_md_ptr = nod_runtime::class_metadata_ptr(origin);
                if origin_md_ptr.is_null() {
                    continue;
                }
                // SAFETY: static-area metadata.
                let origin_md = unsafe { &*origin_md_ptr };
                let origin_offset = origin_md
                    .slots
                    .iter()
                    .find(|s| s.name == slot.name)
                    .map(|s| s.offset)
                    .unwrap_or(slot.offset);
                if origin_offset == slot.offset {
                    // Fixed-offset case — parent's accessor works as-is.
                    continue;
                }
                // Override needed. Emit a fresh getter/setter that bakes
                // the new offset, register it on the slot's generic
                // specialised to this class.
                let getter_name = format!("{}-override-getter-{}", name, slot.name);
                if !module_defines_function(m, &getter_name) {
                    out.push(build_slot_getter(
                        alloc_id(&mut lift_sink),
                        &getter_name,
                        slot.offset,
                        slot_type_to_dfm_kind(slot.type_kind),
                        slot_type_to_estimate(slot.type_kind),
                    ));
                    methods.push(MethodRegistration {
                        generic_name: slot.name.clone(),
                        specialisers: vec![class_id],
                        body_fn_name: getter_name,
                        param_count: 1,
                    });
                }
                if slot.has_setter {
                    let setter_name = format!("{}-override-setter-{}", name, slot.name);
                    if !module_defines_function(m, &setter_name) {
                        out.push(build_slot_setter(
                            alloc_id(&mut lift_sink),
                            &setter_name,
                            slot.offset,
                            slot_type_to_dfm_kind(slot.type_kind),
                        ));
                        methods.push(MethodRegistration {
                            generic_name: format!("{}-setter", slot.name),
                            specialisers: vec![class_id, ClassId::OBJECT],
                            body_fn_name: setter_name,
                            param_count: 2,
                        });
                    }
                }
            }
        }
    }

    // Sprint 28 — Phase 3b: walk `define c-function` items, build the
    // marshaling signature for each, deduplicate `(dll, symbol)` pairs,
    // and allocate the per-module API stub table in the static area.
    // The resulting `c_function_call_map` is threaded through `LowerCtx`
    // so call-site lowering inside Phase 4 can resolve `Beep(...)` to a
    // WinFFI DirectCall against the right entry.
    //
    // We process declarations eagerly so the `entry_ptr` is non-null
    // before any call site is lowered. Unknown / unsupported c-types
    // produce a `signature: None`; call sites of those names then
    // surface a deferral error.
    nod_runtime::ensure_c_ffi_error_registered();
    let mut c_function_specs: Vec<nod_runtime::StubEntrySpec> = Vec::new();
    let mut c_function_pre: Vec<(String, Option<usize>, nod_reader::Span)> = Vec::new();
    let mut c_function_call_map: HashMap<String, CFunctionCallInfo> = HashMap::new();
    let mut spec_dedupe: HashMap<(String, String), usize> = HashMap::new();
    for item in &m.items {
        let Item::DefineCFunction {
            name,
            params,
            return_,
            c_name,
            library,
            span,
            ..
        } = item
        else {
            continue;
        };
        if library.is_empty() {
            // Diagnostic emitted in Phase 4; nothing to register here.
            continue;
        }
        // Build the marshaling signature from parsed types.
        let mut arg_names: Vec<String> = Vec::with_capacity(params.len());
        let mut signature_ok = true;
        for p in params {
            match &p.type_ {
                Some(Expr::Ident(_, n)) => arg_names.push(n.clone()),
                _ => {
                    signature_ok = false;
                    break;
                }
            }
        }
        let return_name: Option<String> = match return_ {
            Some(rs) if rs.values.len() > 1 => {
                signature_ok = false;
                None
            }
            Some(rs) => match rs.values.first() {
                Some(v) => match &v.type_ {
                    Some(Expr::Ident(_, n)) => Some(n.clone()),
                    _ => {
                        signature_ok = false;
                        None
                    }
                },
                None => None,
            },
            None => None,
        };
        if !signature_ok {
            c_function_pre.push((name.clone(), None, *span));
            continue;
        }
        let arg_refs: Vec<&str> = arg_names.iter().map(|s| s.as_str()).collect();
        let sig = match nod_runtime::signature_from_names(&arg_refs, return_name.as_deref()) {
            Ok(sig) => sig,
            Err(_) => {
                c_function_pre.push((name.clone(), None, *span));
                continue;
            }
        };
        let effective_c_name = c_name.clone().unwrap_or_else(|| name.clone());
        let key = (library.clone(), effective_c_name.clone());
        let idx = if let Some(&i) = spec_dedupe.get(&key) {
            i
        } else {
            let i = c_function_specs.len();
            spec_dedupe.insert(key, i);
            c_function_specs.push(nod_runtime::StubEntrySpec {
                dll: library.clone(),
                symbol: effective_c_name.clone(),
                signature: sig,
            });
            i
        };
        c_function_pre.push((name.clone(), Some(idx), *span));
        // The entry_ptr is patched once the table is allocated; we
        // need to know `idx` first, hence the two-phase loop here.
    }

    // Sprint 31: JIT-time API materialization. Walk the module's call
    // sites looking for bare-name callees that haven't already been
    // declared as `define c-function` (user wins) AND don't resolve as
    // a Dylan-side function, generic, class, or builtin. For each such
    // name try the embedded `nod-winapi` index; on a successful match
    // synthesize a `CFunctionBinding` + a stub-table entry on the fly.
    //
    // The materialization respects the same `spec_dedupe` map so two
    // bare references to `GetTickCount64` in the same module share one
    // table slot (and one resolver invocation at init time).
    //
    // Names whose signatures use unsupported types (struct-by-value,
    // function-pointer, opaque pointer-to-pointer, …) decline silently
    // — the call site then falls through to the existing
    // "unknown ident" DirectCall path. We track them so Phase 4's
    // unsupported-signature error can mention "Win32 function exists,
    // but signature uses unsupported types".
    let user_declared_c_names: HashSet<String> = c_function_pre
        .iter()
        .map(|(n, _, _)| n.clone())
        .collect();
    let mut materialized_binding_specs: Vec<MaterializedBindingSpec> = Vec::new();
    let mut materialized_call_names: HashSet<String> = HashSet::new();
    let mut materialized_unsupported: HashMap<String, String> = HashMap::new();
    let mut materialization_candidates: Vec<(String, nod_reader::Span)> = Vec::new();
    collect_bare_call_candidates(
        m,
        &user_declared_c_names,
        &top_names,
        &generics,
        &user_classes,
        &mut materialization_candidates,
    );
    let mut seen_candidate_names: HashSet<String> = HashSet::new();
    for (name, span) in &materialization_candidates {
        if !seen_candidate_names.insert(name.clone()) {
            continue;
        }
        match try_jit_materialize_winapi(name) {
            MaterializationOutcome::Materialized {
                c_name,
                library,
                signature,
            } => {
                let key = (library.clone(), c_name.clone());
                let idx = if let Some(&i) = spec_dedupe.get(&key) {
                    i
                } else {
                    let i = c_function_specs.len();
                    spec_dedupe.insert(key, i);
                    c_function_specs.push(nod_runtime::StubEntrySpec {
                        dll: library.clone(),
                        symbol: c_name.clone(),
                        signature,
                    });
                    i
                };
                materialized_binding_specs.push(MaterializedBindingSpec {
                    dylan_name: name.clone(),
                    c_name,
                    library,
                    span: *span,
                    signature,
                    spec_idx: idx,
                });
                materialized_call_names.insert(name.clone());
                nod_runtime::winffi_record_materialized();
            }
            MaterializationOutcome::UnsupportedSignature { reason, .. } => {
                materialized_unsupported.insert(name.clone(), reason);
            }
            MaterializationOutcome::NotFound => {}
        }
    }

    // Allocate the stub table NOW (in the static area). The returned
    // `entry_ptrs` are stable for the process lifetime; we bake them
    // into per-call IR as `i64` constants.
    let c_function_stub_table_entries: Vec<CFunctionStubEntry>;
    let entry_ptrs: Vec<*const nod_runtime::ApiStubEntry>;
    if !c_function_specs.is_empty() {
        let (_table, ptrs) = nod_runtime::allocate_stub_table(&c_function_specs);
        entry_ptrs = ptrs;
        c_function_stub_table_entries = c_function_specs
            .iter()
            .zip(entry_ptrs.iter())
            .map(|(s, &p)| CFunctionStubEntry {
                dll: s.dll.clone(),
                symbol: s.symbol.clone(),
                signature: s.signature,
                entry_ptr: p as u64,
            })
            .collect();
    } else {
        entry_ptrs = Vec::new();
        c_function_stub_table_entries = Vec::new();
    }
    // Build the per-call lookup map: Dylan name -> entry pointer +
    // arg count. Sprint 38d also carries (dll, symbol, signature_bytes)
    // so the call-site lowering can emit a `ConstValue::StubEntryRef`
    // (which the codegen turns into a `load i64, ptr @nod_stub__*`
    // through a per-module external global instead of baking the
    // per-process entry pointer as an `i64`).
    for (name, idx_opt, _) in &c_function_pre {
        if let Some(idx) = idx_opt {
            let p = entry_ptrs[*idx];
            let spec = &c_function_specs[*idx];
            let sig = spec.signature;
            let signature_bytes = signature_to_bytes(&sig);
            c_function_call_map.insert(
                name.clone(),
                CFunctionCallInfo {
                    entry_ptr: p as u64,
                    arg_count: sig.arg_count as usize,
                    dll: spec.dll.clone(),
                    symbol: spec.symbol.clone(),
                    signature_bytes,
                },
            );
        }
    }
    // Sprint 31: wire materialized bindings into the same lookup map
    // and register a synthesized `CFunctionBinding` so dump-ast and
    // introspection see them. User declarations always sit first in
    // `c_functions` (and in `c_function_call_map`) so explicit names
    // win over JIT materialization automatically — but `c_functions`
    // is a Vec, not a map, so explicit dedupe on `dylan_name` happens
    // here too as a belt-and-braces guard.
    for spec in &materialized_binding_specs {
        if user_declared_c_names.contains(&spec.dylan_name) {
            continue;
        }
        let p = entry_ptrs[spec.spec_idx];
        let sig = spec.signature;
        let signature_bytes = signature_to_bytes(&sig);
        c_function_call_map
            .entry(spec.dylan_name.clone())
            .or_insert(CFunctionCallInfo {
                entry_ptr: p as u64,
                arg_count: sig.arg_count as usize,
                dll: spec.library.clone(),
                symbol: spec.c_name.clone(),
                signature_bytes,
            });
        c_functions.push(CFunctionBinding {
            dylan_name: spec.dylan_name.clone(),
            c_name: spec.c_name.clone(),
            library: spec.library.clone(),
            span: spec.span,
            resolved_in_db: true,
            signature: Some(sig),
            source: BindingSource::JitMaterialized,
        });
    }

    // Phase 4: lower user-defined items.
    let user_classes_snapshot = user_classes.clone();
    for item in &m.items {
        match item {
            Item::DefineConstant { name, value, span, .. } => {
                let mut b = FunctionBuilder::new(alloc_id(&mut lift_sink), name.clone(), *span);
                let mut env = LocalEnv::new();
                let ctx = LowerCtx {
                    top_names: &top_names,
                    generics: &generics,
                    user_classes: &user_classes_snapshot,
                    closures: Some(&closure_registry),
                    c_functions: Some(&c_function_call_map),
                };
                match b.lower_expr(value, &mut env, &ctx) {
                    Ok(t) => {
                        let ty = b.func.temp_type(t);
                        b.func.return_type = ty;
                        b.terminate_current(Terminator::Return { value: Some(t) });
                        out.push(b.finish());
                    }
                    Err(e) => errors.push(e),
                }
            }
            Item::DefineFunction {
                name,
                params,
                body,
                return_,
                span,
                ..
            } => {
                let ctx = LowerCtx {
                    top_names: &top_names,
                    generics: &generics,
                    user_classes: &user_classes_snapshot,
                    closures: Some(&closure_registry),
                    c_functions: Some(&c_function_call_map),
                };
                match lower_function_inner(
                    alloc_id(&mut lift_sink),
                    name,
                    params,
                    return_.as_ref(),
                    body,
                    *span,
                    &ctx,
                    &mut lift_sink,
                ) {
                    Ok(f) => out.push(f),
                    Err(e) => errors.push(e),
                }
            }
            Item::DefineMethod {
                name,
                params,
                body,
                return_,
                span,
                ..
            } => {
                let ctx = LowerCtx {
                    top_names: &top_names,
                    generics: &generics,
                    user_classes: &user_classes_snapshot,
                    closures: Some(&closure_registry),
                    c_functions: Some(&c_function_call_map),
                };
                match lower_method_item(
                    alloc_id(&mut lift_sink),
                    name,
                    params,
                    return_.as_ref(),
                    body,
                    *span,
                    &ctx,
                    &mut lift_sink,
                ) {
                    Ok(method) => {
                        // 0-param methods carry no registration: they are
                        // plain direct-call functions, not dispatched generics.
                        if let Some(reg) = method.registration {
                            methods.push(reg);
                        }
                        out.push(method.function);
                    }
                    Err(e) => errors.push(e),
                }
            }
            Item::DefineGeneric { .. } => {
                // Sprint 12: `define generic` is informational —
                // declares the name. We collected it in `generics`
                // already; no lowering needed.
            }
            Item::DefineClass { .. } => {
                // Already handled in Phase 1.
            }
            Item::DefineVariable { name, value, span, .. } => {
                // GAP-004: lower as two cooperating functions.
                //
                // 1. `<name>()` — the getter. Body emits a DirectCall
                //    to `nod_var_get_by_name(<name-literal>)` which
                //    loads the variable's `<cell>` via the per-name
                //    slot and returns `nod_cell_get(cell)`. Bareword
                //    references in expression position (`format-out(
                //    "%d", foo)`) lower to a zero-arg DirectCall on
                //    `<name>` (Sprint 02's TopNames bareword path,
                //    seen by `Expr::Ident` in `lower_expr`), and that
                //    DirectCall resolves to THIS function by name —
                //    the runtime dispatcher then jumps to its body.
                //
                // 2. `__init-<name>()` — the init thunk. Body lowers
                //    the user's init expression and returns the
                //    result. The AOT resolver / JIT-side init driver
                //    calls this once at startup to obtain the initial
                //    Word, allocates a fresh `<cell>` holding that
                //    Word, and stores the cell pointer in the slot.
                //
                // The setter (`nod_var_set_by_name(value, name)`) is
                // not emitted as a standalone function; `lower_assign`
                // inlines a DirectCall at each `<name> := value` site.
                // This saves a function-call indirection and avoids
                // needing to register `<name>-setter` in the dispatch
                // table just to forward to the shim.
                let ctx = LowerCtx {
                    top_names: &top_names,
                    generics: &generics,
                    user_classes: &user_classes_snapshot,
                    closures: Some(&closure_registry),
                    c_functions: Some(&c_function_call_map),
                };

                // Getter: `<name>() => <object>`.
                let mut getter =
                    FunctionBuilder::new(alloc_id(&mut lift_sink), name.clone(), *span);
                let name_t = getter.fresh_temp(TypeEstimate::String);
                getter.push(Computation::Const {
                    dst: name_t,
                    value: ConstValue::String(name.clone()),
                });
                let value_t = getter.fresh_temp(TypeEstimate::Top);
                getter.push(Computation::DirectCall {
                    dst: value_t,
                    callee: "nod_var_get_by_name".to_string(),
                    args: vec![name_t],
                    safepoint_roots: Vec::new(),
                    is_no_alloc: false,
                });
                getter.func.return_type = TypeEstimate::Top;
                getter.terminate_current(Terminator::Return { value: Some(value_t) });
                out.push(getter.finish());

                // Init thunk: `__init-<name>() => <object>`.
                let init_name = format!("__init-{name}");
                let mut init =
                    FunctionBuilder::new(alloc_id(&mut lift_sink), init_name.clone(), *span);
                let mut env = LocalEnv::new();
                match init.lower_expr(value, &mut env, &ctx) {
                    Ok(t) => {
                        let ty = init.func.temp_type(t);
                        init.func.return_type = ty;
                        init.terminate_current(Terminator::Return { value: Some(t) });
                        out.push(init.finish());
                        variable_registrations.push(VariableRegistration {
                            name: name.clone(),
                            init_fn_name: init_name,
                        });
                    }
                    Err(e) => errors.push(e),
                }
            }
            Item::DefineMacro { .. } => {
                // WHY: Sprint 17 — macro definitions are collected and
                // removed by `nod_macro::expand_module` before lowering.
                // If one survives to here (direct `lower_module_full`
                // call without expansion) it is inert; no codegen needed.
            }
            Item::DefineCFunction {
                name,
                params,
                return_,
                c_name,
                library,
                span,
                ..
            } => {
                // Sprint 27 recorded the binding; Sprint 28 builds
                // the marshaling signature, picks a stub-table slot,
                // and registers a "synthetic top name" so call sites
                // can resolve `Beep(...)` to a WinFFI DirectCall.
                //
                // Validation: require `library:` to be present and
                // non-empty. Probe the embedded `nod-winapi` index
                // for the (DLL, c-name) pair; warn (not error) if
                // missing — user might be targeting a custom DLL
                // the DB doesn't know about.
                if library.is_empty() {
                    errors.push(LoweringError::Unsupported {
                        span: *span,
                        message: format!(
                            "`define c-function {name}`: missing required `library:` attribute"
                        ),
                    });
                    continue;
                }
                let effective_c_name = c_name.clone().unwrap_or_else(|| name.clone());
                let resolved =
                    nod_winapi::find_function(library, &effective_c_name).is_some();
                if !resolved {
                    warnings.push(LoweringWarning::CFunctionNotInDb {
                        span: *span,
                        name: name.clone(),
                        library: library.clone(),
                        c_name: effective_c_name.clone(),
                    });
                }
                // Sprint 28: derive the marshaling signature. Each
                // param's type annotation must be a `<c-…>` ident
                // that maps to a [`nod_runtime::CArgKind`]. Bail out
                // (signature = None) on any unknown type — call sites
                // then surface a deferral error per the Sprint 28
                // brief ("integer / pointer only").
                let mut arg_names: Vec<String> = Vec::with_capacity(params.len());
                let mut signature_ok = true;
                for p in params {
                    let n = match &p.type_ {
                        Some(Expr::Ident(_, n)) => n.clone(),
                        _ => {
                            signature_ok = false;
                            String::new()
                        }
                    };
                    arg_names.push(n);
                }
                let return_name: Option<String> = match return_ {
                    Some(rs) => {
                        if rs.values.len() > 1 {
                            // Multi-value c-function returns are not in
                            // Sprint 28 scope; fall through to
                            // signature = None.
                            signature_ok = false;
                            None
                        } else if let Some(v) = rs.values.first() {
                            match &v.type_ {
                                Some(Expr::Ident(_, n)) => Some(n.clone()),
                                _ => {
                                    signature_ok = false;
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                let signature: Option<nod_runtime::ApiCallSignature> = if signature_ok {
                    let arg_refs: Vec<&str> =
                        arg_names.iter().map(|s| s.as_str()).collect();
                    nod_runtime::signature_from_names(&arg_refs, return_name.as_deref())
                        .ok()
                } else {
                    None
                };
                c_functions.push(CFunctionBinding {
                    dylan_name: name.clone(),
                    c_name: effective_c_name,
                    library: library.clone(),
                    span: *span,
                    resolved_in_db: resolved,
                    signature,
                    source: BindingSource::UserCFunction,
                });
            }
            Item::DefineLibrary { .. } | Item::DefineModule { .. } => {}
            Item::DefineOther { span, keyword, .. } => {
                // `define [sealed] domain GF (types…);` is a sealing
                // declaration — advisory only, with no runtime lowering in
                // our model. Accept it as a no-op. Other unknown definers are
                // unexpanded macro calls and remain errors.
                if keyword == "domain" {
                    // no-op
                } else {
                    errors.push(LoweringError::Unsupported {
                        span: *span,
                        message: format!("`define {keyword}` not lowered in Sprint 06"),
                    });
                }
            }
            Item::Expr(_) => {}
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // Sprint 56c (CONSUME) — under the combined front-end flag
    // (`--frontend-with-dylan` ⇒ `NOD_FRONTEND_WITH_DYLAN=1`), build the
    // `methods` table FROM the Dylan lowering's `=== methods ===` section
    // instead of from the Rust AST walk above, then make it load-bearing for
    // the downstream dispatch pre-registration + `register_methods`. This MUST
    // run BEFORE the pre-registration loop below (which consumes `methods`),
    // and therefore well before the Sprint-55 dfm-dump seam.
    //
    // Behaviour-PRESERVING safety net: before replacing the table we assert the
    // Dylan-built one equals the Rust-built one FIELD BY FIELD
    // (`assert_methods_consume_equal`). A mismatch is a real reconstruction bug
    // (e.g. a body-fn id-form divergence) and fails the compile loudly. The
    // non-frontend `--lower-with-dylan` path is UNCHANGED here (it stays
    // verify-only at the seam below, using the Rust `methods` table).
    //
    // `methods` is fully built at this point (Phase 3 accessors + Phase 4 user
    // methods, in walk order), so the consume can replace it wholesale.
    let mut methods_consumed = false;
    if std::env::var("NOD_FRONTEND_WITH_DYLAN").as_deref() == Ok("1")
        && let Some(dump) = dfm_dump
        && !dump.trim().is_empty()
        && let (_, Some(methods_section)) = split_methods_section(dump)
    {
        let dylan_parsed = parse_dylan_methods(methods_section).map_err(|e| {
            vec![LoweringError::Unsupported {
                span: Span { file_id: nod_reader::FileId(0), lo: 0, hi: 0 },
                message: format!("frontend-with-dylan (methods-consume parse): {e}"),
            }]
        })?;
        let dylan_methods = build_methods_from_dylan(&dylan_parsed).map_err(|e| {
            vec![LoweringError::Unsupported {
                span: Span { file_id: nod_reader::FileId(0), lo: 0, hi: 0 },
                message: format!("frontend-with-dylan (methods-consume build): {e}"),
            }]
        })?;
        assert_methods_consume_equal(&methods, &dylan_methods).map_err(|e| {
            vec![LoweringError::Unsupported {
                span: Span { file_id: nod_reader::FileId(0), lo: 0, hi: 0 },
                message: format!("methods-consume mismatch: {e}"),
            }]
        })?;
        // Replace the Rust-built table with the Dylan-sourced one so the
        // dispatch pre-registration below + `register_methods` are fed from
        // the Dylan lowering.
        methods = dylan_methods;
        methods_consumed = true;
    }

    // Sprint 15 — sealing analysis + dispatch resolution. Runs BEFORE
    // the precise-roots post-pass so any Dispatch → DirectCall (or
    // SealedDirectCall) rewrite happens before liveness sees the
    // call-shaped nodes; rewriting preserves the safepoint-roots
    // discipline transparently because the new call-shaped nodes go
    // through the same `safepoint_roots_mut()` accessor.
    //
    // Pre-register every method's specialiser tuple in the runtime
    // dispatch table so the resolver can enumerate applicable methods.
    // The body pointer is null at this stage — the JIT installs the
    // real address later via `register_methods`. The resolver only
    // reads `specialisers`; the resolver-side symbol name is
    // recomputed from `(generic_name, specialisers)` independently.
    for reg in &methods {
        let g = nod_runtime::get_or_create_generic(&reg.generic_name);
        // Skip if a method with these specialisers is already registered
        // (a prior `lower_module_full` call may have done it).
        let already = g
            .methods
            .read()
            .expect("methods rwlock poisoned")
            .iter()
            .any(|m| m.specialisers == reg.specialisers);
        if !already {
            // Sprint 16: pre-register the JIT body symbol name so the
            // dispatch resolver picks up the actual emitted symbol
            // (slot accessors don't follow the canonical naming
            // convention).
            g.add_method(nod_runtime::Method {
                specialisers: reg.specialisers.clone(),
                body_fn_ptr: std::ptr::null(),
                param_count: reg.param_count,
                body_fn_name: reg.body_fn_name.clone(),
            });
        }
    }
    // Sprint 55 — the LOWERING flip seam. If a Dylan-produced DFM dump was
    // supplied (`--lower-with-dylan`), REPLACE the Phase-3/4 Rust functions
    // with the ones reconstructed from it; the back-end passes below then run
    // on the Dylan-produced DFM exactly as on the Rust one. Classes are already
    // registered (Phase 1 / `analyse_module[_from_dump]`), so the reconstruction
    // resolves class-name labels through the live registry. An empty dump means
    // the Dylan lowering bailed → keep the Rust output. The Dylan dump is a
    // COMPLETE module (accessors + functions), so drop any Rust-lifted thunks to
    // avoid duplicating them at the append below.
    if let Some(dump) = dfm_dump
        && !dump.trim().is_empty()
    {
        // Sprint 56c-T — the Dylan lowering appends a `=== methods ===` section
        // after the function dump. SPLIT it off at the literal `\n=== methods
        // ===\n` boundary so `parse_dfm_module` never sees it; the left part is
        // the unchanged DFM funcs dump. The right part (the methods table) is
        // parsed below and VERIFIED against the Rust `methods` table — NOT
        // consumed (the dump-dfm OUTPUT is unchanged; only the verify runs).
        let (funcs_dump, methods_dump): (&str, Option<&str>) = split_methods_section(dump);
        let parsed = nod_dfm::parse_dfm_module(funcs_dump, &|name| {
            resolve_class_id_by_name(name).map(|c| c.0)
        })
        .map_err(|e| {
            vec![LoweringError::Unsupported {
                span: Span { file_id: nod_reader::FileId(0), lo: 0, hi: 0 },
                message: format!("dfm reconstruct (--lower-with-dylan): {e}"),
            }]
        })?;
        out = parsed;
        lift_sink.functions.clear();

        // Verify the Dylan method table against the Rust `methods` table (built
        // above in WALK ORDER). A mismatch is a Dylan-vs-Rust method-derivation
        // bug; fail the compile loudly rather than silently trusting Rust.
        //
        // Sprint 56c (CONSUME): when the front-end flag already CONSUMED the
        // methods table above (`methods_consumed`), `methods` IS the Dylan-built
        // table and was asserted equal to the Rust one there — re-verifying here
        // is redundant, so skip it. This leaves the non-frontend
        // `--lower-with-dylan` verify-only path exactly as in 56c-T.
        if let Some(methods_section) = methods_dump
            && !methods_consumed
        {
            let dylan_methods = parse_dylan_methods(methods_section).map_err(|e| {
                vec![LoweringError::Unsupported {
                    span: Span { file_id: nod_reader::FileId(0), lo: 0, hi: 0 },
                    message: format!("lower-with-dylan: {e}"),
                }]
            })?;
            verify_dylan_methods(&methods, &dylan_methods).map_err(|e| {
                vec![LoweringError::Unsupported {
                    span: Span { file_id: nod_reader::FileId(0), lo: 0, hi: 0 },
                    message: format!("lower-with-dylan: {e}"),
                }]
            })?;
        }
    }

    // `sealing` was computed in `analyse_module`; install it here (the
    // historical point, before dispatch resolution) to preserve behavior.
    crate::optimise::install_sealing_facts(&sealing);
    let mut resolutions: Vec<crate::optimise::DispatchResolution> = Vec::new();
    for f in &mut out {
        let narrowed = crate::optimise::narrow_function(f);
        let mut log = crate::optimise::resolve_dispatches(f, &narrowed, &sealing);
        resolutions.append(&mut log);
    }

    // Sprint 11b — precise-roots post-pass. Compute the set of
    // Sprint 19: drain any lifted-block thunks into the function list
    // BEFORE the safepoint-roots post-pass runs so the lifted thunks
    // also receive safepoint-roots populated.
    out.append(&mut lift_sink.functions);
    let blocks = std::mem::take(&mut lift_sink.blocks);

    // pointer-shaped temps live across each potentially-allocating
    // call, and stash the list on the call's `safepoint_roots` field.
    // Codegen brackets the call with `nod_register_root` /
    // `nod_unregister_root` pairs so the GC can rewrite the slots if
    // it evacuates the objects mid-call.
    for f in &mut out {
        nod_dfm::populate_safepoint_roots(f);
    }

    // GAP-011 hypothesis probe (env-gated). Set
    // `NOD_DIAG_ARG_ROOT_COVERAGE=1` to enumerate every call site
    // where a GC-typed argument is NOT in `safepoint_roots`. The
    // theory under test: liveness correctly omits args that are
    // dead-after-call, but the value still flows in a register; a
    // callee that allocates before its first safepoint can let GC
    // move the object and leave the register stale. If the probe
    // lights up inside `dump-node` / `acc-string` / their call chain,
    // the fix is to extend `populate_safepoint_roots` to include all
    // heap-typed call arguments regardless of post-call liveness.
    // Set `NOD_DIAG_ARG_ROOT_COVERAGE=summary` to print only a
    // function-level count, or `=full` (or `=1`) for one line per gap.
    if let Ok(mode) = std::env::var("NOD_DIAG_ARG_ROOT_COVERAGE") {
        let want_full = matches!(mode.as_str(), "1" | "full" | "true");
        let mut total_gaps: usize = 0;
        let mut funcs_with_gaps: usize = 0;
        for f in &out {
            let gaps = nod_dfm::diagnose_arg_root_coverage(f);
            if gaps.is_empty() {
                continue;
            }
            funcs_with_gaps += 1;
            total_gaps += gaps.len();
            eprintln!(
                "[ARG-ROOT-COV] fn={} gaps={}",
                f.name,
                gaps.len()
            );
            if want_full {
                for g in &gaps {
                    eprintln!(
                        "[ARG-ROOT-COV]   block=b{} c[{}] callee={} \
                         dst=t{} arg=t{} arg_pos={} arg_type={:?}",
                        g.block.0,
                        g.computation_index,
                        g.callee_label,
                        g.call_dst.0,
                        g.gc_typed_arg.0,
                        g.arg_position,
                        g.arg_type,
                    );
                }
            }
        }
        eprintln!(
            "[ARG-ROOT-COV] TOTAL functions_with_gaps={} gaps={}",
            funcs_with_gaps, total_gaps,
        );
    }

    // Sprint 28: scan the AST for any call expression whose callee
    // is the name of a `define c-function` WHOSE signature couldn't
    // be derived (unsupported c-type, multi-value return, etc.). The
    // happy path (a supported signature) is fully lowered inside
    // `lower_call`. Names with `signature: None` aren't in
    // `c_function_call_map`, so their call sites would otherwise
    // silently fall through to "unknown ident — DirectCall against
    // the bare name"; we surface a deferral diagnostic instead.
    let unsupported_c_names: HashSet<String> = c_functions
        .iter()
        .filter(|c| c.signature.is_none() && c.source == BindingSource::UserCFunction)
        .map(|c| c.dylan_name.clone())
        .collect();
    if !unsupported_c_names.is_empty() {
        let mut call_site_errors: Vec<LoweringError> = Vec::new();
        scan_module_for_c_function_calls(m, &unsupported_c_names, &mut call_site_errors);
        if !call_site_errors.is_empty() {
            return Err(call_site_errors);
        }
    }

    // Sprint 31: bare-name calls whose Win32 entry exists in the index
    // BUT whose signature uses unsupported types (struct-by-value,
    // function-pointer callback, …) decline materialization and would
    // otherwise fall through to "unknown identifier" — surface a more
    // informative error so the user knows the API exists but isn't
    // yet wired up.
    if !materialized_unsupported.is_empty() {
        let mut call_site_errors: Vec<LoweringError> = Vec::new();
        scan_module_for_materialized_unsupported(
            m,
            &materialized_unsupported,
            &mut call_site_errors,
        );
        if !call_site_errors.is_empty() {
            return Err(call_site_errors);
        }
    }
    let _ = materialized_call_names;

    // Sprint 54a — the recording outputs (`top_names` / `generics` / classes
    // / sealing) came from `analyse_module`'s `SemaModel`; snapshot them onto
    // the `LoweredModule` for `dump-sema` / the wire byte-match.
    Ok(LoweredModule {
        functions: out,
        methods,
        sealing,
        resolutions,
        blocks,
        closures: closure_registry,
        c_functions,
        c_function_stub_table: c_function_stub_table_entries,
        warnings,
        user_classes: user_class_registrations,
        variables: variable_registrations,
        top_names: top_names.clone(),
        generics: generics_for_dump,
    })
}

/// Resolve a `ClassId` to its registered class name, or a stable
/// placeholder. Used by `format_sema_model` so the dump references
/// classes by NAME (the portable invariant) rather than numeric id —
/// ids are assigned globally and won't match across the Rust and Dylan
/// sema implementations, but names + slot offsets + CPL order do.
fn sema_class_name(id: ClassId) -> String {
    let p = nod_runtime::class_metadata_ptr(id);
    if p.is_null() {
        format!("#<class {}>", id.0)
    } else {
        // SAFETY: static-area metadata, process-lived.
        unsafe { (*p).name.clone() }
    }
}

/// Sprint 53 — serialise the sema recording model (`SemaModel`:
/// top-names, generics, classes, sealing) to deterministic, stable text.
/// This is the `dump-sema` oracle: the Dylan-computed model must
/// byte-match this. Tables are sorted; classes are in registration
/// order; class references are by name (see `sema_class_name`).
pub fn format_sema_model(model: &SemaModel) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();

    s.push_str("=== top-names ===\n");
    // Sprint 53.2 — `define constant` / `define variable` names are
    // ALSO recorded in `fns` (with arity 0) because codegen lowers them
    // as zero-arg getter functions and `Expr::Ident` resolution consults
    // `top_names.contains()` / `.arity()` (see `collect_top_level_names`
    // + the GAP-002 note in `lower_expr`). But in the SEMA MODEL they are
    // classification-wise constants / variables, not functions: the
    // Dylan-side `collect-top-names` walk emits only `constant <name>` /
    // `variable <name>` lines for them, no `fn` line. Filter them out of
    // the `fns` listing here so the dump byte-matches the Dylan walk. The
    // `fns` recording itself is untouched (still load-bearing for codegen).
    let mut fns: Vec<(&String, &TypeEstimate)> = model
        .top_names
        .fns
        .iter()
        .filter(|(name, _)| {
            !model.top_names.constants.contains(*name) && !model.top_names.variables.contains(*name)
        })
        .collect();
    fns.sort_by(|a, b| a.0.cmp(b.0));
    for (name, est) in fns {
        let arity = model.top_names.fn_arity.get(name).copied().unwrap_or(0);
        // Render the return estimate. A `Class` estimate prints the class by
        // NAME, not the raw process-global id: every other class reference in
        // this dump already goes through `sema_class_name` (parents / cpl /
        // slot origin / sealing) precisely because ids are process-global
        // (53.1). A raw `Class(<id>)` here made the dump non-deterministic
        // across builds that register classes in a different order, and gave
        // the Dylan-side walk an id it has no way to reproduce. By name, both
        // sides agree. Other estimates keep their `TypeEstimate` Debug name.
        let ret = match est {
            TypeEstimate::Class(id) => format!("Class({})", sema_class_name(ClassId(*id))),
            other => format!("{other:?}"),
        };
        // Sprint 60: a function declared with `#rest` carries a trailing
        // ` rest=FIXED` token (FIXED = pre-`#rest` param count) so the
        // dump round-trip preserves the call-site collection metadata.
        match model.top_names.rest_fns.get(name) {
            Some(fixed) => {
                let _ = writeln!(s, "fn {name} arity={arity} return={ret} rest={fixed}");
            }
            None => {
                let _ = writeln!(s, "fn {name} arity={arity} return={ret}");
            }
        }
    }
    let mut consts: Vec<&String> = model.top_names.constants.iter().collect();
    consts.sort();
    for c in consts {
        let _ = writeln!(s, "constant {c}");
    }
    let mut vars: Vec<&String> = model.top_names.variables.iter().collect();
    vars.sort();
    for v in vars {
        let _ = writeln!(s, "variable {v}");
    }

    s.push_str("=== generics ===\n");
    for g in &model.generics {
        let _ = writeln!(s, "generic {g}");
    }

    s.push_str("=== classes ===\n");
    for c in &model.classes {
        let _ = writeln!(s, "class {}", c.name);
        let parents: Vec<String> = c.parents.iter().map(|&id| sema_class_name(id)).collect();
        let _ = writeln!(s, "  parents [{}]", parents.join(", "));
        let cpl: Vec<String> = c.cpl.iter().map(|&id| sema_class_name(id)).collect();
        let _ = writeln!(s, "  cpl [{}]", cpl.join(", "));
        for (i, slot) in c.slots.iter().enumerate() {
            let origin = sema_class_name(c.slot_origin[i]);
            // Sprint 56a-WIRE — the four previously-lossy SlotInfo fields the
            // class consume (`nod_make` / GC / AOT) needs, appended after the
            // original four tokens. `type` is the canonical SlotType label
            // (class slots BY NAME via `sema_class_name`, never a numeric id —
            // 53.5e id-nondeterminism); `init-keyword` is the keyword string or
            // `-`; `required` is the required-init-keyword flag; `default` is
            // the GAP-009 tag the AOT serializer uses (lib.rs:1355-1360).
            let _ = writeln!(
                s,
                "  slot {} @{} setter={} origin={} type={} init-keyword={} required={} default={}",
                slot.name,
                slot.offset,
                slot.has_setter,
                origin,
                slot_type_label(slot.type_kind),
                slot.init_keyword.as_deref().unwrap_or("-"),
                slot.required_init_keyword,
                slot_default_tag(slot.default_init),
            );
        }
    }

    s.push_str("=== sealing ===\n");
    let mut sc: Vec<&String> = model.sealing.sealed_classes.iter().collect();
    sc.sort();
    for c in sc {
        let _ = writeln!(s, "sealed-class {c}");
    }
    let mut sg: Vec<&String> = model.sealing.sealed_generics.iter().collect();
    sg.sort();
    for g in sg {
        let _ = writeln!(s, "sealed-generic {g}");
    }
    let mut doms: Vec<(&String, &Vec<Vec<ClassId>>)> = model.sealing.domains.iter().collect();
    doms.sort_by(|a, b| a.0.cmp(b.0));
    for (g, tuples) in doms {
        for t in tuples {
            let names: Vec<String> = t.iter().map(|&id| sema_class_name(id)).collect();
            let _ = writeln!(s, "sealed-domain {g} ({})", names.join(", "));
        }
    }

    s
}

/// Sprint 54c — inverse of [`format_sema_model`] for the name-keyed sections:
/// parse a `dump-sema` model dump back into `(TopNames, generics,
/// SealingFacts)`. This is how the host reconstructs the recording the Dylan
/// walk produced (via the `dylan-sema-emit` shim, text transport) so the
/// back-end can consume it under `--sema-with-dylan`.
///
/// Classes are NOT reconstructed here — the host registers them from the AST
/// (a runtime mechanism that assigns `ClassId`s) and supplies the
/// `classes` vector separately; this recovers only what crosses by name.
/// Class references inside the recording (the `Class(<name>)` return estimate
/// and `sealed-domain` specialisers) are resolved to `ClassId`s through the
/// registered class table, so the caller MUST register the module's classes
/// before calling (see [`analyse_module_from_dump`]).
pub fn parse_sema_dump(
    dump: &str,
) -> Result<(TopNames, Vec<String>, crate::optimise::SealingFacts), String> {
    let mut fns: HashMap<String, TypeEstimate> = HashMap::new();
    let mut fn_arity: HashMap<String, usize> = HashMap::new();
    let mut constants: HashSet<String> = HashSet::new();
    let mut variables: HashSet<String> = HashSet::new();
    let mut rest_fns: HashMap<String, usize> = HashMap::new();
    let mut generics: Vec<String> = Vec::new();
    let mut sealed_classes: HashSet<String> = HashSet::new();
    let mut sealed_generics: HashSet<String> = HashSet::new();
    let mut domains: HashMap<String, Vec<Vec<ClassId>>> = HashMap::new();

    for raw in dump.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("fn ") {
            // `fn NAME arity=N return=EST` — names never contain " arity=".
            let name_end = rest
                .find(" arity=")
                .ok_or_else(|| format!("malformed fn line: {line}"))?;
            let name = rest[..name_end].to_string();
            let after = &rest[name_end + " arity=".len()..];
            let ret_at = after
                .find(" return=")
                .ok_or_else(|| format!("malformed fn line: {line}"))?;
            let arity: usize = after[..ret_at]
                .parse()
                .map_err(|_| format!("bad arity in: {line}"))?;
            let mut est_str = &after[ret_at + " return=".len()..];
            // Sprint 60: optional trailing ` rest=FIXED` token (see
            // `format_sema_model`). Split it off the return estimate.
            if let Some(rest_at) = est_str.find(" rest=") {
                let fixed_str = &est_str[rest_at + " rest=".len()..];
                let fixed: usize = fixed_str
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad rest= in: {line}"))?;
                rest_fns.insert(name.clone(), fixed);
                est_str = &est_str[..rest_at];
            }
            fns.insert(name.clone(), est_from_dump(est_str));
            fn_arity.insert(name, arity);
        } else if let Some(name) = line.strip_prefix("constant ") {
            // GAP-002 rule: `define constant` / `variable` ALSO live in `fns`
            // (arity 0), but `format_sema_model` filters them out of the `fn`
            // listing. Re-apply the rule so the reconstructed `TopNames`
            // matches `collect_top_level_names`' population exactly.
            constants.insert(name.to_string());
            fns.entry(name.to_string()).or_insert(TypeEstimate::Top);
            fn_arity.entry(name.to_string()).or_insert(0);
        } else if let Some(name) = line.strip_prefix("variable ") {
            variables.insert(name.to_string());
            fns.entry(name.to_string()).or_insert(TypeEstimate::Top);
            fn_arity.entry(name.to_string()).or_insert(0);
        } else if let Some(name) = line.strip_prefix("generic ") {
            generics.push(name.to_string());
        } else if let Some(name) = line.strip_prefix("sealed-class ") {
            sealed_classes.insert(name.to_string());
        } else if let Some(name) = line.strip_prefix("sealed-generic ") {
            sealed_generics.insert(name.to_string());
        } else if let Some(rest) = line.strip_prefix("sealed-domain ") {
            // `sealed-domain G (T1, T2, …)` — resolve specialiser names to ids.
            let paren = rest
                .find(" (")
                .ok_or_else(|| format!("malformed sealed-domain: {line}"))?;
            let g = rest[..paren].to_string();
            let inner = rest[paren + 2..].trim_end_matches(')');
            let tuple: Vec<ClassId> = inner
                .split(", ")
                .filter(|s| !s.is_empty())
                .map(|n| resolve_class_id_by_name(n).unwrap_or(ClassId(0)))
                .collect();
            domains.entry(g).or_default().push(tuple);
        }
        // `=== … ===` headers and `class` / `  parents` / `  cpl` / `  slot`
        // lines are ignored — classes come from the host's registration.
    }

    let top_names = TopNames {
        fns,
        fn_arity,
        constants,
        variables,
        rest_fns,
    };
    let sealing = crate::optimise::SealingFacts {
        domains,
        sealed_generics,
        sealed_classes,
    };
    Ok((top_names, generics, sealing))
}

/// Sprint 56a — a parsed `=== classes ===` record from a `dump-sema` model
/// dump (the Dylan sema walk's class derivation). Carries exactly what
/// [`format_sema_model`] prints per class: the name, the direct parents (by
/// name), the C3 CPL (by name, self first), and the slot layout (name /
/// offset / setter / origin class, all by name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSemaClass {
    pub name: String,
    pub parents: Vec<String>,
    pub cpl: Vec<String>,
    pub slots: Vec<ParsedSemaSlot>,
}

/// Sprint 56a — one slot of a [`ParsedSemaClass`]: `slot NAME @OFFSET
/// setter=BOOL origin=ORIGIN type=<KIND> init-keyword=<KW|-> required=<BOOL>
/// default=<TAG>`. Sprint 56a-WIRE grew the four trailing fields so the dump
/// carries everything `SlotInfo` does (type-kind / init-keyword /
/// required-init-keyword / default-init), the precondition for installing
/// classes from the Dylan records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSemaSlot {
    pub name: String,
    pub offset: usize,
    pub has_setter: bool,
    pub origin: String,
    /// Canonical SlotType label (class slots BY NAME); see `slot_type_label`.
    pub type_kind: String,
    /// The init-keyword string, or `None` when the dump printed `-`.
    pub init_keyword: Option<String>,
    pub required_init_keyword: bool,
    /// GAP-009 default-init tag text; see `slot_default_tag`.
    pub default_tag: String,
}

/// Sprint 56a — parse the `=== classes ===` section of a `dump-sema` model
/// dump into [`ParsedSemaClass`] records, in dump (declaration) order. The
/// inverse of the classes block of [`format_sema_model`].
///
/// Unlike [`parse_sema_dump`] — which deliberately ignores classes because the
/// host registers them from the AST — this recovers the Dylan-derived
/// derivation so [`analyse_module_from_dump`] can VERIFY it against the host
/// registration on the load-bearing path. Lines outside `=== classes ===` are
/// ignored; `Err` on a malformed `class` / `slot` line.
pub fn parse_sema_classes(dump: &str) -> Result<Vec<ParsedSemaClass>, String> {
    fn split_name_list(inner: &str) -> Vec<String> {
        inner
            .split(", ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    let mut classes: Vec<ParsedSemaClass> = Vec::new();
    let mut cur: Option<ParsedSemaClass> = None;
    let mut in_classes = false;
    for raw in dump.lines() {
        let line = raw.trim_end();
        if line == "=== classes ===" {
            in_classes = true;
            continue;
        }
        if line.starts_with("=== ") {
            // Any other section header closes the classes block.
            if let Some(c) = cur.take() {
                classes.push(c);
            }
            in_classes = false;
            continue;
        }
        if !in_classes {
            continue;
        }
        if let Some(name) = line.strip_prefix("class ") {
            if let Some(c) = cur.take() {
                classes.push(c);
            }
            cur = Some(ParsedSemaClass {
                name: name.to_string(),
                parents: Vec::new(),
                cpl: Vec::new(),
                slots: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("  parents [") {
            let c = cur
                .as_mut()
                .ok_or_else(|| format!("sema dump: `parents` before any `class`: {line}"))?;
            c.parents = split_name_list(rest.trim_end_matches(']'));
        } else if let Some(rest) = line.strip_prefix("  cpl [") {
            let c = cur
                .as_mut()
                .ok_or_else(|| format!("sema dump: `cpl` before any `class`: {line}"))?;
            c.cpl = split_name_list(rest.trim_end_matches(']'));
        } else if let Some(rest) = line.strip_prefix("  slot ") {
            let c = cur
                .as_mut()
                .ok_or_else(|| format!("sema dump: `slot` before any `class`: {line}"))?;
            c.slots.push(parse_sema_slot_line(rest)?);
        }
    }
    if let Some(c) = cur.take() {
        classes.push(c);
    }
    Ok(classes)
}

/// Parse the tail of a `  slot ` line: `NAME @OFFSET setter=BOOL origin=ORIGIN
/// type=<KIND> init-keyword=<KW|-> required=<BOOL> default=<TAG>` (slot names,
/// class names, type labels and the default tag are single tokens — no spaces).
/// Sprint 56a-WIRE — the inverse of the widened slot line in `format_sema_model`.
fn parse_sema_slot_line(rest: &str) -> Result<ParsedSemaSlot, String> {
    let err = || format!("sema dump: malformed slot line: `slot {rest}`");
    let at = rest.find(" @").ok_or_else(err)?;
    let name = rest[..at].to_string();
    let after = &rest[at + 2..]; // OFFSET setter=… origin=… type=… …
    let sp = after.find(' ').ok_or_else(err)?;
    let offset: usize = after[..sp].parse().map_err(|_| err())?;
    let after = after[sp + 1..].strip_prefix("setter=").ok_or_else(err)?;
    let sp = after.find(' ').ok_or_else(err)?;
    let has_setter = match &after[..sp] {
        "true" => true,
        "false" => false,
        _ => return Err(err()),
    };
    let after = after[sp + 1..].strip_prefix("origin=").ok_or_else(err)?;
    let sp = after.find(' ').ok_or_else(err)?;
    let origin = after[..sp].to_string();
    let after = after[sp + 1..].strip_prefix("type=").ok_or_else(err)?;
    let sp = after.find(' ').ok_or_else(err)?;
    let type_kind = after[..sp].to_string();
    let after = after[sp + 1..].strip_prefix("init-keyword=").ok_or_else(err)?;
    let sp = after.find(' ').ok_or_else(err)?;
    let init_keyword = match &after[..sp] {
        "-" => None,
        kw => Some(kw.to_string()),
    };
    let after = after[sp + 1..].strip_prefix("required=").ok_or_else(err)?;
    let sp = after.find(' ').ok_or_else(err)?;
    let required_init_keyword = match &after[..sp] {
        "true" => true,
        "false" => false,
        _ => return Err(err()),
    };
    let default_tag = after[sp + 1..].strip_prefix("default=").ok_or_else(err)?.to_string();
    Ok(ParsedSemaSlot {
        name,
        offset,
        has_setter,
        origin,
        type_kind,
        init_keyword,
        required_init_keyword,
        default_tag,
    })
}

/// Sprint 56a — verify that the Dylan sema walk's class derivation (the dump's
/// `=== classes ===` section, [`parse_sema_classes`]) structurally matches the
/// host's `register_module_classes` output, comparing everything by NAME:
/// declaration order + name, direct parents, the C3 CPL, and the slot layout
/// (name / offset / setter / origin). This promotes the offline 53.3/53.4
/// byte-match oracle to a LIVE invariant on the `--sema-with-dylan` path: if
/// the Dylan and Rust class derivations ever diverge, the compile fails loudly
/// here rather than silently trusting Rust. The host still allocates the
/// `ClassId`s — this checks the derivation, the precondition for retiring the
/// Rust class derivation (Sprint 56b+).
fn verify_dylan_classes(
    rust: &[UserClassRegistration],
    dylan: &[ParsedSemaClass],
) -> Result<(), String> {
    if rust.len() != dylan.len() {
        return Err(format!(
            "class count mismatch — host registered {}, Dylan dumped {}",
            rust.len(),
            dylan.len()
        ));
    }
    for (r, d) in rust.iter().zip(dylan.iter()) {
        if r.name != d.name {
            return Err(format!(
                "class order/name mismatch — host {:?}, Dylan {:?}",
                r.name, d.name
            ));
        }
        let r_parents: Vec<String> = r.parents.iter().map(|&id| sema_class_name(id)).collect();
        if r_parents != d.parents {
            return Err(format!(
                "class {} parents mismatch — host {:?}, Dylan {:?}",
                r.name, r_parents, d.parents
            ));
        }
        let r_cpl: Vec<String> = r.cpl.iter().map(|&id| sema_class_name(id)).collect();
        if r_cpl != d.cpl {
            return Err(format!(
                "class {} cpl mismatch — host {:?}, Dylan {:?}",
                r.name, r_cpl, d.cpl
            ));
        }
        if r.slots.len() != d.slots.len() {
            return Err(format!(
                "class {} slot count mismatch — host {}, Dylan {}",
                r.name,
                r.slots.len(),
                d.slots.len()
            ));
        }
        for (i, (rs, ds)) in r.slots.iter().zip(d.slots.iter()).enumerate() {
            let r_origin = sema_class_name(r.slot_origin[i]);
            // Sprint 56a-WIRE — the four grown fields are checked through the
            // SAME label / tag mapping the emitter uses, so a Dylan-vs-Rust
            // divergence in type-kind / init-keyword / required / default fails
            // loudly on the live `--sema-with-dylan` path, not just in bytes.
            let r_type = slot_type_label(rs.type_kind);
            let r_init_kw = rs.init_keyword.as_deref();
            let d_init_kw = ds.init_keyword.as_deref();
            let r_default = slot_default_tag(rs.default_init);
            if rs.name != ds.name
                || rs.offset != ds.offset
                || rs.has_setter != ds.has_setter
                || r_origin != ds.origin
                || r_type != ds.type_kind
                || r_init_kw != d_init_kw
                || rs.required_init_keyword != ds.required_init_keyword
                || r_default != ds.default_tag
            {
                return Err(format!(
                    "class {} slot[{i}] mismatch — host (name={}, @{}, setter={}, origin={}, \
                     type={}, init-keyword={:?}, required={}, default={}), \
                     Dylan (name={}, @{}, setter={}, origin={}, type={}, init-keyword={:?}, \
                     required={}, default={})",
                    r.name, rs.name, rs.offset, rs.has_setter, r_origin,
                    r_type, r_init_kw, rs.required_init_keyword, r_default,
                    ds.name, ds.offset, ds.has_setter, ds.origin, ds.type_kind,
                    d_init_kw, ds.required_init_keyword, ds.default_tag
                ));
            }
        }
    }
    Ok(())
}

/// Sprint 56c-T — a parsed `=== methods ===` record from a `--lower-with-dylan`
/// DFM dump (the Dylan lowering walk's method table). Mirrors one Rust
/// [`MethodRegistration`]: generic name, body-fn name, param count, and the
/// specialiser class NAMES (one per required param, `<object>` for unannotated /
/// the setter's value position). Carries everything BY NAME — the Rust `ClassId`s
/// are resolved through `sema_class_name` at verify time, so `ClassId`
/// correctness rides the 56a class byte-match, not this comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedMethod {
    pub generic_name: String,
    pub body_fn_name: String,
    pub param_count: usize,
    pub specialisers: Vec<String>,
}

/// Sprint 56c-T — the literal boundary the Dylan lowering inserts between the
/// function dump and the `=== methods ===` table. Splitting a `--lower-with-dylan`
/// DFM dump here yields `(funcs_dump, Some(methods_section))`, or `(dump, None)`
/// when the lowering emitted no methods section.
const METHODS_SEP: &str = "\n=== methods ===\n";

/// Split a `--lower-with-dylan` DFM dump at [`METHODS_SEP`] into the function
/// dump (left, fed to `parse_dfm_module`) and the optional `=== methods ===`
/// section (right, fed to [`parse_dylan_methods`]).
fn split_methods_section(dump: &str) -> (&str, Option<&str>) {
    match dump.split_once(METHODS_SEP) {
        Some((funcs, methods)) => (funcs, Some(methods)),
        None => (dump, None),
    }
}

/// Sprint 56c-T — parse the `=== methods ===` section emitted by the Dylan
/// lowering (`dylan-lower-emit`, appended after the function dump) into
/// [`ParsedMethod`] records, in dump (walk) order. Each line is:
/// `method GENERIC body=BODY params=N specialisers=[<a>, <b>, ...]`.
/// The section is the tail produced by splitting the lowering dump at the
/// literal `\n=== methods ===\n`; this parser receives that tail (which begins
/// with the `=== methods ===` header line or the lines after it). Lines before
/// the header are ignored; `Err` on a malformed `method` line.
pub fn parse_dylan_methods(section: &str) -> Result<Vec<ParsedMethod>, String> {
    let mut methods: Vec<ParsedMethod> = Vec::new();
    let mut in_methods = false;
    for raw in section.lines() {
        let line = raw.trim_end();
        if line == "=== methods ===" {
            in_methods = true;
            continue;
        }
        if line.starts_with("=== ") {
            in_methods = false;
            continue;
        }
        if !in_methods {
            // Tolerate a tail that omits the header (the host passes the
            // post-delimiter slice, whose first line is the first `method`).
            if line.starts_with("method ") {
                in_methods = true;
            } else {
                continue;
            }
        }
        if line.is_empty() {
            continue;
        }
        methods.push(parse_dylan_method_line(line)?);
    }
    Ok(methods)
}

/// Parse one `method GENERIC body=BODY params=N specialisers=[<a>, <b>, ...]`
/// line. Generic / body names are identifiers (no spaces); the specialiser list
/// is a comma-space-joined bracketed list of class names.
fn parse_dylan_method_line(line: &str) -> Result<ParsedMethod, String> {
    let err = || format!("methods dump: malformed method line: `{line}`");
    let rest = line.strip_prefix("method ").ok_or_else(err)?;
    // GENERIC ends at " body=".
    let bpos = rest.find(" body=").ok_or_else(err)?;
    let generic_name = rest[..bpos].to_string();
    let after = &rest[bpos + " body=".len()..]; // BODY params=N specialisers=[...]
    let ppos = after.find(" params=").ok_or_else(err)?;
    let body_fn_name = after[..ppos].to_string();
    let after = &after[ppos + " params=".len()..]; // N specialisers=[...]
    let spos = after.find(" specialisers=[").ok_or_else(err)?;
    let param_count: usize = after[..spos].parse().map_err(|_| err())?;
    let after = &after[spos + " specialisers=[".len()..]; // <a>, <b>, ...]
    let inner = after.strip_suffix(']').ok_or_else(err)?;
    let specialisers: Vec<String> = inner
        .split(", ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Ok(ParsedMethod {
        generic_name,
        body_fn_name,
        param_count,
        specialisers,
    })
}

/// The expected Dylan-side body-fn name for a Rust [`MethodRegistration`],
/// resolving any numeric class-id suffix to class NAMES so it matches the
/// by-name form the Dylan lowering emits.
///
/// A USER method's Rust `body_fn_name` is `format!("{generic}${id1}_{id2}…")`
/// (lower.rs `lower_method_item`) — the specialiser `ClassId`s rendered numeric.
/// The Dylan side can't know the ids at lowering time, so it emits the same
/// `{generic}$…` shape with class NAMES (`method-body-name`, dylan-lower.dylan),
/// and `parse_function_header` resolves the suffix back to numbers at the
/// reconstruction seam. To compare the two method TABLES we therefore canonicalise
/// the Rust user-method body name to its by-name form (`{generic}$` + the
/// specialiser names joined by `_`). A slot ACCESSOR body name (`<C>-getter-x` /
/// `<C>-setter-x`) carries no such suffix and is returned unchanged — it already
/// byte-matches the Dylan accessor name.
fn expected_dylan_body_fn_name(r: &MethodRegistration, r_specs: &[String]) -> String {
    let user_method_prefix = format!("{}$", r.generic_name);
    if r.body_fn_name.starts_with(&user_method_prefix) {
        format!("{}{}", user_method_prefix, r_specs.join("_"))
    } else {
        r.body_fn_name.clone()
    }
}

/// Sprint 56c-T — verify that the Dylan lowering walk's method table (the dump's
/// `=== methods ===` section, [`parse_dylan_methods`]) structurally matches the
/// host's Rust [`MethodRegistration`] table built in `lower_module_full_inner`,
/// comparing by NAME: same count, same WALK ORDER, per-method equal
/// `generic_name` / `body_fn_name` / `param_count`, and specialisers compared BY
/// NAME (the Rust `ClassId`s resolved through [`sema_class_name`]). The
/// `body_fn_name` is compared in its by-name canonical form (see
/// [`expected_dylan_body_fn_name`]) so a user method's numeric class-id suffix
/// (`run-task$1082_1`) matches the Dylan by-name suffix (`run-task$<idler>_<integer>`).
/// This is the 56c-T verify-only step — the method table is still Rust-computed;
/// a Dylan-vs-Rust divergence fails the compile loudly here rather than being
/// silently masked. `ClassId` correctness itself rides the 56a class byte-match.
fn verify_dylan_methods(
    rust: &[MethodRegistration],
    dylan: &[ParsedMethod],
) -> Result<(), String> {
    if rust.len() != dylan.len() {
        return Err(format!(
            "method count mismatch — host built {}, Dylan dumped {}",
            rust.len(),
            dylan.len()
        ));
    }
    for (r, d) in rust.iter().zip(dylan.iter()) {
        if r.generic_name != d.generic_name {
            return Err(format!(
                "method order/generic mismatch — host {:?}, Dylan {:?}",
                r.generic_name, d.generic_name
            ));
        }
        if r.param_count != d.param_count {
            return Err(format!(
                "method {} param-count mismatch — host {}, Dylan {}",
                r.generic_name, r.param_count, d.param_count
            ));
        }
        let r_specs: Vec<String> = r.specialisers.iter().map(|&id| sema_class_name(id)).collect();
        if r_specs != d.specialisers {
            return Err(format!(
                "method {} specialisers mismatch — host {:?}, Dylan {:?}",
                r.generic_name, r_specs, d.specialisers
            ));
        }
        let r_body = expected_dylan_body_fn_name(r, &r_specs);
        if r_body != d.body_fn_name {
            return Err(format!(
                "method {} body-fn mismatch — host {:?} (raw {:?}), Dylan {:?}",
                r.generic_name, r_body, r.body_fn_name, d.body_fn_name
            ));
        }
    }
    Ok(())
}

/// Sprint 56c (CONSUME) — build the host's `Vec<MethodRegistration>` table FROM
/// the Dylan lowering's `=== methods ===` section ([`ParsedMethod`]s) instead of
/// from the Rust AST walk. This is the consume counterpart to the 56c-T
/// [`verify_dylan_methods`]: behaviour-preserving (the caller asserts the result
/// equals the Rust-built table field-by-field before using it), it proves the
/// reconstruction is correct ahead of the later skip-Phase-3/4 work.
///
/// For each [`ParsedMethod`]:
///   * `specialisers` — each specialiser NAME resolved through
///     [`resolve_class_id_by_name`] (classes are registered by this point;
///     an unknown name is an `Err`).
///   * `body_fn_name` — the FORWARD of [`expected_dylan_body_fn_name`]. A USER
///     method's `body_fn_name` arrives by-name (`run-task$<idler>_<integer>`,
///     starting with `<generic>$`); reconstruct the NUMERIC-id form
///     (`run-task$1082_1`) that matches the reconstructed function names by
///     joining the resolved specialiser ids with `_`. A slot ACCESSOR body name
///     (no `$`) is kept as-is — it already byte-matches.
///   * `generic_name` / `param_count` — copied through.
fn build_methods_from_dylan(
    parsed: &[ParsedMethod],
) -> Result<Vec<MethodRegistration>, String> {
    let mut out: Vec<MethodRegistration> = Vec::with_capacity(parsed.len());
    for pm in parsed {
        let mut spec_ids: Vec<ClassId> = Vec::with_capacity(pm.specialisers.len());
        for name in &pm.specialisers {
            let id = resolve_class_id_by_name(name).ok_or_else(|| {
                format!(
                    "methods-consume: unknown specialiser class `{name}` for method `{}`",
                    pm.generic_name
                )
            })?;
            spec_ids.push(id);
        }
        let user_method_prefix = format!("{}$", pm.generic_name);
        let body_fn_name = if pm.body_fn_name.starts_with(&user_method_prefix) {
            format!(
                "{}{}",
                user_method_prefix,
                spec_ids
                    .iter()
                    .map(|c| c.0.to_string())
                    .collect::<Vec<_>>()
                    .join("_")
            )
        } else {
            pm.body_fn_name.clone()
        };
        out.push(MethodRegistration {
            generic_name: pm.generic_name.clone(),
            specialisers: spec_ids,
            body_fn_name,
            param_count: pm.param_count,
        });
    }
    Ok(out)
}

/// Sprint 56c (CONSUME) — assert the Dylan-built method table
/// ([`build_methods_from_dylan`]) equals the Rust AST-built one FIELD BY FIELD.
/// The safety net that keeps the consume behaviour-preserving: same count, then
/// per-method equal `generic_name`, `specialisers` (`Vec<ClassId>` by `==`),
/// `body_fn_name`, and `param_count`. A mismatch is a real reconstruction bug.
fn assert_methods_consume_equal(
    rust: &[MethodRegistration],
    dylan: &[MethodRegistration],
) -> Result<(), String> {
    if rust.len() != dylan.len() {
        return Err(format!(
            "method count mismatch — host built {}, Dylan built {}",
            rust.len(),
            dylan.len()
        ));
    }
    for (r, d) in rust.iter().zip(dylan.iter()) {
        if r.generic_name != d.generic_name {
            return Err(format!(
                "method order/generic mismatch — host {:?}, Dylan {:?}",
                r.generic_name, d.generic_name
            ));
        }
        if r.specialisers != d.specialisers {
            return Err(format!(
                "method {} specialisers mismatch — host {:?}, Dylan {:?}",
                r.generic_name,
                r.specialisers.iter().map(|c| c.0).collect::<Vec<_>>(),
                d.specialisers.iter().map(|c| c.0).collect::<Vec<_>>()
            ));
        }
        if r.body_fn_name != d.body_fn_name {
            return Err(format!(
                "method {} body-fn mismatch — host {:?}, Dylan {:?}",
                r.generic_name, r.body_fn_name, d.body_fn_name
            ));
        }
        if r.param_count != d.param_count {
            return Err(format!(
                "method {} param-count mismatch — host {}, Dylan {}",
                r.generic_name, r.param_count, d.param_count
            ));
        }
    }
    Ok(())
}

/// Map a `TypeEstimate` Debug name (as `format_sema_model` emits) back to the
/// estimate. `Class(<name>)` resolves the class name to its `ClassId` via the
/// registered table (caller must have registered classes first); an
/// unresolvable / unknown estimate degrades to `Top` (informational only —
/// codegen lowers a tagged Word regardless).
fn est_from_dump(s: &str) -> TypeEstimate {
    match s {
        "Top" => TypeEstimate::Top,
        "Bottom" => TypeEstimate::Bottom,
        "Integer" => TypeEstimate::Integer,
        "SingleFloat" => TypeEstimate::SingleFloat,
        "DoubleFloat" => TypeEstimate::DoubleFloat,
        "Character" => TypeEstimate::Character,
        "Boolean" => TypeEstimate::Boolean,
        "String" => TypeEstimate::String,
        "Unit" => TypeEstimate::Unit,
        other => other
            .strip_prefix("Class(")
            .and_then(|x| x.strip_suffix(')'))
            .and_then(resolve_class_id_by_name)
            .map(|id| TypeEstimate::Class(id.0))
            .unwrap_or(TypeEstimate::Top),
    }
}

/// Sprint 27: walk the AST collecting any call expressions whose
/// callee is the name of a `define c-function`. Each such call site
/// becomes a `LoweringError::Unsupported` with the Sprint 28
/// deferral text. Sprint 28's call-site lowering will replace this
/// scan with proper FFI codegen.
fn scan_module_for_c_function_calls(
    m: &Module,
    c_names: &HashSet<String>,
    errors: &mut Vec<LoweringError>,
) {
    for item in &m.items {
        match item {
            Item::DefineFunction { body, .. } | Item::DefineMethod { body, .. } => {
                for s in body {
                    scan_stmt_for_c_calls(s, c_names, errors);
                }
            }
            Item::DefineConstant { value, .. } | Item::DefineVariable { value, .. } => {
                scan_expr_for_c_calls(value, c_names, errors);
            }
            Item::Expr(e) => scan_expr_for_c_calls(e, c_names, errors),
            _ => {}
        }
    }
}

fn scan_stmt_for_c_calls(
    s: &nod_reader::Statement,
    c_names: &HashSet<String>,
    errors: &mut Vec<LoweringError>,
) {
    use nod_reader::Statement as S;
    match s {
        S::Expr(e) => scan_expr_for_c_calls(e, c_names, errors),
        S::Let { value, .. } => {
            scan_expr_for_c_calls(value, c_names, errors);
        }
        S::Local { .. } => {
            // local methods carry exprs in bodies. Sprint 27 doesn't
            // recurse into them — c-function call inside a local
            // method is exotic and Sprint 28 will sweep this up.
        }
        S::For { body, finally_, .. } => {
            for s in body {
                scan_stmt_for_c_calls(s, c_names, errors);
            }
            for s in finally_ {
                scan_stmt_for_c_calls(s, c_names, errors);
            }
        }
        S::While { cond, body, .. } | S::Until { cond, body, .. } => {
            scan_expr_for_c_calls(cond, c_names, errors);
            for s in body {
                scan_stmt_for_c_calls(s, c_names, errors);
            }
        }
        S::Block { body, cleanup, afterwards, .. } => {
            for s in body {
                scan_stmt_for_c_calls(s, c_names, errors);
            }
            for s in cleanup {
                scan_stmt_for_c_calls(s, c_names, errors);
            }
            for s in afterwards {
                scan_stmt_for_c_calls(s, c_names, errors);
            }
        }
    }
}

fn scan_expr_for_c_calls(
    e: &Expr,
    c_names: &HashSet<String>,
    errors: &mut Vec<LoweringError>,
) {
    use nod_reader::Expr as E;
    match e {
        E::Call { callee, args, span } => {
            if let E::Ident(_, name) = callee.as_ref()
                && c_names.contains(name)
            {
                errors.push(LoweringError::Unsupported {
                    span: *span,
                    message: format!(
                        "`{name}`: c-function signature couldn't be derived. \
                         Check (a) arity ≤ 12 (Sprint 36b cap); \
                         (b) every param + return is one of the supported \
                         c-types: integer family, <c-bool>, <c-pointer>, \
                         <c-handle>, <c-string>, <c-wide-string>, or a \
                         <c-struct> subclass. Float / variadic / function-pointer \
                         args are not yet supported (Sprint 37+)."
                    ),
                });
            }
            scan_expr_for_c_calls(callee, c_names, errors);
            for a in args {
                scan_expr_for_c_calls(a, c_names, errors);
            }
        }
        E::BinOp { lhs, rhs, .. } => {
            scan_expr_for_c_calls(lhs, c_names, errors);
            scan_expr_for_c_calls(rhs, c_names, errors);
        }
        E::UnOp { operand, .. } => scan_expr_for_c_calls(operand, c_names, errors),
        E::Paren { inner, .. } => scan_expr_for_c_calls(inner, c_names, errors),
        E::If { cond, then_, else_, .. } => {
            scan_expr_for_c_calls(cond, c_names, errors);
            scan_expr_for_c_calls(then_, c_names, errors);
            if let Some(eb) = else_ {
                scan_expr_for_c_calls(eb, c_names, errors);
            }
        }
        E::Begin { body, .. } => {
            for e in body {
                scan_expr_for_c_calls(e, c_names, errors);
            }
        }
        E::Let { value, .. } => scan_expr_for_c_calls(value, c_names, errors),
        E::Method { body, .. } | E::LocalMethod { body, .. } => {
            for e in body {
                scan_expr_for_c_calls(e, c_names, errors);
            }
        }
        E::Stmt(s) => scan_stmt_for_c_calls(s, c_names, errors),
        _ => {}
    }
}

/// Sprint 31: parallel scan to [`scan_module_for_c_function_calls`] that
/// emits a different error message for bare-name calls whose Win32 entry
/// is present in the embedded index but uses an unsupported parameter /
/// return shape. We surface the matched (library, c-name) so the user
/// can declare a shim by hand.
fn scan_module_for_materialized_unsupported(
    m: &Module,
    unsupported: &HashMap<String, String>,
    errors: &mut Vec<LoweringError>,
) {
    for item in &m.items {
        match item {
            Item::DefineFunction { body, .. } | Item::DefineMethod { body, .. } => {
                for s in body {
                    scan_stmt_for_unsupported_winapi(s, unsupported, errors);
                }
            }
            Item::DefineConstant { value, .. } | Item::DefineVariable { value, .. } => {
                scan_expr_for_unsupported_winapi(value, unsupported, errors);
            }
            Item::Expr(e) => scan_expr_for_unsupported_winapi(e, unsupported, errors),
            _ => {}
        }
    }
}

fn scan_stmt_for_unsupported_winapi(
    s: &Statement,
    unsupported: &HashMap<String, String>,
    errors: &mut Vec<LoweringError>,
) {
    match s {
        Statement::Expr(e) => scan_expr_for_unsupported_winapi(e, unsupported, errors),
        Statement::Let { value, .. } => scan_expr_for_unsupported_winapi(value, unsupported, errors),
        Statement::Local { .. } => {}
        Statement::For { body, finally_, .. } => {
            for s in body {
                scan_stmt_for_unsupported_winapi(s, unsupported, errors);
            }
            for s in finally_ {
                scan_stmt_for_unsupported_winapi(s, unsupported, errors);
            }
        }
        Statement::While { cond, body, .. } | Statement::Until { cond, body, .. } => {
            scan_expr_for_unsupported_winapi(cond, unsupported, errors);
            for s in body {
                scan_stmt_for_unsupported_winapi(s, unsupported, errors);
            }
        }
        Statement::Block { body, cleanup, afterwards, .. } => {
            for s in body {
                scan_stmt_for_unsupported_winapi(s, unsupported, errors);
            }
            for s in cleanup {
                scan_stmt_for_unsupported_winapi(s, unsupported, errors);
            }
            for s in afterwards {
                scan_stmt_for_unsupported_winapi(s, unsupported, errors);
            }
        }
    }
}

fn scan_expr_for_unsupported_winapi(
    e: &Expr,
    unsupported: &HashMap<String, String>,
    errors: &mut Vec<LoweringError>,
) {
    match e {
        Expr::Call { callee, args, span } => {
            if let Expr::Ident(_, name) = callee.as_ref()
                && let Some(reason) = unsupported.get(name)
            {
                errors.push(LoweringError::Unsupported {
                    span: *span,
                    message: format!(
                        "Win32 function `{name}` was found in the embedded \
                         windows_api.db index, but its signature uses unsupported \
                         types ({reason}). To use this function, declare an \
                         explicit `define c-function {name} ... library: \"…\"; end;` \
                         with a shim signature, or wait for the relevant FFI \
                         capability sprint (callbacks: Sprint 33; structs: Sprint 34)."
                    ),
                });
            }
            scan_expr_for_unsupported_winapi(callee, unsupported, errors);
            for a in args {
                scan_expr_for_unsupported_winapi(a, unsupported, errors);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            scan_expr_for_unsupported_winapi(lhs, unsupported, errors);
            scan_expr_for_unsupported_winapi(rhs, unsupported, errors);
        }
        Expr::UnOp { operand, .. } => scan_expr_for_unsupported_winapi(operand, unsupported, errors),
        Expr::Paren { inner, .. } => scan_expr_for_unsupported_winapi(inner, unsupported, errors),
        Expr::If { cond, then_, else_, .. } => {
            scan_expr_for_unsupported_winapi(cond, unsupported, errors);
            scan_expr_for_unsupported_winapi(then_, unsupported, errors);
            if let Some(eb) = else_ {
                scan_expr_for_unsupported_winapi(eb, unsupported, errors);
            }
        }
        Expr::Begin { body, .. } => {
            for e in body {
                scan_expr_for_unsupported_winapi(e, unsupported, errors);
            }
        }
        Expr::Let { value, .. } => scan_expr_for_unsupported_winapi(value, unsupported, errors),
        Expr::Method { body, .. } | Expr::LocalMethod { body, .. } => {
            for e in body {
                scan_expr_for_unsupported_winapi(e, unsupported, errors);
            }
        }
        Expr::Stmt(s) => scan_stmt_for_unsupported_winapi(s, unsupported, errors),
        _ => {}
    }
}

// ─── Class registration ────────────────────────────────────────────────────

fn register_class(
    name: &str,
    supers: &[Expr],
    slots: &[nod_reader::SlotDef],
    span: Span,
) -> Result<ClassId, LoweringError> {
    // Sprint 12 refuses redefinition. Sprint 51e: a front-end-shim
    // class of the same name (high id band) is a separate namespace and
    // must NOT count as a prior definition — use the shim-band-aware
    // resolver so a user program can define its own `<token>` etc.
    if resolve_class_id_by_name(name).is_some() {
        return Err(LoweringError::ClassRedefinitionNotSupported {
            span,
            class_name: name.to_string(),
        });
    }
    // Resolve every super to a registered ClassId. Default to a
    // singleton `[<object>]` when no supers were declared, per Dylan
    // convention.
    let parent_ids: Vec<ClassId> = if supers.is_empty() {
        vec![ClassId::OBJECT]
    } else {
        let mut out = Vec::with_capacity(supers.len());
        for super_expr in supers {
            let super_name = match super_expr {
                Expr::Ident(_, n) => n.clone(),
                _ => {
                    return Err(LoweringError::Unsupported {
                        span,
                        message: "superclass expression must be an identifier".to_string(),
                    });
                }
            };
            match resolve_class_id_by_name(&super_name) {
                Some(id) => {
                    // Sprint 15 cross-library refusal — if the parent
                    // was already sealed by a prior lowering call, this
                    // is an attempt to extend a sealed class from a
                    // different "library". The check naturally allows
                    // in-library subclassing because the parent's
                    // `sealed` bit is flipped AFTER `register_class`
                    // returns in this very same `lower_module_full`
                    // call (Phase 1 vs the modifiers-acting loop).
                    let p = class_metadata_ptr(id);
                    if !p.is_null() {
                        // SAFETY: static-area metadata.
                        let sealed = unsafe { (*p).is_sealed() };
                        if sealed {
                            return Err(LoweringError::SealingViolation {
                                span,
                                violation: SealingViolation::SealedClassExtendedAcrossBoundary {
                                    sealed_parent: super_name.clone(),
                                    child: name.to_string(),
                                },
                            });
                        }
                    }
                    out.push(id);
                }
                None => {
                    return Err(LoweringError::UnknownSuperclass {
                        span,
                        class_name: name.to_string(),
                        super_name,
                    });
                }
            }
        }
        out
    };

    // Build SlotInfos for own slots (offsets get patched in
    // `register_simple_user_class`).
    let mut own_slots: Vec<SlotInfo> = Vec::with_capacity(slots.len());
    for slot in slots {
        // `constant slot` is ordinary instance storage with a getter but no
        // setter — lower it like `instance:` and force `has_setter = false`
        // below. Class/each-subclass/virtual allocation remain unsupported.
        let is_constant_slot =
            slot.allocation == nod_reader::SlotAllocation::Constant;
        if slot.allocation != nod_reader::SlotAllocation::Instance
            && !is_constant_slot
        {
            return Err(LoweringError::UnsupportedSlotAllocation {
                span: slot.span,
                class_name: name.to_string(),
                slot_name: slot.name.clone(),
                allocation: format!("{:?}", slot.allocation),
            });
        }
        let type_kind = slot
            .type_
            .as_ref()
            .map(slot_type_from_expr)
            .unwrap_or(SlotType::Top);
        let default_init = match (&slot.init_value, type_kind) {
            (Some(Expr::Integer(_, n)), _) => {
                // Try to encode as a fixnum literal.
                match (*n).try_into() {
                    Ok(i) => Word::from_fixnum(i)
                        .map(SlotDefault::Value)
                        .unwrap_or(SlotDefault::Unbound),
                    Err(_) => SlotDefault::Unbound,
                }
            }
            (Some(Expr::Bool(_, true)), _) => {
                SlotDefault::Value(nod_runtime::literal_pool_immediates().true_)
            }
            (Some(Expr::Bool(_, false)), _) => {
                SlotDefault::Value(nod_runtime::literal_pool_immediates().false_)
            }
            _ => SlotDefault::Unbound,
        };
        let has_setter = if is_constant_slot {
            false
        } else {
            slot.setter.unwrap_or(true)
        };
        own_slots.push(SlotInfo {
            name: slot.name.clone(),
            offset: 0, // patched by registration helper.
            type_kind,
            init_keyword: slot.init_keyword.clone(),
            required_init_keyword: slot.required_init_keyword,
            default_init,
            has_setter,
        });
    }

    // Single-inheritance fast path — preserves Sprint 12 behaviour exactly.
    if parent_ids.len() == 1 {
        let parent_id = parent_ids[0];
        let (id, _addr) =
            register_simple_user_class(name, Some(parent_id), own_slots);
        // Sprint 15: register `id` as a direct subclass of `parent_id`
        // so the dispatch resolver can enumerate bounded subclass sets
        // when the parent is sealed.
        register_direct_subclass(parent_id, id);
        return Ok(id);
    }

    // Multi-inheritance path (Sprint 14).
    // 1. Resolve every parent's name (for C3 + diagnostics).
    let parent_names: Vec<String> = parent_ids
        .iter()
        .map(|id| {
            let md_ptr = class_metadata_ptr(*id);
            if md_ptr.is_null() {
                format!("<unknown:{}>", id.0)
            } else {
                // SAFETY: pointer is to static-area metadata.
                unsafe { (*md_ptr).name.clone() }
            }
        })
        .collect();
    // 2. Run C3 on the parent CPLs (which are names, since that's what
    //    c3.rs takes).
    let parent_cpl_names: Vec<Vec<String>> = parent_ids
        .iter()
        .map(|id| {
            let md = class_metadata_for(*id);
            md.cpl
                .iter()
                .map(|c| {
                    let p = class_metadata_ptr(*c);
                    if p.is_null() {
                        format!("<unknown:{}>", c.0)
                    } else {
                        // SAFETY: static area.
                        unsafe { (*p).name.clone() }
                    }
                })
                .collect()
        })
        .collect();
    let parent_cpl_refs: Vec<&[String]> =
        parent_cpl_names.iter().map(|v| v.as_slice()).collect();
    let cpl_names = c3_linearise(name, &parent_names, &parent_cpl_refs).map_err(
        |e| match e {
            C3Error::InconsistentMerge { class_name } => {
                LoweringError::InconsistentInheritance {
                    span,
                    class_name: class_name.clone(),
                    detail: "C3 merge failed: parents impose conflicting orders on a shared ancestor".to_string(),
                }
            }
            C3Error::UnresolvedParent { class_name, parent_name } => {
                LoweringError::InconsistentInheritance {
                    span,
                    class_name,
                    detail: format!("parent `{parent_name}` has no CPL yet (forward reference?)"),
                }
            }
        },
    )?;
    // 3. Map names back to ClassIds. Sentinel `ClassId(u32::MAX)` for the
    //    self entry at index 0; runtime patches after id minting.
    let self_sentinel = ClassId(u32::MAX);
    let mut cpl: Vec<ClassId> = Vec::with_capacity(cpl_names.len());
    for (i, n) in cpl_names.iter().enumerate() {
        if i == 0 {
            cpl.push(self_sentinel);
        } else {
            match resolve_class_id_by_name(n) {
                Some(id) => cpl.push(id),
                None => {
                    return Err(LoweringError::InconsistentInheritance {
                        span,
                        class_name: name.to_string(),
                        detail: format!("C3-derived ancestor `{n}` is not a registered class"),
                    });
                }
            }
        }
    }
    // 4. Merge slot lists. Walk parents in declaration order (the
    //    "most-specific-first append" policy from the brief): append
    //    each parent's full slot list to the merged list, skipping
    //    slots whose origin class is already present.
    let mut merged_slots: Vec<SlotInfo> = Vec::new();
    let mut merged_origin: Vec<ClassId> = Vec::new();
    for parent_id in &parent_ids {
        let pmd = class_metadata_for(*parent_id);
        for (slot, origin) in pmd.slots.iter().zip(pmd.slot_origin.iter()) {
            // If a slot with the same defining class is already in the
            // merged list, skip it (diamond — same slot reached via two
            // paths).
            if merged_origin.contains(origin) {
                // We already pulled in every slot from this origin via
                // a different parent path; this iteration is a duplicate.
                // But we still need to check slot name conflicts: if
                // two different origins define the same slot NAME, that's
                // an MI conflict.
                continue;
            }
            // Conflict check: another origin already defined a slot with
            // this name?
            if let Some(idx) = merged_slots.iter().position(|s| s.name == slot.name) {
                let prior_origin = merged_origin[idx];
                if prior_origin != *origin {
                    return Err(LoweringError::SlotConflict {
                        span,
                        class_name: name.to_string(),
                        slot_name: slot.name.clone(),
                        first_origin: class_name_of(prior_origin),
                        second_origin: class_name_of(*origin),
                    });
                }
            }
            merged_slots.push(slot.clone());
            merged_origin.push(*origin);
        }
    }
    let inherited_slot_count = merged_slots.len();
    // 5. Append this class's own slots (mark with self-sentinel — runtime
    //    patches after id minting).
    for slot in own_slots {
        // Reject conflict with an inherited slot name.
        if merged_slots.iter().any(|s| s.name == slot.name) {
            return Err(LoweringError::SlotConflict {
                span,
                class_name: name.to_string(),
                slot_name: slot.name.clone(),
                first_origin: "(an ancestor)".to_string(),
                second_origin: name.to_string(),
            });
        }
        merged_slots.push(slot);
        merged_origin.push(self_sentinel);
    }
    let own_slot_count = merged_slots.len() - inherited_slot_count;
    // 6. Patch every slot's offset to its position in the merged list.
    for (i, slot) in merged_slots.iter_mut().enumerate() {
        slot.offset = std::mem::size_of::<nod_runtime::Wrapper>() + i * 8;
    }
    let (id, _addr) = register_mi_user_class(
        name,
        parent_ids.clone(),
        cpl,
        merged_slots,
        merged_origin,
        own_slot_count,
        inherited_slot_count,
    );
    // Sprint 15: record this class as a direct subclass of every
    // declared parent.
    for parent_id in &parent_ids {
        register_direct_subclass(*parent_id, id);
    }
    Ok(id)
}

/// Sprint 15: append `child` to `parent`'s `direct_subclasses` list.
/// No-op if either id has no metadata (defensive against the seed
/// path's tests).
fn register_direct_subclass(parent: ClassId, child: ClassId) {
    let p = class_metadata_ptr(parent);
    if p.is_null() {
        return;
    }
    // SAFETY: pointer is to static-area metadata (process-lived).
    unsafe { (*p).register_subclass(child) };
}

fn class_name_of(id: ClassId) -> String {
    let p = class_metadata_ptr(id);
    if p.is_null() {
        format!("<unknown:{}>", id.0)
    } else {
        // SAFETY: static area.
        unsafe { (*p).name.clone() }
    }
}

fn slot_type_from_expr(e: &Expr) -> SlotType {
    if let Expr::Ident(_, n) = e {
        match n.as_str() {
            "<integer>" => SlotType::Integer,
            "<single-float>" | "<double-float>" | "<float>" => SlotType::DoubleFloat,
            "<boolean>" => SlotType::Boolean,
            "<character>" => SlotType::Character,
            "<string>" | "<byte-string>" => SlotType::String,
            "<symbol>" => SlotType::Symbol,
            "<simple-object-vector>" | "<vector>" => SlotType::Vector,
            "<object>" | "<top>" => SlotType::Top,
            other => {
                // User class? If registered, narrow.
                if let Some(id) = resolve_class_id_by_name(other) {
                    SlotType::Class(id)
                } else {
                    SlotType::Top
                }
            }
        }
    } else {
        SlotType::Top
    }
}

fn slot_type_to_dfm_kind(t: SlotType) -> SlotTypeKind {
    match t {
        SlotType::Integer | SlotType::Character => SlotTypeKind::Integer,
        _ => SlotTypeKind::Object,
    }
}

fn slot_type_to_estimate(t: SlotType) -> TypeEstimate {
    match t {
        SlotType::Integer => TypeEstimate::Integer,
        SlotType::DoubleFloat => TypeEstimate::DoubleFloat,
        SlotType::Boolean => TypeEstimate::Boolean,
        SlotType::Character => TypeEstimate::Character,
        SlotType::String => TypeEstimate::String,
        _ => TypeEstimate::Top,
    }
}

/// Sprint 56a-WIRE — canonical, name-stable `type=` label for a slot in the
/// `=== classes ===` dump. The scalar variants map to their canonical source
/// type name (the SAME bucket the Dylan-side `slot-type-label` produces from
/// the slot's declared type, mirroring `slot_type_from_expr`'s collapsing —
/// e.g. `<byte-string>`→`<string>`, `<simple-object-vector>`→`<vector>`,
/// `<single-float>`/`<float>`→`<double-float>`). A `Class`-typed slot is
/// rendered BY NAME via `sema_class_name`, NOT its numeric `ClassId` (ids are
/// process-global and won't agree across the Rust and Dylan derivations —
/// the 53.5e id-nondeterminism trap); the name IS the slot's source type
/// text, which is exactly what the Dylan side emits, so both sides agree.
fn slot_type_label(t: SlotType) -> String {
    match t {
        SlotType::Integer => "<integer>".to_string(),
        SlotType::DoubleFloat => "<double-float>".to_string(),
        SlotType::Boolean => "<boolean>".to_string(),
        SlotType::Character => "<character>".to_string(),
        SlotType::String => "<string>".to_string(),
        SlotType::Symbol => "<symbol>".to_string(),
        SlotType::Vector => "<vector>".to_string(),
        SlotType::Object => "<object>".to_string(),
        SlotType::Top => "<top>".to_string(),
        SlotType::Class(id) => sema_class_name(id),
    }
}

/// Sprint 56a-WIRE — GAP-009 default-init tag for a slot in the `=== classes ===`
/// dump. The tag space is the SAME the AOT slot serializer uses
/// (`nod-sema/src/lib.rs` ~1355-1360): `unbound` / `true` / `false` / `nil` /
/// `value:<bits>`. `register_class` (lower.rs) only ever derives `Value` from
/// an integer or boolean LITERAL (with the fixnum-overflow→Unbound edge), so in
/// practice only `unbound` / `true` / `false` / `value:<bits>` arise here; the
/// `nil` arm is carried for tag-space completeness / the parser's inverse.
fn slot_default_tag(d: SlotDefault) -> String {
    match d {
        SlotDefault::Unbound => "unbound".to_string(),
        SlotDefault::Value(w) => {
            let imm = nod_runtime::literal_pool_immediates();
            if w.raw() == imm.true_.raw() {
                "true".to_string()
            } else if w.raw() == imm.false_.raw() {
                "false".to_string()
            } else if w.raw() == imm.nil.raw() {
                "nil".to_string()
            } else {
                format!("value:{}", w.raw())
            }
        }
    }
}

fn module_defines_function(m: &Module, name: &str) -> bool {
    m.items.iter().any(|it| match it {
        Item::DefineFunction { name: n, .. } | Item::DefineMethod { name: n, .. } => n == name,
        _ => false,
    })
}

// ─── Slot-accessor synthesis ───────────────────────────────────────────────

fn build_slot_getter(
    id: FunctionId,
    name: &str,
    offset: usize,
    slot_type: SlotTypeKind,
    return_type: TypeEstimate,
) -> Function {
    let span = Span {
        file_id: nod_reader::FileId(0),
        lo: 0,
        hi: 0,
    };
    let entry = BlockId(0);
    let self_temp = TempId(0);
    let result_temp = TempId(1);
    Function {
        id,
        name: name.to_string(),
        params: vec![self_temp],
        entry,
        blocks: vec![Block {
            id: entry,
            label: "entry".to_string(),
            params: Vec::new(),
            computations: vec![Computation::LoadSlot {
                dst: result_temp,
                instance: self_temp,
                offset,
                slot_type,
            }],
            terminator: Terminator::Return {
                value: Some(result_temp),
            },
        }],
        temps: vec![
            Temporary {
                id: self_temp,
                type_estimate: TypeEstimate::Top,
            },
            Temporary {
                id: result_temp,
                type_estimate: return_type,
            },
        ],
        return_type,
        span,
    }
}

fn build_slot_setter(
    id: FunctionId,
    name: &str,
    offset: usize,
    slot_type: SlotTypeKind,
) -> Function {
    let span = Span {
        file_id: nod_reader::FileId(0),
        lo: 0,
        hi: 0,
    };
    let entry = BlockId(0);
    let self_temp = TempId(0);
    let value_temp = TempId(1);
    let result_temp = TempId(2);
    Function {
        id,
        name: name.to_string(),
        params: vec![self_temp, value_temp],
        entry,
        blocks: vec![Block {
            id: entry,
            label: "entry".to_string(),
            params: Vec::new(),
            computations: vec![Computation::StoreSlot {
                dst: result_temp,
                instance: self_temp,
                offset,
                value: value_temp,
                slot_type,
            }],
            terminator: Terminator::Return {
                value: Some(result_temp),
            },
        }],
        temps: vec![
            Temporary {
                id: self_temp,
                type_estimate: TypeEstimate::Top,
            },
            Temporary {
                id: value_temp,
                type_estimate: TypeEstimate::Top,
            },
            Temporary {
                id: result_temp,
                type_estimate: TypeEstimate::Top,
            },
        ],
        return_type: TypeEstimate::Top,
        span,
    }
}

// ─── Method lowering ───────────────────────────────────────────────────────

struct LoweredMethod {
    function: Function,
    /// `None` for a 0-parameter method: it is lowered as a plain
    /// direct-call function (no dispatch), so there is nothing to
    /// register in the generic-method table.
    registration: Option<MethodRegistration>,
}

#[allow(clippy::too_many_arguments)]
fn lower_method_item(
    id: FunctionId,
    name: &str,
    params: &[Param],
    return_sig: Option<&ReturnSig>,
    body: &[Statement],
    span: Span,
    ctx: &LowerCtx,
    sink: &mut LiftSink,
) -> Result<LoweredMethod, LoweringError> {
    if params.is_empty() {
        // A 0-parameter `define method` has no specialisable receiver,
        // so it can't participate in generic dispatch. Dylan permits
        // it; lower it as a plain direct-call function under its bare
        // name (matching `define function`). `collect_top_level_names`
        // registers 0-param methods in `top_names` so call sites emit a
        // DirectCall, and `collect_generic_names` excludes them.
        let function =
            lower_function_inner(id, name, params, return_sig, body, span, ctx, sink)?;
        return Ok(LoweredMethod {
            function,
            registration: None,
        });
    }
    // Sprint 13: collect ONE specialiser per required parameter. An
    // unannotated parameter is `<object>` per Dylan convention.
    let mut specialisers: Vec<ClassId> = Vec::with_capacity(params.len());
    for p in params {
        let cls = match &p.type_ {
            Some(Expr::Ident(_, cls)) => match resolve_class_id_by_name(cls) {
                Some(id) => id,
                None => {
                    return Err(LoweringError::UndefinedIdent {
                        span: p.span,
                        name: cls.clone(),
                    });
                }
            },
            _ => ClassId::OBJECT,
        };
        specialisers.push(cls);
    }
    let receiver_class = specialisers[0];
    // Encode all specialisers in the body fn name so distinct
    // multi-arg methods don't collide at codegen.
    let suffix = specialisers
        .iter()
        .map(|c| c.0.to_string())
        .collect::<Vec<_>>()
        .join("_");
    let body_fn_name = format!("{name}${suffix}");
    // Codegen name is mangled; closure-registry key is the SOURCE name.
    let function = lower_function_inner_keyed(
        id,
        &body_fn_name,
        name,
        params,
        return_sig,
        body,
        span,
        ctx,
        sink,
    )?;
    let _ = receiver_class;
    let registration = MethodRegistration {
        generic_name: name.to_string(),
        specialisers,
        body_fn_name: body_fn_name.clone(),
        param_count: params.len(),
    };
    Ok(LoweredMethod {
        function,
        registration: Some(registration),
    })
}

pub fn lower_function(
    name: &str,
    params: &[Param],
    body: &[Statement],
) -> Result<Function, LoweringError> {
    let span = body
        .first()
        .map(Statement::span)
        .or_else(|| params.first().map(|p| p.span))
        .unwrap_or(Span {
            file_id: nod_reader::FileId(0),
            lo: 0,
            hi: 0,
        });
    let top_names = TopNames::empty();
    let generics: HashSet<String> = HashSet::new();
    let user_classes: HashMap<String, ClassId> = HashMap::new();
    let ctx = LowerCtx {
        top_names: &top_names,
        generics: &generics,
        user_classes: &user_classes,
        closures: None,
        c_functions: None,
    };
    let mut sink = LiftSink::default();
    lower_function_inner(FunctionId(0), name, params, None, body, span, &ctx, &mut sink)
}

#[allow(clippy::too_many_arguments)]
fn lower_function_inner(
    id: FunctionId,
    name: &str,
    params: &[Param],
    return_sig: Option<&ReturnSig>,
    body: &[Statement],
    span: Span,
    ctx: &LowerCtx,
    sink: &mut LiftSink,
) -> Result<Function, LoweringError> {
    lower_function_inner_keyed(id, name, name, params, return_sig, body, span, ctx, sink)
}

/// Like [`lower_function_inner`] but with an explicit `closure_key` — the
/// name under which the lift pre-pass recorded this body's cell-promotion
/// set and local-method lifts. For `define method` the codegen `name` is
/// mangled (`name$specialisers`) while the registry is keyed on the
/// source name, so the two differ.
#[allow(clippy::too_many_arguments)]
fn lower_function_inner_keyed(
    id: FunctionId,
    name: &str,
    closure_key: &str,
    params: &[Param],
    return_sig: Option<&ReturnSig>,
    body: &[Statement],
    span: Span,
    ctx: &LowerCtx,
    sink: &mut LiftSink,
) -> Result<Function, LoweringError> {
    let mut b = FunctionBuilder::new(id, name.to_string(), span);
    b.closure_key = closure_key.to_string();
    let mut env = LocalEnv::new();

    // Sprint 24: closure-body bring-up. If `name` is the lifted body of
    // a closure with non-empty captures, install the env parameter as a
    // synthetic FIRST parameter (matching the runtime ABI in
    // `nod_funcall_N`). The lowerer redirects reads / writes of
    // captured names through `%env-cell` + `%cell-get` / `%cell-set!`.
    //
    // Sprint 60+: a `#rest` closure body is ALWAYS created via
    // emit_make_closure (even with no captures) so the closure Word is
    // tagged FUNCTION_KIND_CLOSURE_REST. emit_make_closure builds an
    // <environment> (empty when there are no captures), so the runtime
    // invokes the body with the `(env, …)` ABI. The body must therefore
    // carry the synthetic env param whenever it is a rest-closure body,
    // even with an empty capture set — otherwise the env Word would be
    // misread as the first fixed argument.
    let closure_info: Option<&ClosureInfo> = ctx
        .closures
        .and_then(|reg| reg.closure_for(name))
        .filter(|info| !info.captured.is_empty() || info.rest_fixed.is_some());
    if let Some(info) = closure_info {
        let env_temp = b.fresh_temp(TypeEstimate::Top);
        b.func.params.push(env_temp);
        let mut index_of: HashMap<String, usize> = HashMap::new();
        for (i, c) in info.captured.iter().enumerate() {
            index_of.insert(c.clone(), i);
        }
        b.cell_ctx.env_captures = Some(EnvCaptures { env_temp, index_of });
    }

    // Sprint 24: cell-promotion locals for this body. Any local in
    // `cell_locals` whose `let` binding is encountered while lowering
    // becomes a `<cell>` allocation; subsequent reads / writes go
    // through the cell. Keyed on `closure_key` (the source name for
    // methods), matching how the lift pre-pass recorded them.
    if let Some(reg) = ctx.closures
        && let Some(cells) = reg.cell_locals_for(closure_key)
    {
        b.cell_ctx.cell_locals = cells.clone();
    }

    for p in params {
        let pty = type_from_expr(p.type_.as_ref());
        let t = b.fresh_temp(pty);
        b.func.params.push(t);
        // Adjective params (`#key x`, `#rest r`, `#next nm`) bind the
        // bare identifier, not the marker-prefixed string. A pure marker
        // (`#all-keys`) binds nothing — we still push a param temp to
        // keep the ABI arity in step with the parser's param count, but
        // skip the env insert. Keyword arguments are passed positionally
        // (matching how `make` / generic dispatch treat them at the call
        // site), so a `#key` param reads its value through the ordinary
        // positional slot.
        let Some(bind_name) = param_binding_name(&p.name) else {
            continue;
        };
        let bind_name = bind_name.to_string();
        // Sprint 24: if this param is itself captured by an inner
        // closure, promote it to a cell so the inner closure (which
        // accesses it through the env) and the outer scope see the
        // same storage. The cell-promoted name maps to the cell-Word
        // in `env`; subsequent reads/writes of `bind_name` go through
        // `%cell-get` / `%cell-set!`.
        if b.cell_ctx.cell_locals.contains(&bind_name) {
            let cell = b.fresh_temp(TypeEstimate::Top);
            b.push(Computation::DirectCall {
                dst: cell,
                callee: "nod_make_cell".to_string(),
                args: vec![t],
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            env.insert(bind_name, cell);
        } else {
            env.insert(bind_name, t);
        }
    }

    let declared_ret = return_sig
        .and_then(|r| r.values.first().and_then(|v| v.type_.as_ref()))
        .map(|e| type_from_expr(Some(e)));

    let last_idx = body.len().saturating_sub(1);
    let mut final_temp: Option<TempId> = None;
    // Move the caller's lift sink into the builder for the duration of
    // the body loop so block forms reachable only through `lower_expr`
    // (a `block … end` in EXPRESSION position) share the same sink and
    // fn-id counter. Restored to `sink` after the loop.
    b.pending_sink = Some(std::mem::take(sink));
    for (i, stmt) in body.iter().enumerate() {
        let is_last = i == last_idx;
        match stmt {
            Statement::Expr(e) => {
                // Flatten `Statement::Expr(Expr::Stmt(Statement::Block {...}))` —
                // produced when a body-shaped macro (e.g. `with-cleanup`) expands
                // to a block form.  The macro re-parser always returns an Expr, so
                // the block gets double-wrapped.  We unwrap it here so
                // `lower_block_form` can lift the body thunk.
                if let Expr::Stmt(inner) = e {
                    if let Statement::Block {
                        span,
                        exit_var,
                        body: blk_body,
                        handlers,
                        cleanup,
                        afterwards,
                    } = inner.as_ref()
                    {
                        let t = b.lower_block_in_expr(
                            &mut env,
                            ctx,
                            *span,
                            exit_var.as_deref(),
                            blk_body,
                            handlers,
                            cleanup,
                            afterwards,
                        )?;
                        if is_last {
                            final_temp = Some(t);
                        }
                        continue;
                    }
                }
                let t = b.lower_expr(e, &mut env, ctx)?;
                if is_last {
                    final_temp = Some(t);
                }
            }
            Statement::Let {
                binders,
                rest,
                value,
                span,
            } => {
                if rest.is_some() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`#rest` binder in `let` not supported yet".to_string(),
                    });
                }
                if binders.is_empty() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`let` with no binders".to_string(),
                    });
                }
                if binders.len() > 1 {
                    // Sprint 47 — multi-binder `let (a, b, …) = expr`.
                    // See `docs/COMPILER_GAPS.md` GAP-003. Delegates to
                    // the shared helper that runs the SBCL-style
                    // secondary-values destructure.
                    let t = b.lower_let_multi_binders(binders, value, &mut env, ctx)?;
                    if is_last {
                        final_temp = Some(t);
                    }
                } else {
                    let bname = &binders[0].name;
                    let t = b.lower_expr(value, &mut env, ctx)?;
                    // Sprint 24: cell-promote the binding if any inner
                    // closure captures it.
                    let bound = if b.cell_ctx.cell_locals.contains(bname) {
                        let cell = b.fresh_temp(TypeEstimate::Top);
                        b.push(Computation::DirectCall {
                            dst: cell,
                            callee: "nod_make_cell".to_string(),
                            args: vec![t],
                            safepoint_roots: Vec::new(), is_no_alloc: false,
                        });
                        cell
                    } else {
                        t
                    };
                    env.insert(bname.clone(), bound);
                    if is_last {
                        final_temp = Some(t);
                    }
                }
            }
            Statement::Local { span, methods } => {
                // `local method … end` — bind each named local method to
                // a closure cell (see `lower_local_methods`). The group
                // has no value of its own.
                let key = b.closure_key.clone();
                b.lower_local_methods(&key, methods, &mut env, ctx, *span)?;
                if is_last {
                    final_temp = None;
                }
            }
            Statement::While { cond, body: wbody, .. } => {
                // Sprint 18: `while (cond) body end`. Three-block CFG
                // with a back-edge: header → loop_body → header / exit.
                // The header block evaluates the condition each
                // iteration; loop_body runs the user statements then
                // unconditionally jumps back to header.
                b.lower_while_like(cond, wbody, false, &mut env, ctx)?;
                if is_last {
                    final_temp = None; // while statements have no value
                }
            }
            Statement::Until { cond, body: wbody, .. } => {
                // Sprint 18: `until (cond) body end`. Same shape as
                // `while` but the condition is negated at the header.
                b.lower_while_like(cond, wbody, true, &mut env, ctx)?;
                if is_last {
                    final_temp = None;
                }
            }
            Statement::For {
                span,
                clauses,
                body: for_body,
                finally_,
            } => {
                // Desugar the `for` into pre-loop `let`s, a `while`, and a
                // trailing result value (the `finally` body, or `#f`).
                // Lower each piece inline. The for-EXPRESSION's value is
                // the result of the trailing statement(s); thread it into
                // `final_temp` when this `for` is the body's last form.
                let desugared = desugar_numeric_for(*span, clauses, for_body, finally_)?;
                let mut for_value: Option<TempId> = None;
                for ds in &desugared {
                    match ds {
                        Statement::Let { binders, value, .. } if binders.len() == 1 => {
                            let bname = &binders[0].name;
                            let t = b.lower_expr(value, &mut env, ctx)?;
                            let bound = if b.cell_ctx.cell_locals.contains(bname) {
                                let cell = b.fresh_temp(TypeEstimate::Top);
                                b.push(Computation::DirectCall {
                                    dst: cell,
                                    callee: "nod_make_cell".to_string(),
                                    args: vec![t],
                                    safepoint_roots: Vec::new(),
                                    is_no_alloc: false,
                                });
                                cell
                            } else {
                                t
                            };
                            env.insert(bname.clone(), bound);
                            for_value = Some(bound);
                        }
                        Statement::While { cond, body: wbody, .. } => {
                            b.lower_while_like(cond, wbody, false, &mut env, ctx)?;
                            for_value = None;
                        }
                        // Trailing `finally` statements (or the `#f`
                        // result): lower as a value-producing form so the
                        // last one supplies the for-expression's value.
                        other => {
                            for_value = Some(b.lower_stmt_as_expr(other, &mut env, ctx)?);
                        }
                    }
                }
                if is_last {
                    final_temp = for_value;
                }
            }
            Statement::Block {
                span,
                exit_var,
                body: blk_body,
                handlers,
                cleanup,
                afterwards,
            } => {
                // Sprint 19: lower `block ... exception ... cleanup ...
                // end` via lifted thunks + a runtime `nod_run_block`
                // call. See `docs/CONDITIONS.md` for the design.
                let t = b.lower_block_in_expr(
                    &mut env,
                    ctx,
                    *span,
                    exit_var.as_deref(),
                    blk_body,
                    handlers,
                    cleanup,
                    afterwards,
                )?;
                if is_last {
                    final_temp = Some(t);
                }
            }
        }
    }
    // Restore the sink to the caller now the body loop is done.
    if let Some(s) = b.pending_sink.take() {
        *sink = s;
    }

    let ret_ty = if let Some(declared) = declared_ret {
        declared
    } else if let Some(t) = final_temp {
        b.func.temp_type(t)
    } else {
        TypeEstimate::Unit
    };
    b.func.return_type = ret_ty;

    let term = if ret_ty == TypeEstimate::Unit {
        Terminator::Return { value: None }
    } else {
        let t = final_temp.ok_or_else(|| LoweringError::Unsupported {
            span,
            message: "function with non-unit return has empty body".to_string(),
        })?;
        Terminator::Return { value: Some(t) }
    };
    b.terminate_current(term);

    Ok(b.finish())
}

// ─── Top-level name set + lowering context ─────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct TopNames {
    fns: HashMap<String, TypeEstimate>,
    /// Sprint 21: arity per top-level function. Populated alongside
    /// `fns` by `collect_top_level_names`. Used to bake the right
    /// arity into `nod_make_function_ref` call sites for `\name`
    /// references. Slot accessors (`<C>-getter-x`) have arity 1;
    /// setters (`<C>-setter-x`) arity 2; user `define function`s
    /// follow their param count.
    fn_arity: HashMap<String, usize>,
    /// GAP-002 fix: names introduced by `define constant`. These ARE
    /// lowered as zero-arg functions (see the `Item::DefineConstant`
    /// arm of the per-item lowering loop), but a bareword reference to
    /// one in expression position should EVALUATE it (zero-arg
    /// DirectCall returning its value), not produce a function-
    /// reference. We track them separately so the `Expr::Ident`
    /// lowering can pick the right shape.
    constants: HashSet<String>,
    /// GAP-004: names introduced by `define variable`. Tracked
    /// separately from `constants` because:
    ///   1. Bareword references evaluate via `<name>()` (the getter
    ///      function, which loads the cell and returns its value) —
    ///      same as constants.
    ///   2. Assignment via `<name> := <expr>` is permitted (constants
    ///      reject it).
    /// The `is_constant_or_variable` accessor unions both sets to
    /// preserve the GAP-002 bareword path; `is_variable` differentiates
    /// for `lower_assign`.
    variables: HashSet<String>,
    /// Sprint 60: top-level functions that take a `#rest` parameter.
    /// Maps the function name to its FIXED (required + `#key`) param
    /// count — the number of positional slots that precede the `#rest`
    /// slot in the callee ABI. A `#rest` callee is lowered with exactly
    /// `fixed + 1` LLVM params: the `fixed` leading args, then ONE final
    /// slot holding a freshly-built `<simple-object-vector>` of the
    /// trailing actuals. The call site (`DirectCall` lowering) consults
    /// this map to bundle args > `fixed` into that SOV before emitting
    /// the call, keeping the fixed-arity LLVM ABI intact (no varargs).
    rest_fns: HashMap<String, usize>,
}

impl TopNames {
    pub fn empty() -> Self {
        Self {
            fns: HashMap::new(),
            fn_arity: HashMap::new(),
            constants: HashSet::new(),
            variables: HashSet::new(),
            rest_fns: HashMap::new(),
        }
    }
    /// Sprint 60: if `name` is a top-level function declared with a
    /// `#rest` parameter, returns its FIXED (pre-`#rest`) param count.
    /// Call-site lowering uses this to bundle the trailing actuals into
    /// the `#rest` SOV slot.
    pub fn rest_fixed_count(&self, name: &str) -> Option<usize> {
        self.rest_fns.get(name).copied()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.fns.contains_key(name)
    }
    pub fn return_type(&self, name: &str) -> Option<TypeEstimate> {
        self.fns.get(name).copied()
    }
    /// Sprint 21: arity for a registered top-level function, if known.
    pub fn arity(&self, name: &str) -> Option<usize> {
        self.fn_arity.get(name).copied()
    }
    /// GAP-002 fix: true iff this name was introduced by
    /// `define constant` or `define variable`. A bareword reference
    /// to such a name in expression position should be lowered as a
    /// zero-arg DirectCall, not as `nod_make_function_ref`.
    pub fn is_constant_or_variable(&self, name: &str) -> bool {
        self.constants.contains(name) || self.variables.contains(name)
    }
    /// GAP-004: true iff this name was introduced by `define variable`
    /// (mutable). Used by `lower_assign` to route `<name> := <expr>`
    /// through the cell-set path; assignment to a `define constant`
    /// name is an error.
    pub fn is_variable(&self, name: &str) -> bool {
        self.variables.contains(name)
    }
}

struct LowerCtx<'a> {
    top_names: &'a TopNames,
    generics: &'a HashSet<String>,
    user_classes: &'a HashMap<String, ClassId>,
    /// Sprint 24: closure registry produced by `lift_anonymous_methods`.
    /// `None` when lowering is invoked outside of `lower_module_full`
    /// (e.g. the `lower_function` test helper); in that case the
    /// lowerer behaves exactly as it did in Sprint 21.
    closures: Option<&'a ClosureRegistry>,
    /// Sprint 28: per-module c-function call site dispatch table.
    /// Maps the Dylan-side name (`Beep`) to the resolved stub-table
    /// entry + signature for code-gen-time lowering. `None` outside
    /// `lower_module_full` (no `define c-function` in scope).
    c_functions: Option<&'a HashMap<String, CFunctionCallInfo>>,
}

/// Sprint 28: per-c-function metadata threaded through `LowerCtx` so
/// call-site lowering can look up the stub-table entry pointer + the
/// marshaling signature for a given Dylan-side name.
///
/// Sprint 38d: in addition to the (still-allocated) static-area entry
/// pointer, carry `dll` / `symbol` / `signature_bytes` so the call-site
/// lowering can emit a `ConstValue::StubEntryRef` instead of baking the
/// per-process `entry_ptr` as an `i64`. The pre-allocation is kept
/// because `nod-sema::lib::initialize_module_winffi` still walks the
/// `c_function_stub_table` to drive cold-path resolution; switching that
/// to go through the slot allocator is part of Sprint 38d's runtime
/// integration but not strictly required (the slot allocator's
/// `resolve_into_entry` call is idempotent, so the eager pre-resolve in
/// sema becomes a no-op).
#[derive(Clone, Debug)]
struct CFunctionCallInfo {
    /// Static-area address of the resolved [`nod_runtime::ApiStubEntry`]
    /// — kept for back-compat with `c_function_stub_table` consumers.
    /// Sprint 38d: no longer baked into the IR; codegen now reads
    /// `dll` / `symbol` / `signature_bytes` and goes through the
    /// `stub_entry_slot_addr` path.
    #[allow(dead_code)]
    entry_ptr: u64,
    /// Argument count from the parsed signature. Drives which
    /// `nod_winffi_call_N` trampoline the call site emits.
    arg_count: usize,
    /// Sprint 38d — DLL name carried verbatim into
    /// `ConstValue::StubEntryRef`. The slot allocator lowercases this
    /// for its case-insensitive key; we keep the original casing here
    /// so debug dumps + sema-side diagnostics match the source.
    dll: String,
    /// Sprint 38d — symbol (effective C name).
    symbol: String,
    /// Sprint 38d — bytewise-encoded [`nod_runtime::ApiCallSignature`]
    /// (`#[repr(C)] Copy`). Carried through into the manifest so the
    /// warm-replay resolver reconstructs the same marshaling shape.
    signature_bytes: Vec<u8>,
}

/// Sprint 24: per-body lowering state for cell-promotion + env access.
/// Threaded alongside `LocalEnv` through every `lower_*` method. Holds:
///
///   * `cell_locals` — names of THIS body's local bindings that should
///     be heap-allocated cells (because some inner closure captures them).
///   * `env_captures` — when lowering a closure body, the synthetic
///     env parameter's `TempId` and the captured-variable-name → env
///     index map. `None` outside closure bodies.
#[derive(Default, Clone, Debug)]
struct CellCtx {
    cell_locals: HashSet<String>,
    env_captures: Option<EnvCaptures>,
}

#[derive(Clone, Debug)]
struct EnvCaptures {
    env_temp: TempId,
    /// Captured-variable name -> index in the env's cells vector.
    index_of: HashMap<String, usize>,
}

/// Sprint 19: accumulator for the lifted thunks each `block` form
/// produces. Threaded through `lower_function_inner` and `FunctionBuilder`
/// so a deeply-nested `block` can deposit its synthesised top-level
/// functions back into the enclosing `lower_module_full` pass.
///
/// `next_fn_id` mirrors the counter `lower_module_full` uses for user
/// `define function`s; lifted thunks get fresh ids in the same space.
/// `name_seed` lets us append a counter to lift-thunk names so two
/// `block` forms in the same parent function don't collide.
#[derive(Default)]
pub struct LiftSink {
    pub functions: Vec<Function>,
    pub blocks: Vec<BlockRegistration>,
    pub next_fn_id: u32,
    pub thunk_counter: u32,
}

impl LiftSink {
    fn alloc_fn_id(&mut self) -> FunctionId {
        let id = FunctionId(self.next_fn_id);
        self.next_fn_id += 1;
        id
    }
    fn alloc_thunk_suffix(&mut self) -> u32 {
        let n = self.thunk_counter;
        self.thunk_counter += 1;
        n
    }
}

/// The bare binding name introduced by a parameter, with any leading
/// adjective marker (`#key `, `#rest `, `#next `) stripped.
///
/// The loose param parser stores adjective params as `"#key x"` /
/// `"#rest r"` / `"#next nm"` (the marker plus a space plus the bound
/// identifier), and pure markers like `"#all-keys"` with no following
/// identifier. Every place that treats a parameter as a *binding* — the
/// lift pre-pass (which records in-scope names so body references resolve
/// to the param rather than being mis-classified as captures) and the
/// body lowerer (which inserts the name into the local env) — must use the
/// bare identifier, not the marker-prefixed string. Returns `None` for a
/// pure marker (`#all-keys`) that binds nothing.
fn param_binding_name(name: &str) -> Option<&str> {
    for marker in ["#key ", "#rest ", "#next "] {
        if let Some(rest) = name.strip_prefix(marker) {
            let bare = rest.trim();
            return if bare.is_empty() { None } else { Some(bare) };
        }
    }
    if name.starts_with('#') {
        // Pure marker (`#all-keys`) — binds nothing.
        return None;
    }
    Some(name)
}

/// Sprint 60: if `params` contains a `#rest var` parameter, return its
/// 0-based index in the param list (= the number of FIXED positional
/// slots preceding it). The loose parser stores the rest param as the
/// string `"#rest var"`. Returns `None` when there is no `#rest`.
///
/// The index doubles as the "fixed param count" used by the call site:
/// args at positions `[0, idx)` map to the leading ABI slots and args
/// at `[idx, …)` are collected into the SOV that fills the rest slot.
fn rest_param_index(params: &[Param]) -> Option<usize> {
    params.iter().position(|p| p.name.starts_with("#rest "))
}

// ─── Sprint 21 / 24: anonymous-method lifting pre-pass ────────────────────
//
// Walks every Item's body, every Expr nested inside, and replaces
// `Expr::Method { params, body }` with an `Expr::Ident` referencing a
// synthesised top-level name. Each replacement also appends an
// `Item::DefineFunction` to the module so the normal lowering flow
// emits the lifted thunk as an ordinary top-level function.
//
// Sprint 21 erred out on any free variable inside a method body with
// "closures land in Sprint 24". Sprint 24 replaces that path with the
// cell-conversion machinery:
//
//   * Compute the **captured set** per `Expr::Method` — names that
//     reference a variable bound in an enclosing scope (and not a
//     top-level / operator / class name).
//   * For each captured local, the enclosing function's body promotes
//     it to a heap-allocated `<cell>`: `let x = E` becomes
//     `let x = %make-cell(E)`, and reads / writes go through
//     `%cell-get` / `%cell-set!` (decided at lowering time via
//     `cell_locals`).
//   * The lifted method body grows a synthetic env parameter; reads /
//     writes of captured names in the body become
//     `%cell-get(%env-cell(env, i))` / `%cell-set!(v, %env-cell(env, i))`.
//   * The closure-creation site (the original `Expr::Method` location)
//     emits `%make-closure(name, arity, env)` where `env` is built by
//     gathering the (cell-promoted) outer-scope variables.
//
// The lifter records all this in a `ClosureRegistry` consumed by the
// lowerer. The registry is keyed by lifted-body-name; the
// per-enclosing-function "which locals to promote" set is computed
// separately and stored under the enclosing function's name.

/// Sprint 24: per-method closure metadata. The lifter produces one of
/// these per `Expr::Method`. The lowerer consults the registry when it
/// sees an `Expr::Ident(lifted_name)` to decide whether to emit a
/// plain function-ref (no captures) or a `%make-closure` site (with
/// captures).
#[derive(Clone, Debug)]
pub struct ClosureInfo {
    pub lifted_name: String,
    /// Captured variable names in stable order. The index into this
    /// vector is the cell's slot index in the environment.
    pub captured: Vec<String>,
    pub arity: usize,
    /// Sprint 60+: if the lifted method declares a `#rest` parameter,
    /// `Some(F)` where `F` is the count of fixed params BEFORE `#rest`
    /// (i.e. `rest_param_index(params)`). `None` for a fixed-arity
    /// method. When set, the closure-creation site emits the
    /// `nod_make_rest_closure` maker (kind-tag = FUNCTION_KIND_CLOSURE_REST)
    /// so the runtime collects trailing actuals into the `#rest` SOV.
    /// `arity` still holds `params.len()` = `F + 1` (the body's
    /// value-arity), the same value the body is registered under.
    pub rest_fixed: Option<usize>,
    pub span: Span,
}

/// Sprint 24 registry built by the lift pre-pass.
///
/// Two pieces of information come out of the pre-pass:
///
///   * **Per-lifted-body**: a `ClosureInfo` describing the body's
///     capture list. Used by the lowerer to (1) recognise that a
///     synthesised `Expr::Ident(lifted_name)` is a closure-creation
///     site, not a plain `\name` reference, and (2) compile the body
///     itself with the synthetic env parameter and the
///     captured-variable indexing scheme.
///
///   * **Per-enclosing-function** ("cell-promote sets"): for each
///     top-level / lifted function in the module, the set of its OWN
///     local-variable names that any inner closure captures. The
///     lowerer's per-body environment management uses this set to
///     cell-promote the matching `let` bindings AND to redirect reads
///     / writes through `%cell-get` / `%cell-set!`.
#[derive(Default, Clone, Debug)]
pub struct ClosureRegistry {
    /// `lifted_name -> ClosureInfo`.
    pub by_lifted_name: HashMap<String, ClosureInfo>,
    /// `enclosing_function_name -> set of locals captured by inner
    /// closures`. Drives cell-promotion in the enclosing body's lowering.
    pub cell_locals_per_function: HashMap<String, HashSet<String>>,
    /// `enclosing_function_name -> (local-method source name ->
    /// lifted-body name)`. Lets the `Statement::Local` lowering find the
    /// `ClosureInfo` for each named local method.
    pub local_lifted_names: HashMap<String, HashMap<String, String>>,
}

impl ClosureRegistry {
    pub fn closure_for(&self, name: &str) -> Option<&ClosureInfo> {
        self.by_lifted_name.get(name)
    }
    pub fn cell_locals_for(&self, function_name: &str) -> Option<&HashSet<String>> {
        self.cell_locals_per_function.get(function_name)
    }
    /// For `function_name`, map a local-method source name to its lifted
    /// top-level body name (and thence its `ClosureInfo`).
    pub fn local_lifted_for(
        &self,
        function_name: &str,
        local_name: &str,
    ) -> Option<&str> {
        self.local_lifted_names
            .get(function_name)
            .and_then(|m| m.get(local_name))
            .map(|s| s.as_str())
    }
}

/// Mutable threading state for the lift pre-pass. Carries the global
/// counters and per-call-site capture metadata that bubble up through
/// recursion.
struct LiftState<'a> {
    /// Set of module-level names (`define function`, classes, …) used
    /// by `check_free_vars` to distinguish "captured local" from
    /// "top-level reference".
    top: &'a HashSet<String>,
    /// Sink for lifted `Item::DefineFunction`s.
    new_items: Vec<Item>,
    /// Lift-time diagnostics.
    errors: Vec<LoweringError>,
    /// The Sprint 24 closure registry being built.
    registry: ClosureRegistry,
}

/// Per-scope rewriting context for the lift pre-pass. Carries the
/// set of names visible in the enclosing scope (so `check_free_vars`
/// can identify captures) and **the name of the enclosing function**
/// (so cell-promotion targets land in the right
/// `cell_locals_per_function` bucket).
struct LiftScope {
    /// Names bound in this lexical scope or any enclosing scope inside
    /// the current top-level function. Walks "outward" the same way
    /// `check_free_vars` does.
    in_scope: HashSet<String>,
    /// Name of the enclosing function (the synthetic top-level name
    /// for lifted bodies; the source name for user functions). The
    /// lift pass deposits "this local must be cell-promoted" under
    /// this name when an inner method captures it.
    enclosing_fn: String,
}

/// Pre-pass entry point. Mutates `module` in place and produces a
/// `ClosureRegistry` describing every closure site discovered. Returns
/// `Err` only for genuine lifting failures (none currently — Sprint 24
/// supports every capture shape Sprint 21 rejected).
fn lift_anonymous_methods(
    module: &mut Module,
) -> (ClosureRegistry, Vec<LoweringError>) {
    // Collect the set of top-level names so the free-variable check
    // can distinguish "captured local" from "module-scope reference".
    // Top-level names include `define function` / `define method` /
    // `define generic` / `define constant` / `define variable` /
    // `define class`. Registered seed / runtime classes (`<integer>`,
    // `<error>`, ...) are also OK because they resolve via
    // `find_class_id_by_name`.
    let mut top_level_names: HashSet<String> = HashSet::new();
    for item in &module.items {
        match item {
            Item::DefineFunction { name, .. }
            | Item::DefineMethod { name, .. }
            | Item::DefineGeneric { name, .. }
            | Item::DefineConstant { name, .. }
            | Item::DefineVariable { name, .. }
            | Item::DefineClass { name, .. } => {
                top_level_names.insert(name.clone());
            }
            _ => {}
        }
    }
    let mut state = LiftState {
        top: &top_level_names,
        new_items: Vec::new(),
        errors: Vec::new(),
        registry: ClosureRegistry::default(),
    };
    // Process each existing item in turn. Replacements append to the
    // module's items via `state.new_items`.
    let mut items = std::mem::take(&mut module.items);
    for mut item in items.drain(..) {
        lift_item(&mut item, &mut state);
        state.new_items.push(item);
    }
    module.items = std::mem::take(&mut state.new_items);
    (state.registry, state.errors)
}

fn lift_item(item: &mut Item, st: &mut LiftState<'_>) {
    match item {
        Item::DefineFunction { name, params, body, .. }
        | Item::DefineMethod { name, params, body, .. } => {
            let mut scope = LiftScope {
                in_scope: st.top.clone(),
                enclosing_fn: name.clone(),
            };
            for p in params.iter() {
                if let Some(b) = param_binding_name(&p.name) {
                    scope.in_scope.insert(b.to_string());
                }
            }
            for s in body.iter_mut() {
                lift_statement(s, &mut scope, st);
            }
        }
        Item::DefineConstant { value, name, .. }
        | Item::DefineVariable { value, name, .. } => {
            let mut scope = LiftScope {
                in_scope: st.top.clone(),
                enclosing_fn: name.clone(),
            };
            lift_expr(value, &mut scope, st);
        }
        Item::Expr(e) => {
            // Top-level expression — Sprint 12+ eval-entry uses
            // "<eval-entry>" as the synthetic enclosing function name.
            let mut scope = LiftScope {
                in_scope: st.top.clone(),
                enclosing_fn: "<eval-entry>".to_string(),
            };
            lift_expr(e, &mut scope, st);
        }
        // DefineClass: nested exprs in supers / slot defaults aren't
        // currently supported as expression-position method literals;
        // skip. DefineGeneric / DefineLibrary / DefineModule /
        // DefineMacro / DefineOther — no expression bodies to lift.
        _ => {}
    }
}

fn lift_statement(
    s: &mut Statement,
    scope: &mut LiftScope,
    st: &mut LiftState<'_>,
) {
    match s {
        Statement::Expr(e) => {
            lift_expr(e, scope, st);
        }
        Statement::Let { binders, value, .. } => {
            lift_expr(value, scope, st);
            for b in binders {
                scope.in_scope.insert(b.name.clone());
            }
        }
        Statement::Local { methods, .. } => {
            // `local method NAME (params) body end` — lift each local
            // method to a top-level `DefineFunction` (sharing the
            // closure/cell machinery used by anonymous `method` literals)
            // and register a `ClosureInfo`. The local-method names are
            // bound in the enclosing scope; references to a sibling or to
            // itself become captures (enabling self / mutual recursion),
            // so they are recorded as cell-promoted locals of the
            // enclosing function. The `Statement::Local` node is left in
            // place; the lowering pass emits, per method:
            //   1. a fresh cell bound to NAME (created up-front for the
            //      whole group so mutual references see live cells),
            //   2. a `%make-closure` capturing those cells,
            //   3. a `%cell-set!` storing the closure into NAME's cell.
            let local_names: HashSet<String> =
                methods.iter().map(|m| m.name.clone()).collect();
            // The local-method names are visible to siblings and to
            // themselves; add them to the enclosing scope first.
            for n in &local_names {
                scope.in_scope.insert(n.clone());
            }
            for m in methods.iter_mut() {
                // Free-var walk for this method's body. inner_scope is
                // the lifted body's own scope (top + its params); a name
                // referenced from the body that lives in the enclosing
                // scope (which now includes sibling/self local-method
                // names) is a capture.
                let mut inner_scope: HashSet<String> = st.top.clone();
                for p in m.params.iter() {
                    if let Some(b) = param_binding_name(&p.name) {
                        inner_scope.insert(b.to_string());
                    }
                }
                let mut free_seq: Vec<(Span, String)> = Vec::new();
                for sub in m.body.iter() {
                    check_free_vars_in_stmt(
                        sub,
                        &mut inner_scope,
                        &scope.in_scope,
                        st.top,
                        &mut free_seq,
                    );
                }
                let mut captured: Vec<String> = Vec::new();
                let mut seen: HashSet<String> = HashSet::new();
                for (_, n) in &free_seq {
                    if seen.insert(n.clone()) {
                        captured.push(n.clone());
                    }
                }

                // Every captured name must be cell-promoted in the
                // enclosing function (its own locals/params become cells;
                // sibling/self local-method names are cells holding the
                // closure Word).
                if !captured.is_empty() {
                    let bucket = st
                        .registry
                        .cell_locals_per_function
                        .entry(scope.enclosing_fn.clone())
                        .or_default();
                    for c in &captured {
                        bucket.insert(c.clone());
                    }
                }
                // The local method's OWN name is always cell-promoted —
                // even with an empty capture set — because the enclosing
                // body stores the freshly-made closure into that cell and
                // call sites read it back through `%cell-get`.
                st.registry
                    .cell_locals_per_function
                    .entry(scope.enclosing_fn.clone())
                    .or_default()
                    .insert(m.name.clone());

                // Lift the body to a top-level function under a unique
                // name (the source name may repeat across enclosing
                // methods). The closure is registered under this lifted
                // name; the `Statement::Local` lowering looks it up by
                // the source name via `local_lifted_name`.
                let id = ANON_METHOD_COUNTER.fetch_add(1, Ordering::SeqCst);
                let lifted_name = format!("__local-method-{}-{}", m.name, id);

                let mut new_fn = Item::DefineFunction {
                    span: m.span,
                    modifiers: Vec::new(),
                    name: lifted_name.clone(),
                    params: m.params.clone(),
                    return_: m.return_.clone(),
                    body: m.body.clone(),
                };
                // Recursively lift nested method literals / local methods
                // inside this body. The new enclosing function is the
                // lifted body itself.
                lift_item(&mut new_fn, st);
                st.new_items.push(new_fn);

                st.registry.by_lifted_name.insert(
                    lifted_name.clone(),
                    ClosureInfo {
                        lifted_name: lifted_name.clone(),
                        captured,
                        arity: m.params.len(),
                        rest_fixed: rest_param_index(&m.params),
                        span: m.span,
                    },
                );
                // Record the source-name → lifted-name mapping for this
                // enclosing function so the lowering pass can find the
                // ClosureInfo from the `Statement::Local` node.
                st.registry
                    .local_lifted_names
                    .entry(scope.enclosing_fn.clone())
                    .or_default()
                    .insert(m.name.clone(), lifted_name);
            }
        }
        Statement::While { cond, body, .. } | Statement::Until { cond, body, .. } => {
            lift_expr(cond, scope, st);
            let saved = scope.in_scope.clone();
            for sub in body {
                lift_statement(sub, scope, st);
            }
            scope.in_scope = saved;
        }
        Statement::For { clauses, body, finally_, .. } => {
            // Lift method literals embedded in clause expressions. These are
            // evaluated in the ENCLOSING scope (before the loop variables
            // bind), so lift them before extending in_scope.
            for clause in clauses.iter_mut() {
                match clause {
                    ForClause::Numeric(n) => {
                        lift_expr(&mut n.from, scope, st);
                        if let Some(e) = &mut n.to { lift_expr(e, scope, st); }
                        if let Some(e) = &mut n.below { lift_expr(e, scope, st); }
                        if let Some(e) = &mut n.above { lift_expr(e, scope, st); }
                        if let Some(e) = &mut n.by { lift_expr(e, scope, st); }
                    }
                    ForClause::From(ff) => {
                        lift_expr(&mut ff.from, scope, st);
                        if let Some(e) = &mut ff.by { lift_expr(e, scope, st); }
                    }
                    ForClause::Step(s) => {
                        lift_expr(&mut s.init, scope, st);
                        if let Some(e) = &mut s.next { lift_expr(e, scope, st); }
                    }
                    ForClause::While { cond, .. } | ForClause::Until { cond, .. } => {
                        lift_expr(cond, scope, st);
                    }
                    ForClause::In { coll, .. } | ForClause::Keyed { coll, .. } => {
                        lift_expr(coll, scope, st);
                    }
                }
            }
            // Bind the loop variables, then lift the body + finally clause.
            let saved = scope.in_scope.clone();
            for clause in clauses.iter() {
                match clause {
                    ForClause::Numeric(n) => { scope.in_scope.insert(n.var.clone()); }
                    ForClause::From(ff) => { scope.in_scope.insert(ff.var.clone()); }
                    ForClause::Step(s) => { scope.in_scope.insert(s.var.clone()); }
                    ForClause::In { var, .. } => { scope.in_scope.insert(var.clone()); }
                    ForClause::Keyed { var, key, .. } => {
                        scope.in_scope.insert(var.clone());
                        scope.in_scope.insert(key.clone());
                    }
                    ForClause::While { .. } | ForClause::Until { .. } => {}
                }
            }
            for sub in body.iter_mut() {
                lift_statement(sub, scope, st);
            }
            for sub in finally_.iter_mut() {
                lift_statement(sub, scope, st);
            }
            scope.in_scope = saved;
        }
        Statement::Block {
            exit_var,
            body,
            handlers,
            cleanup,
            afterwards,
            ..
        } => {
            let saved = scope.in_scope.clone();
            if let Some(ev) = exit_var {
                scope.in_scope.insert(ev.clone());
            }
            for sub in body {
                lift_statement(sub, scope, st);
            }
            for h in handlers {
                let h_saved = scope.in_scope.clone();
                if let Some(v) = &h.var {
                    scope.in_scope.insert(v.clone());
                }
                for sub in &mut h.body {
                    lift_statement(sub, scope, st);
                }
                scope.in_scope = h_saved;
            }
            for sub in cleanup {
                lift_statement(sub, scope, st);
            }
            for sub in afterwards {
                lift_statement(sub, scope, st);
            }
            scope.in_scope = saved;
        }
    }
}

fn lift_expr(
    e: &mut Expr,
    scope: &mut LiftScope,
    st: &mut LiftState<'_>,
) {
    match e {
        Expr::Integer(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::String(..)
        | Expr::Char(..)
        | Expr::Symbol(..)
        | Expr::Ident(..) => {}
        Expr::Paren { inner, .. } => {
            lift_expr(inner, scope, st);
        }
        Expr::BinOp { lhs, rhs, .. } => {
            lift_expr(lhs, scope, st);
            lift_expr(rhs, scope, st);
        }
        Expr::UnOp { operand, .. } => {
            lift_expr(operand, scope, st);
        }
        Expr::If { cond, then_, else_, .. } => {
            lift_expr(cond, scope, st);
            lift_expr(then_, scope, st);
            if let Some(e2) = else_ {
                lift_expr(e2, scope, st);
            }
        }
        Expr::Begin { body, .. } => {
            let saved = scope.in_scope.clone();
            for sub in body {
                lift_expr(sub, scope, st);
            }
            scope.in_scope = saved;
        }
        Expr::Call { callee, args, .. } => {
            lift_expr(callee, scope, st);
            for a in args {
                lift_expr(a, scope, st);
            }
        }
        Expr::Let { binder, value, .. } => {
            lift_expr(value, scope, st);
            scope.in_scope.insert(binder.clone());
        }
        Expr::Case { .. } | Expr::LocalMethod { .. } | Expr::MacroCall { .. } => {
            // Not lowered; leave the unsupported diagnostic to the
            // main lowering pass. `MacroCall` should never reach
            // lowering — the macro engine substitutes it away
            // before lower runs. If we see one here it's a missing
            // macro definition; the diagnostic path catches it.
        }
        Expr::Stmt(s) => {
            lift_statement(s, scope, st);
        }
        Expr::Method { span, params, body } => {
            // Compute the captured set: every Ident referenced inside
            // the method body that isn't (a) one of the method's own
            // params, (b) a top-level name, (c) a fresh `let` binder
            // introduced inside the body, OR (d) an operator / class /
            // generic name. Sprint 24 promotes these to cells; Sprint 21
            // erred out here.
            let mut inner_scope: HashSet<String> = st.top.clone();
            for p in params.iter() {
                if let Some(b) = param_binding_name(&p.name) {
                    inner_scope.insert(b.to_string());
                }
            }
            let mut free_seq: Vec<(Span, String)> = Vec::new();
            for sub in body.iter() {
                check_free_vars(sub, &mut inner_scope, &scope.in_scope, st.top, &mut free_seq);
            }
            // De-duplicate while preserving first-seen order. The order
            // becomes the env-index assignment, so it must be stable.
            let mut captured: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for (_, n) in &free_seq {
                if seen.insert(n.clone()) {
                    captured.push(n.clone());
                }
            }

            // Synthesise a fresh top-level name for the lifted body.
            // Sprint 44: counter is process-global so multi-file
            // builds don't collide on `__anon-method-0` between files.
            let id = ANON_METHOD_COUNTER.fetch_add(1, Ordering::SeqCst);
            let lifted_name = format!("__anon-method-{}", id);

            // Record this closure's captured locals against the
            // enclosing function so the lowerer cell-promotes them.
            if !captured.is_empty() {
                let bucket = st
                    .registry
                    .cell_locals_per_function
                    .entry(scope.enclosing_fn.clone())
                    .or_default();
                for c in &captured {
                    bucket.insert(c.clone());
                }
            }

            // Build the lifted DefineFunction. For closures (non-empty
            // capture set), prepend a synthetic `__env` parameter — the
            // lowerer wires it in when it sees the `ClosureInfo` for
            // this body. We do NOT add the param at AST level (keeps
            // the AST stable for printing); instead, the lowerer reads
            // `ClosureInfo::captured.len() > 0` and inserts the env
            // parameter at the head of the body's params list before
            // lowering proceeds.
            let body_stmts: Vec<Statement> =
                body.iter().cloned().map(Statement::Expr).collect();
            st.new_items.push(Item::DefineFunction {
                span: *span,
                modifiers: Vec::new(),
                name: lifted_name.clone(),
                params: params.clone(),
                return_: None,
                body: body_stmts,
            });

            // Register the closure in the registry. The lowerer
            // consumes this to recognise `Expr::Ident(lifted_name)` as
            // a closure-creation site and to wire the env parameter
            // when lowering the body itself.
            st.registry.by_lifted_name.insert(
                lifted_name.clone(),
                ClosureInfo {
                    lifted_name: lifted_name.clone(),
                    captured: captured.clone(),
                    arity: params.len(),
                    rest_fixed: rest_param_index(params),
                    span: *span,
                },
            );

            // Recursively lift any nested anonymous methods inside the
            // body we just stuffed into the synthetic DefineFunction.
            // Run the pre-pass on it in place; nested closures captured
            // variables that the new enclosing function (lifted_name)
            // owns now.
            let last_idx = st.new_items.len() - 1;
            let mut taken = std::mem::replace(
                &mut st.new_items[last_idx],
                Item::Expr(Expr::Bool(*span, false)),
            );
            lift_item(&mut taken, st);
            st.new_items[last_idx] = taken;

            // Replace the original Method expression with an ident
            // reference to the lifted thunk. The lowerer consults the
            // registry to decide whether to emit `nod_make_function_ref`
            // (no captures) or `%make-closure` (with captures + env).
            *e = Expr::Ident(*span, lifted_name);
        }
    }
}

/// Free-variable walk used inside `lift_expr`'s `Expr::Method` branch.
/// Pushes `(span, name)` into `free` for every Ident in `e` that
/// resolves to a name in `outer_scope` but NOT in `inner_scope` or
/// `top`.
///
/// `inner_scope` starts as `top + method-params` and grows as `let`
/// binders are introduced inside the body.
fn check_free_vars(
    e: &Expr,
    inner_scope: &mut HashSet<String>,
    outer_scope: &HashSet<String>,
    top: &HashSet<String>,
    free: &mut Vec<(Span, String)>,
) {
    match e {
        Expr::Integer(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::String(..)
        | Expr::Char(..)
        | Expr::Symbol(..) => {}
        Expr::Ident(span, name) => {
            // Sprint 21 free-var check: any Ident NOT in inner_scope
            // AND that exists in outer_scope is a capture. Idents that
            // resolve to a registered class / runtime generic stay OK.
            if inner_scope.contains(name) || top.contains(name) {
                return;
            }
            // Operator shims (`+`, `-`, ...) are always available.
            if operator_arity(name).is_some() {
                return;
            }
            // Registered classes (`<integer>`, `<error>`, ...).
            if name.starts_with('<') && name.ends_with('>') {
                return;
            }
            // Registered runtime generics (stdlib `size`, ...).
            if nod_runtime::is_generic_defined(name) {
                return;
            }
            if outer_scope.contains(name) {
                free.push((*span, name.clone()));
            }
            // If neither inner_scope nor outer_scope binds it, leave
            // the diagnostic to the main lowering pass (it'll surface
            // an UndefinedIdent).
        }
        Expr::Paren { inner, .. } => check_free_vars(inner, inner_scope, outer_scope, top, free),
        Expr::BinOp { lhs, rhs, .. } => {
            check_free_vars(lhs, inner_scope, outer_scope, top, free);
            check_free_vars(rhs, inner_scope, outer_scope, top, free);
        }
        Expr::UnOp { operand, .. } => {
            check_free_vars(operand, inner_scope, outer_scope, top, free);
        }
        Expr::If { cond, then_, else_, .. } => {
            check_free_vars(cond, inner_scope, outer_scope, top, free);
            check_free_vars(then_, inner_scope, outer_scope, top, free);
            if let Some(e2) = else_ {
                check_free_vars(e2, inner_scope, outer_scope, top, free);
            }
        }
        Expr::Begin { body, .. } => {
            let saved = inner_scope.clone();
            for sub in body {
                check_free_vars(sub, inner_scope, outer_scope, top, free);
            }
            *inner_scope = saved;
        }
        Expr::Call { callee, args, .. } => {
            check_free_vars(callee, inner_scope, outer_scope, top, free);
            for a in args {
                check_free_vars(a, inner_scope, outer_scope, top, free);
            }
        }
        Expr::Let { binder, value, .. } => {
            check_free_vars(value, inner_scope, outer_scope, top, free);
            inner_scope.insert(binder.clone());
        }
        Expr::Case { .. } | Expr::LocalMethod { .. } | Expr::MacroCall { .. } => {}
        Expr::Method { params, body, .. } => {
            // Nested anonymous method: its own params extend the inner
            // scope; the outer scope is unchanged for the recursive walk
            // (the nested method's free variables vs its enclosing
            // method's scope is what we want — same outer_scope).
            let mut nested_inner = inner_scope.clone();
            for p in params {
                if let Some(b) = param_binding_name(&p.name) {
                    nested_inner.insert(b.to_string());
                }
            }
            for sub in body {
                check_free_vars(sub, &mut nested_inner, outer_scope, top, free);
            }
        }
        Expr::Stmt(s) => check_free_vars_in_stmt(s, inner_scope, outer_scope, top, free),
    }
}

fn check_free_vars_in_stmt(
    s: &Statement,
    inner_scope: &mut HashSet<String>,
    outer_scope: &HashSet<String>,
    top: &HashSet<String>,
    free: &mut Vec<(Span, String)>,
) {
    match s {
        Statement::Expr(e) => check_free_vars(e, inner_scope, outer_scope, top, free),
        Statement::Let { binders, value, .. } => {
            check_free_vars(value, inner_scope, outer_scope, top, free);
            for b in binders {
                inner_scope.insert(b.name.clone());
            }
        }
        Statement::Local { methods, .. } => {
            for m in methods {
                inner_scope.insert(m.name.clone());
            }
        }
        Statement::While { cond, body, .. } | Statement::Until { cond, body, .. } => {
            check_free_vars(cond, inner_scope, outer_scope, top, free);
            let saved = inner_scope.clone();
            for sub in body {
                check_free_vars_in_stmt(sub, inner_scope, outer_scope, top, free);
            }
            *inner_scope = saved;
        }
        Statement::For { clauses, body, finally_, .. } => {
            // Init / bound / collection expressions evaluate in the enclosing
            // scope (before the loop variables bind).
            for clause in clauses.iter() {
                match clause {
                    ForClause::Numeric(n) => {
                        check_free_vars(&n.from, inner_scope, outer_scope, top, free);
                        if let Some(e) = &n.to { check_free_vars(e, inner_scope, outer_scope, top, free); }
                        if let Some(e) = &n.below { check_free_vars(e, inner_scope, outer_scope, top, free); }
                        if let Some(e) = &n.above { check_free_vars(e, inner_scope, outer_scope, top, free); }
                        if let Some(e) = &n.by { check_free_vars(e, inner_scope, outer_scope, top, free); }
                    }
                    ForClause::From(ff) => {
                        check_free_vars(&ff.from, inner_scope, outer_scope, top, free);
                        if let Some(e) = &ff.by { check_free_vars(e, inner_scope, outer_scope, top, free); }
                    }
                    ForClause::Step(s) => {
                        check_free_vars(&s.init, inner_scope, outer_scope, top, free);
                    }
                    ForClause::In { coll, .. } | ForClause::Keyed { coll, .. } => {
                        check_free_vars(coll, inner_scope, outer_scope, top, free);
                    }
                    ForClause::While { .. } | ForClause::Until { .. } => {}
                }
            }
            let saved = inner_scope.clone();
            for clause in clauses.iter() {
                match clause {
                    ForClause::Numeric(n) => { inner_scope.insert(n.var.clone()); }
                    ForClause::From(ff) => { inner_scope.insert(ff.var.clone()); }
                    ForClause::Step(s) => { inner_scope.insert(s.var.clone()); }
                    ForClause::In { var, .. } => { inner_scope.insert(var.clone()); }
                    ForClause::Keyed { var, key, .. } => {
                        inner_scope.insert(var.clone());
                        inner_scope.insert(key.clone());
                    }
                    ForClause::While { .. } | ForClause::Until { .. } => {}
                }
            }
            // Step `next` and while/until conditions reference the loop vars.
            for clause in clauses.iter() {
                match clause {
                    ForClause::Step(s) => {
                        if let Some(e) = &s.next { check_free_vars(e, inner_scope, outer_scope, top, free); }
                    }
                    ForClause::While { cond, .. } | ForClause::Until { cond, .. } => {
                        check_free_vars(cond, inner_scope, outer_scope, top, free);
                    }
                    _ => {}
                }
            }
            for sub in body {
                check_free_vars_in_stmt(sub, inner_scope, outer_scope, top, free);
            }
            for sub in finally_ {
                check_free_vars_in_stmt(sub, inner_scope, outer_scope, top, free);
            }
            *inner_scope = saved;
        }
        Statement::Block {
            exit_var,
            body,
            handlers,
            cleanup,
            afterwards,
            ..
        } => {
            let saved = inner_scope.clone();
            if let Some(ev) = exit_var {
                inner_scope.insert(ev.clone());
            }
            for sub in body {
                check_free_vars_in_stmt(sub, inner_scope, outer_scope, top, free);
            }
            for h in handlers {
                let mut h_scope = inner_scope.clone();
                if let Some(v) = &h.var {
                    h_scope.insert(v.clone());
                }
                for sub in &h.body {
                    check_free_vars_in_stmt(sub, &mut h_scope, outer_scope, top, free);
                }
            }
            for sub in cleanup {
                check_free_vars_in_stmt(sub, inner_scope, outer_scope, top, free);
            }
            for sub in afterwards {
                check_free_vars_in_stmt(sub, inner_scope, outer_scope, top, free);
            }
            *inner_scope = saved;
        }
    }
}

fn collect_top_level_names(m: &Module, user_classes: &HashMap<String, ClassId>) -> TopNames {
    let mut fns = HashMap::new();
    let mut fn_arity: HashMap<String, usize> = HashMap::new();
    let mut constants: HashSet<String> = HashSet::new();
    let mut variables: HashSet<String> = HashSet::new();
    let mut rest_fns: HashMap<String, usize> = HashMap::new();
    for item in &m.items {
        match item {
            Item::DefineFunction { name, params, return_, .. } => {
                let ret = return_
                    .as_ref()
                    .and_then(|r| r.values.first().and_then(|v| v.type_.as_ref()))
                    .map(|e| type_from_expr(Some(e)))
                    .unwrap_or(TypeEstimate::Top);
                fns.insert(name.clone(), ret);
                fn_arity.insert(name.clone(), params.len());
                // Sprint 60: record the fixed (pre-`#rest`) param count so
                // call-site lowering can collect the trailing actuals into
                // the rest SOV slot. Also publish it to the process-global
                // `#rest`-callee registry so OTHER modules' call sites
                // (e.g. user code calling the stdlib's `apply`) can collect
                // too — the per-module `rest_fns` only covers this module.
                if let Some(fixed) = rest_param_index(params) {
                    rest_fns.insert(name.clone(), fixed);
                    nod_runtime::register_rest_callee(name, fixed);
                }
            }
            // A 0-parameter `define method` is lowered as a plain
            // direct-call function (see `lower_method_item`), so it must
            // be in `top_names` for call sites to emit a DirectCall.
            // Methods WITH parameters stay dispatched generics and are
            // intentionally not registered here.
            Item::DefineMethod { name, params, return_, .. } if params.is_empty() => {
                let ret = return_
                    .as_ref()
                    .and_then(|r| r.values.first().and_then(|v| v.type_.as_ref()))
                    .map(|e| type_from_expr(Some(e)))
                    .unwrap_or(TypeEstimate::Top);
                fns.insert(name.clone(), ret);
                fn_arity.insert(name.clone(), 0);
            }
            // Sprint 60: a `define method` with a `#rest` parameter. These
            // are produced by the stdlib loader's
            // `rewrite_define_function_to_method` pass (which turns a
            // `define function f (…, #rest r)` into a single-method generic
            // on `<object>` so `f` is reachable from user code) — and could
            // also be hand-written. Such a method stays a DISPATCHED generic
            // (it has specialised params), but its callee ABI is still
            // `fixed + 1`: the trailing actuals collect into one `#rest` SOV
            // slot, exactly as for the `define function` form. We record the
            // FIXED (pre-`#rest`) param count so the Dispatch call-site
            // lowering collects the rest actuals before dispatching (see the
            // `rest_fixed_count` branch in `lower_call`). Methods without a
            // `#rest` param fall through and stay ordinary dispatched
            // generics (not registered here).
            Item::DefineMethod { name, params, .. }
                if rest_param_index(params).is_some() =>
            {
                if let Some(fixed) = rest_param_index(params) {
                    rest_fns.insert(name.clone(), fixed);
                    // Publish to the process-global registry so cross-module
                    // call sites collect the trailing actuals before
                    // dispatching (the stdlib `apply` / `compose` / `curry` /
                    // `rcurry` are reached this way from user modules).
                    nod_runtime::register_rest_callee(name, fixed);
                }
            }
            // GAP-002 fix + GAP-004: `define constant` and
            // `define variable` are both lowered as zero-arg "getter"
            // functions whose body returns the initial / current value.
            // We register both in `top_names` with arity 0 so bareword
            // references in expression position emit a zero-arg
            // DirectCall (evaluates the constant / loads the cell).
            //
            // We separate the two sets here because `lower_assign`
            // needs to tell them apart: assigning to a variable goes
            // through the cell-set path; assigning to a constant is an
            // error.
            Item::DefineConstant { name, .. } => {
                fns.insert(name.clone(), TypeEstimate::Top);
                fn_arity.insert(name.clone(), 0);
                constants.insert(name.clone());
            }
            Item::DefineVariable { name, .. } => {
                fns.insert(name.clone(), TypeEstimate::Top);
                fn_arity.insert(name.clone(), 0);
                variables.insert(name.clone());
            }
            _ => {}
        }
    }
    // Slot accessors are emitted as top-level functions too; record
    // them so `<C>-getter-foo(p)` resolves to a DirectCall. For MI
    // override accessors (`<C>-override-getter-foo`) — also include
    // them.
    for item in &m.items {
        let Item::DefineClass { name, .. } = item else {
            continue;
        };
        let Some(&class_id) = user_classes.get(name) else {
            continue;
        };
        let md_ptr = nod_runtime::class_metadata_ptr(class_id);
        if md_ptr.is_null() {
            continue;
        }
        // SAFETY: registered class, static-area metadata.
        let metadata = unsafe { &*md_ptr };
        for (idx, slot) in metadata.slots.iter().enumerate() {
            let origin = metadata.slot_origin[idx];
            if origin == class_id {
                let getter = format!("{}-getter-{}", name, slot.name);
                fns.insert(getter.clone(), slot_type_to_estimate(slot.type_kind));
                fn_arity.insert(getter, 1);
                if slot.has_setter {
                    let setter = format!("{}-setter-{}", name, slot.name);
                    fns.insert(setter.clone(), TypeEstimate::Top);
                    fn_arity.insert(setter, 2);
                }
            } else {
                // Inherited slot — if Phase 3 will generate an override
                // (offset differs vs. defining-class layout), the
                // override function needs to be in `top_names` too.
                let origin_md_ptr = nod_runtime::class_metadata_ptr(origin);
                if origin_md_ptr.is_null() {
                    continue;
                }
                // SAFETY: static-area metadata.
                let origin_md = unsafe { &*origin_md_ptr };
                let origin_offset = origin_md
                    .slots
                    .iter()
                    .find(|s| s.name == slot.name)
                    .map(|s| s.offset)
                    .unwrap_or(slot.offset);
                if origin_offset != slot.offset {
                    let getter = format!("{}-override-getter-{}", name, slot.name);
                    fns.insert(getter.clone(), slot_type_to_estimate(slot.type_kind));
                    fn_arity.insert(getter, 1);
                    if slot.has_setter {
                        let setter = format!("{}-override-setter-{}", name, slot.name);
                        fns.insert(setter.clone(), TypeEstimate::Top);
                        fn_arity.insert(setter, 2);
                    }
                }
            }
        }
    }
    TopNames {
        fns,
        fn_arity,
        constants,
        variables,
        rest_fns,
    }
}

fn collect_generic_names(m: &Module) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &m.items {
        match item {
            Item::DefineGeneric { name, .. } => {
                out.insert(name.clone());
            }
            // A 0-parameter method is a plain direct-call function, not a
            // dispatched generic — keep it out of the generic set so a
            // bareword/`\name` reference doesn't route through Dispatch.
            Item::DefineMethod { name, params, .. } if !params.is_empty() => {
                out.insert(name.clone());
            }
            Item::DefineClass { name, .. } => {
                // Auto-generated slot accessors are generics (registered
                // by name into the dispatch table). Adding them here
                // ensures `x(p)` lowers to Dispatch when the function
                // table isn't sufficient (e.g. cross-class methods).
                //
                // For MI: every slot — own or inherited — belongs to a
                // generic with the slot's name. The dispatch picks the
                // right per-class method (override or parent's).
                if let Some(class_id) = resolve_class_id_by_name(name) {
                    let md_ptr = nod_runtime::class_metadata_ptr(class_id);
                    if !md_ptr.is_null() {
                        // SAFETY: registered class.
                        let metadata = unsafe { &*md_ptr };
                        for slot in &metadata.slots {
                            out.insert(slot.name.clone());
                            if slot.has_setter {
                                out.insert(format!("{}-setter", slot.name));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ─── Type-expr → TypeEstimate ────────────────────────────────────────────

fn type_from_expr(ty: Option<&Expr>) -> TypeEstimate {
    let Some(ty) = ty else { return TypeEstimate::Top };
    match ty {
        Expr::Ident(_, n) => match n.as_str() {
            "<integer>" => TypeEstimate::Integer,
            "<single-float>" => TypeEstimate::SingleFloat,
            "<double-float>" | "<float>" => TypeEstimate::DoubleFloat,
            "<boolean>" => TypeEstimate::Boolean,
            "<character>" => TypeEstimate::Character,
            "<string>" | "<byte-string>" => TypeEstimate::String,
            "<object>" | "<top>" => TypeEstimate::Top,
            // Sprint 15 method-specialiser narrowing: a `<foo>`-shaped
            // type ident that resolves to a registered class lights up
            // as `Class(<foo>)`. The dispatch resolver consults this
            // alongside the sealing facts to pick sealed-direct.
            //
            // For unregistered classes we fall back to `Top` (the
            // parameter type is informational only — codegen lowers
            // it as a tagged Word regardless of estimate). The
            // narrowing pass / resolver simply skips temps with `Top`.
            other if other.starts_with('<') && other.ends_with('>') => {
                match resolve_class_id_by_name(other) {
                    Some(id) => TypeEstimate::Class(id.0),
                    None => TypeEstimate::Top,
                }
            }
            _ => TypeEstimate::Top,
        },
        _ => TypeEstimate::Top,
    }
}

// ─── Function builder ────────────────────────────────────────────────────

struct FunctionBuilder {
    func: Function,
    current: usize,
    next_temp: u32,
    next_block: u32,
    /// Sprint 19: last value-producing temp in this function, used by
    /// `lower_statements_into` (the block-lifting helper) to know what
    /// to return from a lifted thunk. Updated as statements lower.
    /// `None` after a statement that produces no value (loops).
    last_temp: Option<TempId>,
    /// Sprint 24: cell-promotion + closure-env context for this body.
    /// Populated by `lower_function_inner` before any statement runs.
    /// The `lower_expr` / `lower_assign` / `let`-statement paths
    /// consult this to redirect captured-local reads/writes through
    /// `%cell-get` / `%cell-set!` and to lower a `%env-cell` indirection
    /// for variables that live in the enclosing environment.
    cell_ctx: CellCtx,
    /// Key used to look this body up in the `ClosureRegistry`
    /// (`cell_locals_per_function`, `local_lifted_names`). For
    /// `define function` and lifted thunks this equals `func.name`; for
    /// `define method` it is the SOURCE name (`func.name` is the mangled
    /// `name$specialisers`, but the lift pre-pass keys on the source
    /// name). Set by `lower_function_inner`.
    closure_key: String,
    /// Sink for lifted thunks (block stages, etc.) that may be produced
    /// while lowering an expression — specifically a `block … end` that
    /// appears in EXPRESSION position (`Expr::Stmt(Block)`), where the
    /// statement-loop's `sink` parameter isn't reachable. The body-level
    /// statement loop moves the caller's sink in here on entry and merges
    /// it back out on finish (see `lower_function_inner_keyed`). Helpers
    /// that need it (`take_sink`/`restore_sink`) borrow it temporarily so
    /// `lower_block_form` can take a `&mut LiftSink` distinct from `self`.
    pending_sink: Option<LiftSink>,
}

impl FunctionBuilder {
    fn new(id: FunctionId, name: String, span: Span) -> Self {
        let entry = BlockId(0);
        let func = Function {
            id,
            name,
            params: Vec::new(),
            entry,
            blocks: vec![Block {
                id: entry,
                label: "entry".to_string(),
                params: Vec::new(),
                computations: Vec::new(),
                terminator: Terminator::Return { value: None },
            }],
            temps: Vec::new(),
            return_type: TypeEstimate::Unit,
            span,
        };
        let closure_key = func.name.clone();
        Self {
            func,
            current: 0,
            next_temp: 0,
            next_block: 1,
            last_temp: None,
            cell_ctx: CellCtx::default(),
            closure_key,
            pending_sink: None,
        }
    }

    /// Lower a `block … end` form that appears in EXPRESSION position.
    /// Borrows the body-level sink (moved into `self.pending_sink` by
    /// `lower_function_inner_keyed`) so `lower_block_form` can take a
    /// `&mut LiftSink` separate from `self`.
    #[allow(clippy::too_many_arguments)]
    fn lower_block_in_expr(
        &mut self,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
        span: Span,
        exit_var: Option<&str>,
        body: &[Statement],
        handlers: &[nod_reader::ExceptionClause],
        cleanup: &[Statement],
        afterwards: &[Statement],
    ) -> Result<TempId, LoweringError> {
        let Some(mut sink) = self.pending_sink.take() else {
            return Err(LoweringError::Unsupported {
                span,
                message: "`block` in expression position needs a lift sink (internal)"
                    .to_string(),
            });
        };
        let parent_name = self.func.name.clone();
        let result = lower_block_form(
            self, &mut sink, env, ctx, span, &parent_name, exit_var, body, handlers,
            cleanup, afterwards,
        );
        self.pending_sink = Some(sink);
        result
    }

    fn finish(self) -> Function {
        self.func
    }

    fn last_temp(&self) -> Option<TempId> {
        self.last_temp
    }

    fn set_last_temp(&mut self, t: TempId) {
        self.last_temp = Some(t);
    }

    fn clear_last_temp(&mut self) {
        self.last_temp = None;
    }

    fn fresh_temp(&mut self, ty: TypeEstimate) -> TempId {
        let id = TempId(self.next_temp);
        self.next_temp += 1;
        self.func.temps.push(Temporary {
            id,
            type_estimate: ty,
        });
        id
    }

    fn new_block(&mut self, label: String) -> BlockId {
        let id = BlockId(self.next_block);
        self.next_block += 1;
        self.func.blocks.push(Block {
            id,
            label,
            params: Vec::new(),
            computations: Vec::new(),
            terminator: Terminator::Return { value: None },
        });
        id
    }

    fn block_mut(&mut self, id: BlockId) -> &mut Block {
        self.func
            .blocks
            .iter_mut()
            .find(|b| b.id == id)
            .expect("block not found")
    }

    fn switch_to(&mut self, id: BlockId) {
        self.current = self
            .func
            .blocks
            .iter()
            .position(|b| b.id == id)
            .expect("block not found");
    }

    fn push(&mut self, c: Computation) {
        self.func.blocks[self.current].computations.push(c);
    }

    fn terminate_current(&mut self, t: Terminator) {
        self.func.blocks[self.current].terminator = t;
    }

    fn add_block_param(&mut self, block: BlockId, ty: TypeEstimate) -> TempId {
        let t = self.fresh_temp(ty);
        self.block_mut(block).params.push(t);
        t
    }

    // ─── Expression lowering ────────────────────────────────────────────

    fn lower_expr(
        &mut self,
        e: &Expr,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<TempId, LoweringError> {
        match e {
            Expr::Integer(span, v) => {
                const FIXNUM_MIN_I128: i128 = -(1_i128 << 62);
                const FIXNUM_MAX_I128: i128 = (1_i128 << 62) - 1;
                if *v < FIXNUM_MIN_I128 || *v > FIXNUM_MAX_I128 {
                    return Err(LoweringError::IntegerOverflow {
                        span: *span,
                        value: *v,
                    });
                }
                let t = self.fresh_temp(TypeEstimate::Integer);
                self.push(Computation::Const {
                    dst: t,
                    value: ConstValue::Integer(*v),
                });
                Ok(t)
            }
            Expr::Float(_, v) => {
                let t = self.fresh_temp(TypeEstimate::DoubleFloat);
                self.push(Computation::Const {
                    dst: t,
                    value: ConstValue::Float(*v),
                });
                Ok(t)
            }
            Expr::Bool(_, v) => {
                let t = self.fresh_temp(TypeEstimate::Boolean);
                self.push(Computation::Const {
                    dst: t,
                    value: ConstValue::Bool(*v),
                });
                Ok(t)
            }
            Expr::String(_, raw) => {
                let decoded = decode_dylan_string_literal(raw);
                let t = self.fresh_temp(TypeEstimate::String);
                self.push(Computation::Const {
                    dst: t,
                    value: ConstValue::String(decoded),
                });
                Ok(t)
            }
            Expr::Char(_, c) => {
                let t = self.fresh_temp(TypeEstimate::Character);
                self.push(Computation::Const {
                    dst: t,
                    value: ConstValue::Char(*c),
                });
                Ok(t)
            }
            Expr::Symbol(_, raw) => {
                // Sprint 22: symbol literals. The parser delivers the
                // raw token text. Three surface forms to normalise:
                //   * `#"foo"` → `"foo"`
                //   * `#:foo`  → `"foo"`
                //   * `foo:`   → `"foo"`
                let name = if let Some(s) = raw
                    .strip_prefix("#\"")
                    .and_then(|s| s.strip_suffix('"'))
                {
                    s.to_string()
                } else if let Some(s) = raw.strip_prefix("#:") {
                    s.to_string()
                } else {
                    raw.trim_end_matches(':').to_string()
                };
                Ok(self.emit_symbol_literal(&name))
            }
            Expr::Ident(span, name) => {
                // Sprint 24: closure body capture — an inner-method
                // body that captures `name` from its outer scope reads
                // it through `%cell-get(%env-cell(env, idx))`.
                if let Some(ec) = self.cell_ctx.env_captures.clone()
                    && let Some(&idx) = ec.index_of.get(name)
                {
                    return Ok(self.emit_captured_var_read(ec.env_temp, idx));
                }
                if let Some(t) = env.get(name).copied() {
                    // Sprint 24: cell-promoted local — the env binds
                    // the CELL Word. Insert a `%cell-get` to read the
                    // underlying value.
                    if self.cell_ctx.cell_locals.contains(name) {
                        let dst = self.fresh_temp(TypeEstimate::Top);
                        self.push(Computation::DirectCall {
                            dst,
                            callee: "nod_cell_get".to_string(),
                            args: vec![t],
                            safepoint_roots: Vec::new(), is_no_alloc: false,
                        });
                        return Ok(dst);
                    }
                    return Ok(t);
                }
                // Sprint 29: stdlib-curated integer constant. The
                // `$MB-OK`, `$WM-PAINT`, … set (and any future stdlib
                // `define constant N = <int>`) lives in a process-
                // global map populated by the stdlib loader. Resolve
                // it here BEFORE the function-ref fallback path so
                // user code reads the constant as a literal integer,
                // not a `<function>` Word.
                //
                // Local bindings shadow the stdlib constant (the
                // `env.get` check above happens first), matching how
                // every other resolution-order step behaves.
                if let Some(v) = crate::stdlib::lookup_constant(name) {
                    const FIXNUM_MIN_I128: i128 = -(1_i128 << 62);
                    const FIXNUM_MAX_I128: i128 = (1_i128 << 62) - 1;
                    if !(FIXNUM_MIN_I128..=FIXNUM_MAX_I128).contains(&v) {
                        return Err(LoweringError::IntegerOverflow {
                            span: *span,
                            value: v,
                        });
                    }
                    let t = self.fresh_temp(TypeEstimate::Integer);
                    self.push(Computation::Const {
                        dst: t,
                        value: ConstValue::Integer(v),
                    });
                    return Ok(t);
                }
                // stdlib-curated float constant (`$single-pi`, `$double-e`, …):
                // resolve to a literal float, same as the integer path above.
                if let Some(v) = crate::stdlib::lookup_float_constant(name) {
                    let t = self.fresh_temp(TypeEstimate::DoubleFloat);
                    self.push(Computation::Const {
                        dst: t,
                        value: ConstValue::Float(v),
                    });
                    return Ok(t);
                }
                // Sprint 12: a `<foo>`-shaped ident may refer to a
                // registered class. Lower as a constant pointer to
                // the class metadata (i.e. a tagged Word).
                if name.starts_with('<')
                    && name.ends_with('>')
                    && let Some(class_id) = ctx.user_classes.get(name).copied().or_else(|| resolve_class_id_by_name(name))
                {
                    return Ok(self.emit_class_ref(class_id));
                }
                // Sprint 24: closure-creation site. The lift pre-pass
                // rewrites `method (...) ... end` to
                // `Expr::Ident(__anon-method-NNNN)`; if that name is in
                // the closure registry AND has a non-empty capture set,
                // emit `%make-closure(name, arity, env)` here. The env
                // is built from the captured locals — each one's
                // cell-Word lives in the current `LocalEnv` as the
                // result of cell-promotion at its `let` (or param)
                // binding site.
                // A rest-closure ALSO takes this path even with NO captures:
                // it must be tagged FUNCTION_KIND_CLOSURE_REST (via
                // emit_make_closure / nod_make_rest_closure), not lowered to
                // a plain function-ref (kind-tag=0), or the funcall shims
                // would apply the exact-arity check and crash on a variadic
                // call. With no captures `captured_cells` is empty and the
                // env is an empty <environment>.
                if let Some(reg) = ctx.closures
                    && let Some(info) = reg.closure_for(name)
                    && (!info.captured.is_empty() || info.rest_fixed.is_some())
                {
                    let mut captured_cells: Vec<TempId> = Vec::with_capacity(info.captured.len());
                    for cap in &info.captured {
                        // Cell lives in `env` because the enclosing
                        // body's cell-promotion logic stored it there.
                        // If for some reason it isn't found, fall
                        // through to UndefinedIdent.
                        let Some(&cell_t) = env.get(cap) else {
                            return Err(LoweringError::UndefinedIdent {
                                span: *span,
                                name: cap.clone(),
                            });
                        };
                        captured_cells.push(cell_t);
                    }
                    return Ok(self.emit_make_closure(
                        name,
                        info.arity,
                        &captured_cells,
                        info.rest_fixed.is_some(),
                    ));
                }
                // GAP-002 fix: a bareword reference to a `define
                // constant` or `define variable` name should EVALUATE
                // it (call the zero-arg function body that returns the
                // constant's value), not produce a function-reference.
                // Dylan constants/variables are *values*, not callable
                // refs — `format-out("%d", $magic)` must pass the
                // integer value, not the function-ref Word.
                //
                // We check this BEFORE the make-function-ref paths
                // because both `top_names.arity()` and `top_names
                // .contains()` would otherwise match (the constant
                // IS registered as a zero-arg function for codegen
                // purposes) and emit the wrong shape.
                if ctx.top_names.is_constant_or_variable(name) {
                    let dst = self.fresh_temp(TypeEstimate::Top);
                    self.push(Computation::DirectCall {
                        dst,
                        callee: name.clone(),
                        args: Vec::new(),
                        safepoint_roots: Vec::new(), is_no_alloc: false,
                    });
                    return Ok(dst);
                }
                // Sprint 21: first-class function references.
                //
                // An ident in expression position that resolves to a
                // registered function (top-level / slot accessor / stdlib
                // method / operator shim) lowers to
                // `nod_make_function_ref(name, arity)`.
                //
                // Arity resolution priority:
                //   1. `top_names::arity(name)` — user functions and
                //      slot accessors in THIS module.
                //   2. operator shims — fixed arity-2.
                //   3. generics — pick the first registered method's
                //      param count via the dispatch table.
                if let Some(arity) = ctx.top_names.arity(name) {
                    return Ok(self.emit_make_function_ref(name, arity));
                }
                if let Some(arity) = operator_arity(name) {
                    return Ok(self.emit_make_function_ref(name, arity));
                }
                if ctx.generics.contains(name) || nod_runtime::is_generic_defined(name) {
                    // Read the arity from the first method registered
                    // under this generic, if any. For stdlib methods
                    // rewritten as `f (x :: <object>, …)`, the param
                    // count IS the arity.
                    let arity = nod_runtime::find_generic(name)
                        .and_then(|g| g.first_method_param_count())
                        .unwrap_or(1);
                    return Ok(self.emit_make_function_ref(name, arity));
                }
                if ctx.top_names.contains(name) {
                    // Should be reachable only if arity lookup somehow
                    // failed; fall back to arity 1 so we don't crash.
                    return Ok(self.emit_make_function_ref(name, 1));
                }
                Err(LoweringError::UndefinedIdent {
                    span: *span,
                    name: name.clone(),
                })
            }
            Expr::Paren { inner, .. } => self.lower_expr(inner, env, ctx),
            Expr::BinOp { op, lhs, rhs, span } => {
                if *op == BinOp::Assign {
                    return self.lower_assign(lhs, rhs, *span, env, ctx);
                }
                // Task #251 — Dylan `|` (or) and `&` (and) are
                // SHORT-CIRCUIT, not bitwise. `a | b` evaluates `a`,
                // returns it if true (anything not `#f`), else returns
                // `b`. `a & b` evaluates `a`, returns it if false, else
                // returns `b`. Both can produce ANY Word, not just
                // `#t`/`#f`. We lower these to a 3-block CFG (mirrors
                // `lower_if`) so the right operand only runs when
                // needed — required for correctness when the right
                // side has side effects or would fault if reached
                // unconditionally (e.g. `until (i = n | element(bs, i)
                // = 10)` indexes out of range when `i = n`).
                if matches!(*op, BinOp::Or | BinOp::And) {
                    return self.lower_short_circuit(*op, lhs, rhs, env, ctx);
                }
                let l = self.lower_expr(lhs, env, ctx)?;
                let r = self.lower_expr(rhs, env, ctx)?;
                let lt = self.func.temp_type(l);
                let rt = self.func.temp_type(r);
                // Sprint 42a — generic `=` dispatch for non-numeric operands.
                // When both operands are pointer-shaped (neither statically
                // `<integer>` nor any float), `=`/`==`/`~=`/`~==` route
                // through `%object-equal?` so byte-strings, symbols, and
                // other heap objects get content equality instead of
                // pointer-compare. The Rust shim (`nod_object_equal_p`)
                // checks raw-bit identity first, so fixnum-tagged Words
                // (which carry their value in the bits) round-trip
                // identically to `PrimOp::EqInt`. We invert via `BoolNot`
                // for the negative operators.
                //
                // The integer / float fast paths below stay exactly as
                // they were — this only diverts when neither operand has
                // a known numeric estimate.
                //
                // `<character>` is carved out: a char lowers to a raw i32
                // holding its code (not a tagged Word), so passing it to
                // the i64-ABI `nod_object_equal_p` shim mis-types the call
                // (i32 vs i64 verify error) AND the shim's raw-bit compare
                // would read the i32 as a 64-bit pattern. Instead chars
                // fall through to `select_binop`, which emits an inline
                // `EqInt`/`NeInt` — both operands are same-width i32, so
                // `build_int_compare` matches them directly. Char codes
                // are unique per character, so bitwise `=` IS identity.
                //
                // Fires only when BOTH operands are `<character>`. A mixed
                // `<character>` = `<integer>` comparison stays on the
                // generic path (the i32 char is widened to i64 at the call
                // boundary in codegen) so it returns a well-defined `#f`
                // — a char is never `=` to an integer — instead of an
                // illegal mismatched-width `EqInt`.
                let char_cmp = matches!(lt, TypeEstimate::Character)
                    && matches!(rt, TypeEstimate::Character);
                if matches!(*op, BinOp::Eq | BinOp::EqEq | BinOp::Ne | BinOp::NeEq)
                    && !char_cmp
                    && !lt.is_integer()
                    && !lt.is_float()
                    && !rt.is_integer()
                    && !rt.is_float()
                {
                    let eq_dst = self.fresh_temp(TypeEstimate::Boolean);
                    self.push(Computation::DirectCall {
                        dst: eq_dst,
                        callee: "nod_object_equal_p".to_string(),
                        args: vec![l, r],
                        safepoint_roots: Vec::new(), is_no_alloc: false,
                    });
                    if matches!(*op, BinOp::Ne | BinOp::NeEq) {
                        let neg_dst = self.fresh_temp(TypeEstimate::Boolean);
                        self.push(Computation::PrimOp {
                            dst: neg_dst,
                            op: PrimOp::BoolNot,
                            args: vec![eq_dst],
                        });
                        return Ok(neg_dst);
                    }
                    return Ok(eq_dst);
                }
                // `a ^ b` exponentiation has no inline PrimOp — route it to
                // the runtime `nod_op_pow` shim (integer power) by its real
                // symbol so AOT codegen can link it (a callee literally named
                // `^` isn't a linkable symbol).
                if *op == BinOp::Pow {
                    let dst = self.fresh_temp(TypeEstimate::Integer);
                    self.push(Computation::DirectCall {
                        dst,
                        callee: "nod_op_pow".to_string(),
                        args: vec![l, r],
                        safepoint_roots: Vec::new(),
                        is_no_alloc: false,
                    });
                    return Ok(dst);
                }
                // Mixed int/float arithmetic: coerce the integer operand to a
                // double-float so both feed the float PrimOp (Dylan's numeric
                // contagion). Only for the arithmetic ops; comparisons handle
                // mixed types on their own float path.
                let mut l = l;
                let mut r = r;
                let mut lt = lt;
                let mut rt = rt;
                if matches!(*op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
                    if lt.is_integer() && rt.is_float() {
                        let f = self.fresh_temp(TypeEstimate::DoubleFloat);
                        self.push(Computation::PrimOp {
                            dst: f,
                            op: PrimOp::IntToFloat,
                            args: vec![l],
                        });
                        l = f;
                        lt = TypeEstimate::DoubleFloat;
                    } else if rt.is_integer() && lt.is_float() {
                        let f = self.fresh_temp(TypeEstimate::DoubleFloat);
                        self.push(Computation::PrimOp {
                            dst: f,
                            op: PrimOp::IntToFloat,
                            args: vec![r],
                        });
                        r = f;
                        rt = TypeEstimate::DoubleFloat;
                    }
                }
                let op = select_binop(*op, lt, rt, *span)?;
                let dst = self.fresh_temp(op.result_type());
                self.push(Computation::PrimOp {
                    dst,
                    op,
                    args: vec![l, r],
                });
                Ok(dst)
            }
            Expr::UnOp { op, operand, span } => {
                let v = self.lower_expr(operand, env, ctx)?;
                let vt = self.func.temp_type(v);
                let op = select_unop(*op, vt, *span)?;
                let dst = self.fresh_temp(op.result_type());
                self.push(Computation::PrimOp {
                    dst,
                    op,
                    args: vec![v],
                });
                Ok(dst)
            }
            Expr::If { cond, then_, else_, span } => {
                // GAP-005 fix: an `if` without an `else` arm returns
                // `#f` per Dylan semantics. Synthesise the missing arm
                // here so `lower_if` doesn't need to know the two
                // shapes apart — both compile to the same 3-block
                // CFG, with the else-arm just yielding the boolean
                // false singleton. Lets users write
                // `if (cond) side-effect end;` for statement-flavour
                // branches without the explicit `else #f` ceremony.
                let synthesized_else;
                let else_ = match else_ {
                    Some(e) => e.as_ref(),
                    None => {
                        synthesized_else = Expr::Bool(*span, false);
                        &synthesized_else
                    }
                };
                self.lower_if(cond, then_, else_, env, ctx)
            }
            Expr::Begin { body, span } => {
                if body.is_empty() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "empty `begin` block not lowered".to_string(),
                    });
                }
                let last_idx = body.len() - 1;
                let mut last = None;
                for (i, e) in body.iter().enumerate() {
                    let t = self.lower_expr(e, env, ctx)?;
                    if i == last_idx {
                        last = Some(t);
                    }
                }
                Ok(last.expect("begin had body"))
            }
            Expr::Call { callee, args, span } => {
                // An operator function-reference used in CALL position —
                // `\=(a, b)`, `\>(x, y)`, or the `select … by \=` desugaring.
                // Lower it identically to the infix form `a op b` (inline
                // PrimOp), instead of a bare DirectCall against the operator
                // symbol (no codegen target). Mirrors how `\op` as a VALUE
                // resolves to the runtime operator shim.
                if let Expr::Ident(_, name) = callee.as_ref() {
                    if args.len() == 2
                        && let Some(op) = binop_from_op_name(name)
                    {
                        let bin = Expr::BinOp {
                            span: *span,
                            op,
                            lhs: Box::new(args[0].clone()),
                            rhs: Box::new(args[1].clone()),
                        };
                        return self.lower_expr(&bin, env, ctx);
                    }
                    if args.len() == 1
                        && let Some(op) = unop_from_op_name(name)
                    {
                        let un = Expr::UnOp {
                            span: *span,
                            op,
                            operand: Box::new(args[0].clone()),
                        };
                        return self.lower_expr(&un, env, ctx);
                    }
                }
                self.lower_call(callee, args, *span, env, ctx)
            }
            Expr::Let { binder, value, .. } => {
                // Sprint 18: lower `let X = E` at expression position
                // — used by macro-emitted `begin let i = … ; while … end end`
                // and by Sprint 03's single-binder `let x = 41; x + 1 end`
                // surface. Inserts the binder into the surrounding env
                // and returns the value temp (so the expression evaluates
                // to the bound value).
                //
                // Sprint 24: if `binder` is captured by an inner closure,
                // promote it to a cell so reads / writes share storage
                // with the env-cell the inner closure accesses.
                let t = self.lower_expr(value, env, ctx)?;
                let bound = if self.cell_ctx.cell_locals.contains(binder) {
                    let cell = self.fresh_temp(TypeEstimate::Top);
                    self.push(Computation::DirectCall {
                        dst: cell,
                        callee: "nod_make_cell".to_string(),
                        args: vec![t],
                        safepoint_roots: Vec::new(), is_no_alloc: false,
                    });
                    cell
                } else {
                    t
                };
                env.insert(binder.clone(), bound);
                Ok(t)
            }
            Expr::Method { span, .. } => {
                // Sprint 21: `method (...) ... end` in expression
                // position should have been rewritten to a synthetic
                // `Expr::Ident(__anon-method-NNNN)` by
                // `lift_anonymous_methods` in the lowering pre-pass
                // (see `lift_anonymous_methods` below). If we got here,
                // the lifting pass missed a Method form — surface as an
                // unsupported diagnostic so the bug is loud.
                Err(LoweringError::Unsupported {
                    span: *span,
                    message: "anonymous method survived the Sprint 21 lift pre-pass — \
                              please report; expected every Expr::Method in expression \
                              position to be rewritten to an ident reference"
                        .to_string(),
                })
            }
            Expr::Case { span, .. } | Expr::LocalMethod { span, .. } => {
                Err(LoweringError::Unsupported {
                    span: *span,
                    message: format!(
                        "expression form `{}` not lowered in Sprint 06",
                        expr_kind(e)
                    ),
                })
            }
            Expr::MacroCall { span, name } => Err(LoweringError::Unsupported {
                span: *span,
                message: format!(
                    "macro call `{name}` reached lowering — no matching `define macro` \
                     in the seeded macro table; expansion was skipped"
                ),
            }),
            Expr::Stmt(s) => self.lower_stmt_as_expr(s, env, ctx),
        }
    }

    /// Sprint 24: emit the IR for reading a captured variable at index
    /// `idx` from the closure body's synthetic env parameter. Expands
    /// to two calls: `%env-cell(env, idx)` to fetch the cell, then
    /// `%cell-get(cell)` to read its value.
    fn emit_captured_var_read(&mut self, env_temp: TempId, idx: usize) -> TempId {
        let idx_t = self.fresh_temp(TypeEstimate::Integer);
        self.push(Computation::Const {
            dst: idx_t,
            value: ConstValue::Integer(idx as i128),
        });
        let cell_t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst: cell_t,
            callee: "nod_env_cell".to_string(),
            args: vec![env_temp, idx_t],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        let val_t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst: val_t,
            callee: "nod_cell_get".to_string(),
            args: vec![cell_t],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        val_t
    }

    /// Sprint 24: emit the IR for writing `value` into the captured
    /// variable at index `idx` (in the closure body's env). Expands to
    /// `%cell-set!(value, %env-cell(env, idx))`.
    fn emit_captured_var_write(
        &mut self,
        env_temp: TempId,
        idx: usize,
        value: TempId,
    ) -> TempId {
        let idx_t = self.fresh_temp(TypeEstimate::Integer);
        self.push(Computation::Const {
            dst: idx_t,
            value: ConstValue::Integer(idx as i128),
        });
        let cell_t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst: cell_t,
            callee: "nod_env_cell".to_string(),
            args: vec![env_temp, idx_t],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        let dst = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst,
            callee: "nod_cell_set".to_string(),
            args: vec![value, cell_t],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        dst
    }

    /// Sprint 24: emit a `%make-closure(name, arity, env)` call. The
    /// `env` is built by gathering the captured locals' cell Words (each
    /// already stored in the current `LocalEnv` as the result of
    /// cell-promotion at the binding site) into a fresh SOV and
    /// wrapping it in an `<environment>`.
    fn emit_make_closure(
        &mut self,
        lifted_name: &str,
        arity: usize,
        captured_cells: &[TempId],
        // Sprint 60+: when the lifted body declares `#rest`, emit the
        // rest-marked maker (`nod_make_rest_closure`) so the runtime tags
        // the closure with FUNCTION_KIND_CLOSURE_REST and collects
        // trailing actuals into the `#rest` SOV at call time. `arity` is
        // still the body value-arity `F + 1`.
        rest: bool,
    ) -> TempId {
        // 1. Allocate the cells vector (len = captured.len()).
        let len_t = self.fresh_temp(TypeEstimate::Integer);
        self.push(Computation::Const {
            dst: len_t,
            value: ConstValue::Integer(captured_cells.len() as i128),
        });
        let sov_t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst: sov_t,
            callee: "nod_make_sov_len".to_string(),
            args: vec![len_t],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        // 2. Fill the SOV slots with the captured cell Words.
        for (i, &cell) in captured_cells.iter().enumerate() {
            let i_t = self.fresh_temp(TypeEstimate::Integer);
            self.push(Computation::Const {
                dst: i_t,
                value: ConstValue::Integer(i as i128),
            });
            let _ = self.emit_sov_element_setter(cell, sov_t, i_t);
        }
        // 3. Wrap the SOV in an `<environment>`.
        let env_t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst: env_t,
            callee: "nod_make_environment".to_string(),
            args: vec![sov_t],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        // 4. Allocate the closure Word with this env.
        let name_word = self.emit_string_literal(lifted_name);
        let arity_t = self.fresh_temp(TypeEstimate::Integer);
        self.push(Computation::Const {
            dst: arity_t,
            value: ConstValue::Integer(arity as i128),
        });
        let dst = self.fresh_temp(TypeEstimate::Top);
        let maker = if rest {
            "nod_make_rest_closure"
        } else {
            "nod_make_closure"
        };
        self.push(Computation::DirectCall {
            dst,
            callee: maker.to_string(),
            args: vec![name_word, arity_t, env_t],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        dst
    }

    /// Lower a `local method … end` group (`Statement::Local`).
    ///
    /// The lift pre-pass already emitted a top-level `DefineFunction` for
    /// each local method's body and recorded a `ClosureInfo` keyed by a
    /// synthetic lifted name (mapped from the source name via
    /// `local_lifted_for`). Here we:
    ///   1. Bind each local-method NAME to a fresh `<cell>` (initialised
    ///      to `#f`) in `env`. All cells are created first so a method can
    ///      refer to a sibling (or itself) that is defined later in the
    ///      group — mutual / self recursion.
    ///   2. For each method, build its closure with `%make-closure`,
    ///      capturing the (now-live) cells, and store the closure Word
    ///      back into NAME's cell via `%cell-set!`.
    ///
    /// NAME is in `cell_ctx.cell_locals` (the lift pass marked it), so a
    /// later `NAME()` call reads the cell via `%cell-get` and invokes the
    /// closure through the `nod_funcall_N` trampoline.
    fn lower_local_methods(
        &mut self,
        enclosing_fn: &str,
        methods: &[nod_reader::LocalMethodDecl],
        env: &mut LocalEnv,
        ctx: &LowerCtx,
        span: Span,
    ) -> Result<(), LoweringError> {
        let Some(reg) = ctx.closures else {
            return Err(LoweringError::Unsupported {
                span,
                message: "`local method` requires the closure pre-pass".to_string(),
            });
        };
        // Pass 1 — allocate one cell per local method, bound to #f.
        for m in methods {
            let init = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::Const {
                dst: init,
                value: ConstValue::Bool(false),
            });
            let cell = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst: cell,
                callee: "nod_make_cell".to_string(),
                args: vec![init],
                safepoint_roots: Vec::new(),
                is_no_alloc: false,
            });
            env.insert(m.name.clone(), cell);
        }
        // Pass 2 — build each closure (capturing the live cells) and
        // store it into its own cell.
        for m in methods {
            let lifted = reg.local_lifted_for(enclosing_fn, &m.name).ok_or_else(|| {
                LoweringError::Unsupported {
                    span: m.span,
                    message: format!(
                        "`local method` `{}` was not lifted (internal)",
                        m.name
                    ),
                }
            })?;
            let info = reg.closure_for(lifted).ok_or_else(|| {
                LoweringError::Unsupported {
                    span: m.span,
                    message: format!(
                        "no closure info for lifted local method `{lifted}` (internal)"
                    ),
                }
            })?;
            // Gather the captured cells from env (every captured name is
            // cell-promoted, including sibling/self local-method names).
            let mut captured_cells: Vec<TempId> = Vec::with_capacity(info.captured.len());
            for cap in &info.captured {
                let Some(&cell_t) = env.get(cap) else {
                    return Err(LoweringError::UndefinedIdent {
                        span: m.span,
                        name: cap.clone(),
                    });
                };
                captured_cells.push(cell_t);
            }
            let lifted_name = info.lifted_name.clone();
            let info_arity = info.arity;
            let is_rest = info.rest_fixed.is_some();
            let no_captures = info.captured.is_empty();
            let closure = if no_captures && !is_rest {
                // No captures AND fixed arity: a plain function reference
                // suffices, but we still store it through the cell so call
                // sites uniformly read NAME via %cell-get.
                self.emit_make_function_ref(&lifted_name, info_arity)
            } else {
                // A rest-closure MUST go through emit_make_closure (even
                // with no captures) so the closure Word is tagged
                // FUNCTION_KIND_CLOSURE_REST — a plain function-ref would be
                // kind-tag=0 and the funcall shims would apply the exact-
                // arity check and crash on a variadic call. With no
                // captures the env is an empty <environment>; env-ptr is
                // non-zero, so the body's (env, …) ABI still holds.
                self.emit_make_closure(&lifted_name, info_arity, &captured_cells, is_rest)
            };
            let cell_t = *env.get(&m.name).expect("cell created in pass 1");
            let set_dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst: set_dst,
                callee: "nod_cell_set".to_string(),
                args: vec![closure, cell_t],
                safepoint_roots: Vec::new(),
                is_no_alloc: false,
            });
        }
        Ok(())
    }

    fn emit_sov_element_setter(
        &mut self,
        value: TempId,
        sov: TempId,
        idx: TempId,
    ) -> TempId {
        let dst = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst,
            callee: "nod_sov_element_setter".to_string(),
            args: vec![value, sov, idx],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        dst
    }

    /// Sprint 60: allocate a fresh `<simple-object-vector>` of length
    /// `elems.len()` (via `nod_make_sov_len`) and install each element
    /// through `nod_sov_element_setter`. Returns the SOV temp. The
    /// element temps MUST already be lowered (so no GC during element
    /// lowering can strand a half-built SOV). Shared by the `#[…]`
    /// vector-literal path and the `#rest` argument-collection path.
    fn emit_build_sov(&mut self, elems: &[TempId]) -> TempId {
        let len_t = self.fresh_temp(TypeEstimate::Integer);
        self.push(Computation::Const {
            dst: len_t,
            value: ConstValue::Integer(elems.len() as i128),
        });
        let sov_t = self.fresh_temp(TypeEstimate::Class(
            nod_runtime::ClassId::SIMPLE_OBJECT_VECTOR.0,
        ));
        self.push(Computation::DirectCall {
            dst: sov_t,
            callee: "nod_make_sov_len".to_string(),
            args: vec![len_t],
            safepoint_roots: Vec::new(),
            is_no_alloc: false,
        });
        for (i, &elt) in elems.iter().enumerate() {
            let i_t = self.fresh_temp(TypeEstimate::Integer);
            self.push(Computation::Const {
                dst: i_t,
                value: ConstValue::Integer(i as i128),
            });
            let _ = self.emit_sov_element_setter(elt, sov_t, i_t);
        }
        sov_t
    }

    /// Sprint 60: build a proper `<list>` from `elems` — a right-nested
    /// `%pair-alloc(e0, %pair-alloc(e1, … %nil()))` chain. Empty `elems`
    /// yields `%nil()` (the canonical `<empty-list>`). Element temps MUST
    /// already be lowered. Shared by the `#(…)` list-literal path and the
    /// variadic `list(…)` call-site form.
    fn emit_build_list(&mut self, elems: &[TempId]) -> TempId {
        let mut tail = self.fresh_temp(TypeEstimate::Class(
            nod_runtime::ClassId::EMPTY_LIST.0,
        ));
        self.push(Computation::DirectCall {
            dst: tail,
            callee: "%nil".to_string(),
            args: Vec::new(),
            safepoint_roots: Vec::new(),
            is_no_alloc: false,
        });
        for &elt in elems.iter().rev() {
            let pair_dst = self.fresh_temp(TypeEstimate::Class(
                nod_runtime::ClassId::PAIR.0,
            ));
            self.push(Computation::DirectCall {
                dst: pair_dst,
                callee: "%pair-alloc".to_string(),
                args: vec![elt, tail],
                safepoint_roots: Vec::new(),
                is_no_alloc: false,
            });
            tail = pair_dst;
        }
        tail
    }

    /// Sprint 21: emit a `nod_make_function_ref(name_bytestring,
    /// arity_fixnum)` call. The result is a pointer-tagged `<function>`
    /// Word; the underlying instance lives in the static area so the
    /// address is stable. Codegen turns this into a DirectCall to the
    /// runtime shim.
    fn emit_make_function_ref(&mut self, name: &str, arity: usize) -> TempId {
        let name_word = self.emit_string_literal(name);
        let arity_temp = self.fresh_temp(TypeEstimate::Integer);
        self.push(Computation::Const {
            dst: arity_temp,
            value: ConstValue::Integer(arity as i128),
        });
        let dst = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst,
            callee: "nod_make_function_ref".to_string(),
            args: vec![name_word, arity_temp],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        dst
    }

    /// Emit a `<byte-string>` literal Word for the supplied Rust `&str`.
    /// The bake goes through the static-area-pinned literal pool so the
    /// address is stable across GC.
    ///
    /// Sprint 38c — emits `ConstValue::StringLiteralRef(text)` instead
    /// of `ConstValue::WordBits(w.raw())`. Codegen lowers this to a
    /// `load i64` through a per-module external global keyed by content,
    /// so the bitcode round-trips across processes.
    fn emit_string_literal(&mut self, s: &str) -> TempId {
        let t = self.fresh_temp(TypeEstimate::String);
        self.push(Computation::Const {
            dst: t,
            value: ConstValue::StringLiteralRef(s.to_string()),
        });
        t
    }

    /// Materialise a class reference as a Word constant pointing at
    /// the class's `ClassMetadata` in the static area. We tag the
    /// address with bit 0 = 1 (pointer tag); slot-load/store codegen
    /// will untag.
    ///
    /// Sprint 38c — emits `ConstValue::ClassMetadataPtr { class_id,
    /// tagged: true }`. Codegen lowers the load via the per-module
    /// external global; the `| 1` pointer-tag is applied AFTER the
    /// load (codegen handles the OR).
    fn emit_class_ref(&mut self, class_id: ClassId) -> TempId {
        let t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::Const {
            dst: t,
            value: ConstValue::ClassMetadataPtr {
                class_id: class_id.0,
                tagged: true,
            },
        });
        t
    }

    fn lower_assign(
        &mut self,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<TempId, LoweringError> {
        // Sprint 18: `local := value` reassigns a local-variable binding
        // in-place. We don't have proper mutable cells — we just rebind
        // the name to the new value's temp in `env`. This makes the
        // post-assignment SSA temp visible to subsequent reads in the
        // same scope; `lower_while_like` snapshots names at the back
        // edge to thread them through the header phi.
        //
        // Sprint 24: cell-promoted locals + captured variables route
        // through the cell-set! / env-cell shims instead of an SSA
        // rebind — the mutation is visible to inner closures (and the
        // outer scope sees inner mutations too).
        if let Expr::Ident(_, name) = lhs {
            // Closure body captured-var write.
            if let Some(ec) = self.cell_ctx.env_captures.clone()
                && let Some(&idx) = ec.index_of.get(name)
            {
                let v = self.lower_expr(rhs, env, ctx)?;
                return Ok(self.emit_captured_var_write(ec.env_temp, idx, v));
            }
            if env.contains_key(name) {
                // Cell-promoted local: write through `%cell-set!`.
                if self.cell_ctx.cell_locals.contains(name) {
                    let v = self.lower_expr(rhs, env, ctx)?;
                    let cell_t = *env.get(name).expect("env entry checked");
                    let dst = self.fresh_temp(TypeEstimate::Top);
                    self.push(Computation::DirectCall {
                        dst,
                        callee: "nod_cell_set".to_string(),
                        args: vec![v, cell_t],
                        safepoint_roots: Vec::new(), is_no_alloc: false,
                    });
                    return Ok(dst);
                }
                // Plain local: rebind the SSA temp.
                let t = self.lower_expr(rhs, env, ctx)?;
                env.insert(name.clone(), t);
                return Ok(t);
            }
            // GAP-004: module-level `define variable` assignment.
            // Routes through `nod_var_set_by_name(value, name)` which
            // looks up the variable's cell via the per-name slot and
            // writes through `nod_cell_set` (write-barriered). The
            // setter shim returns the new value (Dylan setter
            // convention), so we propagate its result as the
            // assignment's value.
            //
            // We check `is_variable` rather than the wider
            // `is_constant_or_variable` so that assigning to a
            // `define constant` falls through to UndefinedIdent
            // territory below — we surface a dedicated "cannot assign
            // to constant" error there.
            if ctx.top_names.is_variable(name) {
                let v = self.lower_expr(rhs, env, ctx)?;
                let name_t = self.fresh_temp(TypeEstimate::String);
                self.push(Computation::Const {
                    dst: name_t,
                    value: ConstValue::String(name.clone()),
                });
                let dst = self.fresh_temp(TypeEstimate::Top);
                self.push(Computation::DirectCall {
                    dst,
                    callee: "nod_var_set_by_name".to_string(),
                    args: vec![v, name_t],
                    safepoint_roots: Vec::new(),
                    is_no_alloc: false,
                });
                return Ok(dst);
            }
            // Assignment to a `define constant` is an error.
            if ctx.top_names.is_constant_or_variable(name) {
                // (is_variable was false above, so this must be a
                // constant.)
                return Err(LoweringError::Unsupported {
                    span,
                    message: format!(
                        "cannot assign to `{name}` — it is a `define constant` \
                         (use `define variable` if you need a mutable binding)"
                    ),
                });
            }
            return Err(LoweringError::UndefinedIdent {
                span,
                name: name.clone(),
            });
        }
        // Sprint 12: only `slot-getter(obj) := value` is supported.
        // I.e. lhs is `Call(Ident(name), [obj])`. We rewrite to a
        // setter dispatch.
        let Expr::Call { callee, args, .. } = lhs else {
            return Err(LoweringError::Unsupported {
                span,
                message: "Sprint 12 only supports `slot-getter(obj) := value` assignment".to_string(),
            });
        };
        let Expr::Ident(_, slot_name) = callee.as_ref() else {
            return Err(LoweringError::Unsupported {
                span,
                message: "Sprint 12 assign-call: callee must be an identifier".to_string(),
            });
        };
        if args.is_empty() {
            return Err(LoweringError::Unsupported {
                span,
                message: "setter: callee must have at least one argument".to_string(),
            });
        }
        // Sprint 22: N-ary setters. For `f(a0, a1, …) := v`, lower to
        // `Dispatch("f-setter", [v, a0, a1, …])` — Dylan's setter
        // calling convention puts the new value first. The unary case
        // (Sprint 12: `slot(obj) := value` → `Dispatch("slot-setter",
        // [obj, value])`) is preserved as a special case below for
        // back-compat with slot-getter rewrites.
        let obj_temps: Vec<TempId> = args
            .iter()
            .map(|a| self.lower_expr(a, env, ctx))
            .collect::<Result<_, _>>()?;
        let value_temp = self.lower_expr(rhs, env, ctx)?;
        if obj_temps.len() == 1
            && let Some(offset) =
                self.try_resolve_slot_offset(obj_temps[0], slot_name, ctx)
        {
            let dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::StoreSlot {
                dst,
                instance: obj_temps[0],
                offset,
                value: value_temp,
                slot_type: SlotTypeKind::Object,
            });
            return Ok(dst);
        }
        let dst = self.fresh_temp(TypeEstimate::Top);
        if obj_temps.len() == 1 {
            // Sprint 12 shape: `slot-setter(obj, value)`.
            self.push(Computation::Dispatch {
                dst,
                generic_name: format!("{slot_name}-setter"),
                args: vec![obj_temps[0], value_temp],
                safepoint_roots: Vec::new(),
            });
        } else {
            // Sprint 22 N-ary shape: `f-setter(value, a0, a1, …)`.
            let mut all = Vec::with_capacity(1 + obj_temps.len());
            all.push(value_temp);
            all.extend(obj_temps);
            self.push(Computation::Dispatch {
                dst,
                generic_name: format!("{slot_name}-setter"),
                args: all,
                safepoint_roots: Vec::new(),
            });
        }
        Ok(dst)
    }

    /// If `obj_temp` carries a user-class type estimate (or its declared
    /// parameter type is one), and `slot_name` is one of that class's
    /// slots, return the byte offset. Otherwise `None`.
    fn try_resolve_slot_offset(
        &self,
        _obj_temp: TempId,
        _slot_name: &str,
        _ctx: &LowerCtx,
    ) -> Option<usize> {
        // Sprint 12: the SSA type lattice doesn't carry user class ids
        // directly. Always go through Dispatch for slot access. The
        // direct LoadSlot path lights up when we add a class-aware
        // type estimate (Sprint 13).
        None
    }

    fn lower_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<TempId, LoweringError> {
        // See through parentheses around the callee. The parser keeps the
        // grouping node (`Expr::Paren`) for `(method (...) ... end)(args)`,
        // `(foo)(args)`, etc. Without unwrapping it here the callee would
        // never match the `Expr::Ident` / computed-callee paths below and
        // every parenthesised callee fell through to the "non-ident callee"
        // error. Unwrap iteratively in case of redundant nesting.
        let mut callee = callee;
        while let Expr::Paren { inner, .. } = callee {
            callee = inner.as_ref();
        }
        // `instance?(v, <class>)` intrinsic.
        if let Expr::Ident(_, name) = callee
            && name == "instance?"
            && args.len() == 2
        {
            return self.lower_instance_check(&args[0], &args[1], env, ctx, span);
        }
        // `as(<integer>, ch)` / `as(<character>, code)` intrinsic. A
        // `<character>` lowers to a raw i32 code that runtime dispatch
        // can't classify (its value isn't a tagged Word, so
        // `word_class_id` would mis-tag / fault), so the DRM `as`
        // coercion for these two immediate classes is resolved here at
        // compile time straight to the `%char-code` / `%code-char`
        // primitives. Any other `as(...)` target falls through to the
        // ordinary generic path below.
        if let Expr::Ident(_, name) = callee
            && name == "as"
            && args.len() == 2
            && let Expr::Ident(_, class_name) = &args[0]
        {
            let prim = match class_name.as_str() {
                "<integer>" => Some(("nod_char_code", TypeEstimate::Integer)),
                "<character>" => Some(("nod_code_char", TypeEstimate::Character)),
                _ => None,
            };
            if let Some((sym, ret_ty)) = prim {
                let v = self.lower_expr(&args[1], env, ctx)?;
                let dst = self.fresh_temp(ret_ty);
                self.push(Computation::DirectCall {
                    dst,
                    callee: sym.to_string(),
                    args: vec![v],
                    safepoint_roots: Vec::new(),
                    is_no_alloc: true,
                });
                return Ok(dst);
            }
        }
        // `make(<class>, kw: v, ...)` intrinsic.
        if let Expr::Ident(_, name) = callee
            && name == "make"
        {
            return self.lower_make(args, env, ctx, span);
        }
        // Sprint 47 — `values(a, b, c)` intrinsic. See
        // `docs/COMPILER_GAPS.md` GAP-003. Lowers as:
        //   `%values-set!(0, b); %values-set!(1, c); ...; return a`
        // so the callee returns its first value through the ordinary
        // single-value ABI and stashes the extras in the thread-local
        // secondary-values buffer. `values()` with no args returns `#f`;
        // `values(x)` is equivalent to `x` and is lowered without any
        // buffer writes.
        if let Expr::Ident(_, name) = callee
            && name == "values"
        {
            return self.lower_values(args, env, ctx, span);
        }
        // Sprint 14: `next-method()` and `next-method?()` intrinsics.
        // Lower to DirectCall against the runtime shim. Explicit-args
        // form `(next-method x y)` is Sprint 17 macro territory; today
        // only the no-args form is supported (the shim re-uses the
        // parent method's args via the thread-local chain frame).
        if let Expr::Ident(_, name) = callee
            && name == "next-method"
        {
            if !args.is_empty() {
                return Err(LoweringError::Unsupported {
                    span,
                    message: "Sprint 14: `next-method` with explicit arguments is Sprint 17 macro work; use the no-args form".to_string(),
                });
            }
            let dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst,
                callee: "nod_next_method".to_string(),
                args: Vec::new(),
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            return Ok(dst);
        }
        if let Expr::Ident(_, name) = callee
            && name == "next-method?"
            && args.is_empty()
        {
            let dst = self.fresh_temp(TypeEstimate::Boolean);
            self.push(Computation::DirectCall {
                dst,
                callee: "nod_has_next_method".to_string(),
                args: Vec::new(),
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            return Ok(dst);
        }
        // Sprint 16: `<pair>` / `<list>` builtins. `pair`, `head`,
        // `tail`, `empty?`, `nil` lower to direct calls into the runtime
        // shims. The codegen layer recognises the `%pair*` / `%nil` /
        // `%empty?` prefixes and emits the right extern declarations +
        // call sites. Estimates carry `Class(<pair>)` for the allocating
        // form so the dispatch resolver can narrow `<pair>`-typed args.
        if let Expr::Ident(_, name) = callee
            && let Some(builtin) = ListBuiltin::from_name(name)
        {
            return self.lower_list_builtin(builtin, args, env, ctx, span);
        }
        // Sprint 20b: `#(a, b, c)` literal lists. The parser emits
        // `Call(Ident("#list"), [a, b, c])`; we lower as a right-nested
        // chain of `pair(elt, tail)` calls bottoming out at `nil`.
        if let Expr::Ident(_, name) = callee
            && name == "#list"
        {
            // Lower each element to a temp, then build the cons-chain
            // right-to-left (empty `#()` bottoms out at `%nil`).
            let elem_temps: Vec<TempId> = args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            return Ok(self.emit_build_list(&elem_temps));
        }
        // Sprint 60: variadic `list(a, b, c, …)` — the DRM N-ary list
        // constructor, un-deferred by `#rest` collection. Built directly
        // at the call site (any arity) as a `<list>`, bypassing the
        // single-method stdlib `list` generic that only handled the
        // 1-arg case. Mirrors the `#(…)` literal path. `list()` → `#()`.
        if let Expr::Ident(_, name) = callee
            && name == "list"
        {
            let elem_temps: Vec<TempId> = args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            return Ok(self.emit_build_list(&elem_temps));
        }
        // Sprint 60: variadic `vector(a, b, c, …)` — the DRM N-ary
        // `<simple-object-vector>` constructor, un-deferred by `#rest`
        // collection. Built directly at the call site (any arity) as an
        // SOV, mirroring the `#[…]` literal path. `vector()` → empty SOV.
        if let Expr::Ident(_, name) = callee
            && name == "vector"
        {
            let elem_temps: Vec<TempId> = args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            return Ok(self.emit_build_sov(&elem_temps));
        }
        // Collection-classes lever: `#[a, b, c]` literal vectors. The
        // parser emits `Call(Ident("#vector"), [a, b, c])`. Lower as a
        // fresh `<simple-object-vector>` of length N (`nod_make_sov_len`)
        // with each element installed via `nod_sov_element_setter` —
        // mirrors `emit_make_closure`'s cells-vector construction. The
        // SOV itself is rooted across the per-element setters by the
        // standard safepoint machinery (the element exprs are lowered
        // before the allocation so no GC-managed intermediate spans it).
        if let Expr::Ident(_, name) = callee
            && name == "#vector"
        {
            // Lower every element first (before the SOV allocation), so a
            // GC during a later element's lowering can't strand a
            // half-built SOV.
            let elem_temps: Vec<TempId> = args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            return Ok(self.emit_build_sov(&elem_temps));
        }
        // Sprint 60: N-ary `max(a, b, c, …)` / `min(…)` — the DRM
        // variadic numeric extrema, un-deferred by `#rest` collection.
        // Folded LEFT at the call site into a chain of binary calls
        // (`max(max(a, b), c)…`), each reusing the existing 2-arg `max` /
        // `min` generic via Dispatch. Only intercepts arity >= 3; the
        // 0/1/2-arg forms (and the bareword `max` / `min` function value)
        // fall through to the normal path unchanged.
        if let Expr::Ident(_, name) = callee
            && (name == "max" || name == "min")
            && args.len() >= 3
        {
            let arg_temps: Vec<TempId> = args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            let mut acc = arg_temps[0];
            for &next in &arg_temps[1..] {
                let dst = self.fresh_temp(TypeEstimate::Top);
                self.push(Computation::Dispatch {
                    dst,
                    generic_name: name.clone(),
                    args: vec![acc, next],
                    safepoint_roots: Vec::new(),
                });
                acc = dst;
            }
            return Ok(acc);
        }
        // Sprint 20b: `%`-prefixed primitive ops. Each entry in
        // `LOWER_PRIMITIVE_TABLE` lowers to a `DirectCall` against a
        // `nod_*` runtime shim. Args are type-checked for arity only;
        // the runtime tolerates Word inputs of the wrong shape (e.g.
        // non-fixnum to `%range-from` returns 0).
        if let Expr::Ident(_, name) = callee
            && name.starts_with('%')
            && let Some((sym, arity, ret_ty)) = lookup_primitive(name)
        {
            if args.len() != arity {
                return Err(LoweringError::Unsupported {
                    span,
                    message: format!(
                        "primitive `{name}` expects {arity} argument(s), got {}",
                        args.len()
                    ),
                });
            }
            let arg_temps: Vec<TempId> = args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            let dst = self.fresh_temp(ret_ty);
            self.push(Computation::DirectCall {
                dst,
                callee: sym.to_string(),
                args: arg_temps,
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            return Ok(dst);
        }
        // Sprint 19: `signal(c)` / `condition-message(c)` builtins.
        if let Expr::Ident(_, name) = callee
            && name == "signal"
            && args.len() == 1
        {
            let a = self.lower_expr(&args[0], env, ctx)?;
            let dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst,
                callee: "%signal".to_string(),
                args: vec![a],
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            return Ok(dst);
        }
        if let Expr::Ident(_, name) = callee
            && name == "condition-message"
            && args.len() == 1
        {
            let a = self.lower_expr(&args[0], env, ctx)?;
            let dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst,
                callee: "%condition-message".to_string(),
                args: vec![a],
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            return Ok(dst);
        }
        // Sprint 19: if the callee is a local binding that refers to an
        // exit procedure (i.e. the `k` in `block (k) ... k(v) ... end`),
        // lower as `%invoke-exit(k, v)`. Detecting the case statically
        // is hard because env doesn't carry "this is an exit-procedure"
        // type info; we can't tell apart from a regular call. Sprint 19
        // simplification: if a name is in env AND is being called with
        // exactly one arg AND we're inside a lifted block thunk whose
        // env binds that name from `exit_var`, treat it as invoke-exit.
        //
        // We use a simple naming convention: the `exit_var` name is
        // stored verbatim in env. To unambiguously trigger
        // `%invoke-exit`, the lowerer special-cases names that resolve
        // to an env entry AND aren't otherwise a known function. The
        // codegen-level `%invoke-exit` handler takes the env-bound Word
        // (the `<exit-procedure>` instance) and the value Word and
        // invokes the runtime shim.
        //
        // Heuristic: if the callee is an ident in `env`, NOT in
        // top_names, and there's exactly one argument, treat as
        // invoke-exit. This is safe for Sprint 19 because the parser /
        // earlier lowering doesn't yet support first-class function
        // values in env; the only env-bound callable values are exit
        // procedures.
        // Sprint 21: env-bound callable Word — could be a `<function>`
        // (introduced via `\name` or `method (...) ... end`) OR an
        // `<exit-procedure>` (the `k` in `block (k) ... end`). Both
        // route through the `nod_funcall_N` trampoline which dispatches
        // on the heap class at runtime. The arity is fixed by the call
        // shape; Sprint 21 supports up to arity-2 directly and uses
        // `nod_apply` for higher arities (deferred).
        //
        // Sprint 24: if `name` is a cell-promoted local OR a captured
        // env-variable, `lower_expr` already inserts the `%cell-get` /
        // `%env-cell` indirection. We route through the regular
        // `lower_expr` to get the unwrapped function Word.
        // Strip kw-arg wrapper for non-make calls (the parser wraps
        // `name: value` arguments as Call(%kw-arg, [Symbol, Value]).
        // For Sprint 12 we treat them as positional values for direct
        // calls. Generic dispatch + make have their own kw handling.
        // Computed-callee funcalls (`(method (#key x) x end)(x: 1)`,
        // `methods[0](y: 1)`) and local-binding funcalls also use this
        // positional view — keyword values are threaded into the same
        // positional slots the `#key` params bind from.
        let mut positional_args: Vec<&Expr> = Vec::with_capacity(args.len());
        for a in args {
            if let Expr::Call { callee: c, args: kwargs, .. } = a
                && let Expr::Ident(_, n) = c.as_ref()
                && n == "%kw-arg"
                && kwargs.len() == 2
            {
                positional_args.push(&kwargs[1]);
            } else {
                positional_args.push(a);
            }
        }
        let callee_name: Option<&str> = match callee {
            Expr::Ident(_, n) => Some(n.as_str()),
            _ => None,
        };
        let captured_in_env = match (self.cell_ctx.env_captures.as_ref(), callee_name) {
            (Some(ec), Some(n)) => ec.index_of.contains_key(n),
            _ => false,
        };
        // Local-binding funcall: an env-bound `<function>` /
        // `<exit-procedure>` Word called by name.
        if let Expr::Ident(_, name) = callee
            && (env.contains_key(name) || captured_in_env)
            && !ctx.top_names.contains(name)
            && !ctx.generics.contains(name)
        {
            let f = self.lower_expr(callee, env, ctx)?;
            let arg_temps: Vec<TempId> = positional_args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            return self.emit_funcall(f, arg_temps, span, Some(name));
        }
        // Computed-callee funcall: the callee is an arbitrary expression
        // that evaluates to a `<function>` Word — a directly-called
        // method literal (lifted to `(__anon-method-N)` after the lift
        // pre-pass, possibly with captures so it must go through the
        // closure Word rather than a direct call), or an indexing /
        // call expression like `methods[0]` / `table[k]`. Lower the
        // callee to its Word and route through the `nod_funcall_N`
        // trampoline. Keyword arguments are already folded into
        // `positional_args` above, so this handles both the plain and
        // keyword-argument forms uniformly.
        let route_through_funcall = match callee {
            Expr::Ident(_, name) => {
                // A lifted closure WITH captures must be created via
                // `%make-closure` (which `lower_expr` does) and invoked
                // through the funcall trampoline; a direct call by name
                // would drop the environment. Capture-free lifted
                // methods and ordinary top-level / generic idents fall
                // through to the existing DirectCall / Dispatch paths.
                // A rest-closure must route through the funcall trampoline
                // (and be created via emit_make_closure) even with no
                // captures, so the variadic call hits invoke_rest_closure
                // instead of a fixed-arity DirectCall.
                ctx.closures
                    .and_then(|reg| reg.closure_for(name))
                    .map(|info| !info.captured.is_empty() || info.rest_fixed.is_some())
                    .unwrap_or(false)
            }
            // Any non-ident callee (after Paren-unwrapping) is a computed
            // function value.
            _ => true,
        };
        if route_through_funcall {
            let f = self.lower_expr(callee, env, ctx)?;
            let arg_temps: Vec<TempId> = positional_args
                .iter()
                .map(|a| self.lower_expr(a, env, ctx))
                .collect::<Result<_, _>>()?;
            return self.emit_funcall(f, arg_temps, span, callee_name);
        }
        let arg_temps: Vec<TempId> = positional_args
            .iter()
            .map(|a| self.lower_expr(a, env, ctx))
            .collect::<Result<_, _>>()?;
        if let Expr::Ident(_, name) = callee {
            // Sprint 28: c-function call — look up the per-module
            // stub table and emit `nod_winffi_call_N(entry_ptr_const,
            // args...)`. Sprint 38d: the entry pointer is now baked
            // as a `ConstValue::StubEntryRef { dll, symbol, sig }` so
            // codegen lowers it to a `load i64, ptr @nod_stub__*`
            // through a per-module external global. The JIT-link path
            // binds the global's address to a stable `u64` slot whose
            // contents are the address of the freshly-allocated
            // `ApiStubEntry` in the current process (resolution is
            // lazy and idempotent inside the slot allocator).
            if let Some(cf_map) = ctx.c_functions
                && let Some(info) = cf_map.get(name)
            {
                if arg_temps.len() != info.arg_count {
                    return Err(LoweringError::Unsupported {
                        span,
                        message: format!(
                            "c-function `{name}` declared with {} parameter(s), called with {}",
                            info.arg_count,
                            arg_temps.len()
                        ),
                    });
                }
                if info.arg_count > 12 {
                    return Err(LoweringError::Unsupported {
                        span,
                        message: format!(
                            "c-function `{name}`: Sprint 36b caps arity at 12, got {}",
                            info.arg_count
                        ),
                    });
                }
                // Sprint 38d — emit a `StubEntryRef` const so codegen
                // routes through the per-module external global.
                // Pre-Sprint-38d code baked `info.entry_ptr` as a
                // `WordBits` `i64` — that worked in-process only
                // because the static-area address survived for the
                // process lifetime, but it pinned the bitcode to one
                // process (cache hits across processes saw stale
                // addresses).
                let entry_t = self.fresh_temp(TypeEstimate::Top);
                self.push(Computation::Const {
                    dst: entry_t,
                    value: ConstValue::StubEntryRef {
                        dll: info.dll.clone(),
                        symbol: info.symbol.clone(),
                        signature_bytes: info.signature_bytes.clone(),
                    },
                });
                let mut call_args = Vec::with_capacity(arg_temps.len() + 1);
                call_args.push(entry_t);
                call_args.extend(arg_temps);
                let callee_sym = match info.arg_count {
                    0 => "nod_winffi_call_0",
                    1 => "nod_winffi_call_1",
                    2 => "nod_winffi_call_2",
                    3 => "nod_winffi_call_3",
                    4 => "nod_winffi_call_4",
                    5 => "nod_winffi_call_5",
                    6 => "nod_winffi_call_6",
                    7 => "nod_winffi_call_7",
                    8 => "nod_winffi_call_8",
                    9 => "nod_winffi_call_9",
                    10 => "nod_winffi_call_10",
                    11 => "nod_winffi_call_11",
                    12 => "nod_winffi_call_12",
                    // Unreachable: arity_count <= 8 enforced above.
                    _ => unreachable!(),
                };
                let dst = self.fresh_temp(TypeEstimate::Top);
                self.push(Computation::DirectCall {
                    dst,
                    callee: callee_sym.to_string(),
                    args: call_args,
                    safepoint_roots: Vec::new(),
                    is_no_alloc: false,
                });
                return Ok(dst);
            }
            // Sprint 60: a callee declared with a `#rest` parameter. The
            // callee ABI is `fixed + 1` params: the `fixed` leading actuals,
            // then ONE final slot holding a freshly-built
            // `<simple-object-vector>` of the trailing actuals (positions
            // >= fixed). We build that SOV here at the call site, keeping the
            // callee's fixed-arity LLVM signature intact (no varargs). A call
            // with fewer than `fixed` args is rejected; a call with exactly
            // `fixed` args collects into a zero-length SOV.
            //
            // The collapsed call has `fixed + 1` actuals. If the name is a
            // dispatched generic (a `#rest` `define method` — including the
            // single-method generics the stdlib loader rewrites every
            // `define function f (…, #rest r)` into so `f` is reachable as a
            // value), we DISPATCH on those `fixed + 1` actuals — the rest SOV
            // sits in the final `<object>`-specialised slot, matching the
            // method's `param_count`. Otherwise (a plain `define function`)
            // we DirectCall the body by name. Either way the callee body sees
            // its `#rest` binding as the collected SOV in the final slot.
            if let Some(fixed) = ctx
                .top_names
                .rest_fixed_count(name)
                .or_else(|| nod_runtime::rest_callee_fixed_count(name))
            {
                if arg_temps.len() < fixed {
                    return Err(LoweringError::Unsupported {
                        span,
                        message: format!(
                            "`{name}` requires at least {fixed} argument(s), called with {}",
                            arg_temps.len()
                        ),
                    });
                }
                let rest_sov = self.emit_build_sov(&arg_temps[fixed..]);
                let mut call_args: Vec<TempId> = arg_temps[..fixed].to_vec();
                call_args.push(rest_sov);
                // A `#rest` generic (dispatched method) is registered in
                // `generics`/the runtime dispatch table but NOT in
                // `top_names.fns` (only `define function` and 0-param methods
                // land there). Route it through Dispatch; a `#rest`
                // `define function` (in `top_names`) takes the DirectCall arm.
                let is_generic = !ctx.top_names.contains(name)
                    && (ctx.generics.contains(name)
                        || nod_runtime::is_generic_defined(name));
                if is_generic {
                    let dst = self.fresh_temp(TypeEstimate::Top);
                    self.push(Computation::Dispatch {
                        dst,
                        generic_name: name.clone(),
                        args: call_args,
                        safepoint_roots: Vec::new(),
                    });
                    return Ok(dst);
                }
                let ret = ctx
                    .top_names
                    .return_type(name)
                    .unwrap_or(TypeEstimate::Top);
                let dst = self.fresh_temp(ret);
                self.push(Computation::DirectCall {
                    dst,
                    callee: name.clone(),
                    args: call_args,
                    safepoint_roots: Vec::new(),
                    is_no_alloc: false,
                });
                return Ok(dst);
            }
            // Sprint 12: prefer dispatch when the name is a known
            // generic AND the receiver's type estimate doesn't statically
            // resolve. For known top-level functions (slot accessors
            // emitted as Functions), DirectCall wins so the JIT inlines
            // straight to the LoadSlot body.
            if ctx.top_names.contains(name) {
                let ret = ctx
                    .top_names
                    .return_type(name)
                    .unwrap_or(TypeEstimate::Top);
                let dst = self.fresh_temp(ret);
                self.push(Computation::DirectCall {
                    dst,
                    callee: name.clone(),
                    args: arg_temps,
                    safepoint_roots: Vec::new(),
                    is_no_alloc: false,
                });
                return Ok(dst);
            }
            if ctx.generics.contains(name) || nod_runtime::is_generic_defined(name) {
                let dst = self.fresh_temp(TypeEstimate::Top);
                self.push(Computation::Dispatch {
                    dst,
                    generic_name: name.clone(),
                    args: arg_temps,
                    safepoint_roots: Vec::new(),
                });
                return Ok(dst);
            }
            if env.contains_key(name) {
                return Err(LoweringError::Unsupported {
                    span,
                    message: format!(
                        "calling local binding `{name}` not lowered in Sprint 06"
                    ),
                });
            }
            // Unknown ident callee — emit DirectCall against the name.
            let dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst,
                callee: name.clone(),
                args: arg_temps,
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            Ok(dst)
        } else {
            // Unreachable in practice: every non-ident callee is routed
            // through the computed-callee funcall path above (which lowers
            // the callee to a `<function>` Word and dispatches via
            // `nod_funcall_N`). Kept as a defensive funcall fallback so a
            // future callee shape can't silently regress to a hard error.
            let f = self.lower_expr(callee, env, ctx)?;
            self.emit_funcall(f, arg_temps, span, None)
        }
    }

    /// Emit a call through the `nod_funcall_N` trampoline against an
    /// already-lowered function Word `f` with the given positional
    /// argument temps. Shared by the local-binding funcall path and the
    /// computed-callee funcall path. `callee_desc` is an optional name
    /// used only to make the over-arity diagnostic readable.
    fn emit_funcall(
        &mut self,
        f: TempId,
        arg_temps: Vec<TempId>,
        span: Span,
        callee_desc: Option<&str>,
    ) -> Result<TempId, LoweringError> {
        // Arities 0..=5 dispatch through the direct `nod_funcall_N`
        // trampolines. Higher arities still need `nod_apply`; surface a
        // "not yet supported" so the lowerer doesn't silently SOV-pack
        // without the caller opting in. `<exit-procedure>` is always
        // arity 1 at the source level; the arity-0 path skips the
        // exit-procedure shortcut inside `nod_funcall0` deliberately.
        let funcall_sym = match arg_temps.len() {
            0 => "nod_funcall0",
            1 => "nod_funcall1",
            2 => "nod_funcall2",
            3 => "nod_funcall3",
            4 => "nod_funcall4",
            5 => "nod_funcall5",
            n => {
                let what = callee_desc
                    .map(|d| format!("`{d}`"))
                    .unwrap_or_else(|| "a computed function value".to_string());
                return Err(LoweringError::Unsupported {
                    span,
                    message: format!(
                        "calling {what} with arity {n} not supported (cap is 5 direct args); use `apply(f, args)` for higher arities"
                    ),
                });
            }
        };
        let mut call_args = Vec::with_capacity(arg_temps.len() + 1);
        call_args.push(f);
        call_args.extend(arg_temps);
        let dst = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst,
            callee: funcall_sym.to_string(),
            args: call_args,
            safepoint_roots: Vec::new(),
            is_no_alloc: false,
        });
        Ok(dst)
    }

    fn lower_make(
        &mut self,
        args: &[Expr],
        env: &mut LocalEnv,
        ctx: &LowerCtx,
        span: Span,
    ) -> Result<TempId, LoweringError> {
        if args.is_empty() {
            return Err(LoweringError::Unsupported {
                span,
                message: "make: missing class argument".to_string(),
            });
        }
        // First arg: class. Expect an identifier resolving to a
        // registered class.
        let class_id = match &args[0] {
            Expr::Ident(_, name) => match ctx
                .user_classes
                .get(name)
                .copied()
                .or_else(|| resolve_class_id_by_name(name))
            {
                Some(id) => id,
                None => {
                    return Err(LoweringError::UndefinedIdent {
                        span: args[0].span(),
                        name: name.clone(),
                    });
                }
            },
            _ => {
                return Err(LoweringError::Unsupported {
                    span: args[0].span(),
                    message: "make: first argument must be a class name".to_string(),
                });
            }
        };
        // Sprint 22: `make(<table>, ...)` requires custom initialisation
        // (the backing buckets SOV has to be allocated and installed
        // before any insertion). The generic keyword-init path can't do
        // that, so we redirect to the `%make-table` primitive. The
        // optional `capacity:` keyword threads through; everything else
        // is silently ignored (Sprint 22 has no other table options).
        if find_class_id_by_name("<table>").map(|c| c == class_id).unwrap_or(false) {
            let mut capacity_temp: Option<TempId> = None;
            for a in &args[1..] {
                if let Expr::Call { callee, args: kwargs, .. } = a
                    && matches!(callee.as_ref(), Expr::Ident(_, n) if n == "%kw-arg")
                    && kwargs.len() == 2
                    && let Expr::Symbol(_, s) = &kwargs[0]
                    && s.trim_end_matches(':') == "capacity"
                {
                    capacity_temp = Some(self.lower_expr(&kwargs[1], env, ctx)?);
                }
            }
            let cap = capacity_temp.unwrap_or_else(|| self.emit_fixnum_const(0));
            let dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst,
                callee: "nod_make_table".to_string(),
                args: vec![cap],
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            return Ok(dst);
        }
        // Collection-classes lever (Part A2): `make(<bit-vector>, size:,
        // fill:)` needs a backing words-SOV allocated before the outer
        // object, which the generic keyword-init path can't do. Redirect
        // to the `nod_bit_vector_allocate(size, fill)` primitive (mirrors
        // the `<table>` arm). A `define method make` would be dead code
        // because this intercepts at the call site. `size:` defaults to 0,
        // `fill:` to `#f` (clear).
        if find_class_id_by_name("<bit-vector>").map(|c| c == class_id).unwrap_or(false) {
            let mut size_temp: Option<TempId> = None;
            let mut fill_temp: Option<TempId> = None;
            for a in &args[1..] {
                if let Expr::Call { callee, args: kwargs, .. } = a
                    && matches!(callee.as_ref(), Expr::Ident(_, n) if n == "%kw-arg")
                    && kwargs.len() == 2
                    && let Expr::Symbol(_, s) = &kwargs[0]
                {
                    match s.trim_end_matches(':') {
                        "size" => size_temp = Some(self.lower_expr(&kwargs[1], env, ctx)?),
                        "fill" => fill_temp = Some(self.lower_expr(&kwargs[1], env, ctx)?),
                        _ => {}
                    }
                }
            }
            let size = size_temp.unwrap_or_else(|| self.emit_fixnum_const(0));
            // Default fill is `#f` (clear) — emit the boolean-false const.
            let fill = fill_temp.unwrap_or_else(|| {
                let t = self.fresh_temp(TypeEstimate::Boolean);
                self.push(Computation::Const {
                    dst: t,
                    value: ConstValue::Bool(false),
                });
                t
            });
            let dst = self.fresh_temp(TypeEstimate::Class(class_id.0));
            self.push(Computation::DirectCall {
                dst,
                callee: "nod_bit_vector_allocate".to_string(),
                args: vec![size, fill],
                safepoint_roots: Vec::new(),
                is_no_alloc: false,
            });
            return Ok(dst);
        }
        let class_word_temp = self.emit_class_metadata_ptr_const(class_id);
        // Remaining args: kw: value pairs (parser-wrapped as
        // `Call(%kw-arg, [Symbol("kw:"), value])`).
        let mut make_args = vec![class_word_temp];
        for a in &args[1..] {
            let (kw_name, value_expr) = match a {
                Expr::Call { callee, args: kwargs, .. }
                    if matches!(callee.as_ref(), Expr::Ident(_, n) if n == "%kw-arg")
                        && kwargs.len() == 2 =>
                {
                    let raw_name = match &kwargs[0] {
                        Expr::Symbol(_, s) => s.trim_end_matches(':').to_string(),
                        _ => {
                            return Err(LoweringError::Unsupported {
                                span: a.span(),
                                message: "make: kw-arg name must be a keyword".to_string(),
                            });
                        }
                    };
                    (raw_name, &kwargs[1])
                }
                _ => {
                    return Err(LoweringError::Unsupported {
                        span: a.span(),
                        message: "make: arguments after the class must be `kw: value` pairs"
                            .to_string(),
                    });
                }
            };
            let name_temp = self.emit_symbol_literal(&kw_name);
            let value_temp = self.lower_expr(value_expr, env, ctx)?;
            make_args.push(name_temp);
            make_args.push(value_temp);
        }
        let dst = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst,
            callee: "%make".to_string(),
            args: make_args,
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        Ok(dst)
    }

    /// Sprint 16: lower one of the `<pair>` / `<list>` builtins to a
    /// runtime-shim DirectCall. Each builtin lowers to a `%pair*` /
    /// `%nil` / `%empty?` synthetic callee that codegen recognises.
    /// Result temps carry the narrowest sound estimate so the dispatch
    /// resolver can pick sealed-direct on subsequent calls — `pair`
    /// returns `Class(<pair>)`, `head`/`tail` return `Top`, `empty?`
    /// returns `Boolean`, `nil` returns `Class(<empty-list>)`.
    fn lower_list_builtin(
        &mut self,
        builtin: ListBuiltin,
        args: &[Expr],
        env: &mut LocalEnv,
        ctx: &LowerCtx,
        span: Span,
    ) -> Result<TempId, LoweringError> {
        // Validate arity up front so the diagnostic points at the call,
        // not at codegen.
        let expected = builtin.arity();
        if args.len() != expected {
            return Err(LoweringError::Unsupported {
                span,
                message: format!(
                    "Sprint 16 builtin `{}` expects {} argument(s), got {}",
                    builtin.name(),
                    expected,
                    args.len(),
                ),
            });
        }
        let arg_temps: Vec<TempId> = args
            .iter()
            .map(|a| self.lower_expr(a, env, ctx))
            .collect::<Result<_, _>>()?;
        let pair_cid = nod_runtime::ClassId::PAIR.0;
        let empty_cid = nod_runtime::ClassId::EMPTY_LIST.0;
        let result_ty = match builtin {
            ListBuiltin::Pair => TypeEstimate::Class(pair_cid),
            ListBuiltin::Head | ListBuiltin::Tail => TypeEstimate::Top,
            ListBuiltin::EmptyP => TypeEstimate::Boolean,
            ListBuiltin::Nil => TypeEstimate::Class(empty_cid),
        };
        let dst = self.fresh_temp(result_ty);
        self.push(Computation::DirectCall {
            dst,
            callee: builtin.callee_symbol().to_string(),
            args: arg_temps,
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        Ok(dst)
    }

    fn emit_fixnum_const(&mut self, n: i64) -> TempId {
        // Sprint 22 helper — emit a small fixnum constant as a temp.
        let w = nod_runtime::Word::from_fixnum(n)
            .expect("emit_fixnum_const value fits in fixnum range");
        let t = self.fresh_temp(TypeEstimate::Integer);
        self.push(Computation::Const {
            dst: t,
            value: ConstValue::WordBits(w.raw()),
        });
        t
    }

    /// Sprint 47 — lower `values(a, b, c, …)` to the SBCL-style
    /// secondary-values protocol (see `docs/COMPILER_GAPS.md` GAP-003
    /// and `src/nod-runtime/src/values.rs`).
    ///
    /// Lowering shape:
    ///   * Empty `values()` → return `#f` (matches Dylan's "no values"
    ///     convention; multi-binder receivers with `count = 0` see all
    ///     binders bound to `#f` via `%values-get` returning `#f` past
    ///     the count boundary).
    ///   * Single `values(x)` → just `x`. No buffer touch. This keeps
    ///     the common "I wrote `values(x)` for symmetry but only want
    ///     one value" case zero-cost.
    ///   * Multi `values(a, b, c)` → emit
    ///     `nod_values_set(0, b); nod_values_set(1, c); return a`.
    ///     The first value flows out through the ordinary single-value
    ///     ABI; extras are stashed in the TLS buffer for the caller to
    ///     pick up via `nod_values_get(i)` after a corresponding
    ///     `nod_values_clear` cleared count to 0.
    fn lower_values(
        &mut self,
        args: &[Expr],
        env: &mut LocalEnv,
        ctx: &LowerCtx,
        _span: Span,
    ) -> Result<TempId, LoweringError> {
        if args.is_empty() {
            // `values()` — no values. Return `#f`. Multi-binder receivers
            // observe all binders as `#f` (since count stays at 0 after
            // the caller's `nod_values_clear`).
            let t = self.fresh_temp(TypeEstimate::Boolean);
            self.push(Computation::Const {
                dst: t,
                value: ConstValue::Bool(false),
            });
            return Ok(t);
        }
        if args.len() == 1 {
            // `values(x)` — degenerate single-value form. The ordinary
            // ABI return is exactly `x`; no extras to stash, no buffer
            // touch.
            return self.lower_expr(&args[0], env, ctx);
        }
        // First, evaluate every argument expression and capture its
        // temp. We do this in source order BEFORE emitting any
        // `nod_values_set` calls so the evaluation effects of arg N+1
        // can't perturb the secondary-values buffer in a way that
        // arg N's set would then overwrite (each `set` writes a
        // specific index, but the cleanest correctness story is "all
        // extras computed before any `set` runs"). It also matches
        // how Dylan call arguments are evaluated.
        let arg_temps: Vec<TempId> = args
            .iter()
            .map(|a| self.lower_expr(a, env, ctx))
            .collect::<Result<_, _>>()?;
        // Stash extras (indices 1..) into the secondary-values buffer.
        // The first arg flows out through the ordinary return path.
        for (extra_idx, &val_temp) in arg_temps.iter().enumerate().skip(1) {
            // Buffer index is `extra_idx - 1` because index 0 of the
            // buffer holds the SECOND return value (the first goes
            // through the normal ABI).
            let buf_idx = (extra_idx - 1) as i64;
            let idx_t = self.emit_fixnum_const(buf_idx);
            let dst = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst,
                callee: "nod_values_set".to_string(),
                args: vec![idx_t, val_temp],
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
        }
        Ok(arg_temps[0])
    }

    /// Sprint 47 — common helper for the multi-binder
    /// `Statement::Let { binders: [a, b, c], value }` lowering. Called
    /// from every site that processes `Statement::Let` (function body,
    /// expression-context `Stmt`, loop body) when `binders.len() > 1`.
    ///
    /// Emits, in order:
    ///   1. `nod_values_clear()` — defensive reset so polluted state
    ///      from any earlier multi-value call doesn't leak into the
    ///      `(b, c, …) = …` destructure if the RHS turns out to be
    ///      single-valued.
    ///   2. The value expression — its result is the first return value
    ///      and is bound to `binders[0]` (with cell-promotion if the
    ///      binder is captured by an inner closure, matching the
    ///      single-binder path).
    ///   3. For each subsequent binder at index `i`, emit
    ///      `nod_values_get(i - 1)` and bind the resulting temp to
    ///      `binders[i]`. If the call returned fewer values than asked
    ///      for, `nod_values_get` returns `#f` past the count boundary
    ///      (standard CL discipline).
    ///
    /// Returns the temp bound to `binders[0]` — that's the "value of
    /// the let" for callers that care (loop-body / Expr::Stmt contexts).
    fn lower_let_multi_binders(
        &mut self,
        binders: &[Binder],
        value: &Expr,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<TempId, LoweringError> {
        debug_assert!(
            binders.len() > 1,
            "lower_let_multi_binders called with {} binders; \
             single-binder path handles len()==1",
            binders.len()
        );
        // 1. Clear the secondary-values buffer.
        let clear_dst = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::DirectCall {
            dst: clear_dst,
            callee: "nod_values_clear".to_string(),
            args: Vec::new(),
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        // 2. Evaluate the RHS. Its primary return goes through the
        //    ordinary ABI; any extras live in the TLS buffer.
        let primary = self.lower_expr(value, env, ctx)?;
        // Bind `binders[0]`, with cell-promotion when the binder is
        // captured by an inner closure (matches the single-binder path
        // in lower_body_stmts).
        let first_name = &binders[0].name;
        let bound_first = if self.cell_ctx.cell_locals.contains(first_name) {
            let cell = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst: cell,
                callee: "nod_make_cell".to_string(),
                args: vec![primary],
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            cell
        } else {
            primary
        };
        env.insert(first_name.clone(), bound_first);
        // 3. Read each extra and bind. Buffer index `b_i - 1` because
        //    buffer slot 0 holds the SECOND return value (the first
        //    one came through the ordinary ABI).
        for (extra_idx, b) in binders.iter().enumerate().skip(1) {
            let buf_idx = (extra_idx - 1) as i64;
            let idx_t = self.emit_fixnum_const(buf_idx);
            let val_t = self.fresh_temp(TypeEstimate::Top);
            self.push(Computation::DirectCall {
                dst: val_t,
                callee: "nod_values_get".to_string(),
                args: vec![idx_t],
                safepoint_roots: Vec::new(), is_no_alloc: false,
            });
            let bound = if self.cell_ctx.cell_locals.contains(&b.name) {
                let cell = self.fresh_temp(TypeEstimate::Top);
                self.push(Computation::DirectCall {
                    dst: cell,
                    callee: "nod_make_cell".to_string(),
                    args: vec![val_t],
                    safepoint_roots: Vec::new(),
                    is_no_alloc: false,
                });
                cell
            } else {
                val_t
            };
            env.insert(b.name.clone(), bound);
        }
        Ok(primary)
    }

    fn emit_class_metadata_ptr_const(&mut self, class_id: ClassId) -> TempId {
        // The class-metadata pointer is the raw address of the
        // `ClassMetadata` struct in the static area — NOT a tagged
        // Word. `nod_make`'s first param is a raw pointer.
        //
        // Sprint 38c — emits `ConstValue::ClassMetadataPtr { class_id,
        // tagged: false }`. Codegen loads through the per-module
        // external global without applying the pointer-tag OR.
        let t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::Const {
            dst: t,
            value: ConstValue::ClassMetadataPtr {
                class_id: class_id.0,
                tagged: false,
            },
        });
        t
    }

    fn emit_symbol_literal(&mut self, name: &str) -> TempId {
        // Symbol literal: pin `:name` in the literal pool's static
        // area and bake the tagged Word.
        //
        // Sprint 38c — emits `ConstValue::SymbolLiteralRef(name)` so
        // codegen lowers via the per-module external global pattern.
        let t = self.fresh_temp(TypeEstimate::Top);
        self.push(Computation::Const {
            dst: t,
            value: ConstValue::SymbolLiteralRef(name.to_string()),
        });
        t
    }

    fn lower_instance_check(
        &mut self,
        value: &Expr,
        class: &Expr,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
        span: Span,
    ) -> Result<TempId, LoweringError> {
        let v = self.lower_expr(value, env, ctx)?;
        let check = match class {
            Expr::Ident(_, name) => match name.as_str() {
                "<integer>" => ClassCheck::Integer,
                "<boolean>" => ClassCheck::Boolean,
                "<string>" | "<byte-string>" => ClassCheck::String,
                "<symbol>" => ClassCheck::Symbol,
                // `<simple-object-vector>` keeps the exact-id fast path:
                // it is the concrete SOV class (id 9), and a real SOV's
                // wrapper carries exactly that id.
                "<simple-object-vector>" => ClassCheck::Vector,
                // Collection-classes lever: `<vector>` / `<array>` /
                // `<simple-vector>` are abstract stdlib classes. A real
                // `<simple-object-vector>` IS one of them, but its CPL
                // (seed class parented on `<object>`) does NOT contain the
                // abstract class id, so an exact-id check OR a CPL walk
                // alone is each insufficient. Route through
                // `VectorOrUserClass`, which codegen lowers as the SOV
                // fast path OR `nod_is_instance_of` — so both a real SOV
                // and a `<bit-vector>` (whose CPL DOES contain `<vector>`)
                // answer `#t`. Resolve the abstract class's runtime id by
                // name (the stdlib `define class`es registered it).
                "<vector>" | "<array>" | "<simple-vector>" => {
                    match ctx
                        .user_classes
                        .get(name)
                        .copied()
                        .or_else(|| resolve_class_id_by_name(name))
                    {
                        Some(id) => ClassCheck::VectorOrUserClass {
                            id: id.0,
                            name: name.clone(),
                        },
                        // Stdlib not yet registered (shouldn't happen in
                        // the real pipeline) — fall back to the SOV fast
                        // path so a real SOV still answers `#t`.
                        None => ClassCheck::Vector,
                    }
                }
                "<character>" => ClassCheck::Character,
                "<empty-list>" => ClassCheck::EmptyList,
                _ => {
                    let cid = ctx
                        .user_classes
                        .get(name)
                        .copied()
                        .or_else(|| resolve_class_id_by_name(name));
                    match cid {
                        Some(id) => ClassCheck::UserClass {
                            id: id.0,
                            name: name.clone(),
                        },
                        None => ClassCheck::Unsupported {
                            name: static_class_name(name),
                        },
                    }
                }
            },
            _ => {
                return Err(LoweringError::Unsupported {
                    span,
                    message: "second argument to `instance?` must be a class name literal"
                        .to_string(),
                });
            }
        };
        let dst = self.fresh_temp(TypeEstimate::Boolean);
        self.push(Computation::TypeCheck {
            dst,
            value: v,
            class: check,
        });
        Ok(dst)
    }

    /// Task #251 — short-circuit lowering for Dylan's `|` (or) and
    /// `&` (and). Mirrors `lower_if`'s 3-block CFG with env merging,
    /// but the "short-circuit" edge just forwards the lhs value
    /// without re-evaluating it.
    ///
    /// CFG shape (for `a | b`):
    /// ```text
    ///   cur:                            lhs evaluated here; env mutations
    ///     l = lower(lhs)                from `lhs` stick unconditionally.
    ///     if l then sc_edge else rhs_b
    ///   sc_edge:                        Trivial trampoline — `Terminator::If`
    ///     jump join(l, pre_rhs_env...)  can't carry args, so we jump from
    ///                                   here with `l` + the pre-rhs values
    ///                                   of every name `rhs` would rebind.
    ///   rhs_b:
    ///     r = lower(rhs)                env mutations from `rhs` only
    ///     jump join(r, post_rhs_env...) committed on this path.
    ///   join(result_param, ...phi):
    ///     // result_param IS the BinOp's value; merge vars rebound in
    ///     // env so post-binop code sees correctly-phi'd values.
    /// ```
    ///
    /// For `a & b` the only difference is the branch edges flip:
    /// truthy goes to `rhs_b`, falsy goes to `sc_edge`. The result
    /// type at the join is `l_ty.join(r_ty)` — same convention as
    /// `lower_if`.
    fn lower_short_circuit(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<TempId, LoweringError> {
        debug_assert!(matches!(op, BinOp::Or | BinOp::And));

        // Step 1: lower lhs in the current block. Any env mutations
        // from `lhs` are committed for every path through this point
        // (lhs always runs, regardless of short-circuit choice). No
        // merging needed for lhs-side rebindings.
        let l = self.lower_expr(lhs, env, ctx)?;
        let l_ty = self.func.temp_type(l);

        // Step 2: walk rhs upfront to find every name it would rebind,
        // so we know what phi params the join block needs. Same
        // discipline as `lower_if`'s `assigned_in_arms` collection.
        //
        // GAP-008: conservatively add ALL GC-managed env bindings to
        // the sc_join params, not just names assigned in the rhs. The
        // rhs block may contain dispatch safepoints that reload a
        // GC-managed temp.  Since `sc_edge` is a sibling predecessor
        // of `sc_join`, those reloads only dominate `sc_rhs` — they do
        // NOT dominate `sc_join` or any block reachable from it.
        // Without explicit phi params, the reload leaks through
        // `block_entry_temps` into downstream if-arm blocks where LLVM
        // correctly rejects it as a non-dominating SSA use.
        // Mirroring `lower_if`'s conservative merge ensures every
        // GC-managed pointer is properly phi'd at the join, giving
        // downstream code a dominating value.
        let mut assigned_in_rhs: HashSet<String> = HashSet::new();
        collect_assigned_in_expr(rhs, env, &mut assigned_in_rhs);
        let mut merge_name_set = assigned_in_rhs;
        for (name, &temp) in env.iter() {
            if self.func.temp_type(temp).needs_gc_protection() {
                merge_name_set.insert(name.clone());
            }
        }
        let mut merge_names: Vec<String> = merge_name_set.into_iter().collect();
        merge_names.sort(); // deterministic param order
        merge_names.retain(|n| env.contains_key(n));
        let pre_rhs_env = env.clone();

        // Step 3: allocate the three blocks. `sc_edge` is the trivial
        // trampoline that carries the short-circuit args; `rhs_b` is
        // where the right operand runs; `join_b` is the merge point.
        let sc_idx = self.next_block;
        let rhs_idx = self.next_block + 1;
        let sc_edge = self.new_block(format!("sc_edge{sc_idx}"));
        let rhs_b = self.new_block(format!("sc_rhs{rhs_idx}"));

        // Step 4: terminate cur with `If`, routing by op:
        //   Or:  truthy → sc_edge (return l), falsy → rhs_b
        //   And: truthy → rhs_b,              falsy → sc_edge (return l)
        let (then_block, else_block) = match op {
            BinOp::Or => (sc_edge, rhs_b),
            BinOp::And => (rhs_b, sc_edge),
            _ => unreachable!(),
        };
        self.terminate_current(Terminator::If {
            cond: l,
            then_block,
            else_block,
        });

        // Step 5: emit sc_edge — jump to join carrying lhs as the
        // value param and the pre-rhs env values for every merge name.
        let sc_merge_temps: Vec<TempId> = merge_names
            .iter()
            .map(|n| *pre_rhs_env.get(n).expect("merge name in pre-rhs env"))
            .collect();
        let mut sc_args: Vec<TempId> = Vec::with_capacity(1 + sc_merge_temps.len());
        sc_args.push(l);
        sc_args.extend(sc_merge_temps.iter().copied());

        // Step 6: emit rhs_b — lower the right operand against a fresh
        // copy of the pre-rhs env (mutations on this path don't leak
        // to the sc_edge path). After lowering, jump to join with the
        // rhs result + post-rhs env values for every merge name.
        // `lower_expr` may extend the CFG (e.g. nested `if`); we
        // terminate `self.current` (which `lower_expr` leaves us at),
        // not necessarily `rhs_b` itself.
        *env = pre_rhs_env.clone();
        self.switch_to(rhs_b);
        let r = self.lower_expr(rhs, env, ctx)?;
        let rhs_end_b = self.func.blocks[self.current].id;
        let r_ty = self.func.temp_type(r);
        let rhs_merge_temps: Vec<TempId> = merge_names
            .iter()
            .map(|n| *env.get(n).expect("merge name bound after rhs eval"))
            .collect();
        let mut rhs_args: Vec<TempId> = Vec::with_capacity(1 + rhs_merge_temps.len());
        rhs_args.push(r);
        rhs_args.extend(rhs_merge_temps.iter().copied());

        // GAP-010: delay `sc_join` creation until after rhs lowering so
        // any nested control-flow blocks inside the rhs appear before the
        // outer join in `func.blocks`. Codegen still walks that order when
        // seeding block-entry temps, so an early outer join can observe a
        // predecessor before its nested join has run and leak a
        // non-dominating SSA value downstream.
        let join_idx = self.next_block;
        let join_b = self.new_block(format!("sc_join{join_idx}"));

        self.switch_to(sc_edge);
        self.terminate_current(Terminator::Jump {
            target: join_b,
            args: sc_args,
        });

        self.switch_to(rhs_end_b);
        self.terminate_current(Terminator::Jump {
            target: join_b,
            args: rhs_args,
        });

        // Step 7: build join block params. Value param first, then one
        // per merged name. Order MUST match jump-args order above.
        let joined_ty = l_ty.join(r_ty);
        let join_value_param = self.add_block_param(join_b, joined_ty);
        let mut join_var_params: Vec<TempId> = Vec::with_capacity(merge_names.len());
        for (i, _n) in merge_names.iter().enumerate() {
            // Type for each merge param: join of pre-rhs (sc path) and
            // post-rhs (rhs path) types for that name.
            let sc_ty = self.func.temp_type(sc_merge_temps[i]);
            let rhs_ty = self.func.temp_type(rhs_merge_temps[i]);
            let ty = sc_ty.join(rhs_ty);
            let p = self.add_block_param(join_b, ty);
            join_var_params.push(p);
        }

        // Step 8: switch to join and rebind env so post-binop code
        // sees phi'd values for every merged name.
        self.switch_to(join_b);
        for (n, p) in merge_names.iter().zip(join_var_params.iter()) {
            env.insert(n.clone(), *p);
        }
        // GAP-012: evict any names introduced inside the rhs expression
        // (e.g. by a nested `begin let x = …; x end`). They are
        // lexically out of scope after the short-circuit join.
        env.retain(|name, _| pre_rhs_env.contains_key(name));
        Ok(join_value_param)
    }

    fn lower_if(
        &mut self,
        cond: &Expr,
        then_: &Expr,
        else_: &Expr,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<TempId, LoweringError> {
        let cond_t = self.lower_expr(cond, env, ctx)?;

        let then_idx = self.next_block;
        let else_idx = self.next_block + 1;
        let then_b = self.new_block(format!("then{then_idx}"));
        let else_b = self.new_block(format!("else{else_idx}"));

        self.terminate_current(Terminator::If {
            cond: cond_t,
            then_block: then_b,
            else_block: else_b,
        });

        // Sprint 42-pre: env-merge at the join. Walk both arms upfront
        // to find every name that gets rebound in either; snapshot the
        // pre-if env so each arm can be lowered against the same state;
        // emit join-block params for the rebound names and route each
        // arm's jump with the correct args. Without this, a then-only
        // (or else-only) `name := value` mutates env in place but no
        // join phi gets created — and when `name` is also a loop-header
        // phi target, the back-edge picks up an arm-local temp that
        // doesn't dominate the back-edge block. LLVM verification then
        // (correctly) rejects the IR with "Instruction does not dominate
        // all uses".
        //
        // Cell-promoted locals are fine without a phi (the cell pointer
        // stays the same in env across arms); we still include them in
        // the args, producing `phi(cell_t, cell_t) = cell_t`. Harmless.
        let mut assigned_in_arms: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        collect_assigned_in_expr(then_, env, &mut assigned_in_arms);
        collect_assigned_in_expr(else_, env, &mut assigned_in_arms);

        let mut merge_name_set = assigned_in_arms;
        for (name, &temp) in env.iter() {
            if self.func.temp_type(temp).needs_gc_protection() {
                merge_name_set.insert(name.clone());
            }
        }

        let mut merge_names: Vec<String> = merge_name_set.into_iter().collect();
        merge_names.sort(); // deterministic param order
        // Filter to names actually bound in env (collect_assigned_in_expr
        // already gates on env.contains_key, but be defensive).
        merge_names.retain(|n| env.contains_key(n));
        let pre_env = env.clone();

        // Lower the then arm against the pre-if env. `self.current`
        // after `lower_expr` is wherever the arm finished (possibly a
        // nested join block, not `then_b`); `terminate_current` uses
        // that, which is what we want — we terminate the *last* block
        // of the arm with the jump to the outer join.
        //
        // GAP-010: create the outer join block only AFTER both arms are
        // lowered. `new_block()` appends to `func.blocks`, and codegen
        // still emits blocks in that order. If the outer join were
        // appended before an arm's nested `if` / short-circuit join,
        // codegen could visit the outer join before its real
        // predecessors had populated `block_entry_temps`, reintroducing
        // the same stale-SSA dominance bug as GAP-009 on branch joins.
        self.switch_to(then_b);
        let then_v = self.lower_expr(then_, env, ctx)?;
        let then_end_b = self.func.blocks[self.current].id;
        let then_ty = self.func.temp_type(then_v);
        let then_merge_temps: Vec<TempId> = merge_names
            .iter()
            .map(|n| *env.get(n).expect("merge name bound after then arm"))
            .collect();
        let mut then_args: Vec<TempId> = Vec::with_capacity(1 + then_merge_temps.len());
        then_args.push(then_v);
        then_args.extend(then_merge_temps.iter().copied());

        // Reset env to pre-if state, then lower the else arm.
        *env = pre_env.clone();
        self.switch_to(else_b);
        let else_v = self.lower_expr(else_, env, ctx)?;
        let else_end_b = self.func.blocks[self.current].id;
        let else_ty = self.func.temp_type(else_v);
        let else_merge_temps: Vec<TempId> = merge_names
            .iter()
            .map(|n| *env.get(n).expect("merge name bound after else arm"))
            .collect();
        let mut else_args: Vec<TempId> = Vec::with_capacity(1 + else_merge_temps.len());
        else_args.push(else_v);
        else_args.extend(else_merge_temps.iter().copied());

        let join_idx = self.next_block;
        let join_b = self.new_block(format!("join{join_idx}"));

        self.switch_to(then_end_b);
        self.terminate_current(Terminator::Jump {
            target: join_b,
            args: then_args,
        });

        self.switch_to(else_end_b);
        self.terminate_current(Terminator::Jump {
            target: join_b,
            args: else_args,
        });

        // Add join params: if-value first, then one per merged name.
        // Param order MUST match the jump-args order above.
        let joined_ty = then_ty.join(else_ty);
        let join_value_param = self.add_block_param(join_b, joined_ty);
        let mut join_var_params: Vec<TempId> = Vec::with_capacity(merge_names.len());
        for (i, _n) in merge_names.iter().enumerate() {
            let then_t_ty = self.func.temp_type(then_merge_temps[i]);
            let else_t_ty = self.func.temp_type(else_merge_temps[i]);
            let ty = then_t_ty.join(else_t_ty);
            let p = self.add_block_param(join_b, ty);
            join_var_params.push(p);
        }

        // Switch to join and rebind env so post-if code sees the phi'd
        // values. The caller (let-binding, sequence, etc.) just reads
        // env normally.
        self.switch_to(join_b);
        for (n, p) in merge_names.iter().zip(join_var_params.iter()) {
            env.insert(n.clone(), *p);
        }
        // GAP-012: evict any names that were introduced by `let`
        // bindings inside one of the arms. Arms execute in a scope
        // lexically nested under the `if`; those names are not visible
        // after the join. Without this, post-if code (another `if`,
        // a dispatch, etc.) finds an arm-local SSA temp in env and
        // conservatively includes it as a GC root, violating LLVM
        // dominance when the temp is used outside its defining block.
        env.retain(|name, _| pre_env.contains_key(name));
        Ok(join_value_param)
    }


    /// Sprint 18: lower `while (cond) body end` / `until (cond) body end`
    /// into a three-block CFG with a back-edge.
    ///
    /// ```text
    ///   entry → header(phi_i, phi_total, …)
    ///   header:
    ///     cond_t = eval(cond)
    ///     if cond_t (or !cond_t for until) → loop_body else exit
    ///   loop_body:
    ///     eval(body…)                 (updates env for loop vars)
    ///     jump header(new_i, new_total, …)   ← back-edge
    ///   exit:
    ///     (fall-through; caller continues here)
    /// ```
    ///
    /// Loop variables — names assigned (`:=`) inside `body` or assigned
    /// by a nested `let` after they were established outside — become
    /// block parameters on `header`. Their initial values come from the
    /// pre-loop env; the back-edge re-supplies the post-body values.
    /// Names that are only *read* inside the loop body need no param.
    ///
    /// `invert_cond` flips the header branch (for `until`).
    fn lower_while_like(
        &mut self,
        cond: &Expr,
        body: &[Statement],
        invert_cond: bool,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<(), LoweringError> {
        // GAP-009: `loop_body` and `loop_exit` are created AFTER
        // `lower_expr(cond)` (see below) so that any blocks emitted by
        // the condition — e.g. `sc_edge`/`sc_rhs`/`sc_join` from a
        // short-circuit operator — appear before them in `func.blocks`.
        // Codegen iterates `func.blocks` in creation order; if
        // `loop_exit` preceded `sc_join`, codegen would process
        // `loop_exit` before its only CFG predecessor (`sc_join`) had
        // run, leaving `block_entry_temps[loop_exit]` empty and causing
        // stale GC-managed values to propagate into post-loop code.
        let header_idx = self.next_block;
        let header_b = self.new_block(format!("loop_header{header_idx}"));

        // Pre-scan: which names must be carried around the loop header?
        // Assigned names obviously need header params so the back-edge can
        // re-supply the updated value. For GC-managed values we have to be
        // more conservative: any live binding in env may be relocated by a
        // safepoint in the loop body, even if the source loop never mentions
        // that name directly. If post-loop code still reads the binding, the
        // exit path must see a header phi, not a body-local reload/join temp.
        let assigned_names = collect_assigned_names_in_stmts(body, env);
        let mut carried_names = assigned_names.clone();
        let mut used_names = collect_used_bound_names_in_expr(cond, env);
        used_names.extend(collect_used_bound_names_in_stmts(body, env));
        for (name, &temp) in env.iter() {
            if self.func.temp_type(temp).needs_gc_protection() {
                carried_names.insert(name.clone());
                continue;
            }
            if used_names.contains(name) {
                carried_names.insert(name.clone());
            }
        }

        // Snapshot pre-loop temps for each assigned name. Create block
        // params on `header` for them; the entry-side jump carries the
        // pre-loop temps as args, the back-edge carries the post-body
        // temps.
        let mut loop_var_order: Vec<String> = carried_names.into_iter().collect();
        loop_var_order.sort(); // deterministic param ordering
        let mut pre_loop_temps: Vec<TempId> = Vec::with_capacity(loop_var_order.len());
        let mut header_params: Vec<TempId> = Vec::with_capacity(loop_var_order.len());
        for n in &loop_var_order {
            // WHY: every loop var must already be in env (introduced by
            // a `let` before the loop); lowering errors out earlier if
            // an unbound name is referenced.
            let outer = *env.get(n).ok_or_else(|| LoweringError::Unsupported {
                span: cond.span(),
                message: format!(
                    "loop variable `{n}` not bound before loop entry (Sprint 18)"
                ),
            })?;
            pre_loop_temps.push(outer);
            let ty = self.func.temp_type(outer);
            let phi = self.add_block_param(header_b, ty);
            header_params.push(phi);
        }

        // Entry-side jump → header with pre-loop temps as initial args.
        self.terminate_current(Terminator::Jump {
            target: header_b,
            args: pre_loop_temps.clone(),
        });

        // Update env so the header / body see the header-block params
        // when reading the loop vars.
        for (n, phi) in loop_var_order.iter().zip(header_params.iter()) {
            env.insert(n.clone(), *phi);
        }

        // ─── header ─── evaluate cond, branch.
        // body_b and exit_b are created HERE, after lower_expr(cond),
        // so that any sc_edge/sc_rhs/sc_join blocks from a short-circuit
        // condition appear before body_b/exit_b in func.blocks (GAP-009).
        self.switch_to(header_b);
        let cond_t = self.lower_expr(cond, env, ctx)?;
        let body_idx = self.next_block;
        let exit_idx = self.next_block + 1;
        let body_b = self.new_block(format!("loop_body{body_idx}"));
        let exit_b = self.new_block(format!("loop_exit{exit_idx}"));
        let (then_block, else_block) = if invert_cond {
            (exit_b, body_b)
        } else {
            (body_b, exit_b)
        };
        self.terminate_current(Terminator::If {
            cond: cond_t,
            then_block,
            else_block,
        });

        // ─── loop_body ─── lower each body stmt, then jump back to
        // header with the post-body temps.
        // GAP-012: snapshot env keys before body lowering so we can
        // evict loop-body-local `let` bindings at loop exit. Loop-body
        // `let`s (e.g. `let b = element(path, i)`) are lexically scoped
        // to the body; leaving them in env after the loop causes a
        // post-loop `lower_if` to include a body-local SSA temp as a GC
        // root, violating LLVM dominance for any use outside the loop.
        let pre_body_env_names: HashSet<String> = env.keys().cloned().collect();
        self.switch_to(body_b);
        for s in body {
            self.lower_loop_body_stmt(s, env, ctx)?;
        }
        let back_args: Vec<TempId> = loop_var_order
            .iter()
            .map(|n| *env.get(n).expect("loop var lost"))
            .collect();
        self.terminate_current(Terminator::Jump {
            target: header_b,
            args: back_args,
        });

        // ─── exit ─── caller continues here. The env's mapping for
        // each loop var should reflect the header's phi (since after
        // the loop, control reaches exit ONLY from the header's false
        // branch, where the latest cond-checked value is the header
        // phi). Restore env to the header param mapping.
        for (n, phi) in loop_var_order.iter().zip(header_params.iter()) {
            env.insert(n.clone(), *phi);
        }
        // GAP-012: evict loop-body-local `let` bindings from env.
        // Only names that existed before the loop body was entered are
        // in scope after the loop exits. Without this, any name
        // introduced by a `let` inside the body (e.g. `let b =
        // element(path, i)`) survives in env pointing at a body-local
        // SSA value. A post-loop `lower_if` would then conservatively
        // add that name to its GC-root merge, producing a store of a
        // non-dominating temp — rejected by the LLVM verifier.
        env.retain(|name, _| pre_body_env_names.contains(name));
        self.switch_to(exit_b);
        Ok(())
    }

    /// Sprint 18: lower a single body statement inside a `while`/`until`
    /// loop. Mirrors the function-body statement loop but never sets
    /// `final_temp` (the loop's value is discarded) and recognises the
    /// nested-loop case by recursing into `lower_while_like`.
    fn lower_loop_body_stmt(
        &mut self,
        s: &Statement,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<(), LoweringError> {
        match s {
            Statement::Expr(e) => {
                self.lower_expr(e, env, ctx)?;
                Ok(())
            }
            Statement::Let {
                binders, rest, value, span,
            } => {
                if rest.is_some() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`#rest` binder in `let` not supported yet".to_string(),
                    });
                }
                if binders.is_empty() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`let` with no binders".to_string(),
                    });
                }
                if binders.len() > 1 {
                    // Sprint 47 — multi-binder `let` inside a loop body.
                    self.lower_let_multi_binders(binders, value, env, ctx)?;
                } else {
                    let bname = &binders[0].name;
                    let t = self.lower_expr(value, env, ctx)?;
                    env.insert(bname.clone(), t);
                }
                Ok(())
            }
            Statement::While { cond, body, .. } => {
                self.lower_while_like(cond, body, false, env, ctx)
            }
            Statement::Until { cond, body, .. } => {
                self.lower_while_like(cond, body, true, env, ctx)
            }
            Statement::For {
                span,
                clauses,
                body,
                finally_,
            } => {
                // Nested numeric-range `for`: desugar to `let` + `while`
                // and lower each via the loop-body statement path.
                let desugared = desugar_numeric_for(*span, clauses, body, finally_)?;
                for ds in &desugared {
                    self.lower_loop_body_stmt(ds, env, ctx)?;
                }
                Ok(())
            }
            Statement::Block { span, .. } => Err(LoweringError::Unsupported {
                span: *span,
                message: "`block` inside loop body not lowered (Sprint 19)".to_string(),
            }),
            Statement::Local { span, methods } => {
                let enclosing = self.closure_key.clone();
                self.lower_local_methods(&enclosing, methods, env, ctx, *span)
            }
        }
    }

    /// Sprint 18: lower an `Expr::Stmt(s)` — used when a macro expansion
    /// produces a statement-shaped form inside an expression position
    /// (e.g. a `Begin` body containing `Expr::Stmt(While {…})`). Returns
    /// a fresh Unit temp; the macro's expansion is in service of side
    /// effects, not a value.
    fn lower_stmt_as_expr(
        &mut self,
        s: &Statement,
        env: &mut LocalEnv,
        ctx: &LowerCtx,
    ) -> Result<TempId, LoweringError> {
        match s {
            Statement::While { cond, body, .. } => {
                self.lower_while_like(cond, body, false, env, ctx)?;
                Ok(self.unit_temp())
            }
            Statement::Until { cond, body, .. } => {
                self.lower_while_like(cond, body, true, env, ctx)?;
                Ok(self.unit_temp())
            }
            Statement::Let {
                binders, rest, value, span,
            } => {
                if rest.is_some() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`#rest` binder in `let` not supported yet".to_string(),
                    });
                }
                if binders.is_empty() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`let` with no binders".to_string(),
                    });
                }
                if binders.len() > 1 {
                    // Sprint 47 — multi-binder `let` in expression-stmt context.
                    let t = self.lower_let_multi_binders(binders, value, env, ctx)?;
                    Ok(t)
                } else {
                    let bname = &binders[0].name;
                    let t = self.lower_expr(value, env, ctx)?;
                    env.insert(bname.clone(), t);
                    Ok(t)
                }
            }
            Statement::Expr(e) => self.lower_expr(e, env, ctx),
            Statement::For {
                span,
                clauses,
                body,
                finally_,
            } => {
                // Desugar `for` to `let`s + `while` + result. The for's
                // value (used when it appears in expression position, e.g.
                // `block (return) for (…) finally R end end`) is the
                // result of its trailing statement(s).
                let desugared = desugar_numeric_for(*span, clauses, body, finally_)?;
                let mut last = self.unit_temp();
                for ds in &desugared {
                    last = self.lower_stmt_as_expr(ds, env, ctx)?;
                }
                Ok(last)
            }
            Statement::Local { span, methods } => {
                let enclosing = self.closure_key.clone();
                self.lower_local_methods(&enclosing, methods, env, ctx, *span)?;
                Ok(self.unit_temp())
            }
            Statement::Block {
                span,
                exit_var,
                body,
                handlers,
                cleanup,
                afterwards,
            } => self.lower_block_in_expr(
                env,
                ctx,
                *span,
                exit_var.as_deref(),
                body,
                handlers,
                cleanup,
                afterwards,
            ),
        }
    }

    /// Sprint 18: produce a fresh `<unit>`-typed temp materialised as a
    /// `Const(Bool(false))` so the SSA verifier sees a definition. Used
    /// when a loop/`Expr::Stmt` lowering needs a placeholder value for
    /// expression-context callers. The temp's `type_estimate` is `Unit`
    /// so the surrounding context knows the value is meaningless.
    fn unit_temp(&mut self) -> TempId {
        let t = self.fresh_temp(TypeEstimate::Unit);
        self.push(Computation::Const {
            dst: t,
            value: ConstValue::Bool(false),
        });
        t
    }
}

/// Desugar a `for` loop into a sequence of `let` bindings, a `while`
/// loop, and a trailing result expression.
///
/// Handles the full clause vocabulary the front-end models:
///
/// * **Numeric** — `VAR [:: T] from FROM [to TO | below BELOW | above
///   ABOVE] [by BY]`. Binds `let VAR = FROM`, contributes the bound test
///   `VAR <= TO` / `VAR < BELOW` / `VAR > ABOVE` to the loop condition,
///   and steps `VAR := VAR + BY` (BY defaults to 1). `to` is inclusive;
///   `below`/`above` are exclusive.
/// * **Step** — `VAR = INIT [then NEXT]`. Binds `let VAR = INIT`; if
///   `then NEXT` is present, steps `VAR := NEXT`. Contributes no bound.
/// * **Bare from** — `VAR` shorthand modelled as a step with no bound.
/// * **`until:` COND** — contributes `~COND` to the loop condition
///   (loop while the until-test is false).
/// * **`while:` COND** — contributes `COND` to the loop condition.
///
/// Multiple comma-separated clauses compose: every clause's `let` runs
/// once before the loop; every clause's step runs each iteration; the
/// loop continues while the conjunction (`&`) of all bound/while/until
/// tests holds. Steps use **parallel** (simultaneous) assignment — all
/// `NEXT` / `VAR + BY` expressions read the values from the start of the
/// iteration. We realise this by evaluating each next-value into a fresh
/// `let`-bound temp first, then assigning them all.
///
/// The desugar shape is:
/// ```text
///   let v1 = init1; …; let vN = initN;
///   while ( <conjunction of all tests> )
///     BODY…;
///     let __for_next_v1 = step1; …;     // parallel next-values
///     v1 := __for_next_v1; …;           // then assign
///   end;
///   <finally-result or #f>
/// ```
/// The trailing statement(s) carry the `for` expression's value: the
/// `finally` body if present (it may reference the loop variables in
/// their post-loop state), otherwise `#f`.
///
/// `in`-collection and `keyed-by` clauses are NOT handled here (the
/// for-each macro expands those); they return an `Unsupported` error so
/// the caller bails cleanly.
fn desugar_numeric_for(
    span: Span,
    clauses: &[ForClause],
    body: &[Statement],
    finally_: &[Statement],
) -> Result<Vec<Statement>, LoweringError> {
    let bail = |msg: &str| LoweringError::Unsupported {
        span,
        message: format!("`for` not lowered: {msg}"),
    };
    if clauses.is_empty() {
        return Err(bail("`for` with no clauses unsupported"));
    }

    // Pre-loop `let` bindings (one per iteration variable), the
    // accumulated loop-continuation tests (conjoined with `&`), and the
    // end-of-iteration step statements (next-value temps, then the
    // parallel assignments).
    let mut lets: Vec<Statement> = Vec::new();
    let mut tests: Vec<Expr> = Vec::new();
    // `while:`/`until:` end-tests are kept separate from the structural tests
    // (numeric bounds, `~fip-finished?`). A user test may reference an
    // `in`-clause variable, which is only bound at the TOP of the body
    // (`head_lets`) — AFTER the structural continuation test runs in the loop
    // head. So we bind the `in`-variables in the loop head too, by wrapping the
    // user test in a `begin let var = current-element(state); … TEST end`. Each
    // `in`-clause records its (var, current-element) here for that wrapper.
    let mut user_tests: Vec<Expr> = Vec::new();
    let mut in_elem_bindings: Vec<(String, Expr)> = Vec::new();
    let mut next_temps: Vec<Statement> = Vec::new();
    let mut assigns: Vec<Statement> = Vec::new();

    // Top-of-iteration `let` rebindings (one per `in`-collection clause):
    // `let var = %fip-current-element(state)`. These MUST come before the
    // user body each iteration so the loop variable holds the current
    // element. The FIP `%fip-advance!(state)` step is appended to
    // `assigns` (end of iteration), composing with numeric/step assigns.
    let mut head_lets: Vec<Statement> = Vec::new();

    // Counter so each step clause gets a unique temp name for its
    // parallel next-value. Names use a sigil-prefixed form that cannot
    // collide with a source identifier.
    let mut next_idx = 0usize;
    // Counter so each `in`-collection clause gets a unique FIP-state temp.
    let mut fip_idx = 0usize;

    for clause in clauses {
        match clause {
            ForClause::Numeric(nfc) => {
                let var = nfc.var.clone();
                let var_ident = Expr::Ident(span, var.clone());
                // Bound → continuation test. At most one is meaningful;
                // prefer below/above/to in that order if several parsed.
                if let Some(b) = &nfc.below {
                    tests.push(Expr::BinOp {
                        span,
                        op: BinOp::Lt,
                        lhs: Box::new(var_ident.clone()),
                        rhs: Box::new(b.clone()),
                    });
                } else if let Some(a) = &nfc.above {
                    tests.push(Expr::BinOp {
                        span,
                        op: BinOp::Gt,
                        lhs: Box::new(var_ident.clone()),
                        rhs: Box::new(a.clone()),
                    });
                } else if let Some(t) = &nfc.to {
                    // `to` is inclusive but DIRECTION-sensitive: with a
                    // negative literal step the loop descends, so the
                    // continuation test is `VAR >= TO`, not `VAR <= TO`.
                    // (For a non-literal `by`, we assume ascending — the
                    // historical behaviour; corpus descending loops all use
                    // a negative integer literal like `by -1`.)
                    let op = if by_is_negative_literal(nfc.by.as_ref()) {
                        BinOp::Ge
                    } else {
                        BinOp::Le
                    };
                    tests.push(Expr::BinOp {
                        span,
                        op,
                        lhs: Box::new(var_ident.clone()),
                        rhs: Box::new(t.clone()),
                    });
                }
                // (A numeric clause with no bound contributes no test —
                // valid as long as some OTHER clause bounds the loop.)

                // BY defaults to 1; step is VAR := VAR + BY.
                let by = nfc.by.clone().unwrap_or(Expr::Integer(span, 1));
                let next = Expr::BinOp {
                    span,
                    op: BinOp::Add,
                    lhs: Box::new(var_ident.clone()),
                    rhs: Box::new(by),
                };
                push_step(span, &mut next_idx, &var, next, &mut next_temps, &mut assigns);

                lets.push(Statement::Let {
                    span,
                    binders: vec![Binder { span, name: var, type_: None }],
                    rest: None,
                    value: nfc.from.clone(),
                });
            }
            ForClause::From(ffc) => {
                // Bare `from` (no bound): bind and step by `by` (default
                // 1). No continuation test of its own.
                let var = ffc.var.clone();
                let var_ident = Expr::Ident(span, var.clone());
                let by = ffc.by.clone().unwrap_or(Expr::Integer(span, 1));
                let next = Expr::BinOp {
                    span,
                    op: BinOp::Add,
                    lhs: Box::new(var_ident.clone()),
                    rhs: Box::new(by),
                };
                push_step(span, &mut next_idx, &var, next, &mut next_temps, &mut assigns);
                lets.push(Statement::Let {
                    span,
                    binders: vec![Binder { span, name: var, type_: None }],
                    rest: None,
                    value: ffc.from.clone(),
                });
            }
            ForClause::Step(sfc) => {
                // `var = init [then next]`: bind init; step to next if a
                // `then` clause was given (otherwise the value is fixed).
                let var = sfc.var.clone();
                if let Some(next) = &sfc.next {
                    push_step(
                        span,
                        &mut next_idx,
                        &var,
                        next.clone(),
                        &mut next_temps,
                        &mut assigns,
                    );
                }
                lets.push(Statement::Let {
                    span,
                    binders: vec![Binder { span, name: var, type_: None }],
                    rest: None,
                    value: sfc.init.clone(),
                });
            }
            ForClause::While { cond, .. } => {
                // `while: T` — loop while T holds.
                user_tests.push(cond.clone());
            }
            ForClause::Until { cond, .. } => {
                // `until: T` — loop while ~T holds.
                user_tests.push(Expr::UnOp {
                    span,
                    op: UnOp::Not,
                    operand: Box::new(cond.clone()),
                });
            }
            ForClause::In { var, coll, .. } => {
                // `var in coll` — forward-iteration protocol. One FIP
                // state per in-clause; mirrors the `for-each` macro shape
                // but composes with the other clauses' tests/steps and
                // supports parallel `in` (stop at the shortest collection):
                //   before: let %state = %fip-init(coll);
                //   test:   ~ %fip-finished?(%state)   (ANDed with others)
                //   head:   let var = %fip-current-element(%state);
                //   end:    %fip-advance!(%state)
                let state_name = format!("__for_fip_{fip_idx}");
                fip_idx += 1;
                let state_ident = Expr::Ident(span, state_name.clone());

                // Pre-loop: let %state = %fip-init(coll);
                lets.push(Statement::Let {
                    span,
                    binders: vec![Binder { span, name: state_name.clone(), type_: None }],
                    rest: None,
                    value: fip_call(span, "%fip-init", coll.clone()),
                });

                // Continuation test: ~ %fip-finished?(%state).
                tests.push(Expr::UnOp {
                    span,
                    op: UnOp::Not,
                    operand: Box::new(fip_call(
                        span,
                        "%fip-finished?",
                        state_ident.clone(),
                    )),
                });

                // Top of iteration: let var = %fip-current-element(%state).
                let elem = fip_call(span, "%fip-current-element", state_ident.clone());
                head_lets.push(Statement::Let {
                    span,
                    binders: vec![Binder { span, name: var.clone(), type_: None }],
                    rest: None,
                    value: elem.clone(),
                });
                // Record (var, current-element) so a `while:`/`until:` test that
                // references `var` can bind it in the loop head (see assembly).
                in_elem_bindings.push((var.clone(), elem));

                // End of iteration: %fip-advance!(%state). Advancing the
                // state is independent of the loop variables, so it joins
                // `assigns` directly (no parallel-step temp needed).
                assigns.push(Statement::Expr(fip_call(
                    span,
                    "%fip-advance!",
                    state_ident,
                )));
            }
            ForClause::Keyed { .. } => {
                // `var keyed-by key in coll` needs a `%fip-current-key`
                // primitive that the runtime doesn't yet export; bail
                // cleanly rather than emit a half-correct loop.
                return Err(bail("`keyed-by` clause unsupported"));
            }
        }
    }

    // With no test at all the loop never terminates — reject (matches the
    // old "needs to/below/above" guard, now also satisfied by any
    // `while:`/`until:` user test).
    if tests.is_empty() && user_tests.is_empty() {
        return Err(bail(
            "loop has no termination test (need `to`/`below`/`above`/`while:`/`until:`)",
        ));
    }

    // The loop condition is `structural & user`. `structural` is the
    // conjunction of numeric bounds and `~fip-finished?` checks; `user` is the
    // conjunction of `while:`/`until:` tests. A user test may reference an
    // `in`-clause variable, so we bind those variables (to the current element)
    // in a `begin` wrapper around the user test. Because `&` is short-circuit
    // and the `~fip-finished?` structural test runs first, `%fip-current-element`
    // is only evaluated when there genuinely is a current element. It's an
    // idempotent read (no advance), so binding it both here and in `head_lets`
    // is safe.
    let structural = conjoin(span, tests);
    let user = conjoin(span, user_tests).map(|u| {
        if in_elem_bindings.is_empty() {
            u
        } else {
            let mut begin_body: Vec<Expr> = in_elem_bindings
                .iter()
                .map(|(var, elem)| Expr::Let {
                    span,
                    binder: var.clone(),
                    value: Box::new(elem.clone()),
                })
                .collect();
            begin_body.push(u);
            Expr::Begin { span, body: begin_body }
        }
    });
    let cond = match (structural, user) {
        (Some(s), Some(u)) => Expr::BinOp {
            span,
            op: BinOp::And,
            lhs: Box::new(s),
            rhs: Box::new(u),
        },
        (Some(s), None) => s,
        (None, Some(u)) => u,
        (None, None) => unreachable!("checked non-empty above"),
    };

    // Loop body = `in`-clause element rebindings (top of iteration), then the
    // user body, then the parallel next-value temps, then the assignments back
    // to the iteration variables (which include each in-clause's
    // `%fip-advance!`).
    let mut while_body: Vec<Statement> = head_lets;
    while_body.extend(body.iter().cloned());
    while_body.extend(next_temps);
    while_body.extend(assigns);

    let while_stmt = Statement::While {
        span,
        cond,
        body: while_body,
    };

    // Assemble: all lets, the while, then the result value. The `finally`
    // body (if any) is the result; otherwise the `for` value is `#f`.
    let mut out = lets;
    out.push(while_stmt);
    if finally_.is_empty() {
        out.push(Statement::Expr(Expr::Bool(span, false)));
    } else {
        out.extend(finally_.iter().cloned());
    }
    Ok(out)
}

/// Build a one-argument call to a FIP primitive (`%fip-init`,
/// `%fip-finished?`, `%fip-current-element`, `%fip-advance!`). These
/// resolve to the registered primitives (see the `PRIM`-style table near
/// the top of this module) which lower to the `nod_fip_*` runtime
/// entry points.
fn fip_call(span: Span, prim: &str, arg: Expr) -> Expr {
    Expr::Call {
        span,
        callee: Box::new(Expr::Ident(span, prim.to_string())),
        args: vec![arg],
    }
}

/// Helper for [`desugar_numeric_for`]: register a parallel step for
/// iteration variable `var` whose end-of-iteration value is `next`.
///
/// Emits `let __for_next_<n> = next;` into `next_temps` and `var :=
/// __for_next_<n>;` into `assigns`. Splitting evaluation (into a fresh
/// temp) from assignment gives Dylan's simultaneous-step semantics: a
/// step's `next` reads every iteration variable's value from the START
/// of the iteration, not the partially-updated values.
fn push_step(
    span: Span,
    next_idx: &mut usize,
    var: &str,
    next: Expr,
    next_temps: &mut Vec<Statement>,
    assigns: &mut Vec<Statement>,
) {
    let tmp = format!("__for_next_{}", *next_idx);
    *next_idx += 1;
    next_temps.push(Statement::Let {
        span,
        binders: vec![Binder { span, name: tmp.clone(), type_: None }],
        rest: None,
        value: next,
    });
    assigns.push(Statement::Expr(Expr::BinOp {
        span,
        op: BinOp::Assign,
        lhs: Box::new(Expr::Ident(span, var.to_string())),
        rhs: Box::new(Expr::Ident(span, tmp)),
    }));
}

/// Is this `by` step a compile-time negative integer literal? Used to
/// pick the direction of a numeric `to` bound (descending → `>=`).
/// Recognises `-N` (parsed as `UnOp::Neg` over a positive `Integer`) and
/// a directly-negative `Integer`. A `None` (absent `by`, defaults to +1)
/// or any non-literal expression is treated as non-negative.
fn by_is_negative_literal(by: Option<&Expr>) -> bool {
    match by {
        Some(Expr::Integer(_, v)) => *v < 0,
        Some(Expr::UnOp { op: UnOp::Neg, operand, .. }) => {
            matches!(operand.as_ref(), Expr::Integer(_, v) if *v > 0)
        }
        _ => false,
    }
}

/// Fold a list of boolean test expressions into a single left-associated
/// `&` conjunction. Returns `None` for an empty list (no test).
fn conjoin(span: Span, tests: Vec<Expr>) -> Option<Expr> {
    let mut it = tests.into_iter();
    let mut acc = it.next()?;
    for t in it {
        acc = Expr::BinOp {
            span,
            op: BinOp::And,
            lhs: Box::new(acc),
            rhs: Box::new(t),
        };
    }
    Some(acc)
}

/// Visit every source expression embedded in a `for`-clause: the
/// numeric `from`/`to`/`below`/`above`/`by`, the step `init`/`next`, the
/// bare-`from` clause's `from`/`by`, the `while:`/`until:` condition, and
/// the `in`/`keyed-by` collection. Used by the enclosing-loop phi
/// analysis (`collect_assigned_*` / `collect_used_bound_*`) so that a
/// nested `for`'s clause expressions are accounted for when they
/// reference — or assign — outer bindings.
fn for_clause_exprs(c: &ForClause, f: &mut impl FnMut(&Expr)) {
    match c {
        ForClause::Numeric(n) => {
            f(&n.from);
            if let Some(e) = &n.to {
                f(e);
            }
            if let Some(e) = &n.below {
                f(e);
            }
            if let Some(e) = &n.above {
                f(e);
            }
            if let Some(e) = &n.by {
                f(e);
            }
        }
        ForClause::From(ff) => {
            f(&ff.from);
            if let Some(e) = &ff.by {
                f(e);
            }
        }
        ForClause::Step(s) => {
            f(&s.init);
            if let Some(e) = &s.next {
                f(e);
            }
        }
        ForClause::While { cond, .. } | ForClause::Until { cond, .. } => f(cond),
        ForClause::In { coll, .. } | ForClause::Keyed { coll, .. } => f(coll),
    }
}

/// Sprint 18: walk a loop body and collect every local-variable name
/// reassigned via `:=` (or shadowed by an inner `let`). Used by
/// [`FunctionBuilder::lower_while_like`] to drive the loop-header phi
/// params: any name in this set needs a header block param so the
/// back-edge can re-supply the post-body value.
///
/// Only names that EXIST in `env` (i.e. are introduced before the loop)
/// qualify — fresh-bound names inside the loop body are scoped to the
/// body and don't need phi participation.
fn collect_assigned_names_in_stmts(
    body: &[Statement],
    env: &LocalEnv,
) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in body {
        collect_assigned_in_stmt(s, env, &mut out);
    }
    out
}

fn collect_used_bound_names_in_stmts(body: &[Statement], env: &LocalEnv) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in body {
        collect_used_bound_names_in_stmt(s, env, &mut out);
    }
    out
}

fn collect_used_bound_names_in_stmt(s: &Statement, env: &LocalEnv, out: &mut HashSet<String>) {
    match s {
        Statement::Expr(e) => collect_used_bound_names_in_expr_into(e, env, out),
        Statement::Let { value, .. } => collect_used_bound_names_in_expr_into(value, env, out),
        Statement::While { cond, body, .. } | Statement::Until { cond, body, .. } => {
            collect_used_bound_names_in_expr_into(cond, env, out);
            for s2 in body {
                collect_used_bound_names_in_stmt(s2, env, out);
            }
        }
        Statement::For {
            clauses,
            body,
            finally_,
            ..
        } => {
            // A nested numeric `for` desugars to `let`+`while`; for the
            // ENCLOSING loop's phi analysis we treat the clause bounds and
            // the for-body as reads of outer bindings. The for's own loop
            // variable is freshly bound inside, so references to it inside
            // the body resolve locally and won't spuriously appear here
            // (it isn't in the enclosing `env` unless it shadows).
            for c in clauses {
                for_clause_exprs(c, &mut |e| collect_used_bound_names_in_expr_into(e, env, out));
            }
            for s2 in body {
                collect_used_bound_names_in_stmt(s2, env, out);
            }
            for s2 in finally_ {
                collect_used_bound_names_in_stmt(s2, env, out);
            }
        }
        Statement::Block { .. } | Statement::Local { .. } => {}
    }
}

fn collect_used_bound_names_in_expr(e: &Expr, env: &LocalEnv) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_used_bound_names_in_expr_into(e, env, &mut out);
    out
}

fn collect_used_bound_names_in_expr_into(e: &Expr, env: &LocalEnv, out: &mut HashSet<String>) {
    match e {
        Expr::Ident(_, name) => {
            if env.contains_key(name) {
                out.insert(name.clone());
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            collect_used_bound_names_in_expr_into(lhs, env, out);
            collect_used_bound_names_in_expr_into(rhs, env, out);
        }
        Expr::UnOp { operand, .. } => collect_used_bound_names_in_expr_into(operand, env, out),
        Expr::Paren { inner, .. } => collect_used_bound_names_in_expr_into(inner, env, out),
        Expr::Call { callee, args, .. } => {
            collect_used_bound_names_in_expr_into(callee, env, out);
            for a in args {
                collect_used_bound_names_in_expr_into(a, env, out);
            }
        }
        Expr::If { cond, then_, else_, .. } => {
            collect_used_bound_names_in_expr_into(cond, env, out);
            collect_used_bound_names_in_expr_into(then_, env, out);
            if let Some(b) = else_ {
                collect_used_bound_names_in_expr_into(b, env, out);
            }
        }
        Expr::Begin { body, .. } => {
            for b in body {
                collect_used_bound_names_in_expr_into(b, env, out);
            }
        }
        Expr::Let { value, .. } => collect_used_bound_names_in_expr_into(value, env, out),
        Expr::Stmt(s) => collect_used_bound_names_in_stmt(s, env, out),
        Expr::Case { arms, otherwise, .. } => {
            for a in arms {
                collect_used_bound_names_in_expr_into(&a.cond, env, out);
                for b in &a.body {
                    collect_used_bound_names_in_expr_into(b, env, out);
                }
            }
            if let Some(o) = otherwise {
                collect_used_bound_names_in_expr_into(o, env, out);
            }
        }
        _ => {}
    }
}

fn collect_assigned_in_stmt(s: &Statement, env: &LocalEnv, out: &mut HashSet<String>) {
    match s {
        Statement::Expr(e) => collect_assigned_in_expr(e, env, out),
        Statement::Let { value, binders, .. } => {
            collect_assigned_in_expr(value, env, out);
            // Sprint 18: a `let X = …` inside a loop body shadows X if
            // X was bound outside; treat the outer X as loop-mutable.
            for b in binders {
                if env.contains_key(&b.name) {
                    out.insert(b.name.clone());
                }
            }
        }
        Statement::While { cond, body, .. } | Statement::Until { cond, body, .. } => {
            collect_assigned_in_expr(cond, env, out);
            for s2 in body {
                collect_assigned_in_stmt(s2, env, out);
            }
        }
        Statement::For {
            clauses,
            body,
            finally_,
            ..
        } => {
            // Outer bindings reassigned inside a nested `for` body must be
            // carried by the enclosing loop's header phi. The for's own
            // loop variable is bound inside the desugared inner `while`, so
            // it isn't in `env` here and won't be (mis)counted. Walk every
            // clause kind's sub-expressions (init/next/from/bounds/cond)
            // since any can contain an `:=` to an outer binding.
            for c in clauses {
                for_clause_exprs(c, &mut |e| collect_assigned_in_expr(e, env, out));
            }
            for s2 in body {
                collect_assigned_in_stmt(s2, env, out);
            }
            for s2 in finally_ {
                collect_assigned_in_stmt(s2, env, out);
            }
        }
        Statement::Block { .. } | Statement::Local { .. } => {}
    }
}

fn collect_assigned_in_expr(e: &Expr, env: &LocalEnv, out: &mut HashSet<String>) {
    match e {
        Expr::BinOp { op: BinOp::Assign, lhs, rhs, .. } => {
            if let Expr::Ident(_, name) = lhs.as_ref()
                && env.contains_key(name)
            {
                out.insert(name.clone());
            }
            collect_assigned_in_expr(rhs, env, out);
        }
        Expr::BinOp { lhs, rhs, .. } => {
            collect_assigned_in_expr(lhs, env, out);
            collect_assigned_in_expr(rhs, env, out);
        }
        Expr::UnOp { operand, .. } => collect_assigned_in_expr(operand, env, out),
        Expr::Paren { inner, .. } => collect_assigned_in_expr(inner, env, out),
        Expr::Call { callee, args, .. } => {
            collect_assigned_in_expr(callee, env, out);
            for a in args {
                collect_assigned_in_expr(a, env, out);
            }
        }
        Expr::If { cond, then_, else_, .. } => {
            collect_assigned_in_expr(cond, env, out);
            collect_assigned_in_expr(then_, env, out);
            if let Some(b) = else_ {
                collect_assigned_in_expr(b, env, out);
            }
        }
        Expr::Begin { body, .. } => {
            for b in body {
                collect_assigned_in_expr(b, env, out);
            }
        }
        Expr::Let { binder, value, .. } => {
            collect_assigned_in_expr(value, env, out);
            // Sprint 18: same shadowing rule as the Statement::Let arm.
            if env.contains_key(binder) {
                out.insert(binder.clone());
            }
        }
        Expr::Stmt(s) => collect_assigned_in_stmt(s, env, out),
        Expr::Case { arms, otherwise, .. } => {
            for a in arms {
                collect_assigned_in_expr(&a.cond, env, out);
                for b in &a.body {
                    collect_assigned_in_expr(b, env, out);
                }
            }
            if let Some(o) = otherwise {
                collect_assigned_in_expr(o, env, out);
            }
        }
        _ => {}
    }
}

/// Strip surrounding `"`s and decode the minimal escape set. Supports
/// `\n`, `\r`, `\t`, `\\`, `\"`, `\0`. Unknown escapes are emitted as
/// the literal escape char so behaviour matches Dylan's tolerant lexer.
fn decode_dylan_string_literal(raw: &str) -> String {
    let s = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(raw);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('0') => out.push('\0'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn static_class_name(name: &str) -> &'static str {
    match name {
        "<object>" => "<object>",
        "<integer>" => "<integer>",
        "<single-float>" => "<single-float>",
        "<double-float>" => "<double-float>",
        "<boolean>" => "<boolean>",
        "<character>" => "<character>",
        "<symbol>" => "<symbol>",
        "<string>" => "<string>",
        "<byte-string>" => "<byte-string>",
        "<simple-object-vector>" => "<simple-object-vector>",
        "<vector>" => "<vector>",
        "<empty-list>" => "<empty-list>",
        _ => "<unknown>",
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Integer(..) => "integer",
        Expr::Float(..) => "float",
        Expr::String(..) => "string",
        Expr::Char(..) => "char",
        Expr::Bool(..) => "bool",
        Expr::Symbol(..) => "symbol",
        Expr::Ident(..) => "ident",
        Expr::Call { .. } => "call",
        Expr::BinOp { .. } => "binop",
        Expr::UnOp { .. } => "unop",
        Expr::Paren { .. } => "paren",
        Expr::If { .. } => "if",
        Expr::Case { .. } => "case",
        Expr::MacroCall { .. } => "macro-call",
        Expr::Begin { .. } => "begin",
        Expr::Let { .. } => "let",
        Expr::LocalMethod { .. } => "local-method",
        Expr::Method { .. } => "method",
        Expr::Stmt(_) => "stmt",
    }
}

// ─── BinOp / UnOp resolution ─────────────────────────────────────────────

/// Map an operator name used as a function-reference in CALL position
/// (`\=(a, b)`, `\>(x, y)`, `select … by \=`) to its `BinOp`, so it lowers
/// via the inline PrimOp path exactly like the infix form `a op b`. `&`/`|`
/// are intentionally excluded: as a strict function call they would differ
/// from the short-circuiting infix forms (and are essentially never used as
/// call-refs); `:=` is not a call form.
fn binop_from_op_name(name: &str) -> Option<BinOp> {
    Some(match name {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "mod" => BinOp::Mod,
        "rem" => BinOp::Rem,
        "^" => BinOp::Pow,
        "=" => BinOp::Eq,
        "==" => BinOp::EqEq,
        "~=" => BinOp::Ne,
        "~==" => BinOp::NeEq,
        "<" => BinOp::Lt,
        ">" => BinOp::Gt,
        "<=" => BinOp::Le,
        ">=" => BinOp::Ge,
        _ => return None,
    })
}

/// Unary operator name used as a function-reference in call position
/// (`\-(x)`, `\~(p)`).
fn unop_from_op_name(name: &str) -> Option<UnOp> {
    Some(match name {
        "-" => UnOp::Neg,
        "~" => UnOp::Not,
        _ => return None,
    })
}

fn select_binop(
    op: BinOp,
    lt: TypeEstimate,
    rt: TypeEstimate,
    span: Span,
) -> Result<PrimOp, LoweringError> {
    let both_int = lt.is_integer() && rt.is_integer();
    let any_float = lt.is_float() || rt.is_float();
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Rem => {
            if both_int {
                Ok(match op {
                    BinOp::Add => PrimOp::AddInt,
                    BinOp::Sub => PrimOp::SubInt,
                    BinOp::Mul => PrimOp::MulInt,
                    BinOp::Div => PrimOp::DivInt,
                    BinOp::Mod => PrimOp::ModInt,
                    BinOp::Rem => PrimOp::RemInt,
                    _ => unreachable!(),
                })
            } else if any_float
                && !matches!(op, BinOp::Mod | BinOp::Rem)
                && (lt.is_float() || lt == TypeEstimate::Top)
                && (rt.is_float() || rt == TypeEstimate::Top)
            {
                Ok(match op {
                    BinOp::Add => PrimOp::AddFloat,
                    BinOp::Sub => PrimOp::SubFloat,
                    BinOp::Mul => PrimOp::MulFloat,
                    BinOp::Div => PrimOp::DivFloat,
                    _ => unreachable!(),
                })
            } else if lt == TypeEstimate::Top && rt == TypeEstimate::Top {
                Ok(match op {
                    BinOp::Add => PrimOp::AddInt,
                    BinOp::Sub => PrimOp::SubInt,
                    BinOp::Mul => PrimOp::MulInt,
                    BinOp::Div => PrimOp::DivInt,
                    BinOp::Mod => PrimOp::ModInt,
                    BinOp::Rem => PrimOp::RemInt,
                    _ => unreachable!(),
                })
            } else if lt.is_integer() && rt == TypeEstimate::Top {
                // Sprint 12: a slot getter return (Top) + an integer
                // local → assume the slot was integer-typed. Choose
                // the integer path. This handles the `<point>` case
                // where `x(p) * x(p)` has Dispatch-typed temps.
                Ok(match op {
                    BinOp::Add => PrimOp::AddInt,
                    BinOp::Sub => PrimOp::SubInt,
                    BinOp::Mul => PrimOp::MulInt,
                    BinOp::Div => PrimOp::DivInt,
                    BinOp::Mod => PrimOp::ModInt,
                    BinOp::Rem => PrimOp::RemInt,
                    _ => unreachable!(),
                })
            } else if lt == TypeEstimate::Top && rt.is_integer() {
                Ok(match op {
                    BinOp::Add => PrimOp::AddInt,
                    BinOp::Sub => PrimOp::SubInt,
                    BinOp::Mul => PrimOp::MulInt,
                    BinOp::Div => PrimOp::DivInt,
                    BinOp::Mod => PrimOp::ModInt,
                    BinOp::Rem => PrimOp::RemInt,
                    _ => unreachable!(),
                })
            } else {
                Err(LoweringError::TypeMismatch {
                    span,
                    message: format!(
                        "mixed int+float operand types ({} {} {}) — explicit coercion not lowered",
                        lt.name(),
                        op.name(),
                        rt.name()
                    ),
                })
            }
        }
        BinOp::Eq | BinOp::EqEq | BinOp::Ne | BinOp::NeEq | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let is_float_cmp = any_float;
            let p = match (op, is_float_cmp) {
                (BinOp::Eq | BinOp::EqEq, false) => PrimOp::EqInt,
                (BinOp::Ne | BinOp::NeEq, false) => PrimOp::NeInt,
                (BinOp::Lt, false) => PrimOp::LtInt,
                (BinOp::Gt, false) => PrimOp::GtInt,
                (BinOp::Le, false) => PrimOp::LeInt,
                (BinOp::Ge, false) => PrimOp::GeInt,
                (BinOp::Eq | BinOp::EqEq, true) => PrimOp::EqFloat,
                (BinOp::Lt, true) => PrimOp::LtFloat,
                (BinOp::Gt, true) => PrimOp::GtFloat,
                (BinOp::Le, true) => PrimOp::LeFloat,
                (BinOp::Ge, true) => PrimOp::GeFloat,
                (BinOp::Ne | BinOp::NeEq, true) => {
                    return Err(LoweringError::Unsupported {
                        span,
                        message: "float-`~=` not lowered (no NeFloat PrimOp in Sprint 06)"
                            .to_string(),
                    });
                }
                _ => unreachable!(),
            };
            Ok(p)
        }
        BinOp::And => Ok(PrimOp::BoolAnd),
        BinOp::Or => Ok(PrimOp::BoolOr),
        BinOp::Pow | BinOp::Assign => Err(LoweringError::Unsupported {
            span,
            message: format!("BinOp `{}` not lowered in Sprint 06", op.name()),
        }),
    }
}

fn select_unop(op: UnOp, vt: TypeEstimate, span: Span) -> Result<PrimOp, LoweringError> {
    match op {
        UnOp::Neg => match vt {
            TypeEstimate::Integer | TypeEstimate::Top => Ok(PrimOp::NegInt),
            TypeEstimate::SingleFloat | TypeEstimate::DoubleFloat => Ok(PrimOp::NegFloat),
            _ => Err(LoweringError::TypeMismatch {
                span,
                message: format!("unary `-` on non-numeric {}", vt.name()),
            }),
        },
        UnOp::Not => Ok(PrimOp::BoolNot),
    }
}

/// Dump every registered class to a multi-line string. Used by the
/// driver's (eventual) `dump-classes` subcommand and by the Sprint 12
/// acceptance tests.
///
/// Sprint 14 extends the per-slot row with an MI-aware annotation:
///   - `[own]` for slots introduced by this class.
///   - `[inherited from <C>, fixed-offset]` for inherited slots whose
///     offset matches the defining class's layout.
///   - `[inherited from <C>, override @N→@M]` for inherited slots
///     whose offset shifted vs. the defining class. The lowering pass
///     generated an override accessor method bound to this receiver
///     class; dispatch picks it.
///
/// The class-header line now lists `parents=[...]` instead of a single
/// `parent=` field.
pub fn dump_classes() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let mut entries: Vec<&'static ClassMetadata> = Vec::new();
    nod_runtime::for_each_class(|md| entries.push(md));
    entries.sort_by_key(|m| m.id.0);
    for md in entries {
        let parents_disp = if md.parents.is_empty() {
            "[]".to_string()
        } else {
            let names: Vec<String> = md
                .parents
                .iter()
                .map(|p| {
                    let ptr = nod_runtime::class_metadata_ptr(*p);
                    if ptr.is_null() {
                        format!("<unknown:{}>", p.0)
                    } else {
                        // SAFETY: static-area metadata.
                        unsafe { (*ptr).name.clone() }
                    }
                })
                .collect();
            format!("[{}]", names.join(", "))
        };
        let cpl_disp = {
            let names: Vec<String> = md
                .cpl
                .iter()
                .map(|c| {
                    let ptr = nod_runtime::class_metadata_ptr(*c);
                    if ptr.is_null() {
                        format!("<unknown:{}>", c.0)
                    } else {
                        // SAFETY: static-area metadata.
                        unsafe { (*ptr).name.clone() }
                    }
                })
                .collect();
            format!("[{}]", names.join(", "))
        };
        let _ = writeln!(
            out,
            "{} (id={}, parents={parents_disp}, cpl={cpl_disp}, slots={}, size={}B)",
            md.name,
            md.id.0,
            md.slots.len(),
            md.instance_size
        );
        for (idx, slot) in md.slots.iter().enumerate() {
            // slot_origin may be shorter than slots in legacy callers;
            // default to "self" if absent.
            let origin = md.slot_origin.get(idx).copied().unwrap_or(md.id);
            let annotation = if origin == md.id {
                "[own]".to_string()
            } else {
                let origin_md_ptr = nod_runtime::class_metadata_ptr(origin);
                if origin_md_ptr.is_null() {
                    format!("[inherited from <unknown:{}>]", origin.0)
                } else {
                    // SAFETY: static-area metadata.
                    let origin_md = unsafe { &*origin_md_ptr };
                    let origin_offset = origin_md
                        .slots
                        .iter()
                        .find(|s| s.name == slot.name)
                        .map(|s| s.offset)
                        .unwrap_or(slot.offset);
                    if origin_offset == slot.offset {
                        format!("[inherited from {}, fixed-offset]", origin_md.name)
                    } else {
                        format!(
                            "[inherited from {}, override @{}→@{}]",
                            origin_md.name, origin_offset, slot.offset
                        )
                    }
                }
            };
            let _ = writeln!(
                out,
                "    slot {} @{}  {:?}  init-keyword={:?}  has-setter={}  {}",
                slot.name, slot.offset, slot.type_kind, slot.init_keyword, slot.has_setter, annotation
            );
        }
    }
    out
}

// ─── Sprint 19: `block` / `exception` / `cleanup` lowering ─────────────────
//
// See `docs/CONDITIONS.md` §"block lowering" for the full design. In
// short: we lift the body, each handler body, the cleanup body, and the
// afterwards body into top-level Dylan functions and emit a single
// runtime call (`%run-block`) at the original `block` site. The runtime
// (`nod_runtime::nod_run_block`) drives the protocol: push handlers,
// `catch_unwind` the body, run cleanup on every exit path (including
// unwound exits), run afterwards on normal exit, pop handlers.
//
// **Captured locals**: we close over every name in the current `env` at
// the moment the `block` form opens. Each lifted thunk receives those
// values as positional `u64` parameters. We cap the total at
// `MAX_BLOCK_CAPTURED` (8); attempting to capture more is rejected
// with a clear "Sprint 19 limitation" error.
//
// **`block (k)` capture**: the exit-procedure `k` is materialised
// up-front via `%make-exit-procedure(block_id)` (a runtime shim) and
// passed as the first captured slot when `exit_var` is present.
//
// **No mutation across the boundary**: Dylan locals in this codebase
// are immutable bindings (`let` always rebinds); the lowerer doesn't
// implement `:=` against captured names. If the lifted body's lowering
// emits an `Assign` against a captured name it surfaces as an
// `Unsupported` (the new function's env would treat the param as a
// fresh binding; mutating it wouldn't write back). The acceptance
// fixtures don't exercise this case.

const BLOCK_RUN_CALLEE: &str = "%run-block";
const BLOCK_MAKE_EXIT_CALLEE: &str = "%make-exit-procedure";

/// One captured local entry: the source name (so lifted bodies can
/// rebind it) and the temp in the enclosing function.
#[derive(Clone)]
struct CapturedLocal {
    name: String,
    outer_temp: TempId,
}

#[allow(clippy::too_many_arguments)]
fn lower_block_form(
    b: &mut FunctionBuilder,
    sink: &mut LiftSink,
    env: &mut LocalEnv,
    ctx: &LowerCtx,
    span: Span,
    parent_name: &str,
    exit_var: Option<&str>,
    body: &[Statement],
    handlers: &[nod_reader::ExceptionClause],
    cleanup: &[Statement],
    afterwards: &[Statement],
) -> Result<TempId, LoweringError> {
    use nod_runtime::MAX_BLOCK_CAPTURED;

    // Collect captured locals from the enclosing function's env (every
    // currently-visible binding). The order is the iteration order of
    // the HashMap — for stability we sort by name so a given source
    // produces deterministic captured ordering across runs.
    let mut captured: Vec<CapturedLocal> = env
        .iter()
        .map(|(name, &outer_temp)| CapturedLocal {
            name: name.clone(),
            outer_temp,
        })
        .collect();
    captured.sort_by(|a, b| a.name.cmp(&b.name));

    // If the block introduces an exit-procedure, reserve slot 0 for it.
    let exit_slot_used = exit_var.is_some();
    let total_captured = captured.len() + if exit_slot_used { 1 } else { 0 };
    if total_captured > MAX_BLOCK_CAPTURED {
        return Err(LoweringError::Unsupported {
            span,
            message: format!(
                "Sprint 19 limitation: `block` captures {total_captured} locals (max = {MAX_BLOCK_CAPTURED}); reduce surrounding bindings or restructure"
            ),
        });
    }

    let thunk_seq = sink.alloc_thunk_suffix();
    // Sprint 37: deterministic block_id derived from (parent_name,
    // thunk_seq). Identical source must produce identical DFM IR for the
    // JIT object-code cache to hit; a process-global counter would change
    // across runs. The id is registered
    // post-JIT with `register_block_fns`, which replaces same-id entries,
    // so collisions across modules are tolerated. The hash is SipHash 1-3
    // via `DefaultHasher`, which has fixed seeds — stable across runs.
    // The id must fit in the tagged-fixnum domain because
    // `make_exit_procedure` packs it via `Word::from_fixnum` (which
    // panics on overflow). The fixnum domain is [-2^62, 2^62-1], so a
    // *non-negative* id must be strictly less than 2^62. We mask to 61
    // bits then OR in bit 61, giving the range [2^61, 2^62-1]: non-zero
    // (0 is the "no block" sentinel) AND inside the fixnum domain, with
    // high collision-resistance.
    let block_id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        parent_name.hash(&mut h);
        thunk_seq.hash(&mut h);
        b"sprint37-block-id".hash(&mut h);
        let raw = h.finish();
        // Mask to 61 bits then set bit 61; gives a non-zero value in
        // [2^61, 2^62-1], fitting `Word::from_fixnum`'s domain
        // (FIXNUM_MAX = 2^62-1).
        (raw & ((1u64 << 61) - 1)) | (1u64 << 61)
    };

    // ─── Lift each stage to a top-level function ────────────────────
    //
    // Lifted-function name shape: `<parent>$$blk<N>$<stage>`. The `$$`
    // separator + `blk` prefix is a marker the dumps + `:handlers`
    // output uses to spot lifted thunks.

    let stage_name = |stage: &str| format!("{parent_name}$$blk{thunk_seq}${stage}");

    let body_fn_name = stage_name("body");
    let cleanup_fn_name = if cleanup.is_empty() {
        None
    } else {
        Some(stage_name("cleanup"))
    };
    let afterwards_fn_name = if afterwards.is_empty() {
        None
    } else {
        Some(stage_name("afterwards"))
    };

    let body_fn = lift_block_stage(
        sink.alloc_fn_id(),
        &body_fn_name,
        &captured,
        exit_var,
        body,
        span,
        ctx,
        sink,
        false,
    )?;
    sink.functions.push(body_fn);

    if let Some(name) = &cleanup_fn_name {
        let f = lift_block_stage(
            sink.alloc_fn_id(),
            name,
            &captured,
            exit_var,
            cleanup,
            span,
            ctx,
            sink,
            false,
        )?;
        sink.functions.push(f);
    }
    if let Some(name) = &afterwards_fn_name {
        let f = lift_block_stage(
            sink.alloc_fn_id(),
            name,
            &captured,
            exit_var,
            afterwards,
            span,
            ctx,
            sink,
            false,
        )?;
        sink.functions.push(f);
    }

    let mut handler_regs: Vec<BlockHandlerRegistration> = Vec::with_capacity(handlers.len());
    for (i, h) in handlers.iter().enumerate() {
        let class_id = match &h.class {
            Expr::Ident(_, n) => ctx
                .user_classes
                .get(n)
                .copied()
                .or_else(|| resolve_class_id_by_name(n))
                .ok_or_else(|| LoweringError::UndefinedIdent {
                    span: h.span,
                    name: n.clone(),
                })?,
            _ => {
                return Err(LoweringError::Unsupported {
                    span: h.span,
                    message: "exception clause: class must be a bare identifier".to_string(),
                });
            }
        };
        let class_name = match &h.class {
            Expr::Ident(_, n) => n.clone(),
            _ => unreachable!("guarded by Ident match above"),
        };
        let fn_name = stage_name(&format!("h{i}"));
        let handler_fn = lift_block_stage_handler(
            sink.alloc_fn_id(),
            &fn_name,
            &captured,
            exit_var,
            h.var.as_deref(),
            &h.body,
            h.span,
            ctx,
            sink,
        )?;
        sink.functions.push(handler_fn);
        handler_regs.push(BlockHandlerRegistration {
            class_id,
            class_name,
            body_fn_name: fn_name,
        });
    }

    // Record the block for post-JIT registration.
    sink.blocks.push(BlockRegistration {
        block_id,
        body_fn_name: body_fn_name.clone(),
        cleanup_fn_name: cleanup_fn_name.clone(),
        afterwards_fn_name: afterwards_fn_name.clone(),
        handlers: handler_regs,
    });

    // ─── Emit the call site in the enclosing function ───────────────
    //
    // Args to `%run-block`: [block_id_const, c0..c7]. Unused slots are
    // zero-filled.

    // Block-id constant.
    let bid_temp = b.fresh_temp(TypeEstimate::Top);
    b.push(Computation::Const {
        dst: bid_temp,
        value: ConstValue::WordBits(block_id),
    });
    let zero_temp = b.fresh_temp(TypeEstimate::Top);
    b.push(Computation::Const {
        dst: zero_temp,
        value: ConstValue::WordBits(0),
    });

    // Optional exit procedure (slot 0 if present): call %make-exit-procedure(block_id).
    let mut capture_temps: Vec<TempId> = Vec::with_capacity(MAX_BLOCK_CAPTURED);
    if exit_slot_used {
        let ep_temp = b.fresh_temp(TypeEstimate::Top);
        b.push(Computation::DirectCall {
            dst: ep_temp,
            callee: BLOCK_MAKE_EXIT_CALLEE.to_string(),
            args: vec![bid_temp],
            safepoint_roots: Vec::new(), is_no_alloc: false,
        });
        capture_temps.push(ep_temp);
    }
    for c in &captured {
        capture_temps.push(c.outer_temp);
    }
    while capture_temps.len() < MAX_BLOCK_CAPTURED {
        capture_temps.push(zero_temp);
    }

    let dst = b.fresh_temp(TypeEstimate::Top);
    let mut args = Vec::with_capacity(1 + MAX_BLOCK_CAPTURED);
    args.push(bid_temp);
    args.extend(capture_temps);
    b.push(Computation::DirectCall {
        dst,
        callee: BLOCK_RUN_CALLEE.to_string(),
        args,
        safepoint_roots: Vec::new(), is_no_alloc: false,
    });

    // The block's result type is intentionally `Top` — Sprint 19
    // doesn't attempt to type-merge the body/handler branches.
    let _ = exit_var;
    Ok(dst)
}

/// Lift one "straight" stage (body / cleanup / afterwards) of a `block`
/// form into a fresh top-level Dylan function. The new function takes
/// `MAX_BLOCK_CAPTURED` positional `u64` params (the captured locals,
/// padded with zeros). Its body is the supplied `stmts` lowered with
/// the captured names bound to the param temps.
#[allow(clippy::too_many_arguments)]
fn lift_block_stage(
    id: FunctionId,
    fn_name: &str,
    captured: &[CapturedLocal],
    exit_var: Option<&str>,
    stmts: &[Statement],
    span: Span,
    ctx: &LowerCtx,
    sink: &mut LiftSink,
    _is_handler: bool,
) -> Result<Function, LoweringError> {
    use nod_runtime::MAX_BLOCK_CAPTURED;
    let mut b = FunctionBuilder::new(id, fn_name.to_string(), span);
    let mut env = LocalEnv::new();
    // Build params: slot 0 = exit-procedure (if any), then captures, padded to 8.
    let mut slot_names: Vec<Option<String>> = Vec::with_capacity(MAX_BLOCK_CAPTURED);
    if let Some(ev) = exit_var {
        slot_names.push(Some(ev.to_string()));
    }
    for c in captured {
        slot_names.push(Some(c.name.clone()));
    }
    while slot_names.len() < MAX_BLOCK_CAPTURED {
        slot_names.push(None);
    }
    for slot_name in &slot_names {
        let t = b.fresh_temp(TypeEstimate::Top);
        b.func.params.push(t);
        if let Some(n) = slot_name {
            env.insert(n.clone(), t);
        }
    }

    lower_statements_into(&mut b, &mut env, ctx, sink, stmts)?;
    b.func.return_type = TypeEstimate::Top;
    // Terminate the current block with a return of the last temp (or
    // zero if the stage was empty). `lower_statements_into` left
    // `current_block`'s terminator as the default `Return None`; we
    // overwrite to surface the final value.
    if let Some(t) = b.last_temp() {
        // SSA: emit the return explicitly.
        let cur_block_id = b.func.blocks[b.current].id;
        let _ = cur_block_id;
        b.terminate_current(Terminator::Return { value: Some(t) });
    } else {
        // Empty stage — return zero (which the runtime interprets as
        // the unit Word for cleanup/afterwards, and the body's value
        // for a body stage; an empty body returns 0).
        let z = b.fresh_temp(TypeEstimate::Top);
        b.push(Computation::Const {
            dst: z,
            value: ConstValue::WordBits(0),
        });
        b.terminate_current(Terminator::Return { value: Some(z) });
    }
    Ok(b.finish())
}

/// Lift one handler clause. Signature: 1 condition Word arg + 8
/// captured-locals slots. The handler's bound condition variable (the
/// `c` in `exception (c :: <error>)`) is bound to the first param.
#[allow(clippy::too_many_arguments)]
fn lift_block_stage_handler(
    id: FunctionId,
    fn_name: &str,
    captured: &[CapturedLocal],
    exit_var: Option<&str>,
    handler_var: Option<&str>,
    stmts: &[Statement],
    span: Span,
    ctx: &LowerCtx,
    sink: &mut LiftSink,
) -> Result<Function, LoweringError> {
    use nod_runtime::MAX_BLOCK_CAPTURED;
    let mut b = FunctionBuilder::new(id, fn_name.to_string(), span);
    let mut env = LocalEnv::new();

    // Param 0: the condition Word. The handler may omit the variable;
    // we always emit a param but only bind it in env when named.
    let cond_temp = b.fresh_temp(TypeEstimate::Top);
    b.func.params.push(cond_temp);
    if let Some(v) = handler_var {
        env.insert(v.to_string(), cond_temp);
    }

    // Params 1..=8: captured locals, same layout as the body thunk
    // (exit-procedure in slot 0 if present, then captures, padded with
    // zeros).
    let mut slot_names: Vec<Option<String>> = Vec::with_capacity(MAX_BLOCK_CAPTURED);
    if let Some(ev) = exit_var {
        slot_names.push(Some(ev.to_string()));
    }
    for c in captured {
        slot_names.push(Some(c.name.clone()));
    }
    while slot_names.len() < MAX_BLOCK_CAPTURED {
        slot_names.push(None);
    }
    for slot_name in &slot_names {
        let t = b.fresh_temp(TypeEstimate::Top);
        b.func.params.push(t);
        if let Some(n) = slot_name {
            env.insert(n.clone(), t);
        }
    }

    lower_statements_into(&mut b, &mut env, ctx, sink, stmts)?;
    b.func.return_type = TypeEstimate::Top;
    if let Some(t) = b.last_temp() {
        b.terminate_current(Terminator::Return { value: Some(t) });
    } else {
        let z = b.fresh_temp(TypeEstimate::Top);
        b.push(Computation::Const {
            dst: z,
            value: ConstValue::WordBits(0),
        });
        b.terminate_current(Terminator::Return { value: Some(z) });
    }
    Ok(b.finish())
}

/// Inline-lower a sequence of statements into the current block of
/// `b`. Returns Ok(()) on success. Used by the block-stage lifting
/// helpers so the lifted thunk can itself contain `let`, `if`,
/// `while`, nested `block`, etc.
fn lower_statements_into(
    b: &mut FunctionBuilder,
    env: &mut LocalEnv,
    ctx: &LowerCtx,
    sink: &mut LiftSink,
    stmts: &[Statement],
) -> Result<(), LoweringError> {
    // Move the sink into the builder so block forms reachable only via
    // `lower_expr` (a `block … end` in expression position inside this
    // thunk) can borrow it. Only the OUTERMOST call seeds it; recursive
    // calls (the `for` desugar arm) see it already set and leave it.
    let seeded = if b.pending_sink.is_none() {
        b.pending_sink = Some(std::mem::take(sink));
        true
    } else {
        false
    };
    let result = lower_statements_into_inner(b, env, ctx, stmts);
    if seeded && let Some(s) = b.pending_sink.take() {
        *sink = s;
    }
    result
}

fn lower_statements_into_inner(
    b: &mut FunctionBuilder,
    env: &mut LocalEnv,
    ctx: &LowerCtx,
    stmts: &[Statement],
) -> Result<(), LoweringError> {
    for stmt in stmts {
        match stmt {
            Statement::Expr(e) => {
                let _t = b.lower_expr(e, env, ctx)?;
                b.set_last_temp(_t);
            }
            Statement::Let {
                binders,
                rest,
                value,
                span,
            } => {
                if rest.is_some() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`#rest` binder in `let` not supported yet".to_string(),
                    });
                }
                if binders.is_empty() {
                    return Err(LoweringError::Unsupported {
                        span: *span,
                        message: "`let` with no binders".to_string(),
                    });
                }
                if binders.len() > 1 {
                    // Sprint 47 — multi-binder `let` in lifted-thunk context.
                    let t = b.lower_let_multi_binders(binders, value, env, ctx)?;
                    b.set_last_temp(t);
                } else {
                    let bname = &binders[0].name;
                    let t = b.lower_expr(value, env, ctx)?;
                    env.insert(bname.clone(), t);
                    b.set_last_temp(t);
                }
            }
            Statement::Local { span, methods } => {
                let enclosing = b.closure_key.clone();
                b.lower_local_methods(&enclosing, methods, env, ctx, *span)?;
                b.clear_last_temp();
            }
            Statement::While { cond, body, .. } => {
                b.lower_while_like(cond, body, false, env, ctx)?;
                b.clear_last_temp();
            }
            Statement::Until { cond, body, .. } => {
                b.lower_while_like(cond, body, true, env, ctx)?;
                b.clear_last_temp();
            }
            Statement::For {
                span,
                clauses,
                body,
                finally_,
            } => {
                // Desugar `for` to `let`s + `while` + result and lower
                // each inline into this thunk. The trailing result
                // statement leaves the for-value in `last_temp`, so a
                // `for (…) finally R end` as the thunk's final form yields
                // R as the thunk value — don't clear it.
                let desugared = desugar_numeric_for(*span, clauses, body, finally_)?;
                for ds in &desugared {
                    lower_statements_into_inner(b, env, ctx, std::slice::from_ref(ds))?;
                }
            }
            Statement::Block {
                span,
                exit_var,
                body,
                handlers,
                cleanup,
                afterwards,
            } => {
                // Nested block — borrow the thunk's sink (held in
                // `pending_sink` by the outer `lower_statements_into`).
                let t = b.lower_block_in_expr(
                    env,
                    ctx,
                    *span,
                    exit_var.as_deref(),
                    body,
                    handlers,
                    cleanup,
                    afterwards,
                )?;
                b.set_last_temp(t);
            }
        }
    }
    Ok(())
}

// ─── Sprint 31 unit tests ────────────────────────────────────────────────

#[cfg(test)]
#[allow(non_snake_case)]
mod sprint31_tests {
    use super::*;

    #[test]
    fn winapi_dll_priority_orders_kernel_first() {
        assert!(winapi_dll_priority("kernel32.dll") < winapi_dll_priority("user32.dll"));
        assert!(winapi_dll_priority("user32.dll") < winapi_dll_priority("gdi32.dll"));
        assert!(winapi_dll_priority("gdi32.dll") < winapi_dll_priority("advapi32.dll"));
        assert!(winapi_dll_priority("advapi32.dll") < winapi_dll_priority("shell32.dll"));
        assert!(winapi_dll_priority("shell32.dll") < winapi_dll_priority("comctl32.dll"));
        // Unknown DLLs sort to the end (alphabetical fallback there).
        assert!(winapi_dll_priority("d3d12.dll") > winapi_dll_priority("comctl32.dll"));
    }

    #[test]
    fn looks_like_win32_export_filters_correctly() {
        // Yes: standard Win32 export shape.
        assert!(looks_like_win32_export("MessageBoxW"));
        assert!(looks_like_win32_export("GetTickCount64"));
        assert!(looks_like_win32_export("Beep"));
        // Yes: lowercase-prefixed Win32 exports like lstrlenW.
        assert!(looks_like_win32_export("lstrlenW"));
        assert!(looks_like_win32_export("lstrlenA"));
        assert!(looks_like_win32_export("wsprintfW"));
        // Yes: mixed-case but starting lowercase.
        assert!(looks_like_win32_export("messageBox"));
        // No: Dylan-side names, punctuated, all-lowercase.
        assert!(!looks_like_win32_export("print"));
        assert!(!looks_like_win32_export("format"));
        assert!(!looks_like_win32_export("<my-class>"));
        assert!(!looks_like_win32_export("c-function"));
        assert!(!looks_like_win32_export("+"));
        assert!(!looks_like_win32_export("instance?"));
        assert!(!looks_like_win32_export("A"));
        assert!(!looks_like_win32_export("ab"));
    }

    #[test]
    fn jit_materialize_GetTickCount64_yields_kernel32_no_args() {
        let outcome = try_jit_materialize_winapi("GetTickCount64");
        match outcome {
            MaterializationOutcome::Materialized { c_name, library, signature } => {
                assert_eq!(c_name, "GetTickCount64");
                assert_eq!(library, "kernel32.dll");
                assert_eq!(signature.arg_count, 0);
            }
            other => panic!("expected materialized; got {other:?}"),
        }
    }

    #[test]
    fn jit_materialize_bare_MessageBox_picks_W() {
        let outcome = try_jit_materialize_winapi("MessageBox");
        match outcome {
            MaterializationOutcome::Materialized { c_name, library, .. } => {
                assert_eq!(c_name, "MessageBoxW");
                assert_eq!(library, "user32.dll");
            }
            other => panic!("expected materialized; got {other:?}"),
        }
    }

    #[test]
    fn jit_materialize_unknown_name_returns_not_found() {
        let outcome = try_jit_materialize_winapi("ThisIsNotAWin32Export");
        assert!(matches!(outcome, MaterializationOutcome::NotFound));
    }

    #[test]
    fn jit_materialize_lstrlenW_succeeds() {
        let outcome = try_jit_materialize_winapi("lstrlenW");
        match outcome {
            MaterializationOutcome::Materialized { c_name, library, signature } => {
                assert_eq!(c_name, "lstrlenW");
                assert_eq!(library, "kernel32.dll");
                assert_eq!(signature.arg_count, 1);
            }
            other => panic!("lstrlenW: expected materialized; got {other:?}"),
        }
    }

    #[test]
    fn jit_materialize_EnumWindows_outcome() {
        // EnumWindows has a callback (WNDENUMPROC). The outcome must
        // be either NotFound (if not in the embedded blob) or
        // UnsupportedSignature (function pointer). Both are acceptable
        // — Sprint 31 only needs the user to get a non-silent error
        // path. The blob filter in `build.rs` may have dropped it
        // entirely (`bad_type=5191`).
        let outcome = try_jit_materialize_winapi("EnumWindows");
        eprintln!("EnumWindows outcome: {outcome:?}");
        match outcome {
            MaterializationOutcome::NotFound
            | MaterializationOutcome::UnsupportedSignature { .. } => {}
            MaterializationOutcome::Materialized { signature, .. } => {
                // If it materialized, accept that — the index doesn't
                // expose callbacks as TypeRef::Function in this build,
                // they collapse to opaque pointers.
                eprintln!("EnumWindows actually materialized with sig {signature:?}");
            }
        }
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod for_desugar_tests {
    //! Coverage for [`desugar_numeric_for`] and helpers: the `for`-loop
    //! lowering for numeric/step/while/until clauses, multiple clauses,
    //! parallel-step ordering, and the `finally` result value.
    use super::*;
    use nod_reader::{FromForClause, NumericForClause, StepForClause};

    fn sp() -> Span {
        Span { file_id: nod_reader::FileId(0), lo: 0, hi: 0 }
    }
    fn ident(n: &str) -> Expr {
        Expr::Ident(sp(), n.to_string())
    }
    fn int(v: i128) -> Expr {
        Expr::Integer(sp(), v)
    }
    fn numeric(var: &str, from: Expr, to: Option<Expr>, by: Option<Expr>) -> ForClause {
        ForClause::Numeric(Box::new(NumericForClause {
            span: sp(),
            var: var.to_string(),
            from,
            to,
            below: None,
            above: None,
            by,
        }))
    }
    fn step(var: &str, init: Expr, next: Option<Expr>) -> ForClause {
        ForClause::Step(Box::new(StepForClause { span: sp(), var: var.to_string(), init, next }))
    }

    // `by -1` is `UnOp::Neg(Integer(1))`, as the parser produces.
    fn neg(v: i128) -> Expr {
        Expr::UnOp { span: sp(), op: UnOp::Neg, operand: Box::new(int(v)) }
    }

    #[test]
    fn by_negative_literal_recognised() {
        assert!(by_is_negative_literal(Some(&neg(1))));
        assert!(by_is_negative_literal(Some(&int(-3))));
        assert!(!by_is_negative_literal(Some(&int(1))));
        assert!(!by_is_negative_literal(Some(&ident("s"))));
        assert!(!by_is_negative_literal(None));
    }

    #[test]
    fn single_numeric_shape_unchanged() {
        // `for (i from 1 to n) body end` → [let i = 1, while (i <= n) …],
        // plus the no-finally `#f` result.
        let clauses = vec![numeric("i", int(1), Some(ident("n")), None)];
        let body = vec![Statement::Expr(ident("body"))];
        let out = desugar_numeric_for(sp(), &clauses, &body, &[]).unwrap();
        assert_eq!(out.len(), 3, "let + while + #f result");
        assert!(matches!(out[0], Statement::Let { .. }));
        let Statement::While { cond, body: wbody, .. } = &out[1] else {
            panic!("expected while, got {:?}", out[1]);
        };
        // ascending `to` → `<=`.
        assert!(matches!(cond, Expr::BinOp { op: BinOp::Le, .. }));
        // body then the increment assignment temp + assign.
        assert!(wbody.len() >= 3, "user body + next-temp + assign");
        // result is `#f` (no finally).
        assert!(matches!(out[2], Statement::Expr(Expr::Bool(_, false))));
    }

    #[test]
    fn descending_to_uses_ge() {
        // `for (n from n to 0 by -1) …` → continuation test `n >= 0`.
        let clauses = vec![numeric("n", ident("n"), Some(int(0)), Some(neg(1)))];
        let out = desugar_numeric_for(sp(), &clauses, &[], &[]).unwrap();
        let Statement::While { cond, .. } = &out[1] else { panic!() };
        assert!(
            matches!(cond, Expr::BinOp { op: BinOp::Ge, .. }),
            "descending `to` must use >=, got {cond:?}"
        );
    }

    #[test]
    fn finally_becomes_result() {
        // `for (i from 1 to 5) … finally s end` → trailing stmt is `s`.
        let clauses = vec![numeric("i", int(1), Some(int(5)), None)];
        let finally_ = vec![Statement::Expr(ident("s"))];
        let out = desugar_numeric_for(sp(), &clauses, &[], &finally_).unwrap();
        let last = out.last().unwrap();
        assert!(matches!(last, Statement::Expr(Expr::Ident(_, n)) if n == "s"));
    }

    #[test]
    fn multi_clause_parallel_steps() {
        // `for (l = l then tail(l), a = #() then pair(head(l), a), until: empty?(l))`
        // → two lets (l, a); while cond is the until's `~empty?(l)`; the
        // loop body holds two next-temps THEN two assignments (parallel).
        let until = ForClause::Until {
            span: sp(),
            cond: Expr::Call { span: sp(), callee: Box::new(ident("empty?")), args: vec![ident("l")] },
        };
        let l_step = step(
            "l",
            ident("l"),
            Some(Expr::Call { span: sp(), callee: Box::new(ident("tail")), args: vec![ident("l")] }),
        );
        let a_step = step(
            "a",
            Expr::Ident(sp(), "#()".to_string()),
            Some(Expr::Call {
                span: sp(),
                callee: Box::new(ident("pair")),
                args: vec![
                    Expr::Call { span: sp(), callee: Box::new(ident("head")), args: vec![ident("l")] },
                    ident("a"),
                ],
            }),
        );
        let clauses = vec![l_step, a_step, until];
        let out = desugar_numeric_for(sp(), &clauses, &[], &[]).unwrap();
        // two pre-loop lets (l, a), then while, then `#f`.
        assert!(matches!(out[0], Statement::Let { .. }));
        assert!(matches!(out[1], Statement::Let { .. }));
        let Statement::While { cond, body, .. } = &out[2] else { panic!() };
        // until → `~empty?(l)`.
        assert!(matches!(cond, Expr::UnOp { op: UnOp::Not, .. }));
        // body: 2 next-temps (let) then 2 assigns — all next-temps come
        // BEFORE any assignment, giving simultaneous-step semantics.
        let lets_before_assigns = {
            let mut seen_assign = false;
            let mut ok = true;
            for s in body {
                match s {
                    Statement::Let { binders, .. }
                        if binders[0].name.starts_with("__for_next_") =>
                    {
                        if seen_assign {
                            ok = false;
                        }
                    }
                    Statement::Expr(Expr::BinOp { op: BinOp::Assign, .. }) => seen_assign = true,
                    _ => {}
                }
            }
            ok
        };
        assert!(lets_before_assigns, "all next-value temps must precede assignments");
    }

    #[test]
    fn no_termination_test_is_rejected() {
        // A bare `from` clause (no bound) and no while/until ⇒ no test ⇒
        // reject rather than emit an infinite loop.
        let clauses = vec![ForClause::From(Box::new(FromForClause {
            span: sp(),
            var: "i".to_string(),
            from: int(0),
            by: None,
        }))];
        let err = desugar_numeric_for(sp(), &clauses, &[], &[]).unwrap_err();
        assert!(matches!(err, LoweringError::Unsupported { .. }));
    }

    #[test]
    fn while_keyword_clause_is_the_test() {
        let clauses = vec![
            step("i", int(1), Some(Expr::BinOp {
                span: sp(),
                op: BinOp::Add,
                lhs: Box::new(ident("i")),
                rhs: Box::new(int(1)),
            })),
            ForClause::While {
                span: sp(),
                cond: Expr::BinOp {
                    span: sp(),
                    op: BinOp::Le,
                    lhs: Box::new(ident("i")),
                    rhs: Box::new(int(4)),
                },
            },
        ];
        let out = desugar_numeric_for(sp(), &clauses, &[], &[]).unwrap();
        let Statement::While { cond, .. } = &out[1] else { panic!() };
        // `while: i <= 4` is used directly (not negated).
        assert!(matches!(cond, Expr::BinOp { op: BinOp::Le, .. }));
    }

    fn in_clause(var: &str, coll: Expr) -> ForClause {
        ForClause::In { span: sp(), var: var.to_string(), coll }
    }

    /// `is this a one-arg call to the named FIP primitive?`
    fn is_fip_call(e: &Expr, name: &str) -> bool {
        matches!(e, Expr::Call { callee, args, .. }
            if matches!(callee.as_ref(), Expr::Ident(_, n) if n == name)
                && args.len() == 1)
    }

    #[test]
    fn in_clause_lowers_to_fip_protocol() {
        // `for (x in c) body end` →
        //   let %state = %fip-init(c);
        //   while (~ %fip-finished?(%state)) {
        //     let x = %fip-current-element(%state);
        //     body; %fip-advance!(%state)
        //   };
        //   #f
        let clauses = vec![in_clause("x", ident("c"))];
        let body = vec![Statement::Expr(ident("body"))];
        let out = desugar_numeric_for(sp(), &clauses, &body, &[]).unwrap();
        // Pre-loop let = %fip-init(c).
        let Statement::Let { value, .. } = &out[0] else {
            panic!("expected fip-init let, got {:?}", out[0]);
        };
        assert!(is_fip_call(value, "%fip-init"), "init: {value:?}");
        // While cond = ~ %fip-finished?(state).
        let Statement::While { cond, body: wbody, .. } = &out[1] else {
            panic!("expected while, got {:?}", out[1]);
        };
        let Expr::UnOp { op: UnOp::Not, operand, .. } = cond else {
            panic!("cond must be `~ finished?`, got {cond:?}");
        };
        assert!(is_fip_call(operand, "%fip-finished?"), "cond: {operand:?}");
        // First body stmt: `let x = %fip-current-element(state)` (top of iter).
        let Statement::Let { binders, value, .. } = &wbody[0] else {
            panic!("body[0] must be element rebind let, got {:?}", wbody[0]);
        };
        assert_eq!(binders[0].name, "x");
        assert!(is_fip_call(value, "%fip-current-element"), "elem: {value:?}");
        // The user body comes after the rebind.
        assert!(matches!(&wbody[1], Statement::Expr(Expr::Ident(_, n)) if n == "body"));
        // Last body stmt: `%fip-advance!(state)`.
        let last = wbody.last().unwrap();
        assert!(matches!(last, Statement::Expr(e) if is_fip_call(e, "%fip-advance!")));
    }

    #[test]
    fn parallel_in_clauses_get_distinct_states_and_conjoined_tests() {
        // `for (x in c1, y in c2) end` → two fip-init lets, while cond is
        // `~finished?(s0) & ~finished?(s1)`, two element rebinds, two
        // advances. Parallel iteration stops at the shortest collection
        // because EITHER finished? short-circuits the conjunction.
        let clauses = vec![in_clause("x", ident("c1")), in_clause("y", ident("c2"))];
        let out = desugar_numeric_for(sp(), &clauses, &[], &[]).unwrap();
        assert!(matches!(&out[0], Statement::Let { value, .. } if is_fip_call(value, "%fip-init")));
        assert!(matches!(&out[1], Statement::Let { value, .. } if is_fip_call(value, "%fip-init")));
        let Statement::While { cond, body: wbody, .. } = &out[2] else { panic!() };
        // cond is an `&` of two `~finished?` tests.
        assert!(matches!(cond, Expr::BinOp { op: BinOp::And, .. }), "cond: {cond:?}");
        // two element rebinds (x, y) at the top of the body.
        let elem_binds: Vec<_> = wbody
            .iter()
            .filter_map(|s| match s {
                Statement::Let { binders, value, .. }
                    if is_fip_call(value, "%fip-current-element") =>
                {
                    Some(binders[0].name.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(elem_binds, vec!["x".to_string(), "y".to_string()]);
        // two advances at the end.
        let advances = wbody
            .iter()
            .filter(|s| matches!(s, Statement::Expr(e) if is_fip_call(e, "%fip-advance!")))
            .count();
        assert_eq!(advances, 2);
    }

    #[test]
    fn in_clause_composes_with_numeric_and_finally() {
        // `for (i from 1 to 9, x in c) body finally R end` — the numeric
        // `to` test and the in-clause `~finished?` are both present, and
        // the finally result is the trailing statement.
        let clauses = vec![
            numeric("i", int(1), Some(int(9)), None),
            in_clause("x", ident("c")),
        ];
        let body = vec![Statement::Expr(ident("body"))];
        let finally_ = vec![Statement::Expr(ident("R"))];
        let out = desugar_numeric_for(sp(), &clauses, &body, &finally_).unwrap();
        // Find the while; its cond conjoins `i <= 9` with `~finished?`.
        let while_stmt = out.iter().find_map(|s| match s {
            Statement::While { cond, body, .. } => Some((cond, body)),
            _ => None,
        });
        let (cond, _) = while_stmt.expect("a while stmt");
        assert!(matches!(cond, Expr::BinOp { op: BinOp::And, .. }), "cond: {cond:?}");
        // finally → trailing `R`.
        let last = out.last().unwrap();
        assert!(matches!(last, Statement::Expr(Expr::Ident(_, n)) if n == "R"));
    }

    #[test]
    fn keyed_clause_still_unsupported() {
        // `keyed-by` needs a `%fip-current-key` primitive the runtime
        // doesn't export yet — must bail cleanly, not lower wrongly.
        let clauses = vec![ForClause::Keyed {
            span: sp(),
            var: "v".to_string(),
            key: "k".to_string(),
            coll: ident("c"),
        }];
        let err = desugar_numeric_for(sp(), &clauses, &[], &[]).unwrap_err();
        assert!(matches!(err, LoweringError::Unsupported { .. }));
    }
}

#[cfg(test)]
mod sprint56a_class_parse_tests {
    //! Sprint 56a — pure-Rust coverage for [`parse_sema_classes`], the inverse
    //! of the classes block of `format_sema_model`. The end-to-end live
    //! verification (`verify_dylan_classes` inside `analyse_module_from_dump`)
    //! is exercised by `dump_dfm_sema_with_dylan_byte_match` over the 38-fixture
    //! corpus; these tests pin the text grammar without needing the shim.
    use super::*;

    const POINT_DUMP: &str = "\
=== top-names ===
fn distance-squared arity=1 return=Top
=== generics ===
generic x
generic y
=== classes ===
class <user-point>
  parents [<object>]
  cpl [<user-point>, <object>]
  slot x @8 setter=true origin=<user-point> type=<integer> init-keyword=x required=false default=unbound
  slot y @16 setter=false origin=<user-point> type=<integer> init-keyword=- required=false default=unbound
=== sealing ===
";

    #[test]
    fn parses_single_class_with_slots() {
        let classes = parse_sema_classes(POINT_DUMP).expect("parse");
        assert_eq!(classes.len(), 1);
        let c = &classes[0];
        assert_eq!(c.name, "<user-point>");
        assert_eq!(c.parents, vec!["<object>".to_string()]);
        assert_eq!(c.cpl, vec!["<user-point>".to_string(), "<object>".to_string()]);
        assert_eq!(c.slots.len(), 2);
        assert_eq!(
            c.slots[0],
            ParsedSemaSlot {
                name: "x".to_string(),
                offset: 8,
                has_setter: true,
                origin: "<user-point>".to_string(),
                type_kind: "<integer>".to_string(),
                init_keyword: Some("x".to_string()),
                required_init_keyword: false,
                default_tag: "unbound".to_string(),
            }
        );
        // `setter=false` must parse as false (not defaulted to true).
        assert_eq!(c.slots[1].name, "y");
        assert_eq!(c.slots[1].offset, 16);
        assert!(!c.slots[1].has_setter);
        assert_eq!(c.slots[1].origin, "<user-point>");
        // `init-keyword=-` must parse back to `None`.
        assert_eq!(c.slots[1].init_keyword, None);
        assert_eq!(c.slots[1].type_kind, "<integer>");
        assert_eq!(c.slots[1].default_tag, "unbound");
    }

    /// Sprint 56a-WIRE — a slot carrying all four grown fields: an
    /// init-keyword, `required=true`, and an integer-literal default
    /// (`value:<bits>` = the source int shifted left one, per the fixnum
    /// encoding `Word::from_fixnum`).
    #[test]
    fn parses_widened_slot_with_required_and_int_default() {
        let dump = "\
=== classes ===
class <cfg>
  parents [<object>]
  cpl [<cfg>, <object>]
  slot timeout @8 setter=true origin=<cfg> type=<integer> init-keyword=timeout required=true default=value:84
  slot flag @16 setter=true origin=<cfg> type=<boolean> init-keyword=- required=false default=true
=== sealing ===
";
        let classes = parse_sema_classes(dump).expect("parse");
        assert_eq!(classes.len(), 1);
        let c = &classes[0];
        assert_eq!(c.slots.len(), 2);
        assert_eq!(
            c.slots[0],
            ParsedSemaSlot {
                name: "timeout".to_string(),
                offset: 8,
                has_setter: true,
                origin: "<cfg>".to_string(),
                type_kind: "<integer>".to_string(),
                init_keyword: Some("timeout".to_string()),
                required_init_keyword: true,
                // 42 encodes as the fixnum Word 42<<1 = 84.
                default_tag: "value:84".to_string(),
            }
        );
        assert_eq!(c.slots[1].type_kind, "<boolean>");
        assert_eq!(c.slots[1].init_keyword, None);
        assert!(!c.slots[1].required_init_keyword);
        assert_eq!(c.slots[1].default_tag, "true");
    }

    #[test]
    fn parses_hierarchy_with_inherited_slot_origin() {
        // A two-class hierarchy: the child inherits the parent's slot, so its
        // origin is the parent — the exact case the live verifier checks
        // against `slot_origin`.
        let dump = "\
=== classes ===
class <animal>
  parents [<object>]
  cpl [<animal>, <object>]
  slot name @8 setter=true origin=<animal> type=<string> init-keyword=name required=false default=unbound
class <dog>
  parents [<animal>]
  cpl [<dog>, <animal>, <object>]
  slot name @8 setter=true origin=<animal> type=<string> init-keyword=name required=false default=unbound
  slot breed @16 setter=true origin=<dog> type=<string> init-keyword=breed required=false default=unbound
=== sealing ===
";
        let classes = parse_sema_classes(dump).expect("parse");
        assert_eq!(classes.len(), 2);
        assert_eq!(classes[0].name, "<animal>");
        assert_eq!(classes[1].name, "<dog>");
        assert_eq!(
            classes[1].cpl,
            vec!["<dog>".to_string(), "<animal>".to_string(), "<object>".to_string()]
        );
        // Inherited slot carries the ancestor's origin.
        assert_eq!(classes[1].slots[0].origin, "<animal>");
        assert_eq!(classes[1].slots[1].origin, "<dog>");
    }

    #[test]
    fn no_classes_section_is_empty() {
        let dump = "\
=== top-names ===
fn f arity=0 return=Top
=== generics ===
=== classes ===
=== sealing ===
";
        assert!(parse_sema_classes(dump).expect("parse").is_empty());
    }

    #[test]
    fn class_with_no_slots() {
        let dump = "\
=== classes ===
class <marker>
  parents [<object>]
  cpl [<marker>, <object>]
=== sealing ===
";
        let classes = parse_sema_classes(dump).expect("parse");
        assert_eq!(classes.len(), 1);
        assert!(classes[0].slots.is_empty());
    }

    #[test]
    fn malformed_slot_line_errors() {
        let dump = "\
=== classes ===
class <bad>
  parents [<object>]
  cpl [<bad>, <object>]
  slot broken-no-offset setter=true origin=<bad>
=== sealing ===
";
        assert!(parse_sema_classes(dump).is_err());
    }

    /// Sprint 56a-CONSUME — `slot_type_from_label` is the exact inverse of
    /// `slot_type_label` on every scalar bucket the emitter can print, so the
    /// round-trip `label → type → label` is the identity. (The `Class(id)` arm
    /// needs the runtime registry and is covered by the live consume gates +
    /// the EXE compile-and-run.)
    #[test]
    fn slot_type_label_round_trips_scalars() {
        for t in [
            SlotType::Integer,
            SlotType::DoubleFloat,
            SlotType::Boolean,
            SlotType::Character,
            SlotType::String,
            SlotType::Symbol,
            SlotType::Vector,
            SlotType::Object,
            SlotType::Top,
        ] {
            let label = slot_type_label(t);
            let back = slot_type_from_label(&label).expect("inverse");
            assert_eq!(back, t, "round-trip failed for label {label:?}");
        }
    }

    /// Sprint 56a-CONSUME — `slot_default_from_tag` is the inverse of
    /// `slot_default_tag` for the process-stable tags. `unbound` round-trips
    /// exactly; a `value:<bits>` fixnum reconstructs the same raw Word. (The
    /// `true`/`false`/`nil` immediates need the live literal pool and are
    /// covered by the compile-and-run gate.) A malformed tag is an `Err`.
    #[test]
    fn slot_default_tag_round_trips_value_and_unbound() {
        assert_eq!(
            slot_default_from_tag("unbound").expect("inverse"),
            SlotDefault::Unbound
        );
        // A fixnum default tag (`value:<raw>`) reconstructs the same Word and
        // tags back identically.
        let w = Word::from_fixnum(42).expect("fixnum");
        let tag = slot_default_tag(SlotDefault::Value(w));
        assert_eq!(tag, "value:84");
        match slot_default_from_tag(&tag).expect("inverse") {
            SlotDefault::Value(got) => assert_eq!(got.raw(), w.raw()),
            other => panic!("expected Value, got {other:?}"),
        }
        assert!(slot_default_from_tag("value:not-a-number").is_err());
        assert!(slot_default_from_tag("garbage").is_err());
    }
}

#[cfg(test)]
mod sprint56c_method_parse_tests {
    //! Sprint 56c-T — pure-Rust coverage for [`parse_dylan_methods`], the inverse
    //! of the `=== methods ===` section emitted by `dylan-lower-emit`. The
    //! end-to-end live verification (`verify_dylan_methods` at the dfm-dump seam)
    //! is exercised by the `--lower-with-dylan` byte-match gates on `point` /
    //! `richards-shape`; these tests pin the text grammar without the shim.
    use super::*;

    // A `point`-shaped table: two slot getters + two setters (the pass-1
    // accessor walk, getter-then-setter per own slot), then a `richards`-shaped
    // user method block (one generic with multiple specialiser tuples).
    const METHODS_DUMP: &str = "\
=== methods ===
method x body=<user-point>-getter-x params=1 specialisers=[<user-point>]
method x-setter body=<user-point>-setter-x params=2 specialisers=[<user-point>, <object>]
method y body=<user-point>-getter-y params=1 specialisers=[<user-point>]
method y-setter body=<user-point>-setter-y params=2 specialisers=[<user-point>, <object>]
method run-task body=run-task$<idler>_<integer> params=2 specialisers=[<idler>, <integer>]
";

    #[test]
    fn parses_accessor_and_user_methods() {
        let methods = parse_dylan_methods(METHODS_DUMP).expect("parse");
        assert_eq!(methods.len(), 5);
        assert_eq!(
            methods[0],
            ParsedMethod {
                generic_name: "x".to_string(),
                body_fn_name: "<user-point>-getter-x".to_string(),
                param_count: 1,
                specialisers: vec!["<user-point>".to_string()],
            }
        );
        assert_eq!(
            methods[1],
            ParsedMethod {
                generic_name: "x-setter".to_string(),
                body_fn_name: "<user-point>-setter-x".to_string(),
                param_count: 2,
                specialisers: vec!["<user-point>".to_string(), "<object>".to_string()],
            }
        );
        // A multi-arg user method: two specialisers, body name carries both.
        assert_eq!(
            methods[4],
            ParsedMethod {
                generic_name: "run-task".to_string(),
                body_fn_name: "run-task$<idler>_<integer>".to_string(),
                param_count: 2,
                specialisers: vec!["<idler>".to_string(), "<integer>".to_string()],
            }
        );
    }

    #[test]
    fn parses_tail_without_header() {
        // The host passes the slice AFTER the `\n=== methods ===\n` delimiter,
        // whose first line is the first `method` (no header line). The parser
        // tolerates that.
        let tail = "\
method id body=<thing>-getter-id params=1 specialisers=[<thing>]
";
        let methods = parse_dylan_methods(tail).expect("parse");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].generic_name, "id");
        assert_eq!(methods[0].body_fn_name, "<thing>-getter-id");
        assert_eq!(methods[0].param_count, 1);
        assert_eq!(methods[0].specialisers, vec!["<thing>".to_string()]);
    }

    #[test]
    fn empty_methods_section_is_empty() {
        let methods = parse_dylan_methods("=== methods ===\n").expect("parse");
        assert!(methods.is_empty());
    }

    #[test]
    fn malformed_method_line_errors() {
        // Missing the ` body=` field.
        let dump = "\
=== methods ===
method broken params=1 specialisers=[<x>]
";
        assert!(parse_dylan_methods(dump).is_err());
    }

    #[test]
    fn verify_matches_equal_tables() {
        // A Rust table whose names resolve via the seeded `<object>` /
        // `<integer>` classes (always registered) verifies against a Dylan dump
        // that reproduces those names.
        let rust = vec![
            MethodRegistration {
                generic_name: "f".to_string(),
                specialisers: vec![ClassId::INTEGER],
                body_fn_name: "f$<integer>".to_string(),
                param_count: 1,
            },
            MethodRegistration {
                generic_name: "g-setter".to_string(),
                specialisers: vec![ClassId::INTEGER, ClassId::OBJECT],
                body_fn_name: "<c>-setter-g".to_string(),
                param_count: 2,
            },
        ];
        let dylan = vec![
            ParsedMethod {
                generic_name: "f".to_string(),
                body_fn_name: "f$<integer>".to_string(),
                param_count: 1,
                specialisers: vec!["<integer>".to_string()],
            },
            ParsedMethod {
                generic_name: "g-setter".to_string(),
                body_fn_name: "<c>-setter-g".to_string(),
                param_count: 2,
                specialisers: vec!["<integer>".to_string(), "<object>".to_string()],
            },
        ];
        nod_runtime::ensure_conditions_registered();
        assert!(verify_dylan_methods(&rust, &dylan).is_ok());
    }

    #[test]
    fn verify_rejects_specialiser_mismatch() {
        let rust = vec![MethodRegistration {
            generic_name: "f".to_string(),
            specialisers: vec![ClassId::INTEGER],
            body_fn_name: "f$<integer>".to_string(),
            param_count: 1,
        }];
        let dylan = vec![ParsedMethod {
            generic_name: "f".to_string(),
            body_fn_name: "f$<integer>".to_string(),
            param_count: 1,
            specialisers: vec!["<object>".to_string()], // wrong — host has <integer>
        }];
        assert!(verify_dylan_methods(&rust, &dylan).is_err());
    }
}
