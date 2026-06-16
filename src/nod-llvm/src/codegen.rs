//! DFM -> LLVM IR lowering.
//!
//! **Sprint 09 ABI.** Every Dylan value of `<integer>` or `<boolean>`
//! type lowers to an `i64` holding a tagged `nod_runtime::Word`:
//!
//! ```text
//!   bit 0 = 0 → fixnum;  upper 63 bits = signed value shifted left by 1.
//!   bit 0 = 1 → pointer; bits [63:1] = 8-byte-aligned heap pointer.
//! ```
//!
//! For Sprint 09, `#t` and `#f` are *immediate* booleans encoded as
//! tagged fixnums 1 and 0 respectively (so `#f` = `Word(0)`). Sprint
//! 10+ may introduce a richer immediate scheme; today's encoding is
//! the minimum that gives `instance?(x, <boolean>)` something to test.
//!
//! **Tagged arithmetic.** `(a<<1) + (b<<1) = (a+b)<<1`, so integer
//! `add` / `sub` / `neg` need no untag/retag. `mul` is asymmetric:
//! `(a<<1) * (b<<1) = (a*b) << 2`, so we right-shift one operand
//! before the multiply to recover `(a*b)<<1`. `div` / `mod` / `rem`
//! untag both operands and retag the result — the cleanest lowering
//! given that signed-division identities don't survive the shift.
//!
//! **Comparisons** run directly on the tagged words (ordering is
//! preserved because both operands shift left by the same amount).
//! The `i1` from `icmp` is `zext`'d to i64 and shifted left by 1 to
//! match the boolean encoding.
//!
//! **Floats** are not tagged — Sprint 09 functions returning
//! `<double-float>` return raw `f64`. Sprint 10 boxes floats on the
//! heap; until then the calling convention for `<double-float>` is
//! the same as Sprint 07.
//!
//! **Sprint 10 changes.**
//!   - `#t` / `#f` are no longer fixnum-shaped; they're pinned heap
//!     wrappers whose addresses come from `nod_runtime::Immediates`.
//!     Codegen bakes those addresses into LLVM constants.
//!   - `<byte-string>` literals are interned in the process-global
//!     literal pool (`nod_runtime::intern_string_literal`); codegen
//!     bakes the resulting tagged Word as an `i64` constant.
//!   - `instance?` against the wrapper-tagged seed classes
//!     (`<byte-string>`, `<symbol>`, `<simple-object-vector>`,
//!     `<empty-list>`, `<boolean>`) reads the wrapper's class id and
//!     compares.
//!   - `format-out` is recognised as a builtin: codegen declares an
//!     `extern "C"` shim and binds `nod_format_out` via
//!     `LLVMAddGlobalMapping` at JIT-engine creation time.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, PhiValue,
};
use nod_dfm::{
    BlockId, ClassCheck, Computation, ConstValue, Function as DfmFunction, PrimOp,
    SafepointLocation, SafepointRootLocation, SlotTypeKind, TempId, Terminator, TypeEstimate,
};
use nod_runtime::ClassId;

use crate::cache::CacheKey;
use crate::symbols::{
    ModuleManifest, RelocKind, cache_slot_symbol, class_md_symbol, generic_symbol,
    imm_false_symbol, imm_false_wrapper_symbol, imm_nil_symbol, imm_true_symbol, strlit_symbol,
    stub_symbol, symlit_symbol,
};

/// Name of the JIT-side `nod_make` external declaration.
pub const NOD_MAKE_SYMBOL: &str = "nod_make";
/// Name of the JIT-side `nod_is_instance_of` external declaration.
pub const NOD_IS_INSTANCE_OF_SYMBOL: &str = "nod_is_instance_of";
/// Name of the JIT-side `nod_dispatch_unary` external declaration.
pub const NOD_DISPATCH_UNARY_SYMBOL: &str = "nod_dispatch_unary";
/// Name of the JIT-side `nod_dispatch_binary` external declaration.
pub const NOD_DISPATCH_BINARY_SYMBOL: &str = "nod_dispatch_binary";
/// Sprint 13: variadic dispatch entry. Takes the generic pointer, a
/// cache-slot pointer, an arity, and up to 8 arguments.
pub const NOD_DISPATCH_SYMBOL: &str = "nod_dispatch";
/// Name of the JIT-side card-mark shim (`nod_card_mark`).
pub const NOD_CARD_MARK_SYMBOL: &str = "nod_card_mark";

/// Sprint 11b: Name of the JIT-side `nod_register_root` external. The
/// codegen layer brackets every potentially-allocating call with a
/// `register_root(slot)` ... call ... `unregister_root(slot)` pair for
/// each pointer-shaped live-across temp. The runtime walks registered
/// slots during GC and rewrites them if the target object moves.
pub const NOD_REGISTER_ROOT_SYMBOL: &str = "nod_register_root";
/// Sprint 11b: companion to `NOD_REGISTER_ROOT_SYMBOL`.
pub const NOD_UNREGISTER_ROOT_SYMBOL: &str = "nod_unregister_root";
/// Sprint 45d: JIT runtime hook that marks one active safepoint frame
/// so the collector can consume the codegen-emitted callsite map.
pub const NOD_JIT_BEGIN_SAFEPOINT_SYMBOL: &str = "nod_jit_begin_safepoint";
/// Sprint 45d: companion to `NOD_JIT_BEGIN_SAFEPOINT_SYMBOL`.
pub const NOD_JIT_END_SAFEPOINT_SYMBOL: &str = "nod_jit_end_safepoint";
/// Sprint 45c: image-only runtime hook that records the pre-safepoint
/// root-stack baseline and checks the executing site against the
/// registered AOT safepoint table.
pub const NOD_AOT_BEGIN_SAFEPOINT_SYMBOL: &str = "nod_aot_begin_safepoint";
/// Sprint 45c: image-only runtime hook that verifies the current root
/// stack matches the static safepoint plan after registration.
pub const NOD_AOT_VERIFY_SAFEPOINT_SYMBOL: &str = "nod_aot_verify_safepoint";
/// Sprint 45c: image-only runtime hook that verifies safepoint cleanup
/// restored the pre-call root-stack baseline.
pub const NOD_AOT_END_SAFEPOINT_SYMBOL: &str = "nod_aot_end_safepoint";
/// Sprint 45e: poll hook emitted at every function entry and every
/// loop back-edge target.  Fast path is a single relaxed load; the
/// slow path parks the calling thread until the GC clears the flag.
pub const NOD_SAFEPOINT_POLL_SYMBOL: &str = "nod_safepoint_poll";

/// Sprint 14: invoke the next-most-specific applicable method on the
/// current dispatch chain, forwarding the current method's args
/// verbatim. Lowered from Dylan-side `next-method()` calls.
pub const NOD_NEXT_METHOD_SYMBOL: &str = "nod_next_method";
/// Sprint 14: predicate `next-method?()` — `#t` iff there's a next
/// method in the chain.
pub const NOD_HAS_NEXT_METHOD_SYMBOL: &str = "nod_has_next_method";

/// Sprint 55: `%is-generic?(name)` — `#t` iff `name` is a registered generic.
/// Called only by the Dylan-side lowering shim (to classify a callee as
/// Dispatch vs DirectCall).
pub const NOD_IS_GENERIC_DEFINED_SYMBOL: &str = "nod_is_generic_defined";

/// Sprint 55: `%is-class?(name)` — `#t` iff `name` is a registered class.
/// Called only by the Dylan-side lowering shim (to type a param as `<class>`).
pub const NOD_IS_CLASS_DEFINED_SYMBOL: &str = "nod_is_class_defined";

/// Sprint 15: push a `next-method` chain frame on entry to a
/// sealed-direct multimethod call. Codegen emits a call to this just
/// before the resolved-method direct call so `next-method()` walks
/// the fallback chain (resolved at compile time).
pub const NOD_PUSH_SEALED_CHAIN_SYMBOL: &str = "nod_push_sealed_chain_frame";

/// Sprint 15: pop the chain frame after a sealed-direct multimethod
/// call returns. Paired with `NOD_PUSH_SEALED_CHAIN_SYMBOL`.
pub const NOD_POP_SEALED_CHAIN_SYMBOL: &str = "nod_pop_sealed_chain_frame";

/// Name of the JIT-side `format-out` external declaration. Resolved
/// to `nod_runtime::nod_format_out` via `LLVMAddGlobalMapping` at
/// engine-creation time (see `nod-llvm::jit`).
pub const FORMAT_OUT_SYMBOL: &str = "nod_format_out";

// ─── Sprint 16: `<pair>` / `<list>` builtins ───────────────────────────────
//
// Each Dylan-source builtin lowers to a synthetic `%pair-*` / `%empty?` /
// `%nil` callee in the DFM `DirectCall`. Codegen recognises the callee,
// declares the extern with the matching ABI, and emits the call. The
// JIT layer (`jit.rs`) resolves the extern's symbol to the runtime
// shim's address via `LLVMAddGlobalMapping`.

/// Sprint 16: `nod_pair_alloc(head, tail) -> <pair>`.
pub const NOD_PAIR_ALLOC_SYMBOL: &str = "nod_pair_alloc";
/// Sprint 16: `nod_pair_head(p) -> <object>`.
pub const NOD_PAIR_HEAD_SYMBOL: &str = "nod_pair_head";
/// Sprint 16: `nod_pair_tail(p) -> <object>`.
pub const NOD_PAIR_TAIL_SYMBOL: &str = "nod_pair_tail";
/// Sprint 16: `nod_empty_p(p) -> <boolean>`. Identity against `nil`.
pub const NOD_EMPTY_P_SYMBOL: &str = "nod_empty_p";
/// Sprint 16: `nod_nil() -> <empty-list>`.
pub const NOD_NIL_SYMBOL: &str = "nod_nil";

// ─── Sprint 19: conditions + block/exception/cleanup ───────────────────────
/// Sprint 19: `nod_signal(cond) -> u64`. Diverges via panic-based NLX.
pub const NOD_SIGNAL_SYMBOL: &str = "nod_signal";
/// Sprint 19: `nod_run_block(block_id, c0..c7) -> u64`. Drives the
/// block protocol.
pub const NOD_RUN_BLOCK_SYMBOL: &str = "nod_run_block";
/// Sprint 19: `nod_make_exit_procedure(block_id_word) -> u64`. Wraps
/// the Rust-side `make_exit_procedure` so codegen can mint exit
/// procedures from a `block (k)` site.
pub const NOD_MAKE_EXIT_PROCEDURE_SYMBOL: &str = "nod_make_exit_procedure";
/// Sprint 19: `nod_invoke_exit(ep, value) -> u64`. Diverges via NLX.
pub const NOD_INVOKE_EXIT_SYMBOL: &str = "nod_invoke_exit";
/// Sprint 19: `nod_condition_message(c) -> <byte-string>`.
pub const NOD_CONDITION_MESSAGE_SYMBOL: &str = "nod_condition_message";
/// `error(msg)` — construct a `<simple-error>` and signal it. Diverges.
pub const NOD_ERROR_SYMBOL: &str = "nod_error";
/// `write-to-string(val)` — return a `<byte-string>` representation.
pub const NOD_WRITE_TO_STRING_SYMBOL: &str = "nod_write_to_string";
/// `integer-to-string(n)` — decimal string from a Dylan fixnum.
pub const NOD_INTEGER_TO_STRING_SYMBOL: &str = "nod_integer_to_string";

// ─── Sprint 20b — collection / FIP / primitive-op shims ───────────────────
//
// Each shim mirrors a `%`-prefixed primitive callee emitted by
// `nod-sema/src/lower.rs::LOWER_PRIMITIVE_TABLE`. Codegen recognises the
// callee in `emit_direct_call`, declares the extern with `(i64, …) -> i64`,
// and `jit.rs` binds the symbol to the runtime shim address at engine
// creation. The shim sources live in `nod-runtime/src/collections.rs`.

pub const NOD_COLLECTION_SIZE_SYMBOL: &str = "nod_collection_size";
pub const NOD_COLLECTION_CONCATENATE_SYMBOL: &str = "nod_collection_concatenate";
pub const NOD_RANGE_FROM_SYMBOL: &str = "nod_range_from";
pub const NOD_RANGE_TO_SYMBOL: &str = "nod_range_to";
pub const NOD_RANGE_BY_SYMBOL: &str = "nod_range_by";
pub const NOD_SOV_SIZE_SYMBOL: &str = "nod_sov_size";
pub const NOD_SOV_ELEMENT_SYMBOL: &str = "nod_sov_element";
pub const NOD_SOV_ELEMENT_SETTER_SYMBOL: &str = "nod_sov_element_setter";
pub const NOD_STRETCHY_VECTOR_SIZE_SYMBOL: &str = "nod_stretchy_vector_size";
pub const NOD_STRETCHY_VECTOR_ELEMENT_SYMBOL: &str = "nod_stretchy_vector_element";
pub const NOD_STRETCHY_VECTOR_ELEMENT_SETTER_SYMBOL: &str = "nod_stretchy_vector_element_setter";
pub const NOD_STRETCHY_VECTOR_PUSH_SYMBOL: &str = "nod_stretchy_vector_push";
pub const NOD_FIP_INIT_SYMBOL: &str = "nod_fip_init";
pub const NOD_FIP_FINISHED_P_SYMBOL: &str = "nod_fip_finished_p";
pub const NOD_FIP_CURRENT_ELEMENT_SYMBOL: &str = "nod_fip_current_element";
pub const NOD_FIP_ADVANCE_SYMBOL: &str = "nod_fip_advance";
pub const NOD_MAKE_RANGE_SYMBOL: &str = "nod_make_range";
pub const NOD_MAKE_STRETCHY_VECTOR_SYMBOL: &str = "nod_make_stretchy_vector";
pub const NOD_GET_ARGV2_SYMBOL: &str = "nod_get_argv2";
pub const NOD_PRINT_GC_STATS_SYMBOL: &str = "nod_print_gc_stats";

// ─── Sprint 21 — first-class function values ──────────────────────────────
//
// `nod_make_function_ref(name_bytestring, arity_fixnum) -> <function>`
// allocates (or returns the cached) `<function>` Word for the supplied
// Dylan-source name. The codegen emits a call to this shim whenever the
// lowerer sees an `Expr::Ident` resolving to a registered function used
// in expression (not call-head) position.
//
// `nod_funcall1`, `nod_funcall2`, `nod_apply` are the trampolines for
// invoking a `<function>` Word. See `nod-runtime::functions` for the
// implementation.
pub const NOD_MAKE_FUNCTION_REF_SYMBOL: &str = "nod_make_function_ref";
pub const NOD_FUNCALL0_SYMBOL: &str = "nod_funcall0";
pub const NOD_FUNCALL1_SYMBOL: &str = "nod_funcall1";
pub const NOD_FUNCALL2_SYMBOL: &str = "nod_funcall2";
pub const NOD_FUNCALL3_SYMBOL: &str = "nod_funcall3";
pub const NOD_FUNCALL4_SYMBOL: &str = "nod_funcall4";
pub const NOD_FUNCALL5_SYMBOL: &str = "nod_funcall5";
pub const NOD_APPLY_SYMBOL: &str = "nod_apply";
pub const NOD_MAKE_SOV_LEN_SYMBOL: &str = "nod_make_sov_len";

// ─── Sprint 28 — Win64 FFI trampolines ────────────────────────────────────
//
// One trampoline per arity 0..=8. Lowering emits a DirectCall against
// the synthetic `%winffi-call-N` callee; codegen recognises the prefix
// and declares the matching extern. The first arg of each trampoline
// is the static-area pointer of the entry's [`ApiStubEntry`] (baked as
// an `i64` constant by lowering); the remaining args are the Dylan
// caller's args, each passed as a tagged `i64` Word.
pub const NOD_WINFFI_CALL_0_SYMBOL: &str = "nod_winffi_call_0";
pub const NOD_WINFFI_CALL_1_SYMBOL: &str = "nod_winffi_call_1";
pub const NOD_WINFFI_CALL_2_SYMBOL: &str = "nod_winffi_call_2";
pub const NOD_WINFFI_CALL_3_SYMBOL: &str = "nod_winffi_call_3";
pub const NOD_WINFFI_CALL_4_SYMBOL: &str = "nod_winffi_call_4";
pub const NOD_WINFFI_CALL_5_SYMBOL: &str = "nod_winffi_call_5";
pub const NOD_WINFFI_CALL_6_SYMBOL: &str = "nod_winffi_call_6";
pub const NOD_WINFFI_CALL_7_SYMBOL: &str = "nod_winffi_call_7";
pub const NOD_WINFFI_CALL_8_SYMBOL: &str = "nod_winffi_call_8";
// Sprint 36b: extend trampoline family to arity 12 (CreateWindowExW).
pub const NOD_WINFFI_CALL_9_SYMBOL: &str = "nod_winffi_call_9";
pub const NOD_WINFFI_CALL_10_SYMBOL: &str = "nod_winffi_call_10";
pub const NOD_WINFFI_CALL_11_SYMBOL: &str = "nod_winffi_call_11";
pub const NOD_WINFFI_CALL_12_SYMBOL: &str = "nod_winffi_call_12";

// ─── Sprint 32 — closure → C callback function-pointer trampolines ────────
//
// One extern per Win32 callback signature. Each takes a Dylan closure
// Word and returns a `<c-pointer>` Word whose payload is the raw
// trampoline address that Win32 will call through standard Win64 ABI.
pub const NOD_REGISTER_WNDPROC_SYMBOL: &str = "nod_register_wndproc";
pub const NOD_REGISTER_WNDENUMPROC_SYMBOL: &str = "nod_register_wndenumproc";

// ─── Sprint 47 — multi-value return secondary-values buffer (GAP-003) ─────
//
// SBCL-style TLS buffer + count. `nod_values_clear` resets the count to
// 0; `nod_values_set` writes an extra at the given fixnum index and
// updates the count; `nod_values_get` reads an extra (returning `#f`
// past the count); `nod_values_count` reads the current count as a
// fixnum. See `src/nod-runtime/src/values.rs` and
// `docs/COMPILER_GAPS.md` GAP-003. Wired into the stdlib via the four
// `%values-*` primitives in `nod-sema::lower::LOWER_PRIMITIVE_TABLE`.
pub const NOD_VALUES_CLEAR_SYMBOL: &str = "nod_values_clear";
pub const NOD_VALUES_SET_SYMBOL: &str = "nod_values_set";
pub const NOD_VALUES_GET_SYMBOL: &str = "nod_values_get";
pub const NOD_VALUES_COUNT_SYMBOL: &str = "nod_values_count";

// ─── Sprint 24 — closures: <cell> and <environment> ───────────────────────
pub const NOD_MAKE_CELL_SYMBOL: &str = "nod_make_cell";
pub const NOD_CELL_GET_SYMBOL: &str = "nod_cell_get";
pub const NOD_CELL_SET_SYMBOL: &str = "nod_cell_set";
pub const NOD_ENV_CELL_SYMBOL: &str = "nod_env_cell";
pub const NOD_MAKE_ENVIRONMENT_SYMBOL: &str = "nod_make_environment";
pub const NOD_MAKE_CLOSURE_SYMBOL: &str = "nod_make_closure";
pub const NOD_MAKE_REST_CLOSURE_SYMBOL: &str = "nod_make_rest_closure";

// ─── GAP-004 — `define variable` getter/setter shims by name ──────────────
pub const NOD_VAR_GET_BY_NAME_SYMBOL: &str = "nod_var_get_by_name";
pub const NOD_VAR_SET_BY_NAME_SYMBOL: &str = "nod_var_set_by_name";

// ─── Sprint 34 — <c-struct> field accessor primitives ─────────────────────
//
// Each (get, set) pair takes the struct Word and a byte offset and
// returns/writes a typed value (fixnum-shaped). Wired into the stdlib
// via `%struct-get-*` / `%struct-set-*` primitives.
pub const NOD_STRUCT_GET_I32_SYMBOL: &str = "nod_struct_get_i32";
pub const NOD_STRUCT_SET_I32_SYMBOL: &str = "nod_struct_set_i32";
pub const NOD_STRUCT_GET_I64_SYMBOL: &str = "nod_struct_get_i64";
pub const NOD_STRUCT_SET_I64_SYMBOL: &str = "nod_struct_set_i64";
pub const NOD_STRUCT_GET_U16_SYMBOL: &str = "nod_struct_get_u16";
pub const NOD_STRUCT_SET_U16_SYMBOL: &str = "nod_struct_set_u16";
pub const NOD_STRUCT_GET_U32_SYMBOL: &str = "nod_struct_get_u32";
pub const NOD_STRUCT_SET_U32_SYMBOL: &str = "nod_struct_set_u32";
pub const NOD_STRUCT_GET_U64_SYMBOL: &str = "nod_struct_get_u64";
pub const NOD_STRUCT_SET_U64_SYMBOL: &str = "nod_struct_set_u64";
pub const NOD_STRUCT_GET_POINTER_SYMBOL: &str = "nod_struct_get_pointer";
pub const NOD_STRUCT_SET_POINTER_SYMBOL: &str = "nod_struct_set_pointer";

// ─── Sprint 35 — COM shim symbols ────────────────────────────────────────
pub const NOD_COM_RELEASE_SYMBOL: &str = "nod_com_release";
pub const NOD_COM_REGISTRY_LEN_SYMBOL: &str = "nod_com_registry_len";
pub const NOD_COM_LAST_HRESULT_SYMBOL: &str = "nod_com_last_hresult";
pub const NOD_COM_CLEAR_LAST_HRESULT_SYMBOL: &str = "nod_com_clear_last_hresult";
pub const NOD_DXGI_CREATE_FACTORY_SYMBOL: &str = "nod_dxgi_create_factory";
pub const NOD_DXGI_DEVICE_FROM_D3D_DEVICE_SYMBOL: &str = "nod_dxgi_device_from_d3d_device";
pub const NOD_DXGI_CREATE_SURFACE_FROM_TEXTURE_SYMBOL: &str = "nod_dxgi_create_surface_from_texture";
pub const NOD_D3D11_CREATE_DEVICE_SYMBOL: &str = "nod_d3d11_create_device";
pub const NOD_D3D11_GET_IMMEDIATE_CONTEXT_SYMBOL: &str = "nod_d3d11_get_immediate_context";
pub const NOD_D3D11_CREATE_TEXTURE_2D_SYMBOL: &str = "nod_d3d11_create_texture_2d";
pub const NOD_D3D11_COPY_TO_STAGING_AND_MAP_SYMBOL: &str = "nod_d3d11_copy_to_staging_and_map";
pub const NOD_D3D11_LAST_STAGING_HANDLE_SYMBOL: &str = "nod_d3d11_last_staging_handle";
pub const NOD_D3D11_LAST_MAPPED_ROW_PITCH_SYMBOL: &str = "nod_d3d11_last_mapped_row_pitch";
pub const NOD_D3D11_UNMAP_SYMBOL: &str = "nod_d3d11_unmap";
pub const NOD_D2D_CREATE_FACTORY_SYMBOL: &str = "nod_d2d_create_factory";
pub const NOD_D2D_CREATE_DEVICE_SYMBOL: &str = "nod_d2d_create_device";
pub const NOD_D2D_CREATE_DEVICE_CONTEXT_SYMBOL: &str = "nod_d2d_create_device_context";
pub const NOD_D2D_CREATE_BITMAP_FOR_TARGET_SYMBOL: &str = "nod_d2d_create_bitmap_for_target";
pub const NOD_D2D_SET_TARGET_SYMBOL: &str = "nod_d2d_set_target";
pub const NOD_D2D_BEGIN_DRAW_SYMBOL: &str = "nod_d2d_begin_draw";
pub const NOD_D2D_END_DRAW_SYMBOL: &str = "nod_d2d_end_draw";
pub const NOD_D2D_CLEAR_SYMBOL: &str = "nod_d2d_clear";
pub const NOD_D2D_SET_TRANSFORM_IDENTITY_SYMBOL: &str = "nod_d2d_set_transform_identity";
pub const NOD_D2D_CREATE_SOLID_COLOR_BRUSH_SYMBOL: &str = "nod_d2d_create_solid_color_brush";
pub const NOD_D2D_DRAW_TEXT_LAYOUT_SYMBOL: &str = "nod_d2d_draw_text_layout";
pub const NOD_D2D_DRAW_RECTANGLE_SYMBOL: &str = "nod_d2d_draw_rectangle";
pub const NOD_D2D_FILL_RECTANGLE_SYMBOL: &str = "nod_d2d_fill_rectangle";
pub const NOD_DWRITE_CREATE_FACTORY_SYMBOL: &str = "nod_dwrite_create_factory";
pub const NOD_DWRITE_CREATE_TEXT_FORMAT_SYMBOL: &str = "nod_dwrite_create_text_format";
pub const NOD_DWRITE_CREATE_TEXT_LAYOUT_SYMBOL: &str = "nod_dwrite_create_text_layout";
pub const NOD_DWRITE_GET_LAYOUT_METRICS_SYMBOL: &str = "nod_dwrite_get_layout_metrics";
pub const NOD_DWRITE_HIT_TEST_TEXT_POSITION_SYMBOL: &str = "nod_dwrite_hit_test_text_position";
pub const NOD_DWRITE_HIT_TEST_POINT_SYMBOL: &str = "nod_dwrite_hit_test_point";
pub const NOD_DWRITE_SET_DRAWING_EFFECT_SYMBOL: &str = "nod_dwrite_set_drawing_effect";
pub const NOD_DWRITE_SET_LINE_SPACING_SYMBOL: &str = "nod_dwrite_set_line_spacing";
pub const NOD_COUNT_NON_ZERO_RED_SYMBOL: &str = "nod_count_non_zero_red";

// ─── Sprint 36 — HWND-bound swap chain + IDE-shell window plumbing ────────
pub const NOD_DXGI_FACTORY_FROM_D3D_DEVICE_SYMBOL: &str = "nod_dxgi_factory_from_d3d_device";
pub const NOD_DXGI_CREATE_SWAP_CHAIN_FOR_HWND_SYMBOL: &str =
    "nod_dxgi_create_swap_chain_for_hwnd";
pub const NOD_D2D_CREATE_BITMAP_FROM_SWAP_CHAIN_SYMBOL: &str =
    "nod_d2d_create_bitmap_from_swap_chain";
pub const NOD_DXGI_SWAP_CHAIN_PRESENT_SYMBOL: &str = "nod_dxgi_swap_chain_present";
pub const NOD_DXGI_SWAP_CHAIN_RESIZE_BUFFERS_SYMBOL: &str =
    "nod_dxgi_swap_chain_resize_buffers";
pub const NOD_REGISTER_WINDOW_CLASS_SYMBOL: &str = "nod_register_window_class";
pub const NOD_CREATE_MESSAGE_ONLY_WINDOW_SYMBOL: &str = "nod_create_message_only_window";
pub const NOD_CREATE_HIDDEN_WINDOW_SYMBOL: &str = "nod_create_hidden_window";
pub const NOD_DESTROY_WINDOW_SYMBOL: &str = "nod_destroy_window";
pub const NOD_POST_MESSAGE_SYMBOL: &str = "nod_post_message";
pub const NOD_PUMP_ONE_MESSAGE_SYMBOL: &str = "nod_pump_one_message";
/// Sprint 41a — canonical blocking `GetMessage` / `Translate` /
/// `Dispatch` loop. Arity 0; returns the fixnum-tagged WM_QUIT exit
/// code.
pub const NOD_RUN_MESSAGE_LOOP_SYMBOL: &str = "nod_run_message_loop";
pub const NOD_DEF_WINDOW_PROC_SYMBOL: &str = "nod_def_window_proc";

// ─── Sprint 41b — IDE source-viewer primitives ────────────────────────────
/// Read a file's contents into a fresh Dylan `<byte-string>` (or `nil`
/// on error). Arity-1; takes a `<byte-string>` path Word.
pub const NOD_READ_FILE_TO_STRING_SYMBOL: &str = "nod_read_file_to_string";
/// Return `argv[1]` as a Dylan `<byte-string>` (or `nil` if absent).
/// Arity-0.
pub const NOD_GET_ARGV1_SYMBOL: &str = "nod_get_argv1";
/// Low 16 bits of an integer (for `LOWORD(lparam)` in WM_SIZE).
pub const NOD_LO_WORD_SYMBOL: &str = "nod_lo_word";
/// Bits 16-31 of an integer (for `HIWORD(lparam)` in WM_SIZE).
pub const NOD_HI_WORD_SYMBOL: &str = "nod_hi_word";

// ─── Sprint 41c — scrollbar primitives ────────────────────────────────────
/// Configure a scrollbar (vertical or horizontal) on a window. Arity-7.
pub const NOD_SET_SCROLL_INFO_SYMBOL: &str = "nod_set_scroll_info";
/// Read a scrollbar's current position. Arity-2.
pub const NOD_GET_SCROLL_POS_SYMBOL: &str = "nod_get_scroll_pos";
// `nod_count_newlines` / `nod_max_line_chars` consts removed in Sprint 42a
// Phase E — those shims are now pure Dylan in nod-ide.dylan.

// ─── Sprint 41e — File → Open common dialog ──────────────────────────────
/// Shim that wraps `GetOpenFileNameW` + an `OPENFILENAMEW` struct, returning
/// the chosen path as a `<byte-string>` Word (or `nil` on cancel). Arity-1
/// (owner HWND).
pub const NOD_SHOW_OPEN_FILE_DIALOG_SYMBOL: &str = "nod_show_open_file_dialog";

// ─── Sprint 41g — File → Save / Save As + Recent submenu ─────────────────
/// Write a Dylan `<byte-string>` payload to a file whose path is also
/// a `<byte-string>`. Returns fixnum 1 on success / 0 on I/O error.
/// Arity-2.
pub const NOD_WRITE_FILE_FROM_STRING_SYMBOL: &str = "nod_write_file_from_string";
/// Shim that wraps `GetSaveFileNameW` (same `OPENFILENAMEW` struct as
/// the open-dialog shim) with `OFN_OVERWRITEPROMPT`. Returns the chosen
/// path as a `<byte-string>` Word (or `nil` on cancel). Arity-1
/// (owner HWND).
pub const NOD_SHOW_SAVE_FILE_DIALOG_SYMBOL: &str = "nod_show_save_file_dialog";
// `nod_load_recent` / `nod_add_recent` / `nod_basename` consts removed
// in Sprint 42a Phase E — Dylan now handles recent-list persistence
// (and basename) via its own helpers built on the byte-string ops.

// ─── Sprint 22 — <table> + hashing ─────────────────────────────────────────
pub const NOD_MAKE_TABLE_SYMBOL: &str = "nod_make_table";
pub const NOD_TABLE_SIZE_SYMBOL: &str = "nod_table_size";
pub const NOD_TABLE_ELEMENT_SYMBOL: &str = "nod_table_element";
pub const NOD_TABLE_ELEMENT_OR_DEFAULT_SYMBOL: &str = "nod_table_element_or_default";
pub const NOD_TABLE_ELEMENT_SETTER_SYMBOL: &str = "nod_table_element_setter";
pub const NOD_TABLE_REMOVE_KEY_SYMBOL: &str = "nod_table_remove_key";
pub const NOD_TABLE_KEYS_SYMBOL: &str = "nod_table_keys";
pub const NOD_TABLE_VALUES_SYMBOL: &str = "nod_table_values";
pub const NOD_OBJECT_HASH_SYMBOL: &str = "nod_object_hash";
pub const NOD_OBJECT_EQUAL_P_SYMBOL: &str = "nod_object_equal_p";
pub const NOD_OP_POW_SYMBOL: &str = "nod_op_pow";
pub const NOD_SUBTYPE_P_SYMBOL: &str = "nod_subtype_p";
pub const NOD_INSTANCE_P_SYMBOL: &str = "nod_instance_p";

// Sprint 42a — <byte-string> primitives. Five-op minimum surface; all
// user-visible byte-string methods (`size`, `element`, `concatenate`,
// `copy-sequence`, `starts-with?`, `find-substring`, `as-uppercase`, …)
// live in `stdlib.dylan` and call these.
pub const NOD_BYTE_STRING_ALLOCATE_SYMBOL: &str = "nod_byte_string_allocate";
pub const NOD_BYTE_STRING_SIZE_SYMBOL: &str = "nod_byte_string_size";
pub const NOD_BYTE_STRING_ELEMENT_SYMBOL: &str = "nod_byte_string_element";
pub const NOD_BYTE_STRING_ELEMENT_SETTER_SYMBOL: &str = "nod_byte_string_element_setter";
pub const NOD_BYTE_STRING_COPY_BYTES_SYMBOL: &str = "nod_byte_string_copy_bytes";

// `<character>` ↔ `<integer>` code-point conversion. `%char-code`
// sign-extends its i32 char arg to the i64 ABI at the call boundary;
// `%code-char` returns the code in the low 32 bits of an i64, truncated
// back to the i32 `<character>` register by `coerce_call_result`.
pub const NOD_CHAR_CODE_SYMBOL: &str = "nod_char_code";
pub const NOD_CODE_CHAR_SYMBOL: &str = "nod_code_char";

// Collection-classes lever (Part A2) — <bit-vector> + word bitwise ops.
pub const NOD_BIT_VECTOR_ALLOCATE_SYMBOL: &str = "nod_bit_vector_allocate";
pub const NOD_BIT_VECTOR_REF_SYMBOL: &str = "nod_bit_vector_ref";
pub const NOD_BIT_VECTOR_SET_SYMBOL: &str = "nod_bit_vector_set";
pub const NOD_BIT_VECTOR_SIZE_SYMBOL: &str = "nod_bit_vector_size";
pub const NOD_BIT_VECTOR_COUNT_SYMBOL: &str = "nod_bit_vector_count";
pub const NOD_LOGAND_SYMBOL: &str = "nod_logand";
pub const NOD_LOGIOR_SYMBOL: &str = "nod_logior";
pub const NOD_LOGXOR_SYMBOL: &str = "nod_logxor";
pub const NOD_LOGNOT_SYMBOL: &str = "nod_lognot";
pub const NOD_ASH_SYMBOL: &str = "nod_ash";

/// Sprint 20b: `(dylan-name-as-emitted-by-lower, runtime-symbol, arity)`.
/// The lower pass emits the LHS name as the DirectCall callee; codegen
/// matches it here and emits a call into the RHS extern.
const SPRINT_20B_PRIMITIVES: &[(&str, &str, usize)] = &[
    ("nod_collection_size", NOD_COLLECTION_SIZE_SYMBOL, 1),
    ("nod_collection_concatenate", NOD_COLLECTION_CONCATENATE_SYMBOL, 2),
    ("nod_range_from", NOD_RANGE_FROM_SYMBOL, 1),
    ("nod_range_to", NOD_RANGE_TO_SYMBOL, 1),
    ("nod_range_by", NOD_RANGE_BY_SYMBOL, 1),
    ("nod_sov_size", NOD_SOV_SIZE_SYMBOL, 1),
    ("nod_sov_element", NOD_SOV_ELEMENT_SYMBOL, 2),
    ("nod_sov_element_setter", NOD_SOV_ELEMENT_SETTER_SYMBOL, 3),
    ("nod_stretchy_vector_size", NOD_STRETCHY_VECTOR_SIZE_SYMBOL, 1),
    ("nod_stretchy_vector_element", NOD_STRETCHY_VECTOR_ELEMENT_SYMBOL, 2),
    (
        "nod_stretchy_vector_element_setter",
        NOD_STRETCHY_VECTOR_ELEMENT_SETTER_SYMBOL,
        3,
    ),
    ("nod_stretchy_vector_push", NOD_STRETCHY_VECTOR_PUSH_SYMBOL, 2),
    ("nod_fip_init", NOD_FIP_INIT_SYMBOL, 1),
    ("nod_fip_finished_p", NOD_FIP_FINISHED_P_SYMBOL, 1),
    ("nod_fip_current_element", NOD_FIP_CURRENT_ELEMENT_SYMBOL, 1),
    ("nod_fip_advance", NOD_FIP_ADVANCE_SYMBOL, 1),
    ("nod_make_range", NOD_MAKE_RANGE_SYMBOL, 3),
    ("nod_make_stretchy_vector", NOD_MAKE_STRETCHY_VECTOR_SYMBOL, 1),
    // Sprint 21 — first-class function values.
    ("nod_make_function_ref", NOD_MAKE_FUNCTION_REF_SYMBOL, 2),
    ("nod_funcall0", NOD_FUNCALL0_SYMBOL, 1),
    ("nod_funcall1", NOD_FUNCALL1_SYMBOL, 2),
    ("nod_funcall2", NOD_FUNCALL2_SYMBOL, 3),
    ("nod_funcall3", NOD_FUNCALL3_SYMBOL, 4),
    ("nod_funcall4", NOD_FUNCALL4_SYMBOL, 5),
    ("nod_funcall5", NOD_FUNCALL5_SYMBOL, 6),
    ("nod_apply", NOD_APPLY_SYMBOL, 2),
    ("nod_make_sov_len", NOD_MAKE_SOV_LEN_SYMBOL, 1),
    // Sprint 22 — <table> + hashing.
    ("nod_make_table", NOD_MAKE_TABLE_SYMBOL, 1),
    ("nod_table_size", NOD_TABLE_SIZE_SYMBOL, 1),
    ("nod_table_element", NOD_TABLE_ELEMENT_SYMBOL, 2),
    ("nod_table_element_or_default", NOD_TABLE_ELEMENT_OR_DEFAULT_SYMBOL, 3),
    ("nod_table_element_setter", NOD_TABLE_ELEMENT_SETTER_SYMBOL, 3),
    ("nod_table_remove_key", NOD_TABLE_REMOVE_KEY_SYMBOL, 2),
    ("nod_table_keys", NOD_TABLE_KEYS_SYMBOL, 1),
    ("nod_table_values", NOD_TABLE_VALUES_SYMBOL, 1),
    ("nod_object_hash", NOD_OBJECT_HASH_SYMBOL, 1),
    ("nod_object_equal_p", NOD_OBJECT_EQUAL_P_SYMBOL, 2),
    ("nod_op_pow", NOD_OP_POW_SYMBOL, 2),
    ("nod_subtype_p", NOD_SUBTYPE_P_SYMBOL, 2),
    ("nod_instance_p", NOD_INSTANCE_P_SYMBOL, 2),
    // Sprint 42a — <byte-string> primitives.
    ("nod_byte_string_allocate", NOD_BYTE_STRING_ALLOCATE_SYMBOL, 1),
    ("nod_byte_string_size", NOD_BYTE_STRING_SIZE_SYMBOL, 1),
    ("nod_byte_string_element", NOD_BYTE_STRING_ELEMENT_SYMBOL, 2),
    ("nod_byte_string_element_setter", NOD_BYTE_STRING_ELEMENT_SETTER_SYMBOL, 3),
    ("nod_byte_string_copy_bytes", NOD_BYTE_STRING_COPY_BYTES_SYMBOL, 5),
    ("nod_char_code", NOD_CHAR_CODE_SYMBOL, 1),
    ("nod_code_char", NOD_CODE_CHAR_SYMBOL, 1),
    // Collection-classes lever (Part A2) — <bit-vector> + word bitwise ops.
    ("nod_bit_vector_allocate", NOD_BIT_VECTOR_ALLOCATE_SYMBOL, 2),
    ("nod_bit_vector_ref", NOD_BIT_VECTOR_REF_SYMBOL, 2),
    ("nod_bit_vector_set", NOD_BIT_VECTOR_SET_SYMBOL, 3),
    ("nod_bit_vector_size", NOD_BIT_VECTOR_SIZE_SYMBOL, 1),
    ("nod_bit_vector_count", NOD_BIT_VECTOR_COUNT_SYMBOL, 1),
    ("nod_logand", NOD_LOGAND_SYMBOL, 2),
    ("nod_logior", NOD_LOGIOR_SYMBOL, 2),
    ("nod_logxor", NOD_LOGXOR_SYMBOL, 2),
    ("nod_lognot", NOD_LOGNOT_SYMBOL, 1),
    ("nod_ash", NOD_ASH_SYMBOL, 2),
    // Sprint 55 — generic-name + class-name classifiers for the Dylan shim.
    ("nod_is_generic_defined", NOD_IS_GENERIC_DEFINED_SYMBOL, 1),
    ("nod_is_class_defined", NOD_IS_CLASS_DEFINED_SYMBOL, 1),
    // Sprint 24 — closures.
    ("nod_make_cell", NOD_MAKE_CELL_SYMBOL, 1),
    ("nod_cell_get", NOD_CELL_GET_SYMBOL, 1),
    ("nod_cell_set", NOD_CELL_SET_SYMBOL, 2),
    ("nod_env_cell", NOD_ENV_CELL_SYMBOL, 2),
    ("nod_make_environment", NOD_MAKE_ENVIRONMENT_SYMBOL, 1),
    ("nod_make_closure", NOD_MAKE_CLOSURE_SYMBOL, 3),
    ("nod_make_rest_closure", NOD_MAKE_REST_CLOSURE_SYMBOL, 3),
    // Sprint 47 — multi-value return secondary-values buffer (GAP-003).
    ("nod_values_clear", NOD_VALUES_CLEAR_SYMBOL, 0),
    ("nod_values_set", NOD_VALUES_SET_SYMBOL, 2),
    ("nod_values_get", NOD_VALUES_GET_SYMBOL, 1),
    ("nod_values_count", NOD_VALUES_COUNT_SYMBOL, 0),
    // GAP-004 — `define variable` shims (name-by-byte-string).
    ("nod_var_get_by_name", NOD_VAR_GET_BY_NAME_SYMBOL, 1),
    ("nod_var_set_by_name", NOD_VAR_SET_BY_NAME_SYMBOL, 2),
    // Sprint 28 — Win64 FFI trampolines. Arity here is the trampoline's
    // C-ABI arity (entry-pointer + user args), so `nod_winffi_call_N`
    // entry takes `N + 1` Dylan-side args.
    ("nod_winffi_call_0", NOD_WINFFI_CALL_0_SYMBOL, 1),
    ("nod_winffi_call_1", NOD_WINFFI_CALL_1_SYMBOL, 2),
    ("nod_winffi_call_2", NOD_WINFFI_CALL_2_SYMBOL, 3),
    ("nod_winffi_call_3", NOD_WINFFI_CALL_3_SYMBOL, 4),
    ("nod_winffi_call_4", NOD_WINFFI_CALL_4_SYMBOL, 5),
    ("nod_winffi_call_5", NOD_WINFFI_CALL_5_SYMBOL, 6),
    ("nod_winffi_call_6", NOD_WINFFI_CALL_6_SYMBOL, 7),
    ("nod_winffi_call_7", NOD_WINFFI_CALL_7_SYMBOL, 8),
    ("nod_winffi_call_8", NOD_WINFFI_CALL_8_SYMBOL, 9),
    ("nod_winffi_call_9", NOD_WINFFI_CALL_9_SYMBOL, 10),
    ("nod_winffi_call_10", NOD_WINFFI_CALL_10_SYMBOL, 11),
    ("nod_winffi_call_11", NOD_WINFFI_CALL_11_SYMBOL, 12),
    ("nod_winffi_call_12", NOD_WINFFI_CALL_12_SYMBOL, 13),
    // Sprint 32 — closure-to-C-callback trampoline registration.
    ("nod_register_wndproc", NOD_REGISTER_WNDPROC_SYMBOL, 1),
    ("nod_register_wndenumproc", NOD_REGISTER_WNDENUMPROC_SYMBOL, 1),
    // Sprint 34 — <c-struct> field accessors. Each get/set is (s, offset)
    // / (value, s, offset), with `offset` baked as a plain fixnum literal
    // by the stdlib accessor body.
    ("nod_struct_get_i32", NOD_STRUCT_GET_I32_SYMBOL, 2),
    ("nod_struct_set_i32", NOD_STRUCT_SET_I32_SYMBOL, 3),
    ("nod_struct_get_i64", NOD_STRUCT_GET_I64_SYMBOL, 2),
    ("nod_struct_set_i64", NOD_STRUCT_SET_I64_SYMBOL, 3),
    ("nod_struct_get_u16", NOD_STRUCT_GET_U16_SYMBOL, 2),
    ("nod_struct_set_u16", NOD_STRUCT_SET_U16_SYMBOL, 3),
    ("nod_struct_get_u32", NOD_STRUCT_GET_U32_SYMBOL, 2),
    ("nod_struct_set_u32", NOD_STRUCT_SET_U32_SYMBOL, 3),
    ("nod_struct_get_u64", NOD_STRUCT_GET_U64_SYMBOL, 2),
    ("nod_struct_set_u64", NOD_STRUCT_SET_U64_SYMBOL, 3),
    ("nod_struct_get_pointer", NOD_STRUCT_GET_POINTER_SYMBOL, 2),
    ("nod_struct_set_pointer", NOD_STRUCT_SET_POINTER_SYMBOL, 3),
    // Sprint 35 — COM shim primitives. All take and return integer-shaped
    // u64 handles (see nod-runtime::com_shim module docs for the integer-
    // encoded color/coordinate convention). Each arity matches the C-ABI
    // signature: zero `nod_*_release()` style probes take no args, every
    // other shim takes 1..=7 args.
    ("nod_com_release", NOD_COM_RELEASE_SYMBOL, 1),
    ("nod_com_registry_len", NOD_COM_REGISTRY_LEN_SYMBOL, 0),
    ("nod_com_last_hresult", NOD_COM_LAST_HRESULT_SYMBOL, 0),
    ("nod_com_clear_last_hresult", NOD_COM_CLEAR_LAST_HRESULT_SYMBOL, 0),
    ("nod_dxgi_create_factory", NOD_DXGI_CREATE_FACTORY_SYMBOL, 0),
    ("nod_dxgi_device_from_d3d_device", NOD_DXGI_DEVICE_FROM_D3D_DEVICE_SYMBOL, 1),
    ("nod_dxgi_create_surface_from_texture", NOD_DXGI_CREATE_SURFACE_FROM_TEXTURE_SYMBOL, 1),
    ("nod_d3d11_create_device", NOD_D3D11_CREATE_DEVICE_SYMBOL, 0),
    ("nod_d3d11_get_immediate_context", NOD_D3D11_GET_IMMEDIATE_CONTEXT_SYMBOL, 1),
    ("nod_d3d11_create_texture_2d", NOD_D3D11_CREATE_TEXTURE_2D_SYMBOL, 4),
    ("nod_d3d11_copy_to_staging_and_map", NOD_D3D11_COPY_TO_STAGING_AND_MAP_SYMBOL, 5),
    ("nod_d3d11_last_staging_handle", NOD_D3D11_LAST_STAGING_HANDLE_SYMBOL, 0),
    ("nod_d3d11_last_mapped_row_pitch", NOD_D3D11_LAST_MAPPED_ROW_PITCH_SYMBOL, 0),
    ("nod_d3d11_unmap", NOD_D3D11_UNMAP_SYMBOL, 2),
    ("nod_d2d_create_factory", NOD_D2D_CREATE_FACTORY_SYMBOL, 0),
    ("nod_d2d_create_device", NOD_D2D_CREATE_DEVICE_SYMBOL, 2),
    ("nod_d2d_create_device_context", NOD_D2D_CREATE_DEVICE_CONTEXT_SYMBOL, 1),
    ("nod_d2d_create_bitmap_for_target", NOD_D2D_CREATE_BITMAP_FOR_TARGET_SYMBOL, 2),
    ("nod_d2d_set_target", NOD_D2D_SET_TARGET_SYMBOL, 2),
    ("nod_d2d_begin_draw", NOD_D2D_BEGIN_DRAW_SYMBOL, 1),
    ("nod_d2d_end_draw", NOD_D2D_END_DRAW_SYMBOL, 1),
    ("nod_d2d_clear", NOD_D2D_CLEAR_SYMBOL, 5),
    ("nod_d2d_set_transform_identity", NOD_D2D_SET_TRANSFORM_IDENTITY_SYMBOL, 1),
    ("nod_d2d_create_solid_color_brush", NOD_D2D_CREATE_SOLID_COLOR_BRUSH_SYMBOL, 5),
    ("nod_d2d_draw_text_layout", NOD_D2D_DRAW_TEXT_LAYOUT_SYMBOL, 5),
    ("nod_d2d_draw_rectangle", NOD_D2D_DRAW_RECTANGLE_SYMBOL, 7),
    ("nod_d2d_fill_rectangle", NOD_D2D_FILL_RECTANGLE_SYMBOL, 6),
    ("nod_dwrite_create_factory", NOD_DWRITE_CREATE_FACTORY_SYMBOL, 0),
    ("nod_dwrite_create_text_format", NOD_DWRITE_CREATE_TEXT_FORMAT_SYMBOL, 4),
    ("nod_dwrite_create_text_layout", NOD_DWRITE_CREATE_TEXT_LAYOUT_SYMBOL, 5),
    ("nod_dwrite_get_layout_metrics", NOD_DWRITE_GET_LAYOUT_METRICS_SYMBOL, 1),
    ("nod_dwrite_hit_test_text_position", NOD_DWRITE_HIT_TEST_TEXT_POSITION_SYMBOL, 3),
    ("nod_dwrite_hit_test_point", NOD_DWRITE_HIT_TEST_POINT_SYMBOL, 3),
    ("nod_dwrite_set_drawing_effect", NOD_DWRITE_SET_DRAWING_EFFECT_SYMBOL, 4),
    ("nod_dwrite_set_line_spacing", NOD_DWRITE_SET_LINE_SPACING_SYMBOL, 3),
    ("nod_count_non_zero_red", NOD_COUNT_NON_ZERO_RED_SYMBOL, 4),
    // Sprint 36 — HWND-bound swap chain + window-class registration helpers.
    // All fixnum in / fixnum out; see com_shim.rs for the C-ABI shapes.
    ("nod_dxgi_factory_from_d3d_device", NOD_DXGI_FACTORY_FROM_D3D_DEVICE_SYMBOL, 1),
    ("nod_dxgi_create_swap_chain_for_hwnd", NOD_DXGI_CREATE_SWAP_CHAIN_FOR_HWND_SYMBOL, 5),
    ("nod_d2d_create_bitmap_from_swap_chain", NOD_D2D_CREATE_BITMAP_FROM_SWAP_CHAIN_SYMBOL, 2),
    ("nod_dxgi_swap_chain_present", NOD_DXGI_SWAP_CHAIN_PRESENT_SYMBOL, 1),
    ("nod_dxgi_swap_chain_resize_buffers", NOD_DXGI_SWAP_CHAIN_RESIZE_BUFFERS_SYMBOL, 3),
    ("nod_register_window_class", NOD_REGISTER_WINDOW_CLASS_SYMBOL, 2),
    ("nod_create_message_only_window", NOD_CREATE_MESSAGE_ONLY_WINDOW_SYMBOL, 1),
    ("nod_create_hidden_window", NOD_CREATE_HIDDEN_WINDOW_SYMBOL, 1),
    ("nod_destroy_window", NOD_DESTROY_WINDOW_SYMBOL, 1),
    ("nod_post_message", NOD_POST_MESSAGE_SYMBOL, 4),
    ("nod_pump_one_message", NOD_PUMP_ONE_MESSAGE_SYMBOL, 1),
    // Sprint 41a — blocking message loop. Arity-0 in the C-ABI sense
    // (no Dylan-side args, no HWND filter) matches the canonical Win32
    // C idiom that blocks on the whole thread's queue.
    ("nod_run_message_loop", NOD_RUN_MESSAGE_LOOP_SYMBOL, 0),
    ("nod_def_window_proc", NOD_DEF_WINDOW_PROC_SYMBOL, 4),
    // Sprint 41b — IDE source-viewer primitives. Arity matches the
    // C-ABI shim signatures: `nod_read_file_to_string(path_word)`
    // takes the `<byte-string>` Word and returns a Word; `nod_get_argv1`
    // takes no args and returns a Word.
    ("nod_read_file_to_string", NOD_READ_FILE_TO_STRING_SYMBOL, 1),
    ("nod_get_argv1", NOD_GET_ARGV1_SYMBOL, 0),
    ("nod_get_argv2", NOD_GET_ARGV2_SYMBOL, 0),
    ("nod_print_gc_stats", NOD_PRINT_GC_STATS_SYMBOL, 0),
    ("nod_lo_word", NOD_LO_WORD_SYMBOL, 1),
    ("nod_hi_word", NOD_HI_WORD_SYMBOL, 1),
    // Sprint 41c — scrollbar primitives. Arity matches the Rust shim:
    // SetScrollInfo flattened to 7 u64 args; GetScrollPos takes (hwnd, nbar).
    ("nod_set_scroll_info", NOD_SET_SCROLL_INFO_SYMBOL, 7),
    ("nod_get_scroll_pos", NOD_GET_SCROLL_POS_SYMBOL, 2),
    // Sprint 42a Phase E retired five IDE shims (`nod_count_newlines`,
    // `nod_max_line_chars`, `nod_load_recent`, `nod_add_recent`,
    // `nod_basename`) — those are now pure Dylan in nod-ide.dylan
    // over the byte-string ops and the file-I/O primitives.
    // Sprint 41e — File → Open dialog shim.
    ("nod_show_open_file_dialog", NOD_SHOW_OPEN_FILE_DIALOG_SYMBOL, 1),
    // Sprint 41g — File → Save / Save As shims. `nod_write_file_from_string`
    // is the byte-string-to-file writer; `nod_show_save_file_dialog`
    // mirrors the open-dialog shim but calls `GetSaveFileNameW`.
    ("nod_write_file_from_string", NOD_WRITE_FILE_FROM_STRING_SYMBOL, 2),
    ("nod_show_save_file_dialog", NOD_SHOW_SAVE_FILE_DIALOG_SYMBOL, 1),
];

fn sprint_20b_primitive(name: &str) -> Option<(&'static str, usize)> {
    SPRINT_20B_PRIMITIVES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, sym, ar)| (*sym, *ar))
}

pub type FunctionMap<'ctx> = HashMap<String, FunctionValue<'ctx>>;

/// Sprint 38b — module-level codegen state shared by every emitter.
/// Holds the per-module cache key (for symbol naming) and the manifest
/// builder (every emitted external global registers one [`RelocKind`]
/// row here). Lives inside [`CodegenOutput`] so the caller can serialise
/// the manifest next to the bitcode at cold-compile time.
///
/// The manifest is wrapped in a [`RefCell`] because the emitters that
/// allocate external globals (e.g. inside `retag_bool`) only have
/// `&self` access — we can't take `&mut Emit` through the call graph
/// without restructuring every primop path. The borrow is always brief
/// (push one entry, drop) so dynamic-borrow checking is fine in practice.
pub(crate) struct ModuleCodegenCtx {
    pub key: CacheKey,
    pub manifest: RefCell<ModuleManifest>,
    safepoint_sites: RefCell<Vec<EmittedSafepointSite>>,
    install_surface: SafepointInstallSurface,
    /// Sprint 38c — per-module content-keyed dedup table for string
    /// literals. The first time a given UTF-8 text is referenced, we
    /// assign it the next sequential index (used to namespace its
    /// external global `@nod_strlit__<key>__<idx>`); subsequent
    /// references to the same text reuse that index, so the IR has
    /// exactly one external global per distinct literal per module.
    pub string_lit_idx: RefCell<HashMap<String, u32>>,
    /// Sprint 38c — same shape for `<symbol>` literals.
    pub symbol_lit_idx: RefCell<HashMap<String, u32>>,
    /// Sprint 38d — per-module content-keyed dedup table for Win32
    /// stub-entry pointers. Key is `(dll.to_lowercase(), symbol)` so
    /// case differences in DLL names don't fragment the dedup (Win32
    /// DLL names are case-insensitive). Symbol names are case-sensitive
    /// and matched verbatim. The value is the per-module index used to
    /// namespace the external global `@nod_stub__<key>__<idx>`.
    pub stub_entry_idx: RefCell<HashMap<(String, String), u32>>,
    /// Sprint 38e — per-module dedup record for inline-cache dispatch
    /// slots. Keyed by `site_id` (the codegen's stable per-MODULE
    /// counter in `next_safepoint_site_id` below); the value is unit
    /// because the external global's name already encodes the site_id
    /// directly (`@nod_cache_slot__<key>__<site_id>`). The set tracks
    /// which site_ids have already had their external global declared
    /// in this module so a second dispatch site reusing the same
    /// site_id (which can't happen today but could under future
    /// refactors) doesn't double-emit the manifest row.
    pub cache_slot_seen: RefCell<HashSet<u64>>,
    /// Sprint 38e — per-module dedup table for generic-function
    /// pointers, keyed by generic name. Multiple dispatch sites in the
    /// same module that target the same generic share one external
    /// global and one manifest row. The name itself encodes the
    /// identity (`@nod_generic__<key>__<sanitised-name>`); the set
    /// tracks first-seen.
    pub generic_function_seen: RefCell<HashSet<String>>,
    /// Sprint 38e / 45c — module-wide monotonic counter for
    /// call-shaped safepoint site IDs.
    ///
    /// **Pre-Sprint-38e this counter lived per-function** (an `Emit`
    /// field), which was fine because the cache slot's address was
    /// baked into IR as an i64 constant minted by
    /// `allocate_cache_slot(site_id)` — distinct functions calling
    /// `allocate_cache_slot(0)` got distinct static-area addresses
    /// (the allocator doesn't dedupe by site_id), so collisions on
    /// `site_id == 0` between functions were harmless.
    ///
    /// **Sprint 38e** routes the cache slot lookup through a
    /// per-module symbol `@nod_cache_slot__<key>__<site_id>` whose
    /// JIT-link address is `cache_slot_slot_addr(site_id)`. That slot
    /// allocator DOES dedupe by site_id (process-globally), so two
    /// functions in the same module each using `site_id == 0` would
    /// share the same `CacheSlot` — semantically wrong (cross-talk
    /// between unrelated call sites would scramble the inline cache).
    ///
    /// Promoting the counter to module scope makes every dispatch site
    /// in a module distinct, restoring the pre-Sprint-38e invariant
    /// that no two sites share a `CacheSlot`. Cross-module collisions
    /// are already prevented by the per-module `<key>` prefix in the
    /// symbol name.
    pub next_safepoint_site_id: RefCell<u64>,
}

impl ModuleCodegenCtx {
    fn new(key: CacheKey, install_surface: SafepointInstallSurface) -> Self {
        Self {
            key,
            manifest: RefCell::new(ModuleManifest::new(key)),
            safepoint_sites: RefCell::new(Vec::new()),
            install_surface,
            string_lit_idx: RefCell::new(HashMap::new()),
            symbol_lit_idx: RefCell::new(HashMap::new()),
            stub_entry_idx: RefCell::new(HashMap::new()),
            cache_slot_seen: RefCell::new(HashSet::new()),
            generic_function_seen: RefCell::new(HashSet::new()),
            next_safepoint_site_id: RefCell::new(0),
        }
    }

    /// Sprint 38e / 45c — mint the next module-wide unique safepoint
    /// site id. Dispatch consumes these ids for cache-slot identity;
    /// other call-shaped safepoints currently use them only as stable
    /// emitted/debug handles until runtime stack maps land.
    pub fn next_safepoint_site_id(&self) -> u64 {
        let mut id = self.next_safepoint_site_id.borrow_mut();
        let v = *id;
        *id += 1;
        v
    }

    /// Sprint 38c — look up or assign the per-module index for a
    /// `<byte-string>` literal. The returned index is stable across
    /// repeated calls with the same `text` in the same module.
    pub fn string_lit_index(&self, text: &str) -> u32 {
        let mut map = self.string_lit_idx.borrow_mut();
        if let Some(&idx) = map.get(text) {
            return idx;
        }
        let idx = map.len() as u32;
        map.insert(text.to_string(), idx);
        idx
    }

    /// Sprint 38c — look up or assign the per-module index for a
    /// `<symbol>` literal.
    pub fn symbol_lit_index(&self, name: &str) -> u32 {
        let mut map = self.symbol_lit_idx.borrow_mut();
        if let Some(&idx) = map.get(name) {
            return idx;
        }
        let idx = map.len() as u32;
        map.insert(name.to_string(), idx);
        idx
    }

    /// Sprint 38d — look up or assign the per-module index for a Win32
    /// stub-entry pointer. The key is `(dll.to_lowercase(), symbol)` —
    /// see `stub_entry_idx`'s doc comment for the case-sensitivity
    /// rationale.
    pub fn stub_entry_index(&self, dll: &str, symbol: &str) -> u32 {
        let key = (dll.to_lowercase(), symbol.to_string());
        let mut map = self.stub_entry_idx.borrow_mut();
        if let Some(&idx) = map.get(&key) {
            return idx;
        }
        let idx = map.len() as u32;
        map.insert(key, idx);
        idx
    }
}

/// Sprint 38b — look up (or create) a named external global for one
/// of the four immediate-singleton Words (`#t`, `#f`, `nil`, the
/// untagged `#f` wrapper). The global has type `i64` and is marked
/// external + externally-initialized so MCJIT treats it as a relocation
/// target. The caller `build_load`s through it to recover the runtime
/// value; the JIT-link layer (`Jit::add_module_from_bitcode` and the
/// cold path's symbol registration) maps the symbol to the actual
/// in-process address before MCJIT finalises.
///
/// Idempotent: a second call with the same `kind` returns the existing
/// global without duplicating the manifest row.
fn get_or_add_imm_global<'ctx>(
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    mctx: &ModuleCodegenCtx,
    kind: RelocKind,
) -> GlobalValue<'ctx> {
    let symbol = match kind {
        RelocKind::ImmTrue => imm_true_symbol(mctx.key),
        RelocKind::ImmFalse => imm_false_symbol(mctx.key),
        RelocKind::ImmNil => imm_nil_symbol(mctx.key),
        RelocKind::ImmFalseWrapper => imm_false_wrapper_symbol(mctx.key),
        _ => unreachable!("get_or_add_imm_global called with non-immediate kind: {kind:?}"),
    };
    if let Some(g) = module.get_global(&symbol) {
        return g;
    }
    let i64_ty = ctx.i64_type();
    let g = module.add_global(i64_ty, Some(inkwell::AddressSpace::default()), &symbol);
    g.set_linkage(Linkage::External);
    g.set_externally_initialized(true);
    // Push one row to the manifest. The JIT-link path uses the kind to
    // recompute the address against the current process's runtime
    // state; cold-compile in-process binding uses the same path
    // (see `Jit::add_module`'s Sprint 38b binding loop).
    mctx.manifest.borrow_mut().push(symbol, kind);
    g
}

/// Sprint 38c — look up (or create) the per-module external global for
/// a class-metadata pointer keyed by `class_id`. The global's value at
/// runtime (after JIT-link mapping) is a `u64` slot whose contents are
/// `class_metadata_ptr(class_id) as u64` (the raw, untagged metadata
/// pointer in the current process). Codegen `load`s from the global
/// to recover the pointer; the OR-with-1 pointer-tag (if needed) is
/// applied at the use site.
///
/// Idempotent — same `class_id` reuses the same global and registers
/// only one manifest row per module.
fn get_or_add_class_metadata_global<'ctx>(
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    mctx: &ModuleCodegenCtx,
    class_id: u32,
) -> GlobalValue<'ctx> {
    let symbol = class_md_symbol(mctx.key, class_id);
    if let Some(g) = module.get_global(&symbol) {
        return g;
    }
    let i64_ty = ctx.i64_type();
    let g = module.add_global(i64_ty, Some(inkwell::AddressSpace::default()), &symbol);
    g.set_linkage(Linkage::External);
    g.set_externally_initialized(true);
    mctx.manifest
        .borrow_mut()
        .push(symbol, RelocKind::ClassMetadata { class_id });
    g
}

/// Sprint 38c — per-module external global for an interned
/// `<byte-string>` literal. The slot's contents are the Word's raw
/// bits (tagged) in the current process.
///
/// `idx` is the per-module content-keyed index assigned by
/// `ModuleCodegenCtx::string_lit_index`. Calls within one module
/// dedup on `text` so the global is created exactly once.
fn get_or_add_string_literal_global<'ctx>(
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    mctx: &ModuleCodegenCtx,
    idx: u32,
    text: &str,
) -> GlobalValue<'ctx> {
    let symbol = strlit_symbol(mctx.key, idx);
    if let Some(g) = module.get_global(&symbol) {
        return g;
    }
    let i64_ty = ctx.i64_type();
    let g = module.add_global(i64_ty, Some(inkwell::AddressSpace::default()), &symbol);
    g.set_linkage(Linkage::External);
    g.set_externally_initialized(true);
    mctx.manifest.borrow_mut().push(
        symbol,
        RelocKind::StringLiteral {
            text: text.to_string(),
        },
    );
    g
}

/// Sprint 38c — per-module external global for an interned `<symbol>`
/// literal.
fn get_or_add_symbol_literal_global<'ctx>(
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    mctx: &ModuleCodegenCtx,
    idx: u32,
    name: &str,
) -> GlobalValue<'ctx> {
    let symbol = symlit_symbol(mctx.key, idx);
    if let Some(g) = module.get_global(&symbol) {
        return g;
    }
    let i64_ty = ctx.i64_type();
    let g = module.add_global(i64_ty, Some(inkwell::AddressSpace::default()), &symbol);
    g.set_linkage(Linkage::External);
    g.set_externally_initialized(true);
    mctx.manifest.borrow_mut().push(
        symbol,
        RelocKind::SymbolLiteral {
            name: name.to_string(),
        },
    );
    g
}

/// Sprint 38d — per-module external global for a Win32 stub-entry
/// pointer. The slot's contents at JIT-link time are the `u64`-cast
/// address of an [`nod_runtime::ApiStubEntry`] freshly allocated in the
/// current process (via [`nod_runtime::stub_entry_slot_addr`]); the
/// loaded value is the entry pointer that `nod_winffi_call_N` takes
/// as its first argument.
///
/// `idx` is the per-module content-keyed index assigned by
/// `ModuleCodegenCtx::stub_entry_index`. Calls within one module dedup
/// on `(dll.to_lowercase(), symbol)` so the global is created exactly
/// once per distinct Win32 API. `signature_bytes` is bytewise-encoded
/// [`nod_runtime::ApiCallSignature`] and is carried in the manifest so
/// the warm-replay resolver can reconstruct the same marshaling shape
/// without redoing sema-side type analysis.
fn get_or_add_stub_entry_global<'ctx>(
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    mctx: &ModuleCodegenCtx,
    idx: u32,
    dll: &str,
    symbol: &str,
    signature_bytes: &[u8],
) -> GlobalValue<'ctx> {
    let sym = stub_symbol(mctx.key, idx);
    if let Some(g) = module.get_global(&sym) {
        return g;
    }
    let i64_ty = ctx.i64_type();
    let g = module.add_global(i64_ty, Some(inkwell::AddressSpace::default()), &sym);
    g.set_linkage(Linkage::External);
    g.set_externally_initialized(true);
    mctx.manifest.borrow_mut().push(
        sym,
        RelocKind::StubEntry {
            dll: dll.to_string(),
            symbol: symbol.to_string(),
            signature_bytes: signature_bytes.to_vec(),
        },
    );
    g
}

/// Sprint 38e — per-module external global for an inline-cache dispatch
/// slot. The slot's contents at JIT-link time are the `u64`-cast
/// address of a freshly-allocated (empty) [`nod_runtime::CacheSlot`] in
/// the current process's static area (via
/// [`nod_runtime::cache_slot_slot_addr`]); the loaded value is the
/// pointer codegen needs to derive the field-offset addresses (class /
/// method / generation / hits) for atomic loads + the slow-path
/// dispatch arg.
///
/// Idempotent: the second call for the same `site_id` reuses the
/// existing global and records no extra manifest row.
fn get_or_add_cache_slot_global<'ctx>(
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    mctx: &ModuleCodegenCtx,
    site_id: u64,
) -> GlobalValue<'ctx> {
    let sym = cache_slot_symbol(mctx.key, site_id);
    if let Some(g) = module.get_global(&sym) {
        return g;
    }
    let i64_ty = ctx.i64_type();
    let g = module.add_global(i64_ty, Some(inkwell::AddressSpace::default()), &sym);
    g.set_linkage(Linkage::External);
    g.set_externally_initialized(true);
    // Only emit the manifest row on first sight of this site_id.
    if mctx.cache_slot_seen.borrow_mut().insert(site_id) {
        mctx.manifest
            .borrow_mut()
            .push(sym, RelocKind::CacheSlot { site_id });
    }
    g
}

/// Sprint 38e — per-module external global for a `GenericFunction`
/// pointer keyed by generic name. The slot's contents at JIT-link time
/// are the `u64`-cast address of the `&'static GenericFunction`
/// returned by [`nod_runtime::get_or_create_generic`]; the loaded
/// value is the generic pointer codegen needs to derive the
/// `generation` field address (via `add i64`).
///
/// Multiple dispatch sites in the same module that target the same
/// generic share one external global and one manifest row — dedup is
/// per-name (case-sensitive, matching Dylan's binding rules).
fn get_or_add_generic_function_global<'ctx>(
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    mctx: &ModuleCodegenCtx,
    name: &str,
) -> GlobalValue<'ctx> {
    let sym = generic_symbol(mctx.key, name);
    if let Some(g) = module.get_global(&sym) {
        return g;
    }
    let i64_ty = ctx.i64_type();
    let g = module.add_global(i64_ty, Some(inkwell::AddressSpace::default()), &sym);
    g.set_linkage(Linkage::External);
    g.set_externally_initialized(true);
    // Only emit the manifest row on first sight of this name.
    if mctx
        .generic_function_seen
        .borrow_mut()
        .insert(name.to_string())
    {
        mctx.manifest.borrow_mut().push(
            sym,
            RelocKind::Generic {
                name: name.to_string(),
            },
        );
    }
    g
}

pub struct CodegenOutput<'ctx> {
    pub module: Module<'ctx>,
    pub function_map: FunctionMap<'ctx>,
    pub safepoint_namespace: u64,
    /// Sprint 45c — location-based safepoint planning surface.
    ///
    /// This is intentionally debug/introspection-first: it captures the
    /// per-callsite root-location plan codegen will eventually lower
    /// into installed-code safepoint maps. The active GC behaviour uses
    /// the precise slot-slab path (JIT or AOT) for both surfaces.
    pub safepoint_plans: Vec<SafepointPlan>,
    /// Canonical emitted-site descriptors for eventual install-time
    /// safepoint metadata writers. Unlike [`SafepointPlan`], this
    /// carries install-region identity so in-memory and image output
    /// can be distinguished without rebuilding that context.
    pub safepoint_installs: Vec<SafepointInstallRecord>,
    /// Sprint 38b — manifest of named-symbol → [`RelocKind`] rows for
    /// every external global this module references. Empty if no
    /// process-local addresses were materialised (e.g. a pure-arithmetic
    /// module with no booleans / nil / class-id reads).
    pub manifest: ModuleManifest,
}

pub(crate) const JIT_SAFEPOINT_METADATA_SYMBOL_PREFIX: &str = "nod_jit_safepoint__";

/// Location-based description of one safepoint in one function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafepointPlan {
    /// Module-order stable safepoint site id.
    ///
    /// Sprint 45c keeps this purely as a planning/debug identifier.
    /// Unlike dispatch-site ids, it is not yet baked into installed
    /// runtime metadata, but it gives tests and future codegen work a
    /// stable handle for a safepoint independent of block labels.
    pub site_id: u64,
    /// Codegen-owned placeholder anchor for the eventual installed
    /// safepoint/patchpoint identity. This is intentionally not a real
    /// PC yet; Sprint 45c uses a stable string handle so downstream
    /// metadata code can stop keying solely on block/computation pairs.
    pub patchpoint_label: String,
    pub kind: SafepointKind,
    pub function: String,
    pub block_label: String,
    pub computation_index: usize,
    pub roots: Vec<SafepointRootLocation>,
}

/// Where shared-codegen output is intended to be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeInstallSurface {
    InMemory,
    Image,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SafepointInstallKey {
    install_surface: SafepointInstallSurface,
    module_site_ordinal: u64,
}

impl SafepointInstallKey {
    fn site_id(self) -> u64 {
        self.module_site_ordinal
    }

    fn patchpoint_label(self) -> String {
        match self.install_surface {
            SafepointInstallSurface::InMemoryCodeText
            | SafepointInstallSurface::ImageCodeText => {
                safepoint_patchpoint_label(self.module_site_ordinal)
            }
        }
    }
}

/// Installation surface for one emitted safepoint site.
///
/// NewOpenDylan has one code generator; the distinction here is where
/// the resulting machine code is ultimately installed, not whether it
/// came from separate "JIT" and "AOT" compiler pipelines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SafepointInstallSurface {
    InMemoryCodeText,
    ImageCodeText,
}

impl From<CodeInstallSurface> for SafepointInstallSurface {
    fn from(value: CodeInstallSurface) -> Self {
        match value {
            CodeInstallSurface::InMemory => SafepointInstallSurface::InMemoryCodeText,
            CodeInstallSurface::Image => SafepointInstallSurface::ImageCodeText,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstalledTextRegionKind {
    InMemory,
    Image,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstalledTextRegion {
    pub kind: InstalledTextRegionKind,
    pub section_label: &'static str,
}

impl SafepointInstallSurface {
    fn installed_text_region(self) -> InstalledTextRegion {
        match self {
            SafepointInstallSurface::InMemoryCodeText => InstalledTextRegion {
                kind: InstalledTextRegionKind::InMemory,
                section_label: "mem.code.text",
            },
            SafepointInstallSurface::ImageCodeText => InstalledTextRegion {
                kind: InstalledTextRegionKind::Image,
                section_label: "image.code.text",
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EmittedSafepointSite {
    namespace: u64,
    install_key: SafepointInstallKey,
    installed_text_region: InstalledTextRegion,
    kind: SafepointKind,
    function: String,
    block_label: String,
    computation_index: usize,
    roots: Vec<SafepointRootLocation>,
}

fn emitted_safepoint_site(
    namespace: u64,
    install_key: SafepointInstallKey,
    kind: SafepointKind,
    function: String,
    block_label: String,
    computation_index: usize,
    roots: Vec<SafepointRootLocation>,
) -> EmittedSafepointSite {
    EmittedSafepointSite {
        namespace,
        installed_text_region: install_key.install_surface.installed_text_region(),
        install_key,
        kind,
        function,
        block_label,
        computation_index,
        roots,
    }
}

impl From<EmittedSafepointSite> for SafepointPlan {
    fn from(site: EmittedSafepointSite) -> Self {
        Self {
            site_id: site.install_key.site_id(),
            patchpoint_label: site.install_key.patchpoint_label(),
            kind: site.kind,
            function: site.function,
            block_label: site.block_label,
            computation_index: site.computation_index,
            roots: site.roots,
        }
    }
}

/// Canonical install-time descriptor for one emitted safepoint site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafepointInstallRecord {
    pub namespace: u64,
    pub site_id: u64,
    pub patchpoint_label: String,
    pub installed_text_region: InstalledTextRegion,
    pub kind: SafepointKind,
    pub function: String,
    pub block_label: String,
    pub computation_index: usize,
    pub roots: Vec<SafepointRootLocation>,
}

impl From<EmittedSafepointSite> for SafepointInstallRecord {
    fn from(site: EmittedSafepointSite) -> Self {
        Self {
            namespace: site.namespace,
            site_id: site.install_key.site_id(),
            patchpoint_label: site.install_key.patchpoint_label(),
            installed_text_region: site.installed_text_region,
            kind: site.kind,
            function: site.function,
            block_label: site.block_label,
            computation_index: site.computation_index,
            roots: site.roots,
        }
    }
}

/// Normalized codegen-facing category for an emitted safepoint site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafepointKind {
    DirectCall,
    Dispatch,
    SealedDirectCall,
}

#[derive(Debug)]
pub enum CodegenError {
    UnknownCallee { in_function: String, callee: String },
    IndirectCallNotSupported { in_function: String },
    Builder(String),
    /// Sprint 11 stub. The `WriteBarrier` IR node exists for Sprint 12+
    /// slot setters; no lowering path emits it today.
    WriteBarrierNotEmitted { in_function: String },
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodegenError::UnknownCallee { in_function, callee } => {
                write!(f, "codegen: unknown callee `{callee}` in function `{in_function}`")
            }
            CodegenError::IndirectCallNotSupported { in_function } => write!(
                f,
                "codegen: indirect Call IR node not supported in Sprint 07 \
                 (function `{in_function}`)"
            ),
            CodegenError::Builder(e) => write!(f, "codegen: builder: {e}"),
            CodegenError::WriteBarrierNotEmitted { in_function } => write!(
                f,
                "codegen: WriteBarrier IR node emitted but Sprint 11 has no \
                 lowering path (function `{in_function}`); Sprint 12+ wires it"
            ),
        }
    }
}

impl std::error::Error for CodegenError {}

pub fn codegen_module<'ctx>(
    ctx: &'ctx Context,
    fns: &[DfmFunction],
    module_name: &str,
) -> Result<CodegenOutput<'ctx>, CodegenError> {
    codegen_module_for_surface(ctx, fns, module_name, CodeInstallSurface::InMemory)
}

pub fn codegen_module_for_surface<'ctx>(
    ctx: &'ctx Context,
    fns: &[DfmFunction],
    module_name: &str,
    install_surface: CodeInstallSurface,
) -> Result<CodegenOutput<'ctx>, CodegenError> {
    // Sprint 38b — synthesise a deterministic CacheKey from the module
    // name + a digest of the function names. Callers that already have a
    // real cache key (the eval pipeline) use
    // [`codegen_module_with_key`] directly; this convenience wrapper
    // covers stdlib-load / dump-llvm / bench / run_function_to_i64
    // call sites that don't compute a key. Symbol namespacing only
    // requires a 16-char prefix; collision-resistance is satisfied as
    // long as distinct modules synthesize distinct keys.
    let synth_key = synth_cache_key_from_module(module_name, fns);
    codegen_module_with_key_for_surface(
        ctx,
        fns,
        module_name,
        synth_key,
        install_surface,
    )
}

/// Sprint 38b — the canonical codegen entry point. `key` namespaces
/// every emitted named external global via the symbol-naming scheme in
/// [`crate::symbols`], and seeds the [`ModuleManifest`] returned in
/// [`CodegenOutput::manifest`].
pub fn codegen_module_with_key<'ctx>(
    ctx: &'ctx Context,
    fns: &[DfmFunction],
    module_name: &str,
    key: CacheKey,
) -> Result<CodegenOutput<'ctx>, CodegenError> {
    codegen_module_with_key_for_surface(ctx, fns, module_name, key, CodeInstallSurface::InMemory)
}

pub fn codegen_module_with_key_for_surface<'ctx>(
    ctx: &'ctx Context,
    fns: &[DfmFunction],
    module_name: &str,
    key: CacheKey,
    install_surface: CodeInstallSurface,
) -> Result<CodegenOutput<'ctx>, CodegenError> {
    let module = ctx.create_module(module_name);
    let builder = ctx.create_builder();
    let mctx = ModuleCodegenCtx::new(key, install_surface.into());

    // Pass 1: forward-declare every function so direct calls can resolve
    // regardless of declaration order (handles mutual recursion).
    let mut function_map: FunctionMap<'ctx> = HashMap::new();
    for f in fns {
        let fty = function_type(ctx, f);
        let fv = module.add_function(&f.name, fty, None);
        function_map.insert(f.name.clone(), fv);
    }

    // Pass 2: emit each body.
    for f in fns {
        let fv = function_map[&f.name];
        emit_function(ctx, &module, &builder, &function_map, &mctx, f, fv)?;
    }

    let safepoint_sites = mctx.safepoint_sites.into_inner();
    if matches!(install_surface, CodeInstallSurface::InMemory) {
        emit_jit_safepoint_metadata_globals(&module, &safepoint_sites);
    }

    Ok(CodegenOutput {
        module,
        function_map,
        safepoint_namespace: mctx.key.0[0],
        safepoint_plans: safepoint_sites
            .clone()
            .into_iter()
            .map(SafepointPlan::from)
            .collect(),
        safepoint_installs: safepoint_sites
            .into_iter()
            .map(SafepointInstallRecord::from)
            .collect(),
        manifest: mctx.manifest.into_inner(),
    })
}

fn emit_jit_safepoint_metadata_globals<'ctx>(
    module: &Module<'ctx>,
    sites: &[EmittedSafepointSite],
) {
    let i8_ty = module.get_context().i8_type();
    for site in sites {
        let sym = jit_safepoint_metadata_symbol(
            site.namespace,
            site.install_key.site_id(),
            &site.roots,
        );
        if module.get_global(&sym).is_some() {
            continue;
        }
        let g = module.add_global(i8_ty, Some(inkwell::AddressSpace::default()), &sym);
        g.set_linkage(Linkage::Internal);
        g.set_initializer(&i8_ty.const_zero());
        g.set_constant(true);
    }
}

fn jit_safepoint_metadata_symbol(
    namespace: u64,
    site_id: u64,
    roots: &[SafepointRootLocation],
) -> String {
    let slots = if roots.is_empty() {
        "none".to_string()
    } else {
        roots
            .iter()
            .map(|root| match root.location {
                SafepointLocation::FrameSlot(slot_idx) => slot_idx.to_string(),
                SafepointLocation::SavedRegister(reg_idx) => format!("r{reg_idx}"),
            })
            .collect::<Vec<_>>()
            .join("_")
    };
    format!(
        "{JIT_SAFEPOINT_METADATA_SYMBOL_PREFIX}{namespace:016x}__{site_id}__{slots}"
    )
}

/// Sprint 45c — compute a location-based safepoint plan for each
/// call-shaped computation in `fns`.
///
/// This planner is intentionally narrow: it preserves today's
/// `safepoint_roots` live-set computation and assigns future-facing
/// frame-slot locations in root order. The active codegen path still
/// lowers through `begin_safepoint` / `end_safepoint`; this function
/// exists so tests and debug tooling can lock the new contract before
/// runtime GC behavior changes.
pub fn plan_safepoints(fns: &[DfmFunction]) -> Vec<SafepointPlan> {
    let mut out = Vec::new();
    let mut next_site_id = 0u64;
    for f in fns {
        for block in &f.blocks {
            for (computation_index, computation) in block.computations.iter().enumerate() {
                let Some(roots) = computation.safepoint_roots() else {
                    continue;
                };
                let Some(kind) = planned_safepoint_kind(computation) else {
                    continue;
                };
                let roots: Vec<SafepointRootLocation> = roots
                    .iter()
                    .enumerate()
                    .map(|(slot_idx, temp)| SafepointRootLocation {
                        temp: *temp,
                        location: SafepointLocation::FrameSlot(slot_idx as u32),
                    })
                    .collect();
                out.push(SafepointPlan::from(emitted_safepoint_site(
                    0,
                    SafepointInstallKey {
                        install_surface: SafepointInstallSurface::InMemoryCodeText,
                        module_site_ordinal: next_site_id,
                    },
                    kind,
                    f.name.clone(),
                    block.label.clone(),
                    computation_index,
                    roots,
                )));
                next_site_id += 1;
            }
        }
    }
    out
}

fn planned_safepoint_kind(computation: &Computation) -> Option<SafepointKind> {
    match computation {
        Computation::DirectCall { .. } => Some(SafepointKind::DirectCall),
        Computation::Dispatch { .. } => Some(SafepointKind::Dispatch),
        Computation::SealedDirectCall { .. } => Some(SafepointKind::SealedDirectCall),
        Computation::Call { .. } => Some(SafepointKind::DirectCall),
        _ => None,
    }
}

fn safepoint_patchpoint_label(site_id: u64) -> String {
    format!("gc.s{site_id}")
}

#[cfg(test)]
mod tests {
    use super::{
        codegen_module, codegen_module_for_surface, emitted_safepoint_site,
        plan_safepoints, CodeInstallSurface, InstalledTextRegion, InstalledTextRegionKind,
        NOD_JIT_BEGIN_SAFEPOINT_SYMBOL, NOD_JIT_END_SAFEPOINT_SYMBOL,
        NOD_AOT_BEGIN_SAFEPOINT_SYMBOL, NOD_AOT_END_SAFEPOINT_SYMBOL,
        NOD_AOT_VERIFY_SAFEPOINT_SYMBOL, SafepointInstallKey, SafepointInstallRecord,
        SafepointInstallSurface,
    };
    use inkwell::context::Context;
    use nod_dfm::{
        Block, BlockId, Computation, ConstValue, Function, FunctionId, SafepointLocation,
        SafepointRootLocation, Temporary, TempId, Terminator, TypeEstimate, FileId, Span,
    };
    use crate::codegen::SafepointKind;

    fn test_span() -> Span {
        Span::new(FileId(0), 0, 0)
    }

    #[test]
    fn plans_location_based_safepoints_per_callsite() {
        let f = Function {
            id: FunctionId(0),
            name: "two_calls".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![
                    Computation::Const {
                        dst: TempId(1),
                        value: ConstValue::Integer(7),
                    },
                    Computation::DirectCall {
                        dst: TempId(2),
                        callee: "alloc_a".to_string(),
                        args: vec![TempId(0)],
                        safepoint_roots: vec![TempId(0)],
                        is_no_alloc: false,
                    },
                    Computation::DirectCall {
                        dst: TempId(3),
                        callee: "alloc_b".to_string(),
                        args: vec![TempId(0), TempId(2)],
                        safepoint_roots: vec![TempId(0), TempId(2)],
                        is_no_alloc: false,
                    },
                ],
                terminator: Terminator::Return {
                    value: Some(TempId(3)),
                },
            }],
            temps: vec![
                Temporary {
                    id: TempId(0),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(1),
                    type_estimate: TypeEstimate::Integer,
                },
                Temporary {
                    id: TempId(2),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(3),
                    type_estimate: TypeEstimate::String,
                },
            ],
            return_type: TypeEstimate::String,
            span: test_span(),
        };

        let plans = plan_safepoints(&[f]);
        assert_eq!(plans.len(), 2);

        assert_eq!(plans[0].site_id, 0);
        assert_eq!(plans[0].patchpoint_label, "gc.s0");
        assert_eq!(plans[0].kind, SafepointKind::DirectCall);

        assert_eq!(plans[0].function, "two_calls");
        assert_eq!(plans[0].block_label, "entry");
        assert_eq!(plans[0].computation_index, 1);
        assert_eq!(plans[0].roots.len(), 1);
        assert_eq!(plans[0].roots[0].temp, TempId(0));
        assert_eq!(plans[0].roots[0].location, SafepointLocation::FrameSlot(0));

        assert_eq!(plans[1].site_id, 1);
        assert_eq!(plans[1].patchpoint_label, "gc.s1");
        assert_eq!(plans[1].kind, SafepointKind::DirectCall);
        assert_eq!(plans[1].computation_index, 2);
        assert_eq!(plans[1].roots.len(), 2);
        assert_eq!(plans[1].roots[0].temp, TempId(0));
        assert_eq!(plans[1].roots[0].location, SafepointLocation::FrameSlot(0));
        assert_eq!(plans[1].roots[1].temp, TempId(2));
        assert_eq!(plans[1].roots[1].location, SafepointLocation::FrameSlot(1));
    }

    #[test]
    fn plans_dispatch_safepoints_with_stable_site_ids() {
        let f = Function {
            id: FunctionId(1),
            name: "dispatch_site".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![
                    Computation::Dispatch {
                        dst: TempId(1),
                        generic_name: "g".to_string(),
                        args: vec![TempId(0)],
                        safepoint_roots: vec![TempId(0)],
                    },
                    Computation::DirectCall {
                        dst: TempId(2),
                        callee: "alloc_after_dispatch".to_string(),
                        args: vec![TempId(1)],
                        safepoint_roots: vec![TempId(0), TempId(1)],
                        is_no_alloc: false,
                    },
                ],
                terminator: Terminator::Return {
                    value: Some(TempId(2)),
                },
            }],
            temps: vec![
                Temporary {
                    id: TempId(0),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(1),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(2),
                    type_estimate: TypeEstimate::String,
                },
            ],
            return_type: TypeEstimate::String,
            span: test_span(),
        };

        let plans = plan_safepoints(&[f]);
        assert_eq!(plans.len(), 2);

        assert_eq!(plans[0].site_id, 0);
        assert_eq!(plans[0].patchpoint_label, "gc.s0");
        assert_eq!(plans[0].kind, SafepointKind::Dispatch);
        assert_eq!(plans[0].function, "dispatch_site");
        assert_eq!(plans[0].computation_index, 0);
        assert_eq!(plans[0].roots.len(), 1);
        assert_eq!(plans[0].roots[0].temp, TempId(0));
        assert_eq!(plans[0].roots[0].location, SafepointLocation::FrameSlot(0));

        assert_eq!(plans[1].site_id, 1);
        assert_eq!(plans[1].patchpoint_label, "gc.s1");
        assert_eq!(plans[1].kind, SafepointKind::DirectCall);
        assert_eq!(plans[1].computation_index, 1);
        assert_eq!(plans[1].roots.len(), 2);
        assert_eq!(plans[1].roots[0].temp, TempId(0));
        assert_eq!(plans[1].roots[0].location, SafepointLocation::FrameSlot(0));
        assert_eq!(plans[1].roots[1].temp, TempId(1));
        assert_eq!(plans[1].roots[1].location, SafepointLocation::FrameSlot(1));
    }

    #[test]
    fn emits_site_scoped_gc_markers_for_direct_and_dispatch_safepoints() {
        let callee = Function {
            id: FunctionId(1),
            name: "alloc_a".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![],
                terminator: Terminator::Return {
                    value: Some(TempId(0)),
                },
            }],
            temps: vec![Temporary {
                id: TempId(0),
                type_estimate: TypeEstimate::String,
            }],
            return_type: TypeEstimate::String,
            span: test_span(),
        };
        let caller = Function {
            id: FunctionId(0),
            name: "caller".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![
                    Computation::DirectCall {
                        dst: TempId(1),
                        callee: "alloc_a".to_string(),
                        args: vec![TempId(0)],
                        safepoint_roots: vec![TempId(0)],
                        is_no_alloc: false,
                    },
                    Computation::Dispatch {
                        dst: TempId(2),
                        generic_name: "g".to_string(),
                        args: vec![TempId(1)],
                        safepoint_roots: vec![TempId(0), TempId(1)],
                    },
                ],
                terminator: Terminator::Return {
                    value: Some(TempId(2)),
                },
            }],
            temps: vec![
                Temporary {
                    id: TempId(0),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(1),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(2),
                    type_estimate: TypeEstimate::String,
                },
            ],
            return_type: TypeEstimate::String,
            span: test_span(),
        };

        let ctx = Context::create();
        let out = codegen_module(&ctx, &[caller, callee], "safepoint_sites").expect("codegen ok");
        let ir = out.module.print_to_string().to_string();

        assert!(ir.contains("gc.s0.reload.t0"), "missing direct-call reload marker: {ir}");
        assert!(ir.contains("disp.s1.fast_call"), "missing dispatch site block label: {ir}");
        assert!(ir.contains("gc.s1.reload.t1"), "missing dispatch reload marker: {ir}");
    }

    #[test]
    fn codegen_output_reports_emitted_safepoint_plans() {
        let callee = Function {
            id: FunctionId(1),
            name: "alloc_a".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![],
                terminator: Terminator::Return {
                    value: Some(TempId(0)),
                },
            }],
            temps: vec![Temporary {
                id: TempId(0),
                type_estimate: TypeEstimate::String,
            }],
            return_type: TypeEstimate::String,
            span: test_span(),
        };
        let caller = Function {
            id: FunctionId(0),
            name: "caller".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![
                    Computation::DirectCall {
                        dst: TempId(1),
                        callee: "alloc_a".to_string(),
                        args: vec![TempId(0)],
                        safepoint_roots: vec![TempId(0)],
                        is_no_alloc: false,
                    },
                    Computation::Dispatch {
                        dst: TempId(2),
                        generic_name: "g".to_string(),
                        args: vec![TempId(1)],
                        safepoint_roots: vec![TempId(0), TempId(1)],
                    },
                ],
                terminator: Terminator::Return {
                    value: Some(TempId(2)),
                },
            }],
            temps: vec![
                Temporary {
                    id: TempId(0),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(1),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(2),
                    type_estimate: TypeEstimate::String,
                },
            ],
            return_type: TypeEstimate::String,
            span: test_span(),
        };

        let ctx = Context::create();
        let out = codegen_module(&ctx, &[caller, callee], "safepoint_sites").expect("codegen ok");

        assert_eq!(out.safepoint_plans.len(), 2);
        assert_eq!(out.safepoint_plans[0].site_id, 0);
        assert_eq!(out.safepoint_plans[0].patchpoint_label, "gc.s0");
        assert_eq!(out.safepoint_plans[0].kind, SafepointKind::DirectCall);
        assert_eq!(out.safepoint_plans[0].function, "caller");
        assert_eq!(out.safepoint_plans[0].block_label, "entry");
        assert_eq!(out.safepoint_plans[0].computation_index, 0);
        assert_eq!(out.safepoint_plans[0].roots.len(), 1);
        assert_eq!(out.safepoint_plans[0].roots[0].temp, TempId(0));
        assert_eq!(out.safepoint_plans[0].roots[0].location, SafepointLocation::FrameSlot(0));

        assert_eq!(out.safepoint_plans[1].site_id, 1);
        assert_eq!(out.safepoint_plans[1].patchpoint_label, "gc.s1");
        assert_eq!(out.safepoint_plans[1].kind, SafepointKind::Dispatch);
        assert_eq!(out.safepoint_plans[1].function, "caller");
        assert_eq!(out.safepoint_plans[1].block_label, "entry");
        assert_eq!(out.safepoint_plans[1].computation_index, 1);
        assert_eq!(out.safepoint_plans[1].roots.len(), 2);
        assert_eq!(out.safepoint_plans[1].roots[0].temp, TempId(0));
        assert_eq!(out.safepoint_plans[1].roots[0].location, SafepointLocation::FrameSlot(0));
        assert_eq!(out.safepoint_plans[1].roots[1].temp, TempId(1));
        assert_eq!(out.safepoint_plans[1].roots[1].location, SafepointLocation::FrameSlot(1));
    }

    #[test]
    fn codegen_output_reports_surface_specific_safepoint_installs() {
        let callee = Function {
            id: FunctionId(1),
            name: "alloc_a".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![],
                terminator: Terminator::Return {
                    value: Some(TempId(0)),
                },
            }],
            temps: vec![Temporary {
                id: TempId(0),
                type_estimate: TypeEstimate::String,
            }],
            return_type: TypeEstimate::String,
            span: test_span(),
        };
        let caller = Function {
            id: FunctionId(0),
            name: "caller".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![Computation::DirectCall {
                    dst: TempId(1),
                    callee: "alloc_a".to_string(),
                    args: vec![TempId(0)],
                    safepoint_roots: vec![TempId(0)],
                    is_no_alloc: false,
                }],
                terminator: Terminator::Return {
                    value: Some(TempId(1)),
                },
            }],
            temps: vec![
                Temporary {
                    id: TempId(0),
                    type_estimate: TypeEstimate::String,
                },
                Temporary {
                    id: TempId(1),
                    type_estimate: TypeEstimate::String,
                },
            ],
            return_type: TypeEstimate::String,
            span: test_span(),
        };

        let ctx = Context::create();
        let mem_out = codegen_module_for_surface(
            &ctx,
            &[caller.clone(), callee.clone()],
            "safepoint_sites_mem",
            CodeInstallSurface::InMemory,
        )
        .expect("mem codegen ok");
        let image_out = codegen_module_for_surface(
            &ctx,
            &[caller, callee],
            "safepoint_sites_image",
            CodeInstallSurface::Image,
        )
        .expect("image codegen ok");

        assert_eq!(mem_out.safepoint_installs.len(), 1);
        assert_eq!(image_out.safepoint_installs.len(), 1);

        let mem_install: &SafepointInstallRecord = &mem_out.safepoint_installs[0];
        let image_install: &SafepointInstallRecord = &image_out.safepoint_installs[0];

        assert_eq!(mem_install.namespace, mem_out.safepoint_namespace);
        assert_eq!(image_install.namespace, image_out.safepoint_namespace);
        assert_eq!(mem_install.site_id, image_install.site_id);
        assert_eq!(mem_install.patchpoint_label, image_install.patchpoint_label);
        assert_eq!(mem_install.kind, image_install.kind);
        assert_eq!(mem_install.function, image_install.function);
        assert_eq!(mem_install.block_label, image_install.block_label);
        assert_eq!(mem_install.computation_index, image_install.computation_index);
        assert_eq!(mem_install.roots, image_install.roots);
        assert_eq!(
            mem_install.installed_text_region,
            InstalledTextRegion {
                kind: InstalledTextRegionKind::InMemory,
                section_label: "mem.code.text",
            }
        );
        assert_eq!(
            image_install.installed_text_region,
            InstalledTextRegion {
                kind: InstalledTextRegionKind::Image,
                section_label: "image.code.text",
            }
        );

        let mem_ir = mem_out.module.print_to_string().to_string();
        let image_ir = image_out.module.print_to_string().to_string();
        assert!(
            mem_ir.contains(NOD_JIT_BEGIN_SAFEPOINT_SYMBOL),
            "in-memory surface missing JIT begin safepoint hook: {mem_ir}"
        );
        assert!(
            mem_ir.contains(NOD_JIT_END_SAFEPOINT_SYMBOL),
            "in-memory surface missing JIT end safepoint hook: {mem_ir}"
        );
        assert!(
            !mem_ir.contains(super::NOD_REGISTER_ROOT_SYMBOL),
            "in-memory surface should not emit legacy root registration hooks: {mem_ir}"
        );
        assert!(
            !mem_ir.contains(super::NOD_UNREGISTER_ROOT_SYMBOL),
            "in-memory surface should not emit legacy root unregister hooks: {mem_ir}"
        );
        assert!(
            mem_ir.contains(super::JIT_SAFEPOINT_METADATA_SYMBOL_PREFIX),
            "in-memory surface missing embedded JIT safepoint metadata: {mem_ir}"
        );
        assert!(
            !mem_ir.contains(NOD_AOT_BEGIN_SAFEPOINT_SYMBOL),
            "in-memory surface should not emit AOT runtime safepoint hooks: {mem_ir}"
        );
        assert!(
            !image_ir.contains(super::JIT_SAFEPOINT_METADATA_SYMBOL_PREFIX),
            "image surface should not emit JIT safepoint metadata globals: {image_ir}"
        );
        assert!(
            image_ir.contains(NOD_AOT_BEGIN_SAFEPOINT_SYMBOL),
            "image surface missing AOT begin safepoint hook: {image_ir}"
        );
        assert!(
            !image_ir.contains(super::NOD_REGISTER_ROOT_SYMBOL),
            "image surface should not emit legacy root registration hooks: {image_ir}"
        );
        assert!(
            !image_ir.contains(super::NOD_UNREGISTER_ROOT_SYMBOL),
            "image surface should not emit legacy root unregister hooks: {image_ir}"
        );
        assert!(
            image_ir.contains(NOD_AOT_VERIFY_SAFEPOINT_SYMBOL),
            "image surface missing AOT verify safepoint hook: {image_ir}"
        );
        assert!(
            image_ir.contains(NOD_AOT_END_SAFEPOINT_SYMBOL),
            "image surface missing AOT end safepoint hook: {image_ir}"
        );
    }

    #[test]
    fn image_surface_zero_root_safepoint_uses_null_slot_base() {
        let callee = Function {
            id: FunctionId(1),
            name: "alloc_no_roots".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![],
                terminator: Terminator::Return {
                    value: Some(TempId(0)),
                },
            }],
            temps: vec![Temporary {
                id: TempId(0),
                type_estimate: TypeEstimate::Integer,
            }],
            return_type: TypeEstimate::Integer,
            span: test_span(),
        };
        let caller = Function {
            id: FunctionId(0),
            name: "caller_zero_roots".to_string(),
            params: vec![TempId(0)],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".to_string(),
                params: vec![],
                computations: vec![Computation::DirectCall {
                    dst: TempId(1),
                    callee: "alloc_no_roots".to_string(),
                    args: vec![TempId(0)],
                    safepoint_roots: vec![],
                    is_no_alloc: false,
                }],
                terminator: Terminator::Return {
                    value: Some(TempId(1)),
                },
            }],
            temps: vec![
                Temporary {
                    id: TempId(0),
                    type_estimate: TypeEstimate::Integer,
                },
                Temporary {
                    id: TempId(1),
                    type_estimate: TypeEstimate::Integer,
                },
            ],
            return_type: TypeEstimate::Integer,
            span: test_span(),
        };

        let ctx = Context::create();
        let out = codegen_module_for_surface(
            &ctx,
            &[caller, callee],
            "safepoint_zero_roots_image",
            CodeInstallSurface::Image,
        )
        .expect("image codegen ok");

        let image_ir = out.module.print_to_string().to_string();
        assert!(
            image_ir.contains(NOD_AOT_BEGIN_SAFEPOINT_SYMBOL),
            "image surface missing AOT begin safepoint hook: {image_ir}"
        );
        assert!(
            image_ir.contains("ptr null"),
            "zero-root image safepoint should pass null slot base: {image_ir}"
        );
    }

    #[test]
    fn emitted_safepoint_sites_track_install_surface_region() {
        let roots = vec![SafepointRootLocation {
            temp: TempId(0),
            location: SafepointLocation::FrameSlot(0),
        }];

        let mem_site = emitted_safepoint_site(
            0,
            SafepointInstallKey {
                install_surface: SafepointInstallSurface::InMemoryCodeText,
                module_site_ordinal: 7,
            },
            SafepointKind::DirectCall,
            "f".to_string(),
            "entry".to_string(),
            0,
            roots.clone(),
        );
        let image_site = emitted_safepoint_site(
            0,
            SafepointInstallKey {
                install_surface: SafepointInstallSurface::ImageCodeText,
                module_site_ordinal: 7,
            },
            SafepointKind::DirectCall,
            "f".to_string(),
            "entry".to_string(),
            0,
            roots,
        );

        assert_eq!(mem_site.install_key.site_id(), image_site.install_key.site_id());
        assert_eq!(
            mem_site.install_key.patchpoint_label(),
            image_site.install_key.patchpoint_label()
        );
        assert_eq!(
            mem_site.installed_text_region,
            InstalledTextRegion {
                kind: InstalledTextRegionKind::InMemory,
                section_label: "mem.code.text",
            }
        );
        assert_eq!(
            image_site.installed_text_region,
            InstalledTextRegion {
                kind: InstalledTextRegionKind::Image,
                section_label: "image.code.text",
            }
        );
    }
}

/// Sprint 38b — produce a deterministic [`CacheKey`] from a module name
/// and its DFM functions for the non-cache-aware codegen entry points.
/// Uses the existing [`crate::cache::cache_key_for_dfm`] over the
/// formatted DFM text so identical inputs produce identical keys (and
/// therefore identical symbol namespaces in the resulting IR).
fn synth_cache_key_from_module(module_name: &str, fns: &[DfmFunction]) -> CacheKey {
    let mut text = String::with_capacity(64 + module_name.len());
    text.push_str("module:");
    text.push_str(module_name);
    text.push('\n');
    text.push_str(&nod_dfm::format_for_cache_key(fns));
    crate::cache::cache_key_for_dfm(&text)
}

fn function_type<'ctx>(ctx: &'ctx Context, f: &DfmFunction) -> FunctionType<'ctx> {
    let param_types: Vec<BasicMetadataTypeEnum<'ctx>> = f
        .params
        .iter()
        .map(|p| {
            let ty = f.temp_type(*p);
            llvm_basic_type(ctx, ty).into()
        })
        .collect();
    match llvm_return_type(ctx, f.return_type) {
        Some(ret) => ret.fn_type(&param_types, false),
        None => ctx.void_type().fn_type(&param_types, false),
    }
}

fn llvm_basic_type<'ctx>(ctx: &'ctx Context, ty: TypeEstimate) -> BasicTypeEnum<'ctx> {
    match ty {
        // Sprint 09 ABI: `<integer>` and `<boolean>` are both tagged
        // `Word` values — a single `i64` per register/stack slot.
        // Sprint 10 promotes `<string>` to the same shape (tagged
        // pointer to a `<byte-string>` heap object).
        TypeEstimate::Integer | TypeEstimate::Boolean | TypeEstimate::String => {
            ctx.i64_type().into()
        }
        TypeEstimate::SingleFloat => ctx.f32_type().into(),
        TypeEstimate::DoubleFloat => ctx.f64_type().into(),
        TypeEstimate::Character => ctx.i32_type().into(),
        TypeEstimate::Unit | TypeEstimate::Top | TypeEstimate::Bottom => {
            // Top / Bottom default to i64 (kernel-subset choice; see DEFERRED).
            // Unit only appears as a return; values of Unit type never flow
            // through SSA, so this fallback never reads back.
            ctx.i64_type().into()
        }
        // Sprint 15: `Class(_)` is a tagged-pointer Word like `String`
        // — the lowering pass stores the runtime ClassId for narrowing
        // / dispatch purposes, but at the register level a user-class
        // instance is the same i64 tagged pointer as any other heap
        // object. `Singleton(_)` is reserved (not populated in Sprint
        // 15); same i64 fallback.
        TypeEstimate::Class(_) | TypeEstimate::Singleton(_) => ctx.i64_type().into(),
    }
}

fn llvm_return_type<'ctx>(
    ctx: &'ctx Context,
    ty: TypeEstimate,
) -> Option<BasicTypeEnum<'ctx>> {
    if matches!(ty, TypeEstimate::Unit) {
        None
    } else {
        Some(llvm_basic_type(ctx, ty))
    }
}

/// Per-function emission state. SSA values produced inside the function
/// are kept in `temps`; LLVM basic blocks are keyed on `BlockId`; phi
/// nodes are recorded in `phi_inputs` for a second-pass `add_incoming`.
struct Emit<'ctx, 'a> {
    ctx: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    function_map: &'a FunctionMap<'ctx>,
    /// Sprint 38b — shared per-module codegen context (cache key +
    /// manifest builder). Every emitter that allocates a named external
    /// global pushes one [`RelocKind`] row here.
    mctx: &'a ModuleCodegenCtx,
    func: &'a DfmFunction,
    llvm_fn: FunctionValue<'ctx>,
    blocks: HashMap<BlockId, BasicBlock<'ctx>>,
    block_phis: HashMap<BlockId, Vec<PhiValue<'ctx>>>,
    block_entry_temps: HashMap<BlockId, HashMap<TempId, BasicValueEnum<'ctx>>>,
    temps: HashMap<TempId, BasicValueEnum<'ctx>>,
    /// (target block, source block, arg SSA values) — recorded as we
    /// emit each terminator. After all blocks are emitted, we walk this
    /// and call `add_incoming` on each phi node. Done in two phases so
    /// every basic block exists before phis reference them.
    ///
    /// GAP-007 (docs/COMPILER_GAPS.md): we snapshot the resolved
    /// `BasicValueEnum` at jump-emit time rather than the symbolic
    /// `TempId`. `end_safepoint` rebinds `state.temps[t]` to a fresh
    /// `gc.reload.tN` SSA value in whichever block currently owns the
    /// reload — if phi-wiring resolved TempIds at end-of-function, every
    /// loop-header phi would read the LAST reload across the entire
    /// function, breaking dominance and corrupting heap references.
    /// The value captured here flowed out of the actual predecessor.
    pending_incoming: Vec<(BlockId, BasicBlock<'ctx>, Vec<BasicValueEnum<'ctx>>)>,
    /// Sprint 45d: one entry-block slab backing every safepoint slot in
    /// the function. Slot indices in the emitted safepoint maps index
    /// into this array directly.
    safepoint_slot_capacity: usize,
    safepoint_slot_slab: Option<inkwell::values::PointerValue<'ctx>>,
    /// GAP-011 fix (2026-05-30): per-function-parameter "home" alloca
    /// for every GC-typed parameter. The home is the source of truth
    /// across block boundaries:
    ///   - Entry block: store `%fn.argN` into the home.
    ///   - Every block-entry binding of `state.temps[p]` is a fresh
    ///     `load %p.home`, NOT a stale `get_nth_param` (which would
    ///     reinstall the pre-GC value).
    ///   - `end_safepoint` writes every reloaded function-param value
    ///     back to the home, so the next block's load picks up the
    ///     post-GC address.
    ///
    /// Non-GC params (Integer / Boolean / Character / SingleFloat /
    /// DoubleFloat / Unit) do not need a home and stay as raw
    /// `get_nth_param` values — `param_homes` simply doesn't contain
    /// them.
    param_homes: HashMap<TempId, inkwell::values::PointerValue<'ctx>>,
    current_block_label: String,
    current_computation_index: usize,
    /// GAP-011 diagnostic (`NOD_DIAG_MERGE_DIVERGENCE`): when set, every
    /// value carried across a CFG edge by `note_successor_entry_temps` is
    /// recorded as (target block, source label, temp, value). After the
    /// block loop we report any GC-typed temp that arrives at a block from
    /// two predecessors with DIFFERENT LLVM values yet is NOT a block param
    /// — the exact stale-reload signature. Off by default; zero cost when
    /// the env var is unset.
    diag_merge: bool,
    merge_diag: Vec<(BlockId, String, TempId, BasicValueEnum<'ctx>)>,
    // Sprint 38e / 45c — site ids were previously dispatch-local. The
    // module-wide allocator now lives in
    // `ModuleCodegenCtx::next_safepoint_site_id` so site_ids are
    // module-wide unique; see the doc comment there for the
    // cross-function-collision rationale.
}

fn emit_function<'ctx, 'a>(
    ctx: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    function_map: &'a FunctionMap<'ctx>,
    mctx: &'a ModuleCodegenCtx,
    func: &'a DfmFunction,
    llvm_fn: FunctionValue<'ctx>,
) -> Result<(), CodegenError> {
    // ── Cross-block heap-value contract ──────────────────────────────────
    // The lowering (nod-sema lower.rs) MUST thread any Dylan heap-shaped
    // value that is live across a GC point through a block-arg phi node.
    // Specifically: if a `TempId` produced in block A holds a heap pointer
    // and is consumed in block B (where a GC may occur between A and B),
    // the lowering must wire it as a block-arg / phi parameter rather than
    // referencing the A-local LLVM instruction directly.
    //
    // This codegen step enforces that contract by resetting `state.temps[t]`
    // to the canonical SSA value (function-param alloca or block-param phi)
    // at every block switch (see the `switch_to` and `emit_block` paths).
    // If lowering violates the contract — fails to thread a live heap ref
    // through a phi — this reset will silently install the *pre-evac* LLVM
    // value after a GC, causing a use-after-collection. The fix is always
    // in the lowering, not here.
    // ─────────────────────────────────────────────────────────────────────
    let safepoint_slot_capacity = max_safepoint_slots(func);
    let mut state = Emit {
        ctx,
        module,
        builder,
        function_map,
        mctx,
        func,
        llvm_fn,
        blocks: HashMap::new(),
        block_phis: HashMap::new(),
        block_entry_temps: HashMap::new(),
        temps: HashMap::new(),
        pending_incoming: Vec::new(),
        safepoint_slot_capacity,
        safepoint_slot_slab: None,
        param_homes: HashMap::new(),
        current_block_label: String::new(),
        current_computation_index: 0,
        diag_merge: std::env::var_os("NOD_DIAG_MERGE_DIVERGENCE").is_some(),
        merge_diag: Vec::new(),
    };

    // Pre-create every LLVM basic block so terminators can branch
    // forward.
    for b in &func.blocks {
        let bb = ctx.append_basic_block(llvm_fn, &b.label);
        state.blocks.insert(b.id, bb);
    }

    state.init_safepoint_slot_slab()?;

    // Pre-compute the set of loop-header blocks (back-edge targets).
    // Polls are emitted at these blocks and at the function entry block.
    let loop_headers = find_loop_headers(func);

    // GAP-011 fix: spill every GC-typed function parameter to a stable
    // "home" alloca in the entry block. All subsequent reads of that
    // parameter go through the home, so any safepoint reload that writes
    // back to the home (see `end_safepoint`) is observed by the *next*
    // block's load.
    //
    // Non-GC params (Integer / Boolean / Character / floats / Unit) skip
    // the home — their values can't be moved by GC, so the raw
    // `get_nth_param` SSA value is fine across block boundaries.
    {
        let entry_bb = state.blocks[&func.entry];
        builder.position_at_end(entry_bb);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        for (i, p) in func.params.iter().enumerate() {
            let pv = llvm_fn
                .get_nth_param(i as u32)
                .expect("parameter index in range");
            let ty = func.temp_type(*p);
            if ty.needs_gc_protection() {
                // Build the home alloca via the standard entry-block helper
                // (preserves the GAP-010 "no allocas outside entry" guard).
                let home = state.build_entry_alloca(
                    ctx.i64_type(),
                    &format!("p.t{}.home", p.0),
                )?;
                // Position back at entry-block insertion point and seed.
                builder.position_at_end(entry_bb);
                builder.build_store(home, pv).map_err(map_err)?;
                state.param_homes.insert(*p, home);
            }
        }
        // Establish the entry block's initial binding for every param via
        // the same helper the block-restart path uses, so behavior is
        // identical across all blocks: GC-typed → load from home, plain →
        // raw arg.
        for &p in &func.params {
            let v = state.rebind_param(p)?;
            state.temps.insert(p, v);
        }
    }
    state
        .block_entry_temps
        .insert(func.entry, state.temps.clone());

    // For each non-entry block with `params`, create phi nodes at the
    // block's start. Phi values feed the block-arg temps.
    for b in &func.blocks {
        if b.params.is_empty() {
            continue;
        }
        let bb = state.blocks[&b.id];
        builder.position_at_end(bb);
        let mut phis = Vec::with_capacity(b.params.len());
        for &param in &b.params {
            let ty = func.temp_type(param);
            let llty = llvm_basic_type(ctx, ty);
            let phi = builder
                .build_phi(llty, &format!("phi.t{}", param.0))
                .map_err(|e| CodegenError::Builder(e.to_string()))?;
            state.temps.insert(param, phi.as_basic_value());
            phis.push(phi);
        }
        state.block_phis.insert(b.id, phis);
    }

    // Emit every block's computations + terminator.
    for b in &func.blocks {
        let bb = state.blocks[&b.id];
        builder.position_at_end(bb);
        state.current_block_label = b.label.clone();
        if let Some(entry_temps) = state.block_entry_temps.get(&b.id).cloned() {
            for (temp, value) in entry_temps {
                state.temps.insert(temp, value);
            }
        }
        // Safepoint reloads intentionally rebind `state.temps[temp]` to a
        // fresh SSA value, but that rebind is only valid within the block
        // that performed the reload. When we move on to a sibling block,
        // restore canonical block-entry bindings so later uses do not pick
        // up a reload defined in a non-dominating predecessor.
        //
        // GAP-011 fix (2026-05-30): for GC-typed function parameters the
        // "canonical" binding is now `load %p.home`, NOT `get_nth_param`.
        // The home is the source of truth across block boundaries; reading
        // raw `get_nth_param` here would reinstall the pre-GC entry value
        // every time we cross a block boundary, silently undoing every
        // safepoint reload of a function param performed in a sibling
        // block. Non-GC params still get the raw `get_nth_param` value
        // because their values can't be moved.
        for &p in &func.params {
            let v = state.rebind_param(p)?;
            state.temps.insert(p, v);
        }
        if let Some(phis) = state.block_phis.get(&b.id) {
            for (&param, phi) in b.params.iter().zip(phis.iter()) {
                state.temps.insert(param, phi.as_basic_value());
            }
        }
        // Safepoint poll at function entry and at every loop-header
        // block (back-edge target) so the GC can stop-the-world even
        // in non-allocating tight loops.
        if b.id == func.entry || loop_headers.contains(&b.id) {
            state.emit_safepoint_poll()?;
        }
        for (computation_index, c) in b.computations.iter().enumerate() {
            state.current_computation_index = computation_index;
            state.emit_computation(c)?;
        }
        state.emit_terminator(&b.terminator)?;
    }

    // Now that every block has been emitted, wire up phi incomings.
    // GAP-007: `arg_vals` are pre-resolved SSA values captured at
    // jump-emit time, so we don't re-consult `state.temps` here (which
    // safepoint reloads have since mutated).
    for (target_block, source_bb, arg_vals) in &state.pending_incoming {
        let Some(phis) = state.block_phis.get(target_block) else {
            continue;
        };
        for (phi, v) in phis.iter().zip(arg_vals.iter()) {
            phi.add_incoming(&[(v, *source_bb)]);
        }
    }

    // GAP-011 diagnostic analysis (NOD_DIAG_MERGE_DIVERGENCE): report any
    // GC-typed temp that arrives at a block from ≥2 predecessors with
    // DIFFERENT LLVM values, yet is NOT a block param — the exact codegen
    // stale-reload signature. Such a temp would be installed via
    // `note_successor_entry_temps` (first-writer-wins), so a non-dominating
    // predecessor's reload value can leak into a sibling/merge block.
    if state.diag_merge {
        let mut groups: HashMap<(BlockId, TempId), Vec<BasicValueEnum<'ctx>>> = HashMap::new();
        for (tgt, _label, t, v) in &state.merge_diag {
            groups.entry((*tgt, *t)).or_default().push(*v);
        }
        let mut hits = 0usize;
        for ((tgt, t), vals) in &groups {
            let mut distinct: Vec<BasicValueEnum<'ctx>> = Vec::new();
            for v in vals {
                if !distinct.iter().any(|d| d == v) {
                    distinct.push(*v);
                }
            }
            if distinct.len() < 2 {
                continue; // dominating value (same on all edges) — safe
            }
            let blk = func.blocks.iter().find(|b| b.id == *tgt);
            let is_param = blk.map(|b| b.params.contains(t)).unwrap_or(false);
            if is_param {
                continue; // handled correctly by the phi / pending_incoming path
            }
            let ty = func.temp_type(*t);
            if !ty.needs_gc_protection() {
                continue; // a stale non-pointer is a wrong-value bug, not the crash
            }
            hits += 1;
            eprintln!(
                "MERGE-DIVERGENCE fn={} block={} temp=t{} type={:?} distinct_values={} edges={}",
                func.name,
                blk.map(|b| b.label.as_str()).unwrap_or("?"),
                t.0,
                ty,
                distinct.len(),
                vals.len(),
            );
        }
        if hits > 0 {
            eprintln!(
                "MERGE-DIVERGENCE: {} GC stale-reload site(s) in fn={}",
                hits, func.name
            );
        }
    }

    // GAP-010 guard: every `alloca` MUST live in the entry block. An
    // alloca reached on a loop back-edge is executed each iteration and,
    // at -O0, never reclaimed until the function returns (LLVM inserts no
    // `stackrestore`) — so a scratch buffer in a hot loop grows the frame
    // without bound until the stack overflows. That was GAP-010 (the
    // sealed-call sd.args/sd.chain slabs). All scratch allocas must route
    // through `build_entry_alloca` / be placed like `init_safepoint_slot_slab`.
    // Catch a regression at codegen time rather than as a silent
    // STATUS_STACK_OVERFLOW at runtime.
    {
        use inkwell::values::InstructionOpcode;
        for bb in llvm_fn.get_basic_blocks().iter().skip(1) {
            let mut inst = bb.get_first_instruction();
            while let Some(i) = inst {
                if i.get_opcode() == InstructionOpcode::Alloca {
                    return Err(CodegenError::Builder(format!(
                        "GAP-010 guard: `alloca` emitted outside the entry block in \
                         `{}` — a loop-reachable alloca leaks stack every iteration \
                         at -O0. Route the scratch buffer through `build_entry_alloca`.",
                        func.name
                    )));
                }
                inst = i.get_next_instruction();
            }
        }
    }

    Ok(())
}
impl<'ctx, 'a> Emit<'ctx, 'a> {
    fn note_successor_entry_temps(&mut self, target: BlockId) {
        let entry = self
            .block_entry_temps
            .entry(target)
            .or_default();
        for (&temp, &value) in &self.temps {
            entry.entry(temp).or_insert(value);
        }
        if self.diag_merge {
            // GAP-011 diagnostic: snapshot what value THIS predecessor would
            // carry into `target` for every temp it currently binds. Cloned
            // out first to release the borrow of `self.temps`.
            let label = self.current_block_label.clone();
            let recs: Vec<(TempId, BasicValueEnum<'ctx>)> =
                self.temps.iter().map(|(&t, &v)| (t, v)).collect();
            for (t, v) in recs {
                self.merge_diag.push((target, label.clone(), t, v));
            }
        }
    }

    fn begin_emitted_safepoint(
        &mut self,
        kind: SafepointKind,
        roots: &[TempId],
    ) -> Result<EmittedSafepoint<'ctx>, CodegenError> {
        let site_id = self.mctx.next_safepoint_site_id();
        let rented = self.begin_safepoint(site_id, roots)?;
        self.record_safepoint_plan(site_id, kind, &rented);
        Ok(EmittedSafepoint { site_id, rented })
    }

    fn end_emitted_safepoint(
        &mut self,
        emitted: &EmittedSafepoint<'ctx>,
    ) -> Result<(), CodegenError> {
        self.end_safepoint(emitted.site_id, &emitted.rented)
    }

    fn record_safepoint_plan(
        &self,
        site_id: u64,
        kind: SafepointKind,
        rented: &[SafepointSlot<'ctx>],
    ) {
        let roots = rented
            .iter()
            .map(|slot_info| SafepointRootLocation {
                temp: slot_info.temp,
                location: slot_info.home.as_safepoint_location(),
            })
            .collect();
        self.mctx.safepoint_sites.borrow_mut().push(emitted_safepoint_site(
            self.mctx.key.0[0],
            SafepointInstallKey {
                install_surface: self.mctx.install_surface,
                module_site_ordinal: site_id,
            },
            kind,
            self.func.name.clone(),
            self.current_block_label.clone(),
            self.current_computation_index,
            roots,
        ));
    }

    /// Sprint 38b — load the runtime `#t` Word from the external global
    /// declared (or reused) for this module. Replaces the Sprint 10
    /// pattern of baking `imm.true_.raw()` as an `i64` constant.
    fn load_imm_true(&self) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let g = get_or_add_imm_global(self.ctx, self.module, self.mctx, RelocKind::ImmTrue);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(self.ctx.i64_type(), g.as_pointer_value(), "imm.true.load")
            .map_err(map_err)?;
        Ok(v.into_int_value())
    }
    /// Sprint 38b — load the runtime `#f` Word from the external global.
    fn load_imm_false(&self) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let g = get_or_add_imm_global(self.ctx, self.module, self.mctx, RelocKind::ImmFalse);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(self.ctx.i64_type(), g.as_pointer_value(), "imm.false.load")
            .map_err(map_err)?;
        Ok(v.into_int_value())
    }

    /// Sprint 38b — load the runtime `nil` (empty-list) Word.
    fn load_imm_nil(&self) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let g = get_or_add_imm_global(self.ctx, self.module, self.mctx, RelocKind::ImmNil);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(self.ctx.i64_type(), g.as_pointer_value(), "imm.nil.load")
            .map_err(map_err)?;
        Ok(v.into_int_value())
    }

    /// Sprint 38b — load the runtime `#f` *untagged-wrapper* address
    /// used as a fault-free fallback target in the branchless class-id
    /// reads (see `emit_class_id_load` / `emit_wrapper_class_check`).
    fn load_imm_false_wrapper(&self) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let g = get_or_add_imm_global(
            self.ctx,
            self.module,
            self.mctx,
            RelocKind::ImmFalseWrapper,
        );
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(
                self.ctx.i64_type(),
                g.as_pointer_value(),
                "imm.false_wrapper.load",
            )
            .map_err(map_err)?;
        Ok(v.into_int_value())
    }

    /// Sprint 38c — load the runtime class-metadata pointer for
    /// `class_id` from the per-module external global. If `tagged`
    /// is true, OR `| 1` onto the loaded value to materialise a
    /// pointer-tagged Dylan Word (`emit_class_ref` semantics); if
    /// false, return the raw pointer bits (`nod_make`'s class-arg
    /// shape, `emit_class_metadata_ptr_const` semantics).
    fn load_class_metadata(
        &self,
        class_id: u32,
        tagged: bool,
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let g = get_or_add_class_metadata_global(self.ctx, self.module, self.mctx, class_id);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64_ty = self.ctx.i64_type();
        let raw = self
            .builder
            .build_load(i64_ty, g.as_pointer_value(), "class_md.load")
            .map_err(map_err)?
            .into_int_value();
        if tagged {
            let one = i64_ty.const_int(1, false);
            let v = self
                .builder
                .build_or(raw, one, "class_md.tagged")
                .map_err(map_err)?;
            Ok(v)
        } else {
            Ok(raw)
        }
    }

    /// Sprint 38c — load the interned `<byte-string>` Word for `text`
    /// from the per-module external global. The text is deduped within
    /// the module so repeated references emit one global, one manifest
    /// row, and one IR-level load instruction per use.
    fn load_string_literal(
        &self,
        text: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let idx = self.mctx.string_lit_index(text);
        let g = get_or_add_string_literal_global(self.ctx, self.module, self.mctx, idx, text);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(self.ctx.i64_type(), g.as_pointer_value(), "strlit.load")
            .map_err(map_err)?
            .into_int_value();
        Ok(v)
    }

    /// Sprint 38c — load the interned `<symbol>` Word for `name`.
    fn load_symbol_literal(
        &self,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let idx = self.mctx.symbol_lit_index(name);
        let g = get_or_add_symbol_literal_global(self.ctx, self.module, self.mctx, idx, name);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(self.ctx.i64_type(), g.as_pointer_value(), "symlit.load")
            .map_err(map_err)?
            .into_int_value();
        Ok(v)
    }

    /// Sprint 38d — load the address of an [`nod_runtime::ApiStubEntry`]
    /// for `(dll, symbol)` from the per-module external global. The
    /// loaded `i64` value is the entry pointer that `nod_winffi_call_N`
    /// takes as its first argument — the trampoline then reads
    /// `fn_ptr` and `signature` off the entry struct.
    ///
    /// `(dll, symbol)` is deduped within the module (case-insensitive
    /// dll, case-sensitive symbol) so repeated references emit one
    /// global, one manifest row, and one IR-level load per use.
    fn load_stub_entry(
        &self,
        dll: &str,
        symbol: &str,
        signature_bytes: &[u8],
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let idx = self.mctx.stub_entry_index(dll, symbol);
        let g = get_or_add_stub_entry_global(
            self.ctx,
            self.module,
            self.mctx,
            idx,
            dll,
            symbol,
            signature_bytes,
        );
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(self.ctx.i64_type(), g.as_pointer_value(), "stub_entry.load")
            .map_err(map_err)?
            .into_int_value();
        Ok(v)
    }

    /// Sprint 38e — load the address of a `CacheSlot` for `site_id`
    /// from the per-module external global. The loaded `i64` is the
    /// pointer the dispatch site uses to derive field-offset addresses
    /// (class / method / generation / hits) via `add i64`.
    ///
    /// LLVM at `-O2` will CSE the load + adds within the dispatch
    /// diamond, so the natural IR shape — load once, add for each
    /// field — costs no more than the pre-Sprint-38e baked-constant
    /// shape after optimisation. See user memory note "LLVM does most
    /// optimization".
    fn load_cache_slot_ptr(
        &self,
        site_id: u64,
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let g = get_or_add_cache_slot_global(self.ctx, self.module, self.mctx, site_id);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(
                self.ctx.i64_type(),
                g.as_pointer_value(),
                &format!("disp.s{site_id}.cache_slot.load"),
            )
            .map_err(map_err)?
            .into_int_value();
        Ok(v)
    }

    /// Sprint 38e — load the address of a `GenericFunction` keyed by
    /// `name` from the per-module external global. Multiple dispatch
    /// sites targeting the same generic share one external global +
    /// one manifest row + one IR-level `load i64` per use (LLVM CSE
    /// folds within the function at `-O2`).
    fn load_generic_function_ptr(
        &self,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let g = get_or_add_generic_function_global(self.ctx, self.module, self.mctx, name);
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self
            .builder
            .build_load(self.ctx.i64_type(), g.as_pointer_value(), "generic.load")
            .map_err(map_err)?
            .into_int_value();
        Ok(v)
    }

    /// Sprint 38b — convert an `i1` to a Sprint 10 tagged-boolean Dylan
    /// value. `#t` and `#f` are pointer-tagged Words referring to
    /// pinned heap wrappers; we load both via external globals (Sprint
    /// 38b cross-process replay) and `select` between them on `i1`.
    fn retag_bool(
        &self,
        i1: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let true_v = self.load_imm_true()?;
        let false_v = self.load_imm_false()?;
        self.builder
            .build_select(i1, true_v, false_v, "tag.bool.sel")
            .map_err(map_err)
    }

    /// Sprint 38b — build an `i1` from a Sprint 10 tagged-boolean Word:
    /// `cond != #f`. Used by `Terminator::If` and the boolean PrimOps.
    /// Dylan's truthiness is "everything except `#f` is true", so the
    /// comparison is purely pointer-identity against the pinned `#f`
    /// singleton — now loaded via an external global instead of baked
    /// as an i64 constant.
    fn untag_bool_to_i1(
        &self,
        v: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let false_v = self.load_imm_false()?;
        self.builder
            .build_int_compare(IntPredicate::NE, v, false_v, "untag.bool")
            .map_err(map_err)
    }

    fn emit_computation(&mut self, c: &Computation) -> Result<(), CodegenError> {
        match c {
            Computation::Const { dst, value } => {
                let v = self.emit_const(*dst, value)?;
                self.temps.insert(*dst, v);
            }
            Computation::PrimOp { dst, op, args } => {
                let v = self.emit_primop(*op, args)?;
                self.temps.insert(*dst, v);
            }
            Computation::DirectCall {
                dst,
                callee,
                args,
                safepoint_roots,
                // Sprint 48: codegen doesn't directly consult this — when
                // the call is no_alloc, the liveness pass produces empty
                // safepoint_roots and the existing `!rented.is_empty()`
                // guard in `begin_safepoint` skips the brackets. Pattern
                // mention is for the exhaustive-match requirement only.
                is_no_alloc: _,
            } => {
                let v = self.emit_direct_call(callee, args, *dst, safepoint_roots)?;
                // GAP-006 fix: even when the called function returns
                // void (`v == None`), we must bind `dst` to SOMETHING in
                // `self.temps`. If a downstream Jump arg references this
                // temp (e.g. when the void call is the last expression
                // of an `if`-arm whose join phi takes a value param),
                // the phi-wiring step at the end of the function panics
                // with `phi incoming temp defined`. Sentinel: load the
                // runtime `nil` Word — Dylan's canonical "no meaningful
                // value" — so phi joins get a real i64. Consumers that
                // actually USE the value see `nil` (no use case relies
                // on the void call's "result" being anything else).
                let v = match v {
                    Some(v) => v,
                    None => self.load_imm_nil()?.into(),
                };
                let v = self.coerce_call_result(*dst, v)?;
                self.temps.insert(*dst, v);
            }
            Computation::Call { .. } => {
                return Err(CodegenError::IndirectCallNotSupported {
                    in_function: self.func.name.clone(),
                });
            }
            Computation::TypeCheck { dst, value, class } => {
                let v = self.emit_type_check(*value, class)?;
                self.temps.insert(*dst, v);
            }
            // Sprint 11 stub: no lowering path emits WriteBarrier yet.
            // Sprint 12 (slot setters) is the first emitter; codegen
            // lowers `Computation::WriteBarrier` to a `*slot = value`
            // store plus a call into `nod_runtime::write_barrier`.
            Computation::WriteBarrier { .. } => {
                return Err(CodegenError::WriteBarrierNotEmitted {
                    in_function: self.func.name.clone(),
                });
            }
            Computation::LoadSlot { dst, instance, offset, slot_type } => {
                let v = self.emit_load_slot(*instance, *offset, *slot_type)?;
                self.temps.insert(*dst, v);
            }
            Computation::StoreSlot { dst, instance, offset, value, slot_type } => {
                let v = self.emit_store_slot(*instance, *offset, *value, *slot_type)?;
                self.temps.insert(*dst, v);
            }
            Computation::Dispatch {
                dst,
                generic_name,
                args,
                safepoint_roots,
            } => {
                let v = self.emit_dispatch(generic_name, args, *dst, safepoint_roots)?;
                // GAP-006 fix — see DirectCall arm above for rationale.
                let v = match v {
                    Some(v) => v,
                    None => self.load_imm_nil()?.into(),
                };
                self.temps.insert(*dst, v);
            }
            Computation::SealedDirectCall {
                dst,
                method,
                fallback_chain,
                generic_name,
                args,
                safepoint_roots,
                is_no_alloc: _,
            } => {
                let v = self.emit_sealed_direct_call(
                    method,
                    fallback_chain,
                    generic_name,
                    args,
                    *dst,
                    safepoint_roots,
                )?;
                // GAP-006 fix — see DirectCall arm above for rationale.
                let v = match v {
                    Some(v) => v,
                    None => self.load_imm_nil()?.into(),
                };
                self.temps.insert(*dst, v);
            }
        }
        Ok(())
    }

    /// Sprint 15 sealed-direct call codegen. Brackets the resolved
    /// direct call with a chain-frame push/pop pair so any
    /// `next-method()` inside the body walks the fallback chain
    /// identically to the runtime `nod_dispatch` path. The fallback
    /// chain's method body pointers are resolved by the JIT engine
    /// from the body-symbol names — we emit `ptrtoint(@symbol, i64)`
    /// constants and stash them in a stack-local array along with the
    /// args.
    ///
    /// For Sprint 15 the args are spilled to stack-local i64 slots; the
    /// chain frame's method pointers come from extern function
    /// declarations on the body symbols. The push shim memcpy's both
    /// into a heap-allocated chain frame; the pop shim drops it.
    fn emit_sealed_direct_call(
        &mut self,
        method: &str,
        fallback_chain: &[String],
        generic_name: &str,
        args: &[TempId],
        dst: TempId,
        safepoint_roots: &[TempId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());

        // Resolve the resolved-method body fn we want to call. The
        // body either lives in this module (lowering put it in the
        // function table) OR lives in a different already-JIT'd
        // module — most commonly stdlib, when the dispatch narrower
        // resolves a user call to a stdlib `define method` body. In
        // the cross-module case we declare an extern that the engine
        // resolves via `nod_runtime::find_method_body_ptr` — the same
        // fallback `emit_direct_call` uses for `Dispatch`→`DirectCall`
        // rewrites. Without this, narrower-promoted calls like
        // `size("hello")` → `DirectCall("size$8")` against a stdlib-
        // resident specialiser would hit UnknownCallee.
        let callee_fn = if let Some(&f) = self.function_map.get(method) {
            f
        } else if let Some(existing) = self.module.get_function(method) {
            existing
        } else if nod_runtime::find_method_body_ptr(method).is_some() {
            let params: Vec<BasicMetadataTypeEnum<'ctx>> =
                (0..args.len()).map(|_| i64ty.into()).collect();
            let fty = i64ty.fn_type(&params, false);
            self.module.add_function(method, fty, None)
        } else {
            return Err(CodegenError::UnknownCallee {
                in_function: self.func.name.clone(),
                callee: method.to_string(),
            });
        };

        // Spill args into a stack-local i64 array.
        let arity = args.len();
        let args_arr_ty = i64ty.array_type(arity.max(1) as u32);
        // Entry-block alloca: a loop-body alloca would leak stack every
        // iteration at -O0 (GAP-010 stack overflow). See build_entry_alloca.
        let args_alloca = self.build_entry_alloca(args_arr_ty, "sd.args")?;
        for (i, arg) in args.iter().enumerate() {
            let v = self.temp_val(*arg).into_int_value();
            let idx_const = i64ty.const_int(i as u64, false);
            // SAFETY (rust): GEP into a fixed-size i64 array.
            let gep = unsafe {
                self.builder.build_gep(
                    args_arr_ty,
                    args_alloca,
                    &[i64ty.const_zero(), idx_const],
                    &format!("sd.args.{i}"),
                )
            }
            .map_err(map_err)?;
            self.builder.build_store(gep, v).map_err(map_err)?;
        }

        // Stack-local fn-ptr array for the fallback chain. Each entry
        // is a ptrtoint of an extern declaration on the chain method
        // body symbol.
        let chain_len = fallback_chain.len();
        let chain_arr_ty = i64ty.array_type(chain_len.max(1) as u32);
        // Entry-block alloca (see build_entry_alloca / args_alloca above).
        let chain_alloca = self.build_entry_alloca(chain_arr_ty, "sd.chain")?;
        for (i, body_name) in fallback_chain.iter().enumerate() {
            // Ensure an extern function declaration exists for the
            // body symbol — same shape as `callee_fn`. Same three-tier
            // fallback as the resolved method above: this-module
            // function table → already-declared extern → declare a
            // fresh extern backed by `find_method_body_ptr`.
            let fn_val = if let Some(&f) = self.function_map.get(body_name.as_str()) {
                f
            } else if let Some(existing) = self.module.get_function(body_name) {
                existing
            } else if nod_runtime::find_method_body_ptr(body_name).is_some() {
                let params: Vec<BasicMetadataTypeEnum<'ctx>> =
                    (0..args.len()).map(|_| i64ty.into()).collect();
                let fty = i64ty.fn_type(&params, false);
                self.module.add_function(body_name, fty, None)
            } else {
                return Err(CodegenError::UnknownCallee {
                    in_function: self.func.name.clone(),
                    callee: body_name.clone(),
                });
            };
            let fn_ptr_as_ptr = fn_val.as_global_value().as_pointer_value();
            let fn_ptr_as_int = self
                .builder
                .build_ptr_to_int(fn_ptr_as_ptr, i64ty, &format!("sd.chain.{i}.int"))
                .map_err(map_err)?;
            let idx_const = i64ty.const_int(i as u64, false);
            // SAFETY (rust): GEP into a fixed-size i64 array.
            let gep = unsafe {
                self.builder.build_gep(
                    chain_arr_ty,
                    chain_alloca,
                    &[i64ty.const_zero(), idx_const],
                    &format!("sd.chain.{i}"),
                )
            }
            .map_err(map_err)?;
            self.builder.build_store(gep, fn_ptr_as_int).map_err(map_err)?;
        }

        // Push the chain frame: nod_push_sealed_chain_frame(
        //   args_ptr, arity, methods_ptr, chain_len
        // ).
        let push_fn = match self.module.get_function(NOD_PUSH_SEALED_CHAIN_SYMBOL) {
            Some(f) => f,
            None => {
                let void_ty = self.ctx.void_type();
                let ty = void_ty.fn_type(
                    &[
                        ptr_ty.into(),
                        i64ty.into(),
                        ptr_ty.into(),
                        i64ty.into(),
                    ],
                    false,
                );
                self.module.add_function(NOD_PUSH_SEALED_CHAIN_SYMBOL, ty, None)
            }
        };
        let pop_fn = match self.module.get_function(NOD_POP_SEALED_CHAIN_SYMBOL) {
            Some(f) => f,
            None => {
                let void_ty = self.ctx.void_type();
                let ty = void_ty.fn_type(&[], false);
                self.module.add_function(NOD_POP_SEALED_CHAIN_SYMBOL, ty, None)
            }
        };

        self.builder
            .build_call(
                push_fn,
                &[
                    args_alloca.into(),
                    i64ty.const_int(arity as u64, false).into(),
                    chain_alloca.into(),
                    i64ty.const_int(chain_len as u64, false).into(),
                ],
                "sd.push",
            )
            .map_err(map_err)?;

        // Now the direct call. Bracket with the safepoint pair so any
        // allocation inside the method body is observed by GC.
        let emitted = self.begin_emitted_safepoint(SafepointKind::SealedDirectCall, safepoint_roots)?;
        let arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = args
            .iter()
            .map(|a| self.temp_val(*a).into())
            .collect();
        let name = format!("sd.t{}", dst.0);
        let site = self
            .builder
            .build_call(callee_fn, &arg_vals, &name)
            .map_err(map_err)?;
        self.end_emitted_safepoint(&emitted)?;
        let result = site.try_as_basic_value().basic();

        // Pop the chain frame on the success path. (Panic-unwind from
        // the body would skip this — Sprint 19 wires structured
        // unwinding through `nod_resume`; for Sprint 15 the runtime
        // RAII guard isn't replicated here. Documented in DEFERRED.)
        self.builder
            .build_call(pop_fn, &[], "sd.pop")
            .map_err(map_err)?;

        let _ = generic_name;
        Ok(result)
    }

    fn emit_const(
        &self,
        dst: TempId,
        v: &ConstValue,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let ty = self.func.temp_type(dst);
        Ok(match v {
            ConstValue::Integer(n) => match ty {
                // Sprint 09: `<integer>` literals lower to *tagged*
                // fixnums. Bit 0 = 0, value shifted left by 1.
                TypeEstimate::Integer | TypeEstimate::Top | TypeEstimate::Bottom => {
                    let tagged = ((*n as i64) as u64).wrapping_shl(1);
                    self.ctx.i64_type().const_int(tagged, false).into()
                }
                TypeEstimate::Character => self
                    .ctx
                    .i32_type()
                    .const_int(*n as u64, true)
                    .into(),
                // Sprint 10: a `Boolean` temp arrived via
                // `ConstValue::Integer` — treat 0 as #f, anything else
                // as #t. Sprint 38b: materialise as a load from the
                // per-module external global for `#t` / `#f` so
                // bitcode round-trips across processes without baking
                // a process-local address.
                TypeEstimate::Boolean => {
                    if *n != 0 {
                        self.load_imm_true()?.into()
                    } else {
                        self.load_imm_false()?.into()
                    }
                }
                _ => {
                    let tagged = ((*n as i64) as u64).wrapping_shl(1);
                    self.ctx.i64_type().const_int(tagged, false).into()
                }
            },
            ConstValue::Float(f) => match ty {
                TypeEstimate::SingleFloat => self.ctx.f32_type().const_float(*f).into(),
                _ => self.ctx.f64_type().const_float(*f).into(),
            },
            // Sprint 10: `#t` / `#f` are pinned heap-shape singletons.
            // Sprint 38b: emit a load from the per-module external
            // global so cross-process replay binds the symbol to the
            // new process's runtime address.
            ConstValue::Bool(b) => {
                if *b {
                    self.load_imm_true()?.into()
                } else {
                    self.load_imm_false()?.into()
                }
            }
            ConstValue::Char(c) => self
                .ctx
                .i32_type()
                .const_int(*c as u64, false)
                .into(),
            // Sprint 10: a `<byte-string>` literal is interned in the
            // process-global literal pool. Sprint 38c — load the Word
            // from the per-module external global keyed by content
            // instead of baking the interned Word's bits as an i64
            // constant. The bitcode round-trips across processes.
            ConstValue::String(s) => self.load_string_literal(s)?.into(),
            // Sprint 10: `Unit` constants lower to `nil`'s pinned
            // singleton address. Sprint 38b: load from the per-module
            // external global instead of baking the raw bits.
            ConstValue::Unit => self.load_imm_nil()?.into(),
            // Sprint 12: raw 64-bit constant — used by lowering to
            // bake non-runtime-address Word patterns (block ids, the
            // null sentinel, exit-procedure fixnums). Sprint 38c
            // narrowed the class-ref / string / symbol literal cases
            // to dedicated variants; only fixnum-shaped Word patterns
            // and the legacy raw-bits scaffolding flow here today.
            ConstValue::WordBits(bits) => {
                self.ctx.i64_type().const_int(*bits, false).into()
            }
            // Sprint 38c — class-metadata pointer reference. Loads via
            // per-module external global; ORs `| 1` if tagged.
            ConstValue::ClassMetadataPtr { class_id, tagged } => {
                self.load_class_metadata(*class_id, *tagged)?.into()
            }
            // Sprint 38c — interned `<byte-string>` literal reference.
            // Loads the Word's bits via per-module external global.
            ConstValue::StringLiteralRef(text) => self.load_string_literal(text)?.into(),
            // Sprint 38c — interned `<symbol>` literal reference.
            ConstValue::SymbolLiteralRef(name) => self.load_symbol_literal(name)?.into(),
            // Sprint 38d — Win32 stub-entry pointer reference. Loads the
            // entry pointer via per-module external global; the slot's
            // contents are bound to a freshly-allocated, eagerly-resolved
            // `ApiStubEntry` in the current process by the JIT-link path.
            ConstValue::StubEntryRef {
                dll,
                symbol,
                signature_bytes,
            } => self.load_stub_entry(dll, symbol, signature_bytes)?.into(),
        })
    }

    fn emit_primop(
        &self,
        op: PrimOp,
        args: &[TempId],
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let b = self.builder;
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let int2 = || -> (inkwell::values::IntValue<'ctx>, inkwell::values::IntValue<'ctx>) {
            (self.temp_val(args[0]).into_int_value(), self.temp_val(args[1]).into_int_value())
        };
        let float2 = || -> (inkwell::values::FloatValue<'ctx>, inkwell::values::FloatValue<'ctx>) {
            (
                self.temp_val(args[0]).into_float_value(),
                self.temp_val(args[1]).into_float_value(),
            )
        };
        let i64ty = self.ctx.i64_type();
        let one_i64 = i64ty.const_int(1, false);
        Ok(match op {
            // Tagged-stable: bit 0 of each operand is 0, so the sum's
            // bit 0 is also 0 and the value bits land exactly where
            // (a+b)<<1 expects them.
            PrimOp::AddInt => {
                let (l, r) = int2();
                b.build_int_add(l, r, "tag.add").map_err(map_err)?.into()
            }
            PrimOp::SubInt => {
                let (l, r) = int2();
                b.build_int_sub(l, r, "tag.sub").map_err(map_err)?.into()
            }
            // (a<<1) * (b<<1) = (a*b) << 2 — one bit too many. Shift
            // one operand right by 1 (arithmetic to preserve sign of
            // negative fixnums) before multiplying.
            PrimOp::MulInt => {
                let (l, r) = int2();
                let r_unshifted = b
                    .build_right_shift(r, one_i64, true, "tag.mul.untag")
                    .map_err(map_err)?;
                b.build_int_mul(l, r_unshifted, "tag.mul")
                    .map_err(map_err)?
                    .into()
            }
            // sdiv doesn't compose with shifted operands the way mul
            // does. Untag both, divide, retag: (a/b) << 1.
            PrimOp::DivInt => {
                let (l, r) = int2();
                let lu = b
                    .build_right_shift(l, one_i64, true, "tag.div.lu")
                    .map_err(map_err)?;
                let ru = b
                    .build_right_shift(r, one_i64, true, "tag.div.ru")
                    .map_err(map_err)?;
                let q = b
                    .build_int_signed_div(lu, ru, "tag.div.q")
                    .map_err(map_err)?;
                b.build_left_shift(q, one_i64, "tag.div.retag")
                    .map_err(map_err)?
                    .into()
            }
            PrimOp::ModInt | PrimOp::RemInt => {
                let (l, r) = int2();
                let lu = b
                    .build_right_shift(l, one_i64, true, "tag.rem.lu")
                    .map_err(map_err)?;
                let ru = b
                    .build_right_shift(r, one_i64, true, "tag.rem.ru")
                    .map_err(map_err)?;
                let m = b
                    .build_int_signed_rem(lu, ru, "tag.rem.m")
                    .map_err(map_err)?;
                b.build_left_shift(m, one_i64, "tag.rem.retag")
                    .map_err(map_err)?
                    .into()
            }
            // 0 - (a<<1) = (-a)<<1; bit 0 stays 0.
            PrimOp::NegInt => {
                let v = self.temp_val(args[0]).into_int_value();
                let zero = v.get_type().const_zero();
                b.build_int_sub(zero, v, "tag.neg").map_err(map_err)?.into()
            }
            PrimOp::AddFloat => {
                let (l, r) = float2();
                b.build_float_add(l, r, "fadd").map_err(map_err)?.into()
            }
            PrimOp::SubFloat => {
                let (l, r) = float2();
                b.build_float_sub(l, r, "fsub").map_err(map_err)?.into()
            }
            PrimOp::MulFloat => {
                let (l, r) = float2();
                b.build_float_mul(l, r, "fmul").map_err(map_err)?.into()
            }
            PrimOp::DivFloat => {
                let (l, r) = float2();
                b.build_float_div(l, r, "fdiv").map_err(map_err)?.into()
            }
            PrimOp::NegFloat => {
                let v = self.temp_val(args[0]).into_float_value();
                b.build_float_neg(v, "fneg").map_err(map_err)?.into()
            }
            // Coerce a tagged fixnum (value << 1) to an f64: arithmetic
            // shift right by 1 to untag, then signed-int-to-float.
            PrimOp::IntToFloat => {
                let v = self.temp_val(args[0]).into_int_value();
                let one_i64 = self.ctx.i64_type().const_int(1, false);
                let untag = b
                    .build_right_shift(v, one_i64, true, "itof.untag")
                    .map_err(map_err)?;
                b.build_signed_int_to_float(untag, self.ctx.f64_type(), "itof")
                    .map_err(map_err)?
                    .into()
            }
            // Clear bit 0 (the pointer tag) to recover a raw metadata pointer
            // from a tagged class-value Word.
            PrimOp::StripTag => {
                let v = self.temp_val(args[0]).into_int_value();
                let mask = self.ctx.i64_type().const_int(!1u64, false);
                b.build_and(v, mask, "striptag").map_err(map_err)?.into()
            }
            // Comparisons run directly on tagged operands — the
            // shift-left-by-1 preserves the signed ordering. The i1
            // result is zext'd to i64 and shifted to land in the
            // tagged-boolean encoding (#t = 2, #f = 0).
            PrimOp::EqInt => {
                let (l, r) = int2();
                let i1 = b
                    .build_int_compare(IntPredicate::EQ, l, r, "tag.eq")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::NeInt => {
                let (l, r) = int2();
                let i1 = b
                    .build_int_compare(IntPredicate::NE, l, r, "tag.ne")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::LtInt => {
                let (l, r) = int2();
                let i1 = b
                    .build_int_compare(IntPredicate::SLT, l, r, "tag.lt")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::GtInt => {
                let (l, r) = int2();
                let i1 = b
                    .build_int_compare(IntPredicate::SGT, l, r, "tag.gt")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::LeInt => {
                let (l, r) = int2();
                let i1 = b
                    .build_int_compare(IntPredicate::SLE, l, r, "tag.le")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::GeInt => {
                let (l, r) = int2();
                let i1 = b
                    .build_int_compare(IntPredicate::SGE, l, r, "tag.ge")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::EqFloat => {
                let (l, r) = float2();
                let i1 = b
                    .build_float_compare(FloatPredicate::OEQ, l, r, "fcmp.eq")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::LtFloat => {
                let (l, r) = float2();
                let i1 = b
                    .build_float_compare(FloatPredicate::OLT, l, r, "fcmp.lt")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::GtFloat => {
                let (l, r) = float2();
                let i1 = b
                    .build_float_compare(FloatPredicate::OGT, l, r, "fcmp.gt")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::LeFloat => {
                let (l, r) = float2();
                let i1 = b
                    .build_float_compare(FloatPredicate::OLE, l, r, "fcmp.le")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            PrimOp::GeFloat => {
                let (l, r) = float2();
                let i1 = b
                    .build_float_compare(FloatPredicate::OGE, l, r, "fcmp.ge")
                    .map_err(map_err)?;
                self.retag_bool(i1)?
            }
            // Sprint 10: booleans are pinned-pointer Words; pointer
            // identity (not bit patterns) carries truth. Untag to i1,
            // apply the LLVM bool op, retag.
            PrimOp::BoolAnd => {
                let (l, r) = int2();
                let li = self.untag_bool_to_i1(l)?;
                let ri = self.untag_bool_to_i1(r)?;
                let both = b.build_and(li, ri, "bool.and.i1").map_err(map_err)?;
                self.retag_bool(both)?
            }
            PrimOp::BoolOr => {
                let (l, r) = int2();
                let li = self.untag_bool_to_i1(l)?;
                let ri = self.untag_bool_to_i1(r)?;
                let either = b.build_or(li, ri, "bool.or.i1").map_err(map_err)?;
                self.retag_bool(either)?
            }
            PrimOp::BoolNot => {
                let v = self.temp_val(args[0]).into_int_value();
                let vi = self.untag_bool_to_i1(v)?;
                let one_i1 = self.ctx.bool_type().const_int(1, false);
                let not = b.build_xor(vi, one_i1, "bool.not.i1").map_err(map_err)?;
                self.retag_bool(not)?
            }
        })
    }

    fn emit_type_check(
        &mut self,
        value: TempId,
        class: &ClassCheck,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let b = self.builder;
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let v = self.temp_val(value).into_int_value();
        match class {
            // `<integer>` test: bit 0 == 0. AND with 1, compare to 0.
            ClassCheck::Integer => {
                let one = i64ty.const_int(1, false);
                let masked = b.build_and(v, one, "tcheck.int.mask").map_err(map_err)?;
                let zero = i64ty.const_zero();
                let i1 = b
                    .build_int_compare(IntPredicate::EQ, masked, zero, "tcheck.int.cmp")
                    .map_err(map_err)?;
                self.retag_bool(i1)
            }
            // Wrapper-tagged class tests against seed classes. The helper
            // returns an i1 that's true iff `v` is pointer-tagged AND
            // its wrapper carries the target class id.
            ClassCheck::Boolean => self.emit_wrapper_class_check(v, ClassId::BOOLEAN),
            ClassCheck::String => self.emit_wrapper_class_check(v, ClassId::BYTE_STRING),
            ClassCheck::Symbol => self.emit_wrapper_class_check(v, ClassId::SYMBOL),
            ClassCheck::Vector => self.emit_wrapper_class_check(v, ClassId::SIMPLE_OBJECT_VECTOR),
            ClassCheck::Character => self.emit_wrapper_class_check(v, ClassId::CHARACTER),
            ClassCheck::EmptyList => self.emit_wrapper_class_check(v, ClassId::EMPTY_LIST),
            ClassCheck::UserClass { id, .. } => {
                // Sprint 12: call the runtime `nod_is_instance_of`
                // helper. Walks the value's class CPL — handles both
                // user classes and seed-class supers.
                let is_inst_fn = match self.module.get_function(NOD_IS_INSTANCE_OF_SYMBOL) {
                    Some(f) => f,
                    None => {
                        let ty = i64ty.fn_type(&[i64ty.into(), i64ty.into()], false);
                        self.module.add_function(NOD_IS_INSTANCE_OF_SYMBOL, ty, None)
                    }
                };
                let class_const = i64ty.const_int(*id as u64, false);
                let site = self
                    .builder
                    .build_call(is_inst_fn, &[v.into(), class_const.into()], "tcheck.user")
                    .map_err(map_err)?;
                Ok(site
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| CodegenError::Builder("nod_is_instance_of returned void".into()))?)
            }
            // Collection-classes lever — `<vector>` / `<array>` /
            // `<simple-vector>`: a `<simple-object-vector>` IS one of these
            // but its CPL doesn't carry the abstract id (SOV is a seed
            // class on `<object>`), so neither check alone suffices. Emit
            //   (exact-id SOV fast path) OR nod_is_instance_of(v, id)
            // — the fast path catches real SOVs, the CPL walk catches
            // `<bit-vector>` / user `make(<vector>)`-results. Combine on i1
            // and retag once.
            ClassCheck::VectorOrUserClass { id, .. } => {
                // SOV fast path → tagged-bool Word.
                let sov_word = self
                    .emit_wrapper_class_check(v, ClassId::SIMPLE_OBJECT_VECTOR)?
                    .into_int_value();
                let sov_i1 = self.untag_bool_to_i1(sov_word)?;
                // CPL walk → tagged-bool Word.
                let is_inst_fn = match self.module.get_function(NOD_IS_INSTANCE_OF_SYMBOL) {
                    Some(f) => f,
                    None => {
                        let ty = i64ty.fn_type(&[i64ty.into(), i64ty.into()], false);
                        self.module.add_function(NOD_IS_INSTANCE_OF_SYMBOL, ty, None)
                    }
                };
                let class_const = i64ty.const_int(*id as u64, false);
                let site = self
                    .builder
                    .build_call(
                        is_inst_fn,
                        &[v.into(), class_const.into()],
                        "tcheck.vec.cpl",
                    )
                    .map_err(map_err)?;
                let cpl_word = site
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CodegenError::Builder("nod_is_instance_of returned void".into())
                    })?
                    .into_int_value();
                let cpl_i1 = self.untag_bool_to_i1(cpl_word)?;
                let either = b
                    .build_or(sov_i1, cpl_i1, "tcheck.vec.or")
                    .map_err(map_err)?;
                self.retag_bool(either)
            }
            // Anything else: stub. Sprint 12 wires class-id dispatch.
            ClassCheck::Unsupported { .. } => Ok(i64ty.const_zero().into()),
        }
    }

    /// Read the runtime class id of a Word as an i64. Sprint 13's
    /// inline-cache code uses this to compute the cache key; the same
    /// logic appears in `emit_wrapper_class_check`, factored here so
    /// both stay in sync.
    ///
    /// Pseudocode:
    /// ```text
    ///   is_ptr = (w & 1) == 1
    ///   addr = select(is_ptr, w & ~1, fallback_addr)
    ///   wrapper = load i64, ptr addr
    ///   class_id = wrapper & 0xFFFF_FFFF
    ///   fixnum_class = <integer>'s class id (1)
    ///   result = select(is_ptr, class_id, fixnum_class)
    /// ```
    ///
    /// For fixnum inputs the wrapper load is redirected through a
    /// pinned safe address (the `#f` singleton) so it can't fault, and
    /// the final select substitutes the integer class id.
    fn emit_word_class_id(
        &self,
        v: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        let b = self.builder;
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let one = i64ty.const_int(1, false);
        let not_one = i64ty.const_int(!1_u64, false);

        let tag_bits = b.build_and(v, one, "cls.id.tag").map_err(map_err)?;
        let is_ptr_i1 = b
            .build_int_compare(IntPredicate::EQ, tag_bits, one, "cls.id.isptr")
            .map_err(map_err)?;

        // Fallback address for fixnum inputs: pinned `#f` singleton's
        // wrapper. Used purely to keep the load fault-free; the result
        // is overwritten by the integer-class select below.
        // Sprint 38b: load the wrapper address from the per-module
        // external global so cross-process replay binds the symbol to
        // the new process's runtime wrapper.
        let fallback_const = self.load_imm_false_wrapper()?;

        let masked = b.build_and(v, not_one, "cls.id.untag").map_err(map_err)?;
        let addr_i64 = b
            .build_select(is_ptr_i1, masked, fallback_const, "cls.id.addr")
            .map_err(map_err)?
            .into_int_value();
        let addr_ptr = b
            .build_int_to_ptr(
                addr_i64,
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                "cls.id.ptr",
            )
            .map_err(map_err)?;
        let wrapper = b
            .build_load(i64ty, addr_ptr, "cls.id.wrap")
            .map_err(map_err)?
            .into_int_value();
        let class_mask = i64ty.const_int(0xFFFF_FFFF, false);
        let ptr_class = b
            .build_and(wrapper, class_mask, "cls.id.ptr_class")
            .map_err(map_err)?;
        let integer_class = i64ty.const_int(ClassId::INTEGER.0 as u64, false);
        let result = b
            .build_select(is_ptr_i1, ptr_class, integer_class, "cls.id.value")
            .map_err(map_err)?
            .into_int_value();
        Ok(result)
    }

    /// Emit the wrapper-load-and-class-compare sequence.
    ///
    /// In LLVM-IR shape (the actual IR uses i64 throughout):
    ///
    /// ```text
    ///   ; v is the tagged Word.
    ///   is_ptr = (v & 1) == 1                           ; pointer-tag check
    ///   if !is_ptr -> result = 0 (false)
    ///   addr = v & ~1                                    ; untag
    ///   wrapper = load i64, i64* addr                    ; read header
    ///   class_id = wrapper & 0xFFFF_FFFF                 ; low 32 bits
    ///   class_eq = class_id == target_class_id
    ///   result = (is_ptr AND class_eq) << 1              ; tagged boolean
    /// ```
    ///
    /// The pointer-tag check is preserved with an `AND` so a fixnum
    /// input short-circuits to false — we deliberately do NOT branch
    /// (no new basic block) because every operand to `AND` is a pure
    /// computation. For fixnum inputs we redirect the load through a
    /// pinned fallback address (the `#f` singleton wrapper) so the
    /// load itself never faults; the AND with `is_ptr_i1` then drops
    /// whatever class the fallback happens to carry. This trades a
    /// load on the false path for branchless lowering.
    fn emit_wrapper_class_check(
        &self,
        v: inkwell::values::IntValue<'ctx>,
        target_class: ClassId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let b = self.builder;
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let one = i64ty.const_int(1, false);
        let not_one = i64ty.const_int(!1_u64, false);

        // is_ptr_i1 = (v & 1) == 1
        let tag_bits = b.build_and(v, one, "tcheck.cls.tag").map_err(map_err)?;
        let is_ptr_i1 = b
            .build_int_compare(IntPredicate::EQ, tag_bits, one, "tcheck.cls.isptr")
            .map_err(map_err)?;

        // For fixnum inputs we still need a valid address to load from.
        // Replace with a known-safe pinned address (the `#f` singleton
        // wrapper). The AND with `is_ptr_i1` at the end discards the
        // load result anyway, but the load itself must not fault.
        // Sprint 38b: load the wrapper address from the per-module
        // external global.
        let fallback_const = self.load_imm_false_wrapper()?;

        let masked = b.build_and(v, not_one, "tcheck.cls.untag").map_err(map_err)?;
        let addr_i64 = b
            .build_select(is_ptr_i1, masked, fallback_const, "tcheck.cls.addr")
            .map_err(map_err)?
            .into_int_value();
        let addr_ptr = b
            .build_int_to_ptr(
                addr_i64,
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                "tcheck.cls.ptr",
            )
            .map_err(map_err)?;
        let wrapper = b
            .build_load(i64ty, addr_ptr, "tcheck.cls.wrap")
            .map_err(map_err)?
            .into_int_value();
        let class_mask = i64ty.const_int(0xFFFF_FFFF, false);
        let class_id = b
            .build_and(wrapper, class_mask, "tcheck.cls.id")
            .map_err(map_err)?;
        let target = i64ty.const_int(target_class.0 as u64, false);
        let class_eq_i1 = b
            .build_int_compare(IntPredicate::EQ, class_id, target, "tcheck.cls.eq")
            .map_err(map_err)?;
        let both = b.build_and(is_ptr_i1, class_eq_i1, "tcheck.cls.both").map_err(map_err)?;
        self.retag_bool(both)
    }

    fn emit_direct_call(
        &mut self,
        callee: &str,
        args: &[TempId],
        dst: TempId,
        safepoint_roots: &[TempId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        // Sprint 10 builtin: `format-out` lowers to a call into the
        // `nod_format_out` extern shim. Args are padded with zeros so
        // the C ABI sees a fixed (u64, u64, u64, u64) -> u64.
        if callee == "format-out" {
            return self.emit_format_out_call(args, dst, safepoint_roots);
        }
        if callee == "%make" {
            return self.emit_make_call(args, dst, safepoint_roots);
        }
        // Sprint 14: runtime-resolved `next-method` shims. Take no args
        // and return a single `i64` (a Dylan Word).
        if callee == NOD_NEXT_METHOD_SYMBOL || callee == NOD_HAS_NEXT_METHOD_SYMBOL {
            return self.emit_next_method_call(callee, dst, safepoint_roots);
        }
        // Sprint 16: `<pair>` / `<list>` builtins. The synthetic
        // callee names emitted by `nod-sema::lower::lower_list_builtin`
        // map one-to-one onto the runtime shims declared above.
        if let Some((sym, arity)) = match callee {
            "%pair-alloc" => Some((NOD_PAIR_ALLOC_SYMBOL, 2)),
            "%pair-head" => Some((NOD_PAIR_HEAD_SYMBOL, 1)),
            "%pair-tail" => Some((NOD_PAIR_TAIL_SYMBOL, 1)),
            "%empty?" => Some((NOD_EMPTY_P_SYMBOL, 1)),
            "%nil" => Some((NOD_NIL_SYMBOL, 0)),
            _ => None,
        } {
            return self.emit_list_builtin_call(sym, arity, args, dst, safepoint_roots);
        }
        // Sprint 19: `signal` / `condition-message` / `block`
        // orchestration builtins. Each is a fixed-arity extern shim
        // resolved to a `nod_runtime` symbol at JIT-engine creation.
        if let Some((sym, arity)) = match callee {
            "%signal" => Some((NOD_SIGNAL_SYMBOL, 1)),
            "%condition-message" => Some((NOD_CONDITION_MESSAGE_SYMBOL, 1)),
            "%make-exit-procedure" => Some((NOD_MAKE_EXIT_PROCEDURE_SYMBOL, 1)),
            "%invoke-exit" => Some((NOD_INVOKE_EXIT_SYMBOL, 2)),
            "%run-block" => Some((NOD_RUN_BLOCK_SYMBOL, 9)), // block_id + 8 captured
            "error" => Some((NOD_ERROR_SYMBOL, 1)),
            "add!" => Some((NOD_STRETCHY_VECTOR_PUSH_SYMBOL, 2)),
            "write-to-string" => Some((NOD_WRITE_TO_STRING_SYMBOL, 1)),
            "integer-to-string" => Some((NOD_INTEGER_TO_STRING_SYMBOL, 1)),
            _ => None,
        } {
            return self.emit_list_builtin_call(sym, arity, args, dst, safepoint_roots);
        }
        // Sprint 20b: `%`-prefixed collection / FIP / primitive ops.
        // The lower pass emits the runtime symbol verbatim as the
        // DirectCall callee (see `LOWER_PRIMITIVE_TABLE` in
        // `nod-sema/src/lower.rs`); we match it against the
        // SPRINT_20B_PRIMITIVES table and emit the same fixed-arity
        // i64-shaped call shape used by the Sprint 16 list builtins.
        if let Some((sym, arity)) = sprint_20b_primitive(callee) {
            return self.emit_list_builtin_call(sym, arity, args, dst, safepoint_roots);
        }
        // Sprint 20b: when the callee isn't in this module's function
        // table, check the process-global dispatch registry — stdlib
        // methods (and any other JIT-resident method body) register a
        // body-fn-name → address mapping via `add_method_named`. If
        // one matches the callee, declare it as an extern in this
        // module with the standard `(i64, …) -> i64` ABI; `jit.rs`
        // resolves the symbol via `find_method_body_ptr` at engine
        // creation. This unblocks dispatch-resolver emissions of
        // `DirectCall { callee: "<generic>$<spec>" }` whose body lives
        // in a different JIT module (e.g. `nod-sema::stdlib`).
        let callee_fn = if let Some(&f) = self.function_map.get(callee) {
            f
        } else if let Some(existing) = self.module.get_function(callee) {
            // Already declared as an extern earlier in this emission.
            existing
        } else if nod_runtime::find_method_body_ptr(callee).is_some() {
            // Declare an extern with the standard method ABI. Methods
            // take their args as `u64` and return `u64`. The actual
            // arity matches `args.len()`.
            let i64ty = self.ctx.i64_type();
            let params: Vec<BasicMetadataTypeEnum<'ctx>> =
                (0..args.len()).map(|_| i64ty.into()).collect();
            let fty = i64ty.fn_type(&params, false);
            self.module.add_function(callee, fty, None)
        } else {
            return Err(CodegenError::UnknownCallee {
                in_function: self.func.name.clone(),
                callee: callee.to_string(),
            });
        };
        let name = format!("call.t{}", dst.0);
        // Sprint 11b: bracket the call with register/unregister pairs.
        // Sprint 12-shaped DirectCalls into user-defined Dylan functions
        // may transitively allocate (a callee that calls `make`); we
        // protect across every such call. Pure-arith Sprint 07-style
        // direct calls have an empty `safepoint_roots` list (the
        // liveness pass produced no live pointer-shaped temps) and the
        // bracketing is a no-op.
        let emitted = self.begin_emitted_safepoint(SafepointKind::DirectCall, safepoint_roots)?;
        // Coerce each arg to the callee's DECLARED param width. A
        // `<character>` temp is i32; the callee may want i64 (a runtime
        // shim or an `<object>`-typed Dylan param) or i32 (a Dylan
        // function with a `<character>`-typed param). Sext / trunc as
        // needed so the call site matches the signature exactly.
        let param_types = callee_fn.get_type().get_param_types();
        let arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let want = param_types.get(i).copied();
                Ok(self.coerce_arg_to_param(*a, want)?.into())
            })
            .collect::<Result<_, CodegenError>>()?;
        let site = self
            .builder
            .build_call(callee_fn, &arg_vals, &name)
            .map_err(|e| CodegenError::Builder(e.to_string()))?;
        self.end_emitted_safepoint(&emitted)?;
        Ok(site.try_as_basic_value().basic())
    }

    /// Sprint 12 builtin: `make` lowers to a call into the `nod_make`
    /// extern shim. Lowering produces args in the shape:
    /// `[class_metadata_addr_const, name_0, value_0, name_1, value_1, ...]`.
    /// We pad to the fixed `2 + 2*MAKE_MAX_KW_PAIRS` arity expected by
    /// `nod_make`, inserting `kw_count` as the second argument.
    fn emit_make_call(
        &mut self,
        args: &[TempId],
        dst: TempId,
        safepoint_roots: &[TempId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let max_pairs = nod_runtime::MAKE_MAX_KW_PAIRS;
        if args.is_empty() {
            return Err(CodegenError::Builder(
                "make: missing class argument".to_string(),
            ));
        }
        let pair_count = (args.len() - 1) / 2;
        if pair_count > max_pairs {
            return Err(CodegenError::Builder(format!(
                "make: Sprint 12 supports up to {max_pairs} keyword pairs, got {pair_count}"
            )));
        }
        let make_fn = match self.module.get_function(NOD_MAKE_SYMBOL) {
            Some(f) => f,
            None => {
                // Signature: (class_ptr, kw_count, [name, val] * max_pairs) -> u64
                let mut params: Vec<BasicMetadataTypeEnum<'ctx>> =
                    Vec::with_capacity(2 + 2 * max_pairs);
                params.push(i64ty.into());
                params.push(i64ty.into());
                for _ in 0..max_pairs {
                    params.push(i64ty.into());
                    params.push(i64ty.into());
                }
                let ty = i64ty.fn_type(&params, false);
                self.module.add_function(NOD_MAKE_SYMBOL, ty, None)
            }
        };
        let name = format!("call.t{}", dst.0);
        let emitted = self.begin_emitted_safepoint(SafepointKind::DirectCall, safepoint_roots)?;
        let zero = i64ty.const_zero();
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            Vec::with_capacity(2 + 2 * max_pairs);
        // First arg: class metadata pointer (constant Word baked into the IR).
        let class_arg = self.temp_val(args[0]).into_int_value();
        call_args.push(class_arg.into());
        // Second arg: kw_count.
        call_args.push(i64ty.const_int(pair_count as u64, false).into());
        // Then 2*max_pairs slots for name/value pairs.
        for i in 0..max_pairs {
            if i < pair_count {
                let name_t = args[1 + 2 * i];
                let val_t = args[1 + 2 * i + 1];
                call_args.push(self.temp_val(name_t).into());
                call_args.push(self.temp_val(val_t).into());
            } else {
                call_args.push(zero.into());
                call_args.push(zero.into());
            }
        }
        let site = self
            .builder
            .build_call(make_fn, &call_args, &name)
            .map_err(map_err)?;
        self.end_emitted_safepoint(&emitted)?;
        Ok(site.try_as_basic_value().basic())
    }

    fn emit_format_out_call(
        &mut self,
        args: &[TempId],
        dst: TempId,
        safepoint_roots: &[TempId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        if args.is_empty() || args.len() > 4 {
            return Err(CodegenError::Builder(format!(
                "format-out: Sprint 10 supports arity 1..=4, got {}",
                args.len()
            )));
        }
        // Lookup or declare the extern.
        let fmt_fn = match self.module.get_function(FORMAT_OUT_SYMBOL) {
            Some(f) => f,
            None => {
                let ty = i64ty.fn_type(
                    &[i64ty.into(), i64ty.into(), i64ty.into(), i64ty.into()],
                    false,
                );
                self.module.add_function(FORMAT_OUT_SYMBOL, ty, None)
            }
        };
        let name = format!("call.t{}", dst.0);
        let emitted = self.begin_emitted_safepoint(SafepointKind::DirectCall, safepoint_roots)?;
        // Pad to four i64 args, zero-filling missing slots.
        let zero = i64ty.const_zero();
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(4);
        for i in 0..4 {
            if let Some(t) = args.get(i) {
                call_args.push(self.temp_val(*t).into());
            } else {
                call_args.push(zero.into());
            }
        }
        let site = self
            .builder
            .build_call(fmt_fn, &call_args, &name)
            .map_err(map_err)?;
        self.end_emitted_safepoint(&emitted)?;
        Ok(site.try_as_basic_value().basic())
    }

    /// Sprint 14: lower a call to one of the runtime `next-method`
    /// shims (`nod_next_method` / `nod_has_next_method`). Both take no
    /// args and return a single `i64` (Dylan Word). The dispatch
    /// chain frame the shim consults is pushed by `nod_dispatch`
    /// when the current method was reached through dispatch with more
    /// than one applicable method.
    fn emit_next_method_call(
        &mut self,
        callee: &str,
        dst: TempId,
        safepoint_roots: &[TempId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let fn_ = match self.module.get_function(callee) {
            Some(f) => f,
            None => {
                // Signature: `i64 ()`.
                let ty = i64ty.fn_type(&[], false);
                self.module.add_function(callee, ty, None)
            }
        };
        let name = format!("call.t{}", dst.0);
        let emitted = self.begin_emitted_safepoint(SafepointKind::DirectCall, safepoint_roots)?;
        let site = self
            .builder
            .build_call(fn_, &[], &name)
            .map_err(map_err)?;
        self.end_emitted_safepoint(&emitted)?;
        Ok(site.try_as_basic_value().basic())
    }

    /// Sprint 16: lower a `<pair>` / `<list>` builtin to a call into the
    /// matching runtime shim. `sym` is the JIT-side symbol (one of
    /// `NOD_PAIR_ALLOC_SYMBOL` etc.); `arity` is the number of `i64`
    /// arguments the shim takes (and equals `args.len()` after the
    /// lowering pass's arity check). The return is always a single
    /// `i64` (a Dylan Word).
    ///
    /// All five shims observe the standard safepoint discipline so a
    /// minor GC fired during `pair(...)` finds every live pointer-shaped
    /// temp in the registered-roots table.
    fn emit_list_builtin_call(
        &mut self,
        sym: &str,
        arity: usize,
        args: &[TempId],
        dst: TempId,
        safepoint_roots: &[TempId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        debug_assert_eq!(args.len(), arity, "Sprint 16 builtin arity mismatch");
        let fn_ = match self.module.get_function(sym) {
            Some(f) => f,
            None => {
                let params: Vec<BasicMetadataTypeEnum<'ctx>> =
                    (0..arity).map(|_| i64ty.into()).collect();
                let ty = i64ty.fn_type(&params, false);
                self.module.add_function(sym, ty, None)
            }
        };
        let name = format!("call.t{}", dst.0);
        let emitted = self.begin_emitted_safepoint(SafepointKind::DirectCall, safepoint_roots)?;
        let call_args: Vec<BasicMetadataValueEnum<'ctx>> = args
            .iter()
            .map(|a| Ok(self.temp_val_as_word(*a)?.into()))
            .collect::<Result<_, CodegenError>>()?;
        let site = self
            .builder
            .build_call(fn_, &call_args, &name)
            .map_err(map_err)?;
        self.end_emitted_safepoint(&emitted)?;
        Ok(site.try_as_basic_value().basic())
    }

    /// Lower a `LoadSlot` IR node. Untag the instance Word, GEP to
    /// the slot byte offset, load 8 bytes.
    fn emit_load_slot(
        &self,
        instance: TempId,
        offset: usize,
        _slot_type: SlotTypeKind,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let b = self.builder;
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let i8ty = self.ctx.i8_type();
        let inst = self.temp_val(instance).into_int_value();
        let not_one = i64ty.const_int(!1_u64, false);
        let addr_i64 = b
            .build_and(inst, not_one, "slot.load.untag")
            .map_err(map_err)?;
        let base_ptr = b
            .build_int_to_ptr(
                addr_i64,
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                "slot.load.base",
            )
            .map_err(map_err)?;
        let offset_const = i64ty.const_int(offset as u64, false);
        // SAFETY-equivalent: GEP at byte offset.
        let slot_ptr = unsafe {
            b.build_in_bounds_gep(i8ty, base_ptr, &[offset_const], "slot.load.gep")
                .map_err(map_err)?
        };
        let val = b
            .build_load(i64ty, slot_ptr, "slot.load.val")
            .map_err(map_err)?;
        Ok(val)
    }

    /// Lower a `StoreSlot` IR node. Untag the instance, GEP to the
    /// slot, store the value, then call into `nod_runtime::write_barrier`
    /// (which marks the card if the slot is in old).
    fn emit_store_slot(
        &mut self,
        instance: TempId,
        offset: usize,
        value: TempId,
        _slot_type: SlotTypeKind,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let b = self.builder;
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let i8ty = self.ctx.i8_type();
        let inst = self.temp_val(instance).into_int_value();
        let val = self.temp_val(value).into_int_value();
        let not_one = i64ty.const_int(!1_u64, false);
        let addr_i64 = b
            .build_and(inst, not_one, "slot.store.untag")
            .map_err(map_err)?;
        let base_ptr = b
            .build_int_to_ptr(
                addr_i64,
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                "slot.store.base",
            )
            .map_err(map_err)?;
        let offset_const = i64ty.const_int(offset as u64, false);
        let slot_ptr = unsafe {
            b.build_in_bounds_gep(i8ty, base_ptr, &[offset_const], "slot.store.gep")
                .map_err(map_err)?
        };
        b.build_store(slot_ptr, val).map_err(map_err)?;
        // Card-mark via the runtime. The runtime helper takes the slot
        // pointer's raw address and the new value; we call the
        // `nod_runtime_card_mark` shim (defined alongside the others).
        let card_fn = match self.module.get_function(NOD_CARD_MARK_SYMBOL) {
            Some(f) => f,
            None => {
                let ty = self
                    .ctx
                    .void_type()
                    .fn_type(&[i64ty.into()], false);
                self.module.add_function(NOD_CARD_MARK_SYMBOL, ty, None)
            }
        };
        let slot_addr_const = b
            .build_ptr_to_int(slot_ptr, i64ty, "slot.store.addr")
            .map_err(map_err)?;
        b.build_call(card_fn, &[slot_addr_const.into()], "slot.store.barrier")
            .map_err(map_err)?;
        // The "value" of a store is the stored value (allows `slot := v`
        // to be used as an expression in Dylan).
        Ok(val.into())
    }

    /// Sprint 13: lower a `Dispatch` IR node into an inline-cache
    /// check + fast-path direct call + slow-path runtime dispatch.
    ///
    /// IR shape per call site (with N = args.len(), capped at 8):
    ///
    /// ```text
    ///   ; ----- inline cache check -----
    ///   %r           = args[0]                              ; receiver word
    ///   %r_class     = call <emit_word_class_id>(%r)        ; i64 class id
    ///   %cached_cls  = load atomic i64, ptr @cache_class_for_site_N, monotonic
    ///   %cached_mthd = load atomic i64, ptr @cache_method_for_site_N, monotonic
    ///   %cached_gen  = load atomic i64, ptr @cache_gen_for_site_N, monotonic
    ///   %gen         = load atomic i64, ptr @generic.GENERIC_NAME.generation, monotonic
    ///   %class_ok    = icmp eq i64 %r_class, %cached_cls
    ///   %gen_ok      = icmp eq i64 %gen, %cached_gen
    ///   %nonzero     = icmp ne i64 %cached_cls, 0
    ///   %cache_hit   = and i1 %class_ok, (and i1 %gen_ok, %nonzero)
    ///   br i1 %cache_hit, label %fast_call, label %slow_call
    ///
    /// fast_call:
    ///   ; bump hits counter
    ///   atomicrmw add ptr @cache_hits_for_site_N, i64 1 monotonic
    ///   %fn = inttoptr i64 %cached_mthd to ptr
    ///   %r_fast = call i64 %fn(args...)
    ///   br label %dispatch_done
    ///
    /// slow_call:
    ///   ; nod_dispatch bumps misses and updates the cache itself
    ///   %r_slow = call i64 @nod_dispatch(generic_ptr, cache_slot_ptr,
    ///                                    arity, a0..a7)
    ///   br label %dispatch_done
    ///
    /// dispatch_done:
    ///   %result = phi i64 [ %r_fast, %fast_call ], [ %r_slow, %slow_call ]
    /// ```
    ///
    /// The cache slot's address is baked into the IR as an `i64`
    /// constant. The slot lives in the runtime's static area (pinned
    /// for the process lifetime) so subsequent re-JITs of unrelated
    /// modules don't clobber it.
    ///
    /// Safepoint roots are spilled+registered ONCE before the diamond
    /// and unregistered+reloaded at the join — both paths share the
    /// same root protection, and the post-dispatch `temps[i]` mapping
    /// reflects any GC evacuation that ran in either branch.
    fn emit_dispatch(
        &mut self,
        generic_name: &str,
        args: &[TempId],
        dst: TempId,
        safepoint_roots: &[TempId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());

        if args.is_empty() {
            return Err(CodegenError::Builder(format!(
                "dispatch: `{generic_name}` has no arguments (need at least one receiver)"
            )));
        }
        if args.len() > 8 {
            return Err(CodegenError::Builder(format!(
                "dispatch: `{generic_name}` arity {} exceeds Sprint 13 cap of 8 (lifted in Sprint 23 c-ffi)",
                args.len()
            )));
        }

        // Sprint 38e — reserve a module-wide unique site id for this
        // call site (previously per-function; see
        // `ModuleCodegenCtx::next_dispatch_site_id` doc).
        let emitted = self.begin_emitted_safepoint(SafepointKind::Dispatch, safepoint_roots)?;
        let site_id = emitted.site_id;

        // Sprint 38e — load the GenericFunction + CacheSlot pointers
        // through per-module external globals instead of baking them
        // as per-process `i64` constants. Pre-Sprint-38e this was:
        //
        //   let generic_ptr_const = i64ty.const_int(<runtime-address>, false);
        //   let cache_slot_const = i64ty.const_int(<runtime-address>, false);
        //
        // which pinned the bitcode to the process that produced it.
        // Now codegen emits one `load i64, ptr @nod_generic__<key>__<sanitised-name>`
        // and one `load i64, ptr @nod_cache_slot__<key>__<site_id>`;
        // the JIT-link path binds each global to a stable
        // `&'static u64` slot whose contents are the current process's
        // pointer bits (see `nod_runtime::cache_slot_slot_addr` and
        // `generic_function_slot_addr`).
        //
        // The five field-offset addresses (cache class/method/gen/hits,
        // generic generation) become `add i64 <loaded-ptr>, <offset>`.
        // LLVM at `-O2` CSEs the load + adds and folds the per-field
        // GEP-equivalents — see user memory note "LLVM does most
        // optimization". No manual caching needed on the Rust side.
        let cache_slot_loaded = self.load_cache_slot_ptr(site_id)?;
        let generic_ptr_loaded = self.load_generic_function_ptr(generic_name)?;

        // Field offsets within `CacheSlot` — must match the
        // `#[repr(C)]` layout in `nod_runtime::dispatch`.
        let cache_class_off = i64ty.const_int(offset_of_cache_slot_class() as u64, false);
        let cache_method_off = i64ty.const_int(offset_of_cache_slot_method() as u64, false);
        let cache_gen_off = i64ty.const_int(offset_of_cache_slot_generation() as u64, false);
        let cache_hits_off = i64ty.const_int(offset_of_cache_slot_hits() as u64, false);
        let generic_gen_off = i64ty.const_int(offset_of_generic_generation() as u64, false);

        let cache_class_addr = self
            .builder
            .build_int_add(
                cache_slot_loaded,
                cache_class_off,
                &format!("disp.s{site_id}.cache_class.addr"),
            )
            .map_err(map_err)?;
        let cache_method_addr = self
            .builder
            .build_int_add(
                cache_slot_loaded,
                cache_method_off,
                &format!("disp.s{site_id}.cache_method.addr"),
            )
            .map_err(map_err)?;
        let cache_gen_addr = self
            .builder
            .build_int_add(
                cache_slot_loaded,
                cache_gen_off,
                &format!("disp.s{site_id}.cache_gen.addr"),
            )
            .map_err(map_err)?;
        let cache_hits_addr = self
            .builder
            .build_int_add(
                cache_slot_loaded,
                cache_hits_off,
                &format!("disp.s{site_id}.cache_hits.addr"),
            )
            .map_err(map_err)?;
        // misses bumped by nod_dispatch itself.

        // GenericFunction.generation is the second field; compute its
        // address relative to the loaded generic pointer.
        let generic_gen_addr = self
            .builder
            .build_int_add(
                generic_ptr_loaded,
                generic_gen_off,
                &format!("disp.s{site_id}.gen.addr"),
            )
            .map_err(map_err)?;

        let arity_const = i64ty.const_int(args.len() as u64, false);
        // The slow-path arg shape passes the same `cache_slot` and
        // `generic` pointers `nod_dispatch` already takes; these are
        // the same loaded values we used above (no second load needed —
        // they're SSA values that LLVM will keep around as long as
        // they're used).
        let generic_ptr_const = generic_ptr_loaded;
        let cache_slot_const = cache_slot_loaded;

        // Snapshot arg SSA values after the safepoint bracket is in
        // place so every call path consistently reads the current temp
        // binding at the actual call site. `<character>` args are i32;
        // the dispatch ABI (nod_dispatch + the method-fn fast path) is
        // uniform i64, so widen each arg to the i64 Word shape here.
        let arg_vals: Vec<inkwell::values::IntValue<'ctx>> = args
            .iter()
            .map(|t| Ok(self.temp_val_as_word(*t)?.into_int_value()))
            .collect::<Result<_, CodegenError>>()?;
        let receiver = arg_vals[0];

        // ---- Compute r_class (i64) for the cache key. ----
        let r_class = self.emit_word_class_id(receiver)?;

        // ---- Load cache + generic generation (monotonic atomics). ----
        let cache_class_ptr = self
            .builder
            .build_int_to_ptr(cache_class_addr, ptr_ty, &format!("disp.s{site_id}.cache_class.ptr"))
            .map_err(map_err)?;
        let cache_class_load = self
            .builder
            .build_load(i64ty, cache_class_ptr, &format!("disp.s{site_id}.cache_class"))
            .map_err(map_err)?;
        let cache_class_inst = cache_class_load
            .as_instruction_value()
            .expect("load is an instruction");
        cache_class_inst
            .set_alignment(8)
            .map_err(|e| CodegenError::Builder(format!("set_alignment: {e}")))?;
        cache_class_inst
            .set_atomic_ordering(inkwell::AtomicOrdering::Monotonic)
            .map_err(|e| CodegenError::Builder(format!("atomic ordering: {e}")))?;

        let cache_method_ptr = self
            .builder
            .build_int_to_ptr(cache_method_addr, ptr_ty, &format!("disp.s{site_id}.cache_method.ptr"))
            .map_err(map_err)?;
        let cache_method_load = self
            .builder
            .build_load(i64ty, cache_method_ptr, &format!("disp.s{site_id}.cache_method"))
            .map_err(map_err)?;
        let cache_method_inst = cache_method_load
            .as_instruction_value()
            .expect("load is an instruction");
        cache_method_inst
            .set_alignment(8)
            .map_err(|e| CodegenError::Builder(format!("set_alignment: {e}")))?;
        cache_method_inst
            .set_atomic_ordering(inkwell::AtomicOrdering::Monotonic)
            .map_err(|e| CodegenError::Builder(format!("atomic ordering: {e}")))?;

        let cache_gen_ptr = self
            .builder
            .build_int_to_ptr(cache_gen_addr, ptr_ty, &format!("disp.s{site_id}.cache_gen.ptr"))
            .map_err(map_err)?;
        let cache_gen_load = self
            .builder
            .build_load(i64ty, cache_gen_ptr, &format!("disp.s{site_id}.cache_gen"))
            .map_err(map_err)?;
        let cache_gen_inst = cache_gen_load
            .as_instruction_value()
            .expect("load is an instruction");
        cache_gen_inst
            .set_alignment(8)
            .map_err(|e| CodegenError::Builder(format!("set_alignment: {e}")))?;
        cache_gen_inst
            .set_atomic_ordering(inkwell::AtomicOrdering::Monotonic)
            .map_err(|e| CodegenError::Builder(format!("atomic ordering: {e}")))?;

        let generic_gen_ptr = self
            .builder
            .build_int_to_ptr(generic_gen_addr, ptr_ty, &format!("disp.s{site_id}.gen.ptr"))
            .map_err(map_err)?;
        let generic_gen_load = self
            .builder
            .build_load(i64ty, generic_gen_ptr, &format!("disp.s{site_id}.gen"))
            .map_err(map_err)?;
        let generic_gen_inst = generic_gen_load
            .as_instruction_value()
            .expect("load is an instruction");
        generic_gen_inst
            .set_alignment(8)
            .map_err(|e| CodegenError::Builder(format!("set_alignment: {e}")))?;
        generic_gen_inst
            .set_atomic_ordering(inkwell::AtomicOrdering::Monotonic)
            .map_err(|e| CodegenError::Builder(format!("atomic ordering: {e}")))?;

        let cached_class = cache_class_load.into_int_value();
        let cached_method = cache_method_load.into_int_value();
        let cached_gen = cache_gen_load.into_int_value();
        let generic_gen = generic_gen_load.into_int_value();

        // ---- Cache-hit predicate. ----
        let class_ok = self
            .builder
            .build_int_compare(IntPredicate::EQ, r_class, cached_class, &format!("disp.s{site_id}.class_ok"))
            .map_err(map_err)?;
        let gen_ok = self
            .builder
            .build_int_compare(IntPredicate::EQ, generic_gen, cached_gen, &format!("disp.s{site_id}.gen_ok"))
            .map_err(map_err)?;
        let zero_i64 = i64ty.const_zero();
        let nonzero_class = self
            .builder
            .build_int_compare(IntPredicate::NE, cached_class, zero_i64, &format!("disp.s{site_id}.nonzero_class"))
            .map_err(map_err)?;
        let cg = self
            .builder
            .build_and(class_ok, gen_ok, &format!("disp.s{site_id}.class_and_gen"))
            .map_err(map_err)?;
        let cache_hit = self
            .builder
            .build_and(cg, nonzero_class, &format!("disp.s{site_id}.cache_hit"))
            .map_err(map_err)?;

        // ---- Begin safepoint for both branches. ----
        // Create fast/slow/done blocks. Append AFTER the current end
        // (don't disturb pre-created DFM blocks).
        let fast_bb = self
            .ctx
            .append_basic_block(self.llvm_fn, &format!("disp.s{site_id}.fast_call"));
        let slow_bb = self
            .ctx
            .append_basic_block(self.llvm_fn, &format!("disp.s{site_id}.slow_call"));
        let done_bb = self
            .ctx
            .append_basic_block(self.llvm_fn, &format!("disp.s{site_id}.dispatch_done"));

        self.builder
            .build_conditional_branch(cache_hit, fast_bb, slow_bb)
            .map_err(map_err)?;

        // ---- Fast path: bump hits, transmute cached_method, call. ----
        self.builder.position_at_end(fast_bb);
        let cache_hits_ptr = self
            .builder
            .build_int_to_ptr(cache_hits_addr, ptr_ty, &format!("disp.s{site_id}.hits.ptr"))
            .map_err(map_err)?;
        let one_i64 = i64ty.const_int(1, false);
        let _hits_rmw = self
            .builder
            .build_atomicrmw(
                inkwell::AtomicRMWBinOp::Add,
                cache_hits_ptr,
                one_i64,
                inkwell::AtomicOrdering::Monotonic,
            )
            .map_err(|e| CodegenError::Builder(format!("atomicrmw: {e}")))?;

        // Build function-type for the cached method call.
        let mut fn_param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(args.len());
        for _ in 0..args.len() {
            fn_param_tys.push(i64ty.into());
        }
        let cached_fn_ty: inkwell::types::FunctionType<'ctx> =
            i64ty.fn_type(&fn_param_tys, false);
        let cached_fn_ptr = self
            .builder
            .build_int_to_ptr(cached_method, ptr_ty, &format!("disp.s{site_id}.fast.fn"))
            .map_err(map_err)?;
        let fast_call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            arg_vals.iter().map(|v| (*v).into()).collect();
        let fast_call_site = self
            .builder
            .build_indirect_call(
                cached_fn_ty,
                cached_fn_ptr,
                &fast_call_args,
                &format!("disp.s{site_id}.fast.call"),
            )
            .map_err(map_err)?;
        let fast_result = fast_call_site
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError::Builder("dispatch fast-call returned void".into()))?;
        // Snapshot the current block (fast_bb) for the phi's incoming.
        let fast_pred = self
            .builder
            .get_insert_block()
            .expect("builder positioned");
        self.builder
            .build_unconditional_branch(done_bb)
            .map_err(map_err)?;

        // ---- Slow path: call nod_dispatch with the cache slot. ----
        self.builder.position_at_end(slow_bb);
        let disp_fn = match self.module.get_function(NOD_DISPATCH_SYMBOL) {
            Some(f) => f,
            None => {
                // (generic_ptr, cache_slot_ptr, arity, 8 * args) -> i64
                let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(11);
                params.push(i64ty.into()); // generic_ptr
                params.push(i64ty.into()); // cache_slot_ptr
                params.push(i64ty.into()); // arity
                for _ in 0..8 {
                    params.push(i64ty.into());
                }
                let ty = i64ty.fn_type(&params, false);
                self.module.add_function(NOD_DISPATCH_SYMBOL, ty, None)
            }
        };
        let zero = i64ty.const_zero();
        let mut slow_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(11);
        slow_args.push(generic_ptr_const.into());
        slow_args.push(cache_slot_const.into());
        slow_args.push(arity_const.into());
        for i in 0..8 {
            if let Some(v) = arg_vals.get(i) {
                slow_args.push((*v).into());
            } else {
                slow_args.push(zero.into());
            }
        }
        let slow_call_site = self
            .builder
            .build_call(disp_fn, &slow_args, &format!("disp.s{site_id}.slow.call"))
            .map_err(map_err)?;
        let slow_result = slow_call_site
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError::Builder("dispatch slow-call returned void".into()))?;
        let slow_pred = self
            .builder
            .get_insert_block()
            .expect("builder positioned");
        self.builder
            .build_unconditional_branch(done_bb)
            .map_err(map_err)?;

        // ---- Done block: phi the result + unregister roots. ----
        self.builder.position_at_end(done_bb);
        let phi = self
            .builder
            .build_phi(i64ty, &format!("disp.s{site_id}.result"))
            .map_err(map_err)?;
        phi.add_incoming(&[(&fast_result, fast_pred), (&slow_result, slow_pred)]);

        self.end_emitted_safepoint(&emitted)?;
        let _ = dst;

        Ok(Some(phi.as_basic_value()))
    }

    fn emit_terminator(&mut self, t: &Terminator) -> Result<(), CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        match t {
            Terminator::Return { value: None } => {
                self.builder.build_return(None).map_err(map_err)?;
            }
            Terminator::Return { value: Some(t) } => {
                let v = self.temp_val(*t);
                self.builder.build_return(Some(&v)).map_err(map_err)?;
            }
            Terminator::If { cond, then_block, else_block } => {
                // Sprint 10: every Dylan value except `#f` is true.
                // Compare against the pinned `#f` singleton address.
                let c64 = self.temp_val(*cond).into_int_value();
                let c1 = self.untag_bool_to_i1(c64)?;
                let then_bb = self.blocks[then_block];
                let else_bb = self.blocks[else_block];
                self.note_successor_entry_temps(*then_block);
                self.note_successor_entry_temps(*else_block);
                self.builder
                    .build_conditional_branch(c1, then_bb, else_bb)
                    .map_err(map_err)?;
            }
            Terminator::Jump { target, args } => {
                let target_bb = self.blocks[target];
                // GAP-007: resolve TempIds to SSA values BEFORE branching
                // so the phi captures the value that flowed out of THIS
                // predecessor. If we deferred to end-of-function, a
                // subsequent safepoint reload would clobber
                // `self.temps[t]` and the phi would receive the wrong
                // (likely body-block-local) reload SSA on every edge.
                let arg_vals: Vec<BasicValueEnum<'ctx>> =
                    args.iter().map(|t| self.temp_val(*t)).collect();
                // Snapshot the actual source block at this exact insert
                // point — phi nodes need that, not the logical DFM
                // BlockId (which would resolve to its starting LLVM BB
                // even after intermediate splits).
                let current = self
                    .builder
                    .get_insert_block()
                    .expect("builder positioned");
                self.note_successor_entry_temps(*target);
                self.builder
                    .build_unconditional_branch(target_bb)
                    .map_err(map_err)?;
                self.pending_incoming.push((*target, current, arg_vals));
            }
        }
        Ok(())
    }

    fn temp_val(&self, t: TempId) -> BasicValueEnum<'ctx> {
        *self
            .temps
            .get(&t)
            .unwrap_or_else(|| panic!("undefined TempId({})", t.0))
    }

    /// Coerce a temp value to the i64 runtime-call ABI. `<character>`
    /// lowers to a raw i32 (its code), but every runtime shim takes its
    /// args as i64 Words. Sign-extend the i32 char to i64 at the call
    /// boundary so the LLVM call site matches the `(i64, …) -> i64`
    /// declaration (otherwise the verifier rejects `i32` vs `i64`). All
    /// other temps are already i64-shaped and pass through unchanged.
    fn temp_val_as_word(&self, t: TempId) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let v = self.temp_val(t);
        if let BasicValueEnum::IntValue(iv) = v {
            if iv.get_type().get_bit_width() == 32 {
                let widened = self
                    .builder
                    .build_int_s_extend(iv, self.ctx.i64_type(), "char.widen")
                    .map_err(map_err)?;
                return Ok(widened.into());
            }
        }
        Ok(v)
    }

    /// Coerce an argument temp to a callee's declared LLVM param type.
    /// The only mismatch in practice is integer width: a `<character>`
    /// temp is i32 while many callees declare i64 params (runtime shims,
    /// `<object>`-typed Dylan params), and conversely an i64 integer temp
    /// may flow into a `<character>`-typed (i32) Dylan param. Sign-extend
    /// or truncate to match. `want == None` (variadic / unknown) falls
    /// back to the i64 Word ABI via `temp_val_as_word`.
    fn coerce_arg_to_param(
        &self,
        t: TempId,
        want: Option<BasicMetadataTypeEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let Some(BasicMetadataTypeEnum::IntType(want_int)) = want else {
            // No declared int param type — default to the i64 Word ABI.
            return self.temp_val_as_word(t);
        };
        let v = self.temp_val(t);
        let BasicValueEnum::IntValue(iv) = v else {
            return Ok(v);
        };
        let have_bits = iv.get_type().get_bit_width();
        let want_bits = want_int.get_bit_width();
        if have_bits == want_bits {
            Ok(v)
        } else if have_bits < want_bits {
            Ok(self
                .builder
                .build_int_s_extend(iv, want_int, "arg.sext")
                .map_err(map_err)?
                .into())
        } else {
            Ok(self
                .builder
                .build_int_truncate(iv, want_int, "arg.trunc")
                .map_err(map_err)?
                .into())
        }
    }

    /// Coerce a runtime-call result (always an i64 Word) back to the dst
    /// temp's register shape. The only mismatch is a `<character>` dst:
    /// its register type is i32, but the call returned i64 (the `%code-char`
    /// primitive returns its code in the low 32 bits of an i64). Truncate
    /// to i32 so downstream char temps stay same-width. All other dst
    /// types are i64-shaped and the value passes through unchanged.
    fn coerce_call_result(
        &self,
        dst: TempId,
        v: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        if matches!(self.func.temp_type(dst), TypeEstimate::Character) {
            if let BasicValueEnum::IntValue(iv) = v {
                if iv.get_type().get_bit_width() != 32 {
                    let narrowed = self
                        .builder
                        .build_int_truncate(iv, self.ctx.i32_type(), "char.trunc")
                        .map_err(map_err)?;
                    return Ok(narrowed.into());
                }
            }
        }
        Ok(v)
    }

    /// Produce the canonical block-entry SSA value for function parameter
    /// `p`, used by both the entry-block seed and the per-block restart.
    /// For GC-typed params with a home alloca, emits a fresh `load
    /// %p.home` at the current insert position (which the caller has
    /// positioned at the block's start). For non-GC params, returns the
    /// raw `get_nth_param` LLVM value (no reload needed — these are
    /// non-relocating).
    ///
    /// GAP-011 invariant A: every cross-block use of a GC-typed param
    /// MUST flow through this load. The previous codegen rebound
    /// `state.temps[p]` to `get_nth_param` directly, silently undoing
    /// any in-block safepoint reload at the next block boundary.
    fn rebind_param(&self, p: TempId) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        if let Some(&home) = self.param_homes.get(&p) {
            let v = self
                .builder
                .build_load(
                    self.ctx.i64_type(),
                    home,
                    &format!("p.t{}.reload", p.0),
                )
                .map_err(map_err)?;
            return Ok(v);
        }
        // Non-GC param (or a `param_homes` miss for a TempId that isn't a
        // function parameter — the caller should only invoke this for
        // entries in `func.params`). Recover the LLVM-arg SSA value by
        // index.
        let i = self
            .func
            .params
            .iter()
            .position(|&x| x == p)
            .expect("rebind_param: TempId is a function parameter");
        Ok(self
            .llvm_fn
            .get_nth_param(i as u32)
            .expect("parameter index in range"))
    }

    /// Spill each safepoint root temp into an entry-block-resident
    /// `alloca` slot. Both surfaces use the precise per-site safepoint
    /// map exclusively (JIT via `nod_jit_begin_safepoint`, AOT via
    /// `nod_aot_begin_safepoint` + slot_base); no per-slot
    /// `nod_register_root` calls are emitted.
    fn begin_safepoint(
        &mut self,
        site_id: u64,
        roots: &[TempId],
    ) -> Result<Vec<SafepointSlot<'ctx>>, CodegenError> {
        // ── JIT / AOT asymmetry note ─────────────────────────────────────
        // AOT path: nod_aot_begin_safepoint → (spill roots) → call →
        //           nod_aot_verify_safepoint → nod_aot_end_safepoint.
        //   The extra verify step is gated on NOD_AOT_VERIFY_SAFEPOINTS at
        //   runtime (OnceLock), so it is a no-op in normal IDE use and only
        //   active in debug / verification runs.
        // JIT path: nod_jit_begin_safepoint → (spill roots) → call →
        //           nod_jit_end_safepoint. No verify step.
        //   JIT safepoints are validated via the per-call-site registry
        //   in nod_jit_require_safepoint; the extra AOT verify step is
        //   redundant there and is intentionally omitted.
        // ─────────────────────────────────────────────────────────────────
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let mut rented: Vec<SafepointSlot<'ctx>> = Vec::with_capacity(roots.len());
        if self.emits_image_safepoint_runtime_checks() {
            let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
            let slot_base_ptr = if roots.is_empty() {
                ptr_ty.const_null()
            } else {
                self.builder
                    .build_pointer_cast(
                        self.safepoint_slot_base_ptr()?,
                        ptr_ty,
                        &format!("gc.s{site_id}.aot_slot_base"),
                    )
                    .map_err(map_err)?
            };
            self.builder
                .build_call(
                    self.get_or_declare_aot_begin_safepoint(),
                    &[
                        self.ctx.i64_type().const_int(site_id, false).into(),
                        self.ctx
                            .i64_type()
                            .const_int(roots.len() as u64, false)
                            .into(),
                        slot_base_ptr.into(),
                    ],
                    &format!("gc.s{site_id}.begin"),
                )
                .map_err(map_err)?;
        }
        for (i, t) in roots.iter().enumerate() {
            let cur = self.temp_val(*t);
            let slot = self.rent_safepoint_slot(i)?;
            self.builder.build_store(slot, cur).map_err(map_err)?;
            rented.push(SafepointSlot {
                temp: *t,
                slot,
                home: FrameHome::SafepointPoolSlot(i),
            });
        }
        if self.emits_jit_precise_safepoints() && !rented.is_empty() {
            let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
            let slot_base_ptr = self
                .builder
                .build_pointer_cast(
                    self.safepoint_slot_base_ptr()?,
                    ptr_ty,
                    &format!("gc.s{site_id}.slot_base"),
                )
                .map_err(map_err)?;
            self.builder
                .build_call(
                    self.get_or_declare_jit_begin_safepoint(),
                    &[
                        self.ctx.i64_type().const_int(self.mctx.key.0[0], false).into(),
                        self.ctx.i64_type().const_int(site_id, false).into(),
                        slot_base_ptr.into(),
                    ],
                    &format!("gc.s{site_id}.jit_begin"),
                )
                .map_err(map_err)?;
        }
        if self.emits_image_safepoint_runtime_checks() {
            self.builder
                .build_call(
                    self.get_or_declare_aot_verify_safepoint(),
                    &[self.ctx.i64_type().const_int(site_id, false).into()],
                    &format!("gc.s{site_id}.verify"),
                )
                .map_err(map_err)?;
        }
        // Save current insert position for the caller — the caller
        // continues emitting the actual call into the same block.
        Ok(rented)
    }

    /// Emit the post-call GC root cleanup. Both surfaces close the
    /// precise safepoint frame (JIT via `nod_jit_end_safepoint`, AOT
    /// via `nod_aot_end_safepoint`) and reload potentially-relocated
    /// Words from the slot slab.
    fn end_safepoint(
        &mut self,
        site_id: u64,
        rented: &[SafepointSlot<'ctx>],
    ) -> Result<(), CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        if self.emits_jit_precise_safepoints() && !rented.is_empty() {
            self.builder
                .build_call(
                    self.get_or_declare_jit_end_safepoint(),
                    &[
                        self.ctx.i64_type().const_int(self.mctx.key.0[0], false).into(),
                        self.ctx.i64_type().const_int(site_id, false).into(),
                    ],
                    &format!("gc.s{site_id}.jit_end"),
                )
                .map_err(map_err)?;
        }

        if self.emits_image_safepoint_runtime_checks() {
            self.builder
                .build_call(
                    self.get_or_declare_aot_end_safepoint(),
                    &[self.ctx.i64_type().const_int(site_id, false).into()],
                    &format!("gc.s{site_id}.end"),
                )
                .map_err(map_err)?;
        }
        if !rented.is_empty() {
            for slot_info in rented.iter() {
                let reloaded = self
                    .builder
                    .build_load(
                        i64ty,
                        slot_info.slot,
                        &format!("gc.s{site_id}.reload.t{}", slot_info.temp.0),
                    )
                    .map_err(map_err)?;
                self.temps.insert(slot_info.temp, reloaded);
                // GAP-011 invariant B (2026-05-30): if the reloaded temp
                // is a function parameter with a home alloca, refresh the
                // home so subsequent block-entry `rebind_param` loads
                // pick up the post-GC address. Forgetting this would
                // mean the home stays at its entry-block seed value (the
                // raw `%fn.argN`) forever, and every block transition
                // would re-install the pre-GC pointer — the exact GAP-011
                // crash. Empty hashmap for stdlib functions with no
                // GC-typed params; no-op.
                if let Some(&home) = self.param_homes.get(&slot_info.temp) {
                    self.builder
                        .build_store(home, reloaded)
                        .map_err(map_err)?;
                }
            }
        }
        Ok(())
    }

    fn emits_image_safepoint_runtime_checks(&self) -> bool {
        matches!(
            self.mctx.install_surface,
            SafepointInstallSurface::ImageCodeText
        )
    }

    fn emits_jit_precise_safepoints(&self) -> bool {
        matches!(
            self.mctx.install_surface,
            SafepointInstallSurface::InMemoryCodeText
        )
    }

    /// Allocate `ty` in the function's **entry block** (before its first
    /// instruction), regardless of the current insert position, then
    /// restore the builder to where it was.
    ///
    /// Why this matters: an `alloca` emitted in a loop-body block is, at
    /// `-O0`, executed on *every* iteration and the stack space is only
    /// reclaimed when the function returns (LLVM never auto-inserts
    /// `stackrestore`). A scratch buffer allocated inside a hot loop
    /// therefore grows the frame without bound until the stack overflows
    /// — this was GAP-010: `emit_sealed_direct_call`'s `sd.args` / `sd.chain`
    /// slabs leaked ~32 bytes per iteration of a `size(keep)`-style sealed
    /// call, blowing the 1 MB stack after ~31 K iterations (and the
    /// resulting `STATUS_STACK_OVERFLOW` left no stack for the unhandled
    /// exception filter, so it died silently). Entry-block allocas run
    /// exactly once per activation and are safely reused across iterations,
    /// since the buffer is consumed synchronously before the next one.
    /// Mirrors `init_safepoint_slot_slab`'s placement.
    fn build_entry_alloca(
        &self,
        ty: impl inkwell::types::BasicType<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let saved = self.builder.get_insert_block();
        let entry_bb = self
            .llvm_fn
            .get_first_basic_block()
            .expect("function has at least one block");
        if let Some(first_inst) = entry_bb.get_first_instruction() {
            self.builder.position_before(&first_inst);
        } else {
            self.builder.position_at_end(entry_bb);
        }
        let p = self.builder.build_alloca(ty, name).map_err(map_err)?;
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
        Ok(p)
    }

    fn init_safepoint_slot_slab(&mut self) -> Result<(), CodegenError> {
        if self.safepoint_slot_capacity == 0 {
            return Ok(());
        }
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let i64ty = self.ctx.i64_type();
        let slab_ty = i64ty.array_type(self.safepoint_slot_capacity as u32);
        let saved = self.builder.get_insert_block();
        let entry_bb = self
            .llvm_fn
            .get_first_basic_block()
            .expect("function has at least one block");
        if let Some(first_inst) = entry_bb.get_first_instruction() {
            self.builder.position_before(&first_inst);
        } else {
            self.builder.position_at_end(entry_bb);
        }
        let slab = self
            .builder
            .build_alloca(slab_ty, "gc.root.slots")
            .map_err(map_err)?;
        self.safepoint_slot_slab = Some(slab);
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
        Ok(())
    }

    fn safepoint_slot_base_ptr(&self) -> Result<inkwell::values::PointerValue<'ctx>, CodegenError> {
        let slab = self
            .safepoint_slot_slab
            .expect("safepoint slot slab missing");
        let i64ty = self.ctx.i64_type();
        let slab_ty = i64ty.array_type(self.safepoint_slot_capacity as u32);
        unsafe {
            self.builder.build_gep(
                slab_ty,
                slab,
                &[i64ty.const_zero(), i64ty.const_zero()],
                "gc.root.slots.base",
            )
        }
        .map_err(|e| CodegenError::Builder(e.to_string()))
    }

    /// Return the i-th alloca slot from the function's safepoint pool,
    /// growing the pool if needed. Allocas are placed in the entry
    /// block (LLVM prefers entry-block allocas so the register
    /// allocator can scalarise / promote them on the fast path).
    fn rent_safepoint_slot(
        &mut self,
        idx: usize,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CodegenError> {
        assert!(idx < self.safepoint_slot_capacity, "safepoint slot index out of range");
        let i64ty = self.ctx.i64_type();
        let slab = self
            .safepoint_slot_slab
            .expect("safepoint slot slab missing");
        let slab_ty = i64ty.array_type(self.safepoint_slot_capacity as u32);
        unsafe {
            self.builder.build_gep(
                slab_ty,
                slab,
                &[i64ty.const_zero(), i64ty.const_int(idx as u64, false)],
                &format!("gc.root.slot.{idx}"),
            )
        }
        .map_err(|e| CodegenError::Builder(e.to_string()))
    }

    // Sprint 45e retired `get_or_declare_register_root` and
    // `get_or_declare_unregister_root` — the JIT and AOT call paths
    // now use the precise per-callsite safepoint maps exclusively (see
    // `get_or_declare_jit_begin_safepoint` /
    // `get_or_declare_aot_begin_safepoint` below). The
    // `NOD_REGISTER_ROOT_SYMBOL` / `NOD_UNREGISTER_ROOT_SYMBOL` string
    // constants remain — codegen smoke tests assert NEGATIVELY that
    // emitted IR no longer contains them, guarding against accidental
    // resurrection of the legacy scheme.

    fn get_or_declare_jit_begin_safepoint(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(NOD_JIT_BEGIN_SAFEPOINT_SYMBOL) {
            return f;
        }
        let i64ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let ty = self
            .ctx
            .void_type()
            .fn_type(&[i64ty.into(), i64ty.into(), ptr_ty.into()], false);
        self.module
            .add_function(NOD_JIT_BEGIN_SAFEPOINT_SYMBOL, ty, None)
    }

    fn get_or_declare_jit_end_safepoint(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(NOD_JIT_END_SAFEPOINT_SYMBOL) {
            return f;
        }
        let i64ty = self.ctx.i64_type();
        let ty = self
            .ctx
            .void_type()
            .fn_type(&[i64ty.into(), i64ty.into()], false);
        self.module
            .add_function(NOD_JIT_END_SAFEPOINT_SYMBOL, ty, None)
    }

    fn get_or_declare_aot_begin_safepoint(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(NOD_AOT_BEGIN_SAFEPOINT_SYMBOL) {
            return f;
        }
        let i64ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let ty = self
            .ctx
            .void_type()
            .fn_type(&[i64ty.into(), i64ty.into(), ptr_ty.into()], false);
        self.module
            .add_function(NOD_AOT_BEGIN_SAFEPOINT_SYMBOL, ty, None)
    }

    fn get_or_declare_aot_verify_safepoint(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(NOD_AOT_VERIFY_SAFEPOINT_SYMBOL) {
            return f;
        }
        let i64ty = self.ctx.i64_type();
        let ty = self.ctx.void_type().fn_type(&[i64ty.into()], false);
        self.module
            .add_function(NOD_AOT_VERIFY_SAFEPOINT_SYMBOL, ty, None)
    }

    fn get_or_declare_aot_end_safepoint(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(NOD_AOT_END_SAFEPOINT_SYMBOL) {
            return f;
        }
        let i64ty = self.ctx.i64_type();
        let ty = self.ctx.void_type().fn_type(&[i64ty.into()], false);
        self.module
            .add_function(NOD_AOT_END_SAFEPOINT_SYMBOL, ty, None)
    }

    fn get_or_declare_safepoint_poll(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(NOD_SAFEPOINT_POLL_SYMBOL) {
            return f;
        }
        let ty = self.ctx.void_type().fn_type(&[], false);
        self.module
            .add_function(NOD_SAFEPOINT_POLL_SYMBOL, ty, None)
    }

    /// Emit `call void @nod_safepoint_poll()` at the current insert
    /// point.  Placed at function entry and loop-header blocks so the
    /// GC can stop-the-world even in non-allocating tight loops.
    fn emit_safepoint_poll(&mut self) -> Result<(), CodegenError> {
        let map_err = |e: inkwell::builder::BuilderError| CodegenError::Builder(e.to_string());
        let f = self.get_or_declare_safepoint_poll();
        self.builder
            .build_call(f, &[], "sp.poll")
            .map_err(map_err)?;
        Ok(())
    }
}

/// One rented entry from the function's safepoint slot pool, used by
/// `begin_safepoint` / `end_safepoint`.
struct SafepointSlot<'ctx> {
    temp: TempId,
    slot: inkwell::values::PointerValue<'ctx>,
    home: FrameHome,
}

struct EmittedSafepoint<'ctx> {
    site_id: u64,
    rented: Vec<SafepointSlot<'ctx>>,
}

/// Internal codegen notion of where a safepoint root lives.
///
/// Today every root is materialized in the entry-block alloca pool,
/// but Windows stack-map lowering will need a stable place to grow
/// other home kinds without exposing codegen pool details as the
/// defining contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameHome {
    SafepointPoolSlot(usize),
}

impl FrameHome {
    fn as_safepoint_location(self) -> SafepointLocation {
        match self {
            FrameHome::SafepointPoolSlot(slot_idx) => SafepointLocation::FrameSlot(slot_idx as u32),
        }
    }
}

fn max_safepoint_slots(func: &DfmFunction) -> usize {
    func.blocks
        .iter()
        .flat_map(|block| block.computations.iter())
        .filter_map(|computation| computation.safepoint_roots().map(|roots| roots.len()))
        .max()
        .unwrap_or(0)
}

/// Compute the set of loop-header `BlockId`s in `func`.
///
/// A block is a loop header if it is the target of at least one
/// back edge — i.e., a `Jump` or `If` branch from a block whose
/// position in `func.blocks` is >= the target block's position.
/// This is a linear-order approximation (no full dominance analysis)
/// that correctly identifies all natural loop headers for the DFM IR
/// which is always produced in RPO / depth-first order by the lowering
/// pass.
fn find_loop_headers(func: &DfmFunction) -> std::collections::HashSet<BlockId> {
    let pos: std::collections::HashMap<BlockId, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();
    let mut headers = std::collections::HashSet::new();
    for (i, b) in func.blocks.iter().enumerate() {
        let mut check = |target: &BlockId| {
            if let Some(&j) = pos.get(target) {
                if j <= i {
                    headers.insert(*target);
                }
            }
        };
        match &b.terminator {
            Terminator::Jump { target, .. } => check(target),
            Terminator::If { then_block, else_block, .. } => {
                check(then_block);
                check(else_block);
            }
            Terminator::Return { .. } => {}
        }
    }
    headers
}

// ─── CacheSlot / GenericFunction field offsets ─────────────────────────────
//
// Sprint 13 bakes cache-slot field addresses into the IR as i64
// constants. The offsets here MUST agree with the `#[repr(C)]` layout
// of `nod_runtime::dispatch::CacheSlot` (six `AtomicU64`s, 8 bytes each
// at 8-byte alignment). Static asserts in the runtime crate's tests
// guard against accidental drift.

const fn offset_of_cache_slot_class() -> usize {
    0
}
const fn offset_of_cache_slot_method() -> usize {
    8
}
const fn offset_of_cache_slot_generation() -> usize {
    16
}
const fn offset_of_cache_slot_hits() -> usize {
    24
}

/// Offset of the `generation` AtomicU64 inside `GenericFunction`. The
/// struct layout (Rust default-repr — *not* repr(C), so we read this
/// at runtime through a helper) starts with `name: String` (24 bytes)
/// plus `methods: RwLock<Vec<Method>>` (sized by std), then the
/// `AtomicU64`. Because Rust's struct layout is not guaranteed across
/// versions, the runtime exposes `GenericFunction::generation()` which
/// codegen can't easily call inline; instead we read through this
/// constant offset and assert in the runtime tests that it matches.
///
/// Implementation note: we ALWAYS go through the runtime path for the
/// generation read (the slow path is `nod_dispatch`; the fast path
/// does its OWN read via baked offset). To keep this stable across
/// rustc versions we wrap the GenericFunction inside a `#[repr(C)]`
/// shim. See `nod_runtime::dispatch::generation_offset()`.
fn offset_of_generic_generation() -> usize {
    nod_runtime::generic_generation_offset()
}
