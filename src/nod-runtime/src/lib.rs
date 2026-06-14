//! `nod-runtime` — tagged-pointer ABI, `<wrapper>` headers, generational
//! copying heap, class metadata table, write barrier. The Sprint 09
//! foundation grew into the Sprint 11 GC.
//!
//! # Standard library boundary policy
//!
//! **Before adding stdlib-shape functions to this crate, read
//! `docs/STDLIB_BOUNDARY.md`.** New user-visible stdlib API belongs in
//! `src/nod-dylan/dylan-sources/stdlib.dylan` by default. Rust additions
//! are gated to six legitimate categories (GC, safepoints, FFI/OS, tag
//! manipulation, atomics on shared state, bootstrap primitives). Rule 4
//! is the pre-flight: write the Dylan version first.
//!
//! Sprint 11 lights up:
//!   - **Generational copying GC** (semispace young + 2-semispace
//!     old). Structural lift from NCL's `ncl-runtime/src/heap.rs`,
//!     adapted for Dylan's one-bit tag + Wrapper-with-ClassId.
//!   - **Class-driven scanning** via `ClassMetadata::scan` /
//!     `::size_of` function pointers. Both the tracer and the
//!     collector go through this single data-driven path.
//!   - **Card-marking write barrier** (software, 512-byte cards).
//!     `write_barrier(dst, src)` is the canonical store path for
//!     Rust-side mutations of heap-resident Words.
//!   - **Conservative stack scanning** via `Heap::pin_stack_range`.
//!     Sprint 11b will upgrade to precise stack roots via
//!     `gc.statepoint`.
//!   - **Literal pool moved to static area.** Sprint 10's
//!     `intern_string_literal` / `intern_symbol_literal` now allocate
//!     in pinned storage so JIT-baked addresses survive every GC.
//!
//! Sprint 11 design choice (per the brief's option-(b) allowance):
//! **synchronous GC triggered only at Rust-side allocation sites**, no
//! JIT-side safepoint polls. Threading the JIT into the parking
//! protocol is Sprint 11b.

// Sprint 23: feature-gated GC backend. Exactly one must be enabled.
#[cfg(all(feature = "newgc-backend", feature = "semispace-backend"))]
compile_error!(
    "nod-runtime: features `newgc-backend` and `semispace-backend` are mutually exclusive. \
     The default activates `newgc-backend`; pass `--no-default-features --features semispace-backend` \
     to use the legacy escape-hatch semispace heap."
);
#[cfg(not(any(feature = "newgc-backend", feature = "semispace-backend")))]
compile_error!(
    "nod-runtime: one of `newgc-backend` (default) or `semispace-backend` must be enabled."
);

// Sprint 39a — AOT entry surface. Defines `nod_runtime_init` and
// `nod_aot_main_wrapper`, the two `#[unsafe(no_mangle)]` extern symbols
// the codegen-emitted `i32 @main()` stub calls. Pure code; depends on
// every other module being already initialisable.
mod aot;
// The `nod_user_main` default stub now lives in its OWN crate (`nod-aot-stub`)
// so it gets its own object file regardless of nod-runtime's CGU partitioning —
// making MSVC's on-demand archive-extraction drop reliable (a nod-runtime
// module could be CGU-merged with an always-pulled object → intermittent
// `LNK2005 nod_user_main`). `as _` links the crate for its side-effect symbol
// only (no Rust items used), so the object is still pulled on-demand, never
// forced. See `nod-aot-stub/src/lib.rs`.
extern crate nod_aot_stub as _;
mod bitvectors;
mod c_types;
mod callbacks;
mod classes;
mod closures;
mod collections;
#[cfg(windows)]
mod com_shim;
mod conditions;
/// Signal-safe crash dump: GC metrics + safepoint state on panic/SEH crash.
mod crash_dump;
pub use crash_dump::{GcMetricsSnapshot, gc_metrics_snapshot};
mod dispatch;
mod winffi;
#[cfg(feature = "newgc-backend")]
mod dylan_layout;
mod format_out;
mod functions;
mod gc_trace;
mod heap;
mod heap_common;
mod immediates;
mod lists;
mod make;
mod roots;
mod safepoint_poll;
mod stack_map;
mod static_area;
mod strings;
mod structs;
mod symbols;
mod tables;
mod tracer;
mod values;
mod vectors;
mod word;
mod wrapper;

// Sprint 39a — AOT entry-point symbols. Re-exported so test code can
// reference them (the `#[unsafe(no_mangle)]` extern surface is what the
// linker sees; the `pub use` is just to give cargo-test callers a path).
pub use aot::{nod_aot_main_wrapper, nod_aot_register_variable, nod_runtime_init};
// Collection-classes lever (Part A2) — `<bit-vector>` allocate/ref/set/
// size/count redirects + word-level bitwise primitives. The
// `#[unsafe(no_mangle)]` extern surface is what the AOT linker / JIT
// symbol table see; the `pub use` gives test + driver callers a path.
pub use bitvectors::{
    bit_vector_count, bit_vector_ref, bit_vector_set, bit_vector_size, make_bit_vector,
    nod_ash, nod_bit_vector_allocate, nod_bit_vector_count, nod_bit_vector_ref,
    nod_bit_vector_set, nod_bit_vector_size, nod_logand, nod_logior, nod_lognot, nod_logxor,
};
pub use classes::{
    ClassId, ClassMetadata, ClassTable, LayoutFn, ScanFn, SizeFn, SlotDefault, SlotInfo,
    SlotType, _reset_user_classes_for_tests, allocate_user_class_id, class_metadata_for,
    class_metadata_ptr, find_class_id_by_name, find_class_id_by_name_excluding_shim_band,
    for_each_class, is_subclass, register_user_class, set_shim_class_band_active,
    shim_class_band_active, user_class_layout_fn, user_class_scan_fn, user_class_size_fn,
};
pub use closures::{
    cell_class_id, ensure_registered as ensure_closures_registered, environment_class_id,
    is_cell, is_environment, make_cell, make_environment, nod_cell_get, nod_cell_set,
    nod_env_cell, nod_make_cell, nod_make_environment, nod_var_get_by_name,
    nod_var_set_by_name,
};
pub use collections::{
    FipKind, IterStateSnapshot, OutOfRange, collection_class_id, collection_concatenate,
    collection_do, collection_element, collection_element_setter, collection_map,
    collection_reduce, collection_size, ensure_registered as ensure_collections_registered,
    explicit_key_collection_class_id, forward_iteration_protocol,
    forward_iteration_protocol_init, is_collection, iter_state_advance, iter_state_snapshot,
    iteration_state_class_id, make_out_of_range_error, make_range, make_stretchy_vector,
    mutable_collection_class_id, mutable_sequence_class_id,
    // Sprint 20b primitive-op shims (called from JIT-emitted DirectCalls
    // against `%`-prefixed names; see `nod-sema/src/lower.rs` LOWER_PRIMITIVES).
    nod_collection_concatenate, nod_collection_size, nod_fip_advance,
    nod_fip_current_element, nod_fip_finished_p, nod_fip_init, nod_make_range,
    nod_make_stretchy_vector, nod_range_by, nod_range_from, nod_range_to,
    nod_stretchy_vector_element, nod_stretchy_vector_element_setter,
    nod_stretchy_vector_push, nod_stretchy_vector_size, out_of_range_error_class_id,
    range_class_id, range_fields, sequence_class_id, stretchy_collection_class_id,
    stretchy_vector_class_id, stretchy_vector_fields, stretchy_vector_push,
};
pub use c_types::{
    c_bool_class_id, c_double_class_id, c_dword_class_id, c_float_class_id, c_handle_class_id,
    c_int_class_id, c_pointer_class_id, c_string_class_id, c_wide_string_class_id,
    ensure_float_types_registered, ensure_registered as ensure_c_types_registered,
};
#[cfg(windows)]
pub use com_shim::{
    ComObject, _reset_registry_for_tests as _reset_com_registry_for_tests,
    ensure_com_types_registered, get_d2d_bitmap, get_d2d_device, get_d2d_device_context,
    get_d2d_factory, get_d2d_solid_brush, get_d3d11_device, get_d3d11_device_context,
    get_d3d11_texture_2d, get_dwrite_factory, get_dwrite_text_format, get_dwrite_text_layout,
    get_dxgi_device, get_dxgi_factory, get_dxgi_surface, get_dxgi_swap_chain,
    nod_com_clear_last_hresult, nod_com_last_hresult, nod_com_registry_len, nod_com_release,
    nod_count_non_zero_red, nod_create_hidden_window, nod_create_message_only_window,
    nod_d2d_begin_draw, nod_d2d_clear, nod_d2d_create_bitmap_for_target,
    nod_d2d_create_bitmap_from_swap_chain, nod_d2d_create_device,
    nod_d2d_create_device_context, nod_d2d_create_factory,
    nod_d2d_create_solid_color_brush, nod_d2d_draw_rectangle, nod_d2d_draw_text_layout,
    nod_d2d_end_draw, nod_d2d_fill_rectangle, nod_d2d_set_target,
    nod_d2d_set_transform_identity, nod_d3d11_copy_to_staging_and_map,
    nod_d3d11_create_device, nod_d3d11_create_texture_2d, nod_d3d11_get_immediate_context,
    nod_d3d11_last_mapped_row_pitch, nod_d3d11_last_staging_handle, nod_d3d11_unmap,
    nod_def_window_proc, nod_destroy_window, nod_dwrite_create_factory, nod_dwrite_create_text_format,
    nod_dwrite_create_text_layout, nod_dwrite_get_layout_metrics,
    nod_dwrite_hit_test_point, nod_dwrite_hit_test_text_position,
    nod_dwrite_set_drawing_effect, nod_dwrite_set_line_spacing,
    nod_dxgi_create_factory, nod_dxgi_create_surface_from_texture,
    nod_dxgi_create_swap_chain_for_hwnd, nod_dxgi_device_from_d3d_device,
    nod_dxgi_factory_from_d3d_device, nod_dxgi_swap_chain_present,
    nod_dxgi_swap_chain_resize_buffers, nod_get_argv1, nod_get_argv2, nod_get_scroll_pos,
    nod_hi_word, nod_lo_word, nod_post_message, nod_print_gc_stats, nod_pump_one_message,
    nod_read_file_to_string,
    nod_register_window_class, nod_run_message_loop, nod_set_scroll_info,
    // Sprint 41e — Win32 file-open common dialog shim (wraps
    // GetOpenFileNameW + the 88-byte OPENFILENAMEW struct).
    nod_show_open_file_dialog,
    // Sprint 41g — Win32 file-save common dialog shim (wraps
    // GetSaveFileNameW + the same OPENFILENAMEW struct) and the
    // `<byte-string>`-to-file write shim that backs File → Save and
    // File → Save As. Sprint 42a Phase E retired the recent-files /
    // basename / count-newlines / max-line-chars shims — those live
    // in pure Dylan in nod-ide.dylan now.
    nod_show_save_file_dialog, nod_write_file_from_string,
    register as com_register, registry_len as com_registry_len,
};
pub use callbacks::{
    CallbackSignature, POOL_SIZE as CALLBACK_POOL_SIZE, RegisterError as CallbackRegisterError,
    _occupied_count as _callback_occupied_count_for_tests,
    _reset_callbacks_for_tests, callback_tenure_mode_enabled, nod_register_wndenumproc,
    nod_register_wndproc, register_callback, set_callback_tenure_mode,
    slot_address as callback_slot_address,
};
pub use conditions::{
    BlockFns, HandlerFn, HandlerFrame, MAX_BLOCK_CAPTURED, NlxPayload, _reset_block_registry_for_tests,
    _reset_handler_stack_for_tests, condition_class_id, condition_class_name,
    condition_message, error_class_id, ensure_registered as ensure_conditions_registered,
    exit_procedure_block_id, exit_procedure_class_id, for_each_handler, handler_stack_snapshot,
    handlers_report, invoke_restart, make_exit_procedure, make_no_applicable_methods_error,
    make_simple_condition, make_simple_error, make_simple_restart, make_simple_warning,
    no_applicable_methods_error_class_id, no_next_method_error_class_id, nod_condition_message,
    nod_invoke_exit, nod_make_exit_procedure, nod_pop_handler, nod_push_handler, nod_run_block,
    nod_signal, nod_walk_handlers_dump, register_block_fns, serious_condition_class_id,
    simple_condition_class_id, simple_error_class_id, simple_restart_class_id,
    simple_warning_class_id, warning_class_id,
};
pub use dispatch::{
    CacheSlot, GenericFunction, Method, MethodPtr, MethodTableError, ResolvedDispatchEntry,
    _reset_method_chain_stack_for_tests, add_method, add_method_full, add_method_named,
    dump_dispatch, find_generic, find_initialize_method, find_method_body_ptr,
    for_each_generic, generic_generation_offset, get_or_create_generic, has_next_method,
    invoke_method_with_self, is_generic_defined, lookup_applicable_methods, lookup_method,
    lookup_method_by_receiver, nod_add_method, nod_dispatch, nod_dispatch_binary,
    nod_dispatch_unary, nod_has_next_method, nod_next_method, nod_pop_sealed_chain_frame,
    nod_push_sealed_chain_frame, record_resolved_dispatch, remove_method,
    resolved_dispatch_snapshot, try_add_method_full, word_class_id,
};
pub use make::{
    MAKE_MAX_KW_PAIRS, RootGuard, nod_card_mark, nod_is_instance_of, nod_is_instance_of_word,
    nod_make, nod_register_root, nod_unregister_root, rust_make,
};
pub use format_out::{
    install_test_writer, nod_format_out, take_test_writer, uninstall_test_writer,
};
pub use functions::{
    FUNCTION_KIND_CLOSURE, FUNCTION_KIND_GENERIC_TRAMPOLINE, FUNCTION_KIND_LIFTED_ANON,
    FUNCTION_KIND_TOP_LEVEL, MAX_APPLY_ARITY, _reset_function_registry_for_tests,
    ensure_operator_shims_registered, ensure_registered as ensure_functions_registered,
    function_arity, function_class_id, function_code_ptr, function_env_ptr, function_kind_tag,
    function_name, is_function, lookup_function_code, make_function, make_function_ref,
    make_generic_trampoline_ref, make_wrong_number_of_arguments_error, nod_apply, nod_funcall0,
    nod_funcall1, nod_funcall2, nod_funcall3, nod_funcall4, nod_funcall5, nod_instance_p,
    nod_make_closure, nod_make_function_ref, nod_op_eq, nod_op_eq_eq, nod_op_gt, nod_op_lt,
    nod_op_minus, nod_op_ne, nod_op_ne_eq, nod_op_plus, nod_op_times, register_jit_function,
    register_rust_function, wrong_number_of_arguments_error_class_id,
};
pub use heap::{
    DEFAULT_OLD_BYTES, DEFAULT_RESERVATION_BYTES, DEFAULT_YOUNG_BYTES, GcConfig, HEAP_ALIGN, Heap,
    HeapRanges, for_each_root, register_root as heap_register_root, root_count as heap_root_count,
    unregister_root as heap_unregister_root,
};
pub use immediates::{Immediates, WrapperCell, wrapper_of_unchecked};
pub use lists::{
    PAIR_HEAD_OFFSET, PAIR_TAIL_OFFSET, Pair, nod_empty_p, nod_list_size, nod_nil,
    nod_pair_alloc, nod_pair_head, nod_pair_tail, try_pair,
};
pub use roots::RootSet;
pub use safepoint_poll::{
    SAFEPOINT_PARK_REQUESTED, nod_safepoint_poll,
    safepoint_request_stop, safepoint_resume,
    nod_safepoint_request_stop, nod_safepoint_resume,
};
pub use stack_map::{
    JitSafepointEntry, LiveSlot, ParkedFrame, StackMap, StackMapEntry,
    nod_jit_begin_safepoint, nod_jit_end_safepoint, register_jit_safepoints,
    walk_parked_frame,
};
pub use static_area::StaticArea;
pub use strings::{
    ByteString, nod_byte_string_allocate, nod_byte_string_copy_bytes, nod_byte_string_element,
    nod_byte_string_element_setter, nod_byte_string_size, try_byte_string,
};
pub use structs::{
    c_struct_class_id, ensure_structs_registered, filetime_class_id, is_c_struct_instance,
    msg_class_id, nod_struct_get_i32, nod_struct_get_i64, nod_struct_get_pointer,
    nod_struct_get_u16, nod_struct_get_u32, nod_struct_get_u64, nod_struct_set_i32,
    nod_struct_set_i64, nod_struct_set_pointer, nod_struct_set_u16, nod_struct_set_u32,
    nod_struct_set_u64, paintstruct_class_id, point_class_id, rect_class_id, size_class_id,
    struct_layout_for, systemtime_class_id, wndclassexw_class_id,
};
pub use symbols::{Symbol, SymbolTable, try_symbol};
pub use tables::{
    ensure_registered as ensure_tables_registered, is_table, make_not_hashable_error, make_table,
    nod_make_table, nod_object_equal_p, nod_object_hash, nod_table_element,
    nod_table_element_or_default, nod_table_element_setter, nod_table_keys, nod_table_remove_key,
    nod_table_size, nod_table_values, not_hashable_error_class_id, table_class_id, table_element,
    table_element_setter, table_keys, table_remove_key, table_size, table_values,
};
pub use tracer::{HeapObjectInfo, HeapTrace, trace_heap};
pub use values::{
    nod_values_clear, nod_values_count, nod_values_get, nod_values_set,
    snapshot_active_values_roots,
};
pub use vectors::{
    SimpleObjectVector, nod_make_sov_len, nod_make_sov_literal, nod_sov_element,
    nod_sov_element_setter, nod_sov_size, try_simple_object_vector, try_simple_object_vector_mut,
};
pub use winffi::{
    ApiCallSignature, ApiStubEntry, ApiStubTable, CArgKind, CReturnKind, StubEntrySpec,
    WinFfiStats, _reset_winffi_stats_for_tests, allocate_stub_table, c_ffi_error_class_id,
    ensure_c_ffi_error_registered, initialize_stub_table, make_c_ffi_error,
    nod_winffi_call_0, nod_winffi_call_1, nod_winffi_call_2, nod_winffi_call_3,
    nod_winffi_call_4, nod_winffi_call_5, nod_winffi_call_6, nod_winffi_call_7,
    nod_winffi_call_8, nod_winffi_call_9, nod_winffi_call_10, nod_winffi_call_11,
    nod_winffi_call_12, record_stub_entry_allocated, resolve_into_entry, resolve_symbol,
    signature_from_names, winffi_record_materialized, winffi_stats,
};
pub use word::{FIXNUM_MAX, FIXNUM_MIN, FixnumOverflow, Word};
pub use wrapper::{GcBit, Wrapper};

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Process-global literal pool. Sprint 11 pins string + symbol literals
/// in the `StaticArea` so JIT-baked addresses (the `i64` constants
/// codegen emits) survive every GC cycle. Booleans and `nil` live in
/// the same static area for the same reason.
///
/// The pool also exposes a moveable `Heap` — that's the process-global
/// young generation the Sprint 11 collector mutates. JIT'd code
/// allocates there through the same `nod-sema` shim path it used in
/// Sprint 10.
pub struct LiteralPool {
    pub heap: Heap,
    pub symbols: SymbolTable,
    pub static_area: StaticArea,
    pub classes: ClassTable,
    pub immediates: Immediates,
}

static LITERAL_POOL: LazyLock<Mutex<LiteralPool>> = LazyLock::new(|| {
    let heap = Heap::new();
    let symbols = SymbolTable::new();
    let static_area = StaticArea::new();
    let classes = ClassTable::new();
    let immediates = Immediates::new(&static_area, &classes);
    let pool = LiteralPool {
        heap,
        symbols,
        static_area,
        classes,
        immediates,
    };
    // Sprint 19: register the seed condition classes once the seed
    // class table is alive. `ensure_registered` is idempotent and
    // routes through `register_simple_user_class` (which itself takes
    // the literal-pool mutex), so we have to schedule it AFTER this
    // initialiser returns. We do that by deferring to first-use of
    // any condition accessor — the first `signal()` or `make
    // <error>` from Dylan code triggers `ensure_registered` lazily.
    // Tests that want the classes present unconditionally call
    // `ensure_conditions_registered` from `nod-sema` lowering.
    let _ = (); // doc comment anchor
    Mutex::new(pool)
});

/// Take a brief lock on the process-global literal pool.
pub fn with_literal_pool<R>(f: impl FnOnce(&LiteralPool) -> R) -> R {
    let guard = LITERAL_POOL.lock().expect("literal pool poisoned");
    f(&guard)
}

/// Intern a Dylan string literal in the process-global literal pool
/// and return its tagged `Word`. Sprint 11: allocation goes through
/// the **static area**, not the moveable heap, so the returned
/// address is stable across every GC cycle. Codegen bakes these
/// addresses into LLVM constants.
pub fn intern_string_literal(s: &str) -> Word {
    with_literal_pool(|pool| pool.static_area.alloc_byte_string(s, &pool.classes))
}

/// Intern a Dylan symbol literal in the process-global literal pool
/// and return its tagged `Word`. Sprint 11: allocation goes through
/// the **static area**. Repeated calls with the same `name` return
/// the same Word (the symbol table dedups across heap + static).
pub fn intern_symbol_literal(name: &str) -> Word {
    with_literal_pool(|pool| {
        pool.symbols
            .intern_static(name, &pool.static_area, &pool.classes)
    })
}

/// The process-global boolean / nil singletons. Codegen bakes these
/// addresses into LLVM constants so `#t`, `#f`, and `nil` round-trip
/// through the JIT as stable pointer-tagged words.
pub fn literal_pool_immediates() -> Immediates {
    with_literal_pool(|pool| pool.immediates)
}

/// Sprint 38b — process-global stable storage holding the bit pattern
/// of each of the four immediate-singleton Words. Sprint 38b's codegen
/// emits external globals of `i64` type for `#t`, `#f`, `nil`, and the
/// untagged `#f` wrapper; on cache load + cold compile the JIT-link
/// path calls `LLVMAddGlobalMapping(symbol, address-of-slot)` so a
/// `load i64, ptr @symbol` reads the slot's value.
///
/// The slots are initialised on first read and remain stable for the
/// process lifetime. They are kept in a separate Box<u64> per slot so
/// that taking `&u64` returns an address that is *not* invalidated by
/// any subsequent mutation of the lazy-lock or literal pool.
///
/// Distinct from `literal_pool_immediates()`, which returns the Word
/// **values** by-copy through the literal-pool mutex — those copies
/// have no stable address (the mutex guard owns them transiently).
struct ImmediateSlots {
    true_: &'static u64,
    false_: &'static u64,
    nil: &'static u64,
    false_wrapper: &'static u64,
}

static IMMEDIATE_SLOTS: LazyLock<ImmediateSlots> = LazyLock::new(|| {
    let imm = literal_pool_immediates();
    let t: &'static u64 = Box::leak(Box::new(imm.true_.raw()));
    let f: &'static u64 = Box::leak(Box::new(imm.false_.raw()));
    let n: &'static u64 = Box::leak(Box::new(imm.nil.raw()));
    let fw: &'static u64 = Box::leak(Box::new(imm.false_.raw() & !1_u64));
    ImmediateSlots {
        true_: t,
        false_: f,
        nil: n,
        false_wrapper: fw,
    }
});

/// Sprint 38b — stable address of the i64 slot holding the `#t` Word
/// bits. JIT-link path uses this as the relocation target for
/// `RelocKind::ImmTrue`'s named global.
pub fn imm_true_slot_addr() -> *const u64 {
    IMMEDIATE_SLOTS.true_ as *const u64
}

/// Sprint 38b — stable address of the `#f` slot.
pub fn imm_false_slot_addr() -> *const u64 {
    IMMEDIATE_SLOTS.false_ as *const u64
}

/// Sprint 38b — stable address of the `nil` slot.
pub fn imm_nil_slot_addr() -> *const u64 {
    IMMEDIATE_SLOTS.nil as *const u64
}

/// Sprint 38b — stable address of the `#f` untagged-wrapper slot.
pub fn imm_false_wrapper_slot_addr() -> *const u64 {
    IMMEDIATE_SLOTS.false_wrapper as *const u64
}

// ─── Sprint 38c — slot allocators for static-area pointers ─────────────
//
// Sprint 38b proved the named-external-global pattern for the four
// immediate singletons. Sprint 38c extends it to three more bake-site
// categories: class-metadata pointers, `<byte-string>` literal Words,
// and `<symbol>` literal Words.
//
// Each category needs a **slot address** (stable `&'static u64`) whose
// CONTENTS are the per-process Word/pointer bits. The JIT-link path
// registers the slot's address via `LLVMAddGlobalMapping(@sym, slot)`;
// codegen emits `load i64, ptr @sym` which reads the slot's value.
//
// Memoization is per-content (per class_id, per text, per symbol name)
// so multiple JIT-loaded modules referencing the same literal share one
// slot. All maps are guarded by a single mutex.

/// Sprint 38c — per-class-id slot table mapping `ClassId.0` to the
/// stable address of a `u64` holding `class_metadata_ptr(id) as u64`.
/// The slot's bits are the raw (untagged) metadata pointer; codegen
/// applies the pointer-tag at use (see `emit_class_ref`'s `| 1` after
/// the load).
static CLASS_METADATA_SLOTS: LazyLock<Mutex<HashMap<u32, &'static u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sprint 38c — per-text slot table for `<byte-string>` literals. Each
/// distinct UTF-8 text maps to one stable `&'static u64` whose contents
/// are `intern_string_literal(text).raw()`.
static STRING_LITERAL_SLOTS: LazyLock<Mutex<HashMap<String, &'static u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sprint 38c — per-name slot table for `<symbol>` literals.
static SYMBOL_LITERAL_SLOTS: LazyLock<Mutex<HashMap<String, &'static u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sprint 38d — per-(dll, symbol) slot table for Win32 API stub-entry
/// pointers. Each distinct `(dll.to_lowercase(), symbol)` maps to one
/// stable `&'static u64` whose contents are the address of an
/// [`ApiStubEntry`] freshly allocated + resolved in the current process.
///
/// **Case handling**: Win32 DLL names are case-insensitive (the
/// `LoadLibrary` resolver normalises internally), so the slot table
/// keys on `dll.to_lowercase()`. Symbol names are case-sensitive — the
/// Windows linker matches them byte-for-byte, so we keep them verbatim.
static STUB_ENTRY_SLOTS: LazyLock<Mutex<HashMap<(String, String), &'static u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Test-only: clear the process-global stub-entry slot dedup map so a
/// test starts from a clean slate. The other runtime registries all
/// expose a `_reset_*_for_tests` hook; this one was missing, which made
/// `api_stub_table_deduplicates_call_sites` order-dependent — a sibling
/// test that already registered `(dll, symbol)` left it memoised here, so
/// `stub_entry_slot_addr` early-returned without calling
/// `allocate_stub_table`, the entries counter stayed 0, and the
/// "at least one entry was allocated" assertion failed in the full sweep
/// while passing in isolation.
///
/// Safe between tests: each test compiles, links, and runs its own module
/// exactly once, so no live module re-links against a cleared slot. The
/// previously-leaked `&'static u64` slots stay valid regardless (they are
/// simply orphaned); the next allocation for the same key leaks a fresh
/// one.
#[doc(hidden)]
pub fn _reset_stub_entry_slots_for_tests() {
    STUB_ENTRY_SLOTS
        .lock()
        .expect("stub entry slot table poisoned")
        .clear();
}

/// Sprint 38c — stable address of a `u64` holding the raw metadata
/// pointer (`class_metadata_ptr(id) as u64`) for `class_id`. Repeated
/// calls with the same id return the SAME `*const u64`.
///
/// Used by `Jit::add_module`/`add_module_from_bitcode` to map
/// `@nod_class_md__<key>__<id>` to a stable address whose contents
/// load as the metadata pointer in the current process.
pub fn class_metadata_slot_addr(class_id: ClassId) -> *const u64 {
    let mut guard = CLASS_METADATA_SLOTS
        .lock()
        .expect("class metadata slot table poisoned");
    if let Some(&slot) = guard.get(&class_id.0) {
        return slot as *const u64;
    }
    let md_addr = class_metadata_ptr(class_id) as u64;
    let slot: &'static u64 = Box::leak(Box::new(md_addr));
    guard.insert(class_id.0, slot);
    slot as *const u64
}

/// Sprint 38c — stable address of a `u64` holding the raw bits of the
/// interned `<byte-string>` Word for `text`. Repeated calls with the
/// same text return the SAME `*const u64` (memoised). The interned
/// Word itself is process-stable (pinned in the static area).
pub fn intern_string_literal_slot_addr(text: &str) -> *const u64 {
    let mut guard = STRING_LITERAL_SLOTS
        .lock()
        .expect("string literal slot table poisoned");
    if let Some(&slot) = guard.get(text) {
        return slot as *const u64;
    }
    let bits = intern_string_literal(text).raw();
    let slot: &'static u64 = Box::leak(Box::new(bits));
    guard.insert(text.to_string(), slot);
    slot as *const u64
}

/// Sprint 38c — stable address of a `u64` holding the raw bits of the
/// interned `<symbol>` Word for `name`. Repeated calls with the same
/// name return the SAME `*const u64` (memoised).
pub fn intern_symbol_literal_slot_addr(name: &str) -> *const u64 {
    let mut guard = SYMBOL_LITERAL_SLOTS
        .lock()
        .expect("symbol literal slot table poisoned");
    if let Some(&slot) = guard.get(name) {
        return slot as *const u64;
    }
    let bits = intern_symbol_literal(name).raw();
    let slot: &'static u64 = Box::leak(Box::new(bits));
    guard.insert(name.to_string(), slot);
    slot as *const u64
}

/// Sprint 38d — stable address of a `u64` holding the **address** of an
/// [`ApiStubEntry`] for `(dll, symbol)`. Repeated calls with the same
/// `(dll-case-insensitive, symbol)` return the SAME `*const u64` —
/// multiple JIT-loaded modules referencing the same Win32 API share one
/// underlying entry (and therefore one resolved `fn_ptr` cell).
///
/// The slot's contents are the entry pointer cast to `u64`; codegen
/// emits `load i64, ptr @nod_stub__<key>__<idx>` to recover the entry
/// pointer and passes it as the first arg of `nod_winffi_call_N`.
///
/// **Resolution failure semantics**: on first lookup we allocate the
/// entry via [`allocate_stub_table`] and attempt
/// [`resolve_into_entry`]. If the resolver fails (DLL missing, symbol
/// not found), we still leak the slot — the entry's `fn_ptr` stays
/// null, and the Win64 trampoline at call time notices that and
/// signals `<c-ffi-error>` through the normal Dylan error path. This
/// matches Sprint 28's at-call-time error discipline: the JIT-link
/// step must not crash the loader on a missing Win32 export, because
/// the user's program may handle the condition with `block`/`exception`.
///
/// Memoisation makes the second call's lookup a single map probe even
/// across many call sites — and across processes that load multiple
/// cached modules referencing the same `(dll, symbol)` pair.
pub fn stub_entry_slot_addr(
    dll: &str,
    symbol: &str,
    signature: &winffi::ApiCallSignature,
) -> &'static u64 {
    let key = (dll.to_lowercase(), symbol.to_string());
    let mut guard = STUB_ENTRY_SLOTS
        .lock()
        .expect("stub entry slot table poisoned");
    if let Some(&slot) = guard.get(&key) {
        return slot;
    }
    // First lookup for this (dll, symbol): allocate a fresh stub-table
    // entry in the static area, attempt to resolve it eagerly, and leak
    // a `u64` slot whose contents are the entry pointer's bits.
    let specs = vec![winffi::StubEntrySpec {
        dll: dll.to_string(),
        symbol: symbol.to_string(),
        signature: *signature,
    }];
    let (_table, ptrs) = winffi::allocate_stub_table(&specs);
    let entry_ptr = ptrs[0];
    // SAFETY: `entry_ptr` was just allocated by `allocate_stub_table`
    // and lives in the static area for the process lifetime.
    // `resolve_into_entry` populates `fn_ptr` on success; on failure
    // it leaves `fn_ptr` null and we silently absorb the error here.
    // The Win64 trampoline at call time checks for null `fn_ptr` and
    // signals `<c-ffi-error>` through the normal Dylan condition path.
    let _ = unsafe { winffi::resolve_into_entry(entry_ptr, dll, symbol) };
    let slot: &'static u64 = Box::leak(Box::new(entry_ptr as u64));
    guard.insert(key, slot);
    slot
}

/// Sprint 13: mint a fresh inline-cache slot in the static area and
/// return its raw pointer. Each JIT-emitted `Dispatch` call site
/// receives one via `dispatch::CacheSlot::cold(site_id)` baked into
/// the IR as an `i64`. The slot's address is stable for the process
/// lifetime; the slot's contents are atomically read/written by both
/// the JIT-emitted fast path and the slow-path shim.
pub fn allocate_cache_slot(site_id: u64) -> *const CacheSlot {
    with_literal_pool(|pool| {
        let slot: &'static CacheSlot = pool.static_area.alloc(CacheSlot::cold(site_id));
        slot as *const CacheSlot
    })
}

/// Sprint 38e — per-(module-key-prefix, site_id) slot table for
/// inline-cache dispatch slots. Each distinct
/// `(key_prefix, site_id)` pair maps to one stable `&'static u64`
/// whose contents are the address of a freshly-allocated [`CacheSlot`]
/// in the current process's static area.
///
/// **Why the key prefix is part of the map key**: two distinct modules
/// (e.g. two different Dylan source files compiled into the same
/// process) may each have dispatch sites with `site_id = 0`. Without
/// the key_prefix in the map key, they'd share one underlying
/// `CacheSlot`, which would scramble both modules' inline caches.
/// Within one module, `site_id`s are unique
/// (`ModuleCodegenCtx::next_dispatch_site_id` is module-wide
/// monotonic), so the pair `(key_prefix, site_id)` is process-globally
/// unique.
///
/// **Cross-process state**: the cache slot itself has no persistent
/// state across processes — it's a Sprint 13 polyinline-cache, mutated
/// atomically at runtime by the fast-path/slow-path. When a cached
/// `.bc` is loaded in a fresh process, this slot allocator runs once
/// per `(key_prefix, site_id)` pair and minted slots start out empty
/// (which is exactly what we want: cold and warm both see a fresh
/// cache, no state needs to round-trip).
static CACHE_SLOT_SLOTS: LazyLock<Mutex<HashMap<(String, u64), &'static u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sprint 38e — per-generic-name slot table for `GenericFunction`
/// pointers. Each distinct name maps to one stable `&'static u64` whose
/// contents are the address of the `&'static GenericFunction` returned
/// by [`get_or_create_generic`].
///
/// `get_or_create_generic` already leaks a `&'static GenericFunction`
/// per name (process lifetime), so this slot is just an indirection
/// that lets codegen emit `load i64, ptr @nod_generic__*` instead of
/// baking the per-process address.
static GENERIC_FUNCTION_SLOTS: LazyLock<Mutex<HashMap<String, &'static u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sprint 38e — wraps Sprint 13's `dispatch::_reset_for_tests` to ALSO
/// clear the Sprint 38e slot tables.
///
/// Pre-Sprint-38e, `_reset_dispatch_for_tests` (re-exported from
/// `dispatch.rs`) wiped the `GenericFunction` registry and the
/// resolved-dispatch index. That was sufficient because the cache slot
/// addresses + generic pointers were baked into IR as i64 constants —
/// each test's freshly-JIT-compiled module saw fresh constants, no
/// stale state could leak.
///
/// Sprint 38e routes both through Sprint 38e slot allocators that
/// memoise leaked `&'static u64`s indefinitely. A test that wipes the
/// generic registry without also clearing the slot tables would have
/// the next test's `@nod_generic__*` loads return stale generic
/// pointer bits (pointing at the freed-then-recreated generic, which
/// has zero methods after the registry clear). Wrapping the existing
/// helper here keeps the test-side API identical
/// (`_reset_dispatch_for_tests()`) while threading the additional
/// slot-table clear underneath.
pub fn _reset_dispatch_for_tests() {
    dispatch::_reset_for_tests();
    _reset_sprint38e_slots_for_tests();
}

/// Sprint 38e — test helper: clear both Sprint 38e slot tables.
///
/// The slot allocators memoise leaked `&'static u64`s per-process. In a
/// single-process production run that's the right behaviour (multiple
/// JIT-loaded modules referencing the same generic / cache site share
/// one underlying pointer cell). Tests that call
/// `_reset_dispatch_for_tests` to wipe the `GenericFunction` registry
/// also need to clear these slot tables — otherwise a previous test's
/// stale `&'static GenericFunction` pointer bits remain in the slot,
/// the next test's `@nod_generic__*` load reads those stale bits, and
/// dispatch through the warm IR hits a generic with no methods.
///
/// The leaked `u64` slots themselves are not freed (they're
/// `'static`); this just wipes the map so the next lookup allocates a
/// fresh slot keyed on the new run's generic / cache slot pointers.
pub fn _reset_sprint38e_slots_for_tests() {
    {
        let mut guard = CACHE_SLOT_SLOTS
            .lock()
            .expect("cache slot slot table poisoned");
        guard.clear();
    }
    let mut guard = GENERIC_FUNCTION_SLOTS
        .lock()
        .expect("generic function slot table poisoned");
    guard.clear();
}

/// Sprint 38e — stable address of a `u64` holding the address of a
/// `CacheSlot` for `(key_prefix, site_id)`. Repeated calls with the
/// same `(key_prefix, site_id)` return the SAME `*const u64`.
///
/// `key_prefix` is the per-module symbol-name prefix (the first 16 hex
/// characters of the module's cache key); see `nod-llvm::symbols`. It
/// disambiguates `site_id == 0` between distinct modules sharing the
/// same process.
///
/// First lookup calls [`allocate_cache_slot`], which mints a fresh
/// (empty) `CacheSlot` in the static area; we leak a `Box<u64>` holding
/// the slot's pointer bits and memoise it. Subsequent lookups return
/// the cached `&'static u64`.
///
/// Used by `Jit::add_module`/`add_module_from_bitcode` to map
/// `@nod_cache_slot__<key>__<site_id>` to a stable address whose
/// contents load as the cache slot pointer in the current process.
/// Codegen emits `load i64, ptr @nod_cache_slot__*` once per dispatch
/// site and derives the field-offset addresses (class / method /
/// generation / hits) by `add i64`-ing the loaded value with the
/// `#[repr(C)]` field offsets.
pub fn cache_slot_slot_addr(key_prefix: &str, site_id: u64) -> &'static u64 {
    let key = (key_prefix.to_string(), site_id);
    let mut guard = CACHE_SLOT_SLOTS
        .lock()
        .expect("cache slot slot table poisoned");
    if let Some(&slot) = guard.get(&key) {
        return slot;
    }
    let cache_slot_ptr = allocate_cache_slot(site_id);
    let slot: &'static u64 = Box::leak(Box::new(cache_slot_ptr as u64));
    guard.insert(key, slot);
    slot
}

/// Sprint 38e — stable address of a `u64` holding the address of the
/// `&'static GenericFunction` for `name`. Repeated calls with the same
/// name return the SAME `*const u64`.
///
/// First lookup calls [`get_or_create_generic`] (which itself leaks a
/// `&'static GenericFunction` keyed on name); we leak a `Box<u64>`
/// holding the generic's pointer bits and memoise it. Subsequent
/// lookups return the cached `&'static u64`.
///
/// Used by `Jit::add_module`/`add_module_from_bitcode` to map
/// `@nod_generic__<key>__<sanitised-name>` to a stable address whose
/// contents load as the generic pointer in the current process.
/// Codegen emits `load i64, ptr @nod_generic__*` once per dispatch site
/// and derives the `generation` field address by `add i64`-ing with
/// [`generic_generation_offset`].
pub fn generic_function_slot_addr(name: &str) -> &'static u64 {
    let mut guard = GENERIC_FUNCTION_SLOTS
        .lock()
        .expect("generic function slot table poisoned");
    if let Some(&slot) = guard.get(name) {
        return slot;
    }
    let generic = get_or_create_generic(name);
    let generic_ptr_bits = generic as *const GenericFunction as u64;
    let slot: &'static u64 = Box::leak(Box::new(generic_ptr_bits));
    guard.insert(name.to_string(), slot);
    slot
}

// ─── GAP-004 — per-`define variable` cell slot allocator ───────────────────
//
// Each module-level `define variable` is lowered as:
//   * an `__init-<name>` zero-arg function returning the init expression;
//   * a `<name>()` getter that loads the cell pointer from the variable's
//     slot and calls `nod_cell_get`;
//   * a `<name>-setter(v)` that loads the cell pointer and calls
//     `nod_cell_set`.
//
// The runtime side here owns the **slot**: a process-global
// `&'static u64` per variable name whose bits are the raw pointer of a
// freshly-allocated `<cell>`. The slot starts at 0 (uninitialised) and
// is filled in at startup by [`nod_aot_register_variable`] (AOT path)
// or by the JIT-side init driver in `nod-sema` (JIT path), which
// evaluates the user's init expression, allocates a cell holding the
// result, and stores the cell pointer's raw bits in the slot.
//
// GC-trace note: the cell itself lives in the **moveable heap** (since
// `<cell>` is a GC-traced class). When the GC relocates the cell, the
// slot's bits must be updated to the new address. We achieve that by
// registering the slot's address as a heap root the first time we
// allocate a slot for a variable — the GC's existing pointer-rewriting
// path then walks through the slot's address as if it were any other
// stack-/static-tracked Word.
//
// The user-visible value INSIDE the cell is itself in the moveable
// heap (for pointer values) and is reached via the cell's `value`
// slot, which is already GC-traced by Sprint 24's `<cell>` class. So
// any heap pointer stored in a `define variable` survives GC cycles
// transitively through cell → value.
//
// Storage choice: we leak an `AtomicU64` per variable so atomic
// init-store / load semantics are explicit. Reads from JIT-emitted
// getter bodies use `Relaxed` (no synchronisation contract beyond
// "see the post-init value at some point") since the init runs
// before `nod_user_main` is entered.

/// GAP-004 — per-`define variable`-name slot table mapping the
/// variable's bareword name (as it appears in source) to the stable
/// address of an `AtomicU64` holding the variable's `<cell>` pointer
/// bits. Keyed on `name.to_string()`. Pointers are leaked for the
/// process lifetime; the AtomicU64 starts at 0 and is set to the
/// cell's raw Word bits when the variable is initialised.
static VARIABLE_CELL_SLOTS: LazyLock<
    Mutex<HashMap<String, &'static std::sync::atomic::AtomicU64>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// GAP-004 — return the stable address of the `AtomicU64` slot holding
/// the cell pointer for `name`. Repeated calls with the same name
/// return the SAME `&'static AtomicU64`.
///
/// First lookup allocates a fresh `AtomicU64` initialised to 0 (the
/// "uninitialised" sentinel — `nod_var_get_by_name` panics if it reads
/// 0), leaks it, registers its address as a GC root (so the collector
/// rewrites the cell pointer when the cell is relocated), and memoises.
///
/// **GC root registration**: the slot's address is registered via
/// [`heap_register_root`] so the GC rewrites the slot's bits when the
/// referenced `<cell>` is evacuated. This call MUST happen on the
/// thread that will later be running the GC's pointer-rewriting cycle
/// — single-mutator deployments (current AOT + JIT) trivially satisfy
/// this because allocation and the rewrite both run on the main
/// thread. Multi-mutator support is deferred.
pub fn variable_cell_slot_addr(name: &str) -> &'static std::sync::atomic::AtomicU64 {
    let mut guard = VARIABLE_CELL_SLOTS
        .lock()
        .expect("variable cell slot table poisoned");
    if let Some(&slot) = guard.get(name) {
        return slot;
    }
    let slot: &'static std::sync::atomic::AtomicU64 =
        Box::leak(Box::new(std::sync::atomic::AtomicU64::new(0)));
    // Register the slot's address as a GC root. The slot's bits ARE
    // a Word (cell pointer), so the collector walks through it during
    // root scanning and rewrites the bits if the pointed-at cell is
    // evacuated. AtomicU64 has the same layout as u64, so casting to
    // `*const Word` is sound.
    let slot_as_word: *const Word = slot as *const _ as *const Word;
    heap_register_root(slot_as_word);
    guard.insert(name.to_string(), slot);
    slot
}

/// GAP-004 — clear the variable cell slot table. Used by tests that
/// reset the runtime state between scenarios; the leaked `AtomicU64`s
/// themselves stay alive (still GC roots), but the map is wiped so a
/// fresh test sees fresh init paths.
#[doc(hidden)]
pub fn _reset_variable_cell_slots_for_tests() {
    let mut guard = VARIABLE_CELL_SLOTS
        .lock()
        .expect("variable cell slot table poisoned");
    guard.clear();
}

/// Description of a user class to be registered. Sprint 12 expects
/// callers (the sema layer) to compute slot offsets + CPL up-front and
/// hand them over; the runtime just pins the metadata in the static
/// area. Returns the stable `ClassId` and the static-area address of
/// the new `ClassMetadata`, so the codegen layer can bake the address
/// into LLVM constants.
///
/// Sprint 14 adds `parents` (multiple direct supers) and `slot_origin`
/// (the defining class per slot). The legacy `parent` field is the
/// first parent (`parents[0]` or `None` for `<object>`).
pub struct UserClassSpec {
    pub name: String,
    pub parent: Option<ClassId>,
    pub parents: Vec<ClassId>,
    pub cpl: Vec<ClassId>,
    pub slots: Vec<SlotInfo>,
    pub slot_origin: Vec<ClassId>,
    pub own_slot_count: usize,
    pub inherited_slot_count: usize,
}

/// Pin a fresh `ClassMetadata` for a user class in the static area and
/// register it in the global class table. Returns the assigned
/// `ClassId` and the address of the pinned metadata.
///
/// Both addresses are stable for the process lifetime — the codegen
/// layer can bake them into LLVM `i64` constants.
pub fn register_user_class_metadata(spec: UserClassSpec) -> (ClassId, *const ClassMetadata) {
    let id = allocate_user_class_id();
    let instance_size = std::mem::size_of::<Wrapper>() + 8 * spec.slots.len();
    let md = ClassMetadata {
        id,
        name: spec.name,
        parent: spec.parent,
        parents: spec.parents,
        cpl: spec.cpl,
        slots: spec.slots,
        own_slot_count: spec.own_slot_count,
        inherited_slot_count: spec.inherited_slot_count,
        slot_origin: spec.slot_origin,
        instance_size,
        scan: user_class_scan_fn(),
        size_of: user_class_size_fn(),
        layout: user_class_layout_fn(),
        is_byte_payload: false,
        // Sprint 15: every class starts open; the lowering pass flips
        // `sealed = true` post-registration when the source carries the
        // `sealed` modifier. The atomic store there pairs with reads on
        // the dispatch resolver path.
        sealed: std::sync::atomic::AtomicBool::new(false),
        direct_subclasses: std::sync::RwLock::new(Vec::new()),
    };
    let static_ref: &'static ClassMetadata =
        with_literal_pool(|pool| pool.static_area.alloc(md));
    // SAFETY: static_ref lives in the static area (process-lived).
    unsafe { register_user_class(static_ref) };
    (id, static_ref as *const ClassMetadata)
}

/// Builder-style helper: register a single-inheritance user class given
/// its name, parent, and own slots. The slot offsets are computed
/// automatically (own slots appended after the parent's). Sprint 14:
/// for multi-parent classes, use `register_mi_user_class` which takes
/// the merged slot list directly.
///
/// Sprint 21: a `parent = None` arg is reinterpreted as `parent =
/// Some(<object>)` so the CPL chain reaches `<object>` and
/// `is_subclass(c, <object>)` holds for every user-registered class.
/// This restores the Dylan semantics that every class is implicitly a
/// subclass of `<object>` — required for stdlib methods declared as
/// `(p :: <object>)` to dispatch on user-class instances.
pub fn register_simple_user_class(
    name: &str,
    parent: Option<ClassId>,
    own_slots: Vec<SlotInfo>,
) -> (ClassId, *const ClassMetadata) {
    let parent = parent.or(Some(ClassId::OBJECT));
    let parents: Vec<ClassId> = parent.into_iter().collect();
    register_mi_user_class_simple(name, parent, &parents, own_slots)
}

/// SI fast path used internally — same shape as the Sprint 12 helper.
fn register_mi_user_class_simple(
    name: &str,
    parent: Option<ClassId>,
    parents: &[ClassId],
    own_slots: Vec<SlotInfo>,
) -> (ClassId, *const ClassMetadata) {
    // Inherit parent's slot list, then append our own at the next
    // offset. For SI this matches the Sprint 12 behaviour.
    let (inherited, inherited_origin): (Vec<SlotInfo>, Vec<ClassId>) = match parent {
        Some(p) => {
            let pmd = class_metadata_for(p);
            (pmd.slots.clone(), pmd.slot_origin.clone())
        }
        None => (Vec::new(), Vec::new()),
    };
    let inherited_slot_count = inherited.len();
    let mut all_slots = inherited;
    let mut slot_origin = inherited_origin;
    // Placeholder for "self id" — patched after registration when we know
    // the freshly minted ClassId. Until then, use a sentinel; the post-
    // registration step rewrites both `cpl[0]` and any `slot_origin[i]
    // == sentinel` entries.
    let self_sentinel = ClassId(u32::MAX);
    for (i, mut slot) in own_slots.into_iter().enumerate() {
        let slot_idx = inherited_slot_count + i;
        slot.offset = std::mem::size_of::<Wrapper>() + slot_idx * 8;
        all_slots.push(slot);
        slot_origin.push(self_sentinel);
    }
    let own_slot_count = all_slots.len() - inherited_slot_count;
    // CPL: [self, parent.cpl...]
    let mut cpl = vec![ClassId(0)]; // placeholder for self, filled below
    if let Some(p) = parent {
        let pmd = class_metadata_for(p);
        cpl.extend(pmd.cpl.iter().copied());
    }
    let spec = UserClassSpec {
        name: name.to_string(),
        parent,
        parents: parents.to_vec(),
        cpl,
        slots: all_slots,
        slot_origin,
        own_slot_count,
        inherited_slot_count,
    };
    let (id, md_ptr) = register_user_class_metadata(spec);
    // Patch the CPL's first entry + any `slot_origin == sentinel` entries
    // to point to the freshly minted id.
    // SAFETY: md_ptr points at the just-registered metadata in the
    // static area. We hold exclusive access (registration is the only
    // writer; no GC can touch this metadata).
    unsafe {
        let md_mut = md_ptr as *mut ClassMetadata;
        (&mut (*md_mut).cpl)[0] = id;
        for origin in (*md_mut).slot_origin.iter_mut() {
            if *origin == self_sentinel {
                *origin = id;
            }
        }
    }
    (id, md_ptr)
}

/// Sprint 14: register a user class with explicit MI shape — caller
/// supplies the C3-computed CPL, the merged slot list (one entry per
/// slot in layout order, offsets already patched), the per-slot
/// `slot_origin` vector, and the count split (own vs inherited).
///
/// Used by `nod-sema::lower` for MI classes, which run C3 and the
/// merge-slots pass themselves so the runtime stays algorithm-free.
pub fn register_mi_user_class(
    name: &str,
    parents: Vec<ClassId>,
    cpl: Vec<ClassId>,
    slots: Vec<SlotInfo>,
    slot_origin: Vec<ClassId>,
    own_slot_count: usize,
    inherited_slot_count: usize,
) -> (ClassId, *const ClassMetadata) {
    let parent = parents.first().copied();
    // The supplied CPL must begin with a `ClassId(0)` placeholder for
    // self at index 0 — we patch it after the id is minted. This mirrors
    // the SI helper above so both paths share the post-patch step.
    let spec = UserClassSpec {
        name: name.to_string(),
        parent,
        parents,
        cpl,
        slots,
        slot_origin,
        own_slot_count,
        inherited_slot_count,
    };
    let (id, md_ptr) = register_user_class_metadata(spec);
    // SAFETY: md_ptr points at the just-registered metadata in the
    // static area. We hold exclusive access (registration is the only
    // writer; no GC can touch this metadata).
    unsafe {
        let md_mut = md_ptr as *mut ClassMetadata;
        if let Some(slot0) = (*md_mut).cpl.first_mut()
            && (slot0.0 == 0 || *slot0 == ClassId(u32::MAX))
        {
            *slot0 = id;
        }
        // Patch any `slot_origin` sentinels (for own slots whose origin
        // is "self" — caller can use `ClassId(u32::MAX)` as the sentinel
        // or just hand back `id` directly).
        let self_sentinel = ClassId(u32::MAX);
        for origin in (*md_mut).slot_origin.iter_mut() {
            if *origin == self_sentinel {
                *origin = id;
            }
        }
    }
    (id, md_ptr)
}

/// Atomically store `src` into `*dst_ptr` and mark the corresponding
/// card. The canonical Rust-side write path for storing a Word into a
/// heap-resident slot. Use this anywhere the runtime mutates a Word
/// slot inside an old-generation object — including vector slot writes,
/// symbol intern-table updates, and any future class slot setter.
///
/// JIT-emitted code stores directly (no barrier) until Sprint 12 wires
/// `Computation::WriteBarrier` into the codegen path.
///
/// # Safety
///
/// `dst_ptr` must point at a valid, writable `Word` slot. If the slot
/// is inside the moveable heap (old.live), the write is recorded in
/// the card table; if it isn't, the card mark is a no-op. The caller
/// must not race other writers on the same slot.
pub unsafe fn write_barrier(dst_ptr: *mut Word, src: Word) {
    // Mark first, then store. The reverse ordering would create a brief
    // window in which the new pointer is visible without the card being
    // dirty — fine for synchronous GC (Sprint 11) but the right
    // discipline now for when concurrent GC arrives.
    with_literal_pool(|pool| pool.heap.mark_card_for(dst_ptr));
    // SAFETY: per caller's contract.
    unsafe { *dst_ptr = src };
}

/// Public-facing snapshot of GC counters. Returned by `gc_stats()`.
#[derive(Copy, Clone, Debug, Default)]
pub struct GcStats {
    pub minor_collections: u64,
    pub major_collections: u64,
    pub young_bytes_allocated: u64,
    pub young_bytes_live: u64,
    pub old_bytes_live: u64,
    pub last_minor_pause_ns: u64,
    pub last_major_pause_ns: u64,
    /// Cumulative wall-clock pause time across all minor collections.
    pub total_minor_pause_ns: u64,
    /// Cumulative wall-clock pause time across all major collections.
    pub total_major_pause_ns: u64,
    /// Root-slot count at the most recent minor GC.
    pub roots_at_last_minor: u64,
    /// Root-slot count at the most recent major GC.
    pub roots_at_last_major: u64,
    /// Cumulative bytes promoted from young generation to old.
    pub bytes_promoted: u64,
    pub last_pinned_objects: u64,
    pub peak_young_bytes_live: u64,
    pub peak_old_bytes_live: u64,
    pub heap_backend: &'static str,
}

/// Snapshot the process-global heap's GC stats.
pub fn gc_stats() -> GcStats {
    with_literal_pool(|pool| {
        let s = pool.heap.stats_snapshot();
        GcStats {
            minor_collections: s.minor_collections,
            major_collections: s.major_collections,
            young_bytes_allocated: s.young_bytes_allocated,
            young_bytes_live: s.young_bytes_live,
            old_bytes_live: s.old_bytes_live,
            last_minor_pause_ns: s.last_minor_pause_ns,
            last_major_pause_ns: s.last_major_pause_ns,
            total_minor_pause_ns: s.total_minor_pause_ns,
            total_major_pause_ns: s.total_major_pause_ns,
            roots_at_last_minor: s.roots_at_last_minor,
            roots_at_last_major: s.roots_at_last_major,
            bytes_promoted: s.bytes_promoted,
            last_pinned_objects: s.last_pinned_objects,
            peak_young_bytes_live: s.peak_young_bytes_live,
            peak_old_bytes_live: s.peak_old_bytes_live,
            heap_backend: HEAP_BACKEND_NAME,
        }
    })
}

/// Backend-name string surfaced by `gc_stats().heap_backend`. Sprint 23:
/// `"page-mark-evacuate"` under the default `newgc-backend` feature;
/// `"semispace"` under the `--no-default-features --features
/// semispace-backend` escape hatch.
#[cfg(feature = "newgc-backend")]
const HEAP_BACKEND_NAME: &str = "page-mark-evacuate";
#[cfg(feature = "semispace-backend")]
const HEAP_BACKEND_NAME: &str = "semispace";

/// Trigger a minor GC of the process-global heap. Used by `:gc-stats`,
/// stress tests, and `--gc-trace` callers.
pub fn collect_minor() {
    with_literal_pool(|pool| pool.heap.collect_minor());
}

/// Trigger a full GC of the process-global heap.
pub fn collect_full() {
    with_literal_pool(|pool| pool.heap.collect_full());
}

/// Multi-line text rendering of `gc_stats()` for `:gc-stats` /
/// `--gc-trace`. Stable shape; suitable for assertion in tests.
pub fn gc_stats_report() -> String {
    let s = gc_stats();
    format!(
        "GC stats (backend = {})\n  \
         minor collections : {}\n  \
         major collections : {}\n  \
         young allocated   : {} bytes\n  \
         young live        : {} bytes\n  \
         old live          : {} bytes\n  \
         last minor pause  : {} ns\n  \
         last major pause  : {} ns\n  \
         total minor pause : {} ns\n  \
         total major pause : {} ns\n  \
         roots last minor  : {}\n  \
         roots last major  : {}\n  \
         bytes promoted    : {} bytes\n  \
         last pinned objs  : {}\n  \
         peak young live   : {} bytes\n  \
         peak old live     : {} bytes\n",
        s.heap_backend,
        s.minor_collections,
        s.major_collections,
        s.young_bytes_allocated,
        s.young_bytes_live,
        s.old_bytes_live,
        s.last_minor_pause_ns,
        s.last_major_pause_ns,
        s.total_minor_pause_ns,
        s.total_major_pause_ns,
        s.roots_at_last_minor,
        s.roots_at_last_major,
        s.bytes_promoted,
        s.last_pinned_objects,
        s.peak_young_bytes_live,
        s.peak_old_bytes_live,
    )
}

// -- Tracing flag ------------------------------------------------------------
//
// Set by `--gc-trace` (driver-side). When true, GC entry/exit and pause
// times are logged to stderr. Sprint 11 exposes the toggle; the driver
// wires `--gc-trace` in a follow-up commit.

use std::sync::atomic::{AtomicBool, Ordering};

static GC_TRACE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_gc_trace(enabled: bool) {
    GC_TRACE_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn gc_trace_enabled() -> bool {
    GC_TRACE_ENABLED.load(Ordering::Relaxed)
}
