//! Sprint 24 — closures (free-variable capture).
//!
//! Two new heap-resident classes light up the closure machinery:
//!
//! 1. **`<cell>`** — a one-slot heap box that holds a Dylan Word. Used
//!    to promote captured local variables: instead of living as an SSA
//!    temp / stack frame slot, the captured local becomes a `<cell>`
//!    and reads / writes (including the outer scope's reads / writes,
//!    and `:=` mutation inside the closure body) go through cell-get /
//!    cell-set. This gives by-reference capture — the canonical Dylan
//!    semantics — and lets the outer scope and any number of inner
//!    closures share the same storage.
//!
//! 2. **`<environment>`** — a per-closure heap record holding a
//!    `<simple-object-vector>` of `<cell>` pointers (one slot per
//!    captured variable). The fresh `<function>` Word created at each
//!    closure site has `env-ptr` pointing at the environment; the
//!    runtime `nod_funcall_N` trampolines read `env-ptr` and dispatch
//!    the body either as a plain `(args) -> u64` (no captures) or as
//!    `(env, args) -> u64` (closure).
//!
//! Approach 1 from the brief: a single `<environment>` class with one
//! `cells: <simple-object-vector>` slot. Access is `%cell-get(%env-cell(env, i))`
//! which expands to two loads. Dynamic arity — one class, many closures.
//!
//! ## GC integration
//!
//! - `<cell>::value` is `SlotType::Object` (pointer-shaped); the
//!   `user_class_scan` walker visits it on every collection. Cells live
//!   in the moveable heap.
//! - `<environment>::cells` is `SlotType::Vector` (pointer-shaped); same.
//!   The vector itself stores `<cell>`-tagged Words, scanned via
//!   `SimpleObjectVector`'s own per-slot walker.
//! - The `<function>::env-ptr` slot moves from `SlotType::Integer` to
//!   `SlotType::Object` so closures whose env lives in the moveable
//!   heap survive GC. Top-level functions still pass `env_ptr = 0`
//!   (which is `Word::from_raw(0)` — fixnum-tagged zero); the GC's
//!   classify shunt treats it as an immediate and skips it.

use std::sync::OnceLock;

use crate::classes::{
    ClassId, ClassMetadata, SlotDefault, SlotInfo, SlotType, class_metadata_for,
};
use crate::make::rust_make;
use crate::word::Word;

struct ClosureClassIds {
    cell: ClassId,
    environment: ClassId,
    cell_md: &'static ClassMetadata,
    environment_md: &'static ClassMetadata,
}

static CLOSURE_CLASSES: OnceLock<ClosureClassIds> = OnceLock::new();

/// Register `<cell>` and `<environment>` idempotently. Safe to call
/// repeatedly. Routes through the same `register_simple_user_class`
/// the rest of the runtime uses.
pub fn ensure_registered() {
    let _ = CLOSURE_CLASSES.get_or_init(|| {
        let (cell, _) = crate::register_simple_user_class(
            "<cell>",
            Some(ClassId::OBJECT),
            vec![slot_object("value", "value")],
        );
        let cell_md = class_metadata_for(cell);

        let (environment, _) = crate::register_simple_user_class(
            "<environment>",
            Some(ClassId::OBJECT),
            vec![slot_vector("cells", "cells")],
        );
        let environment_md = class_metadata_for(environment);

        ClosureClassIds {
            cell,
            environment,
            cell_md,
            environment_md,
        }
    });
}

fn slot_object(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::Object,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: true,
    }
}

fn slot_vector(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::Vector,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: true,
    }
}

fn classes() -> &'static ClosureClassIds {
    ensure_registered();
    CLOSURE_CLASSES.get().expect("closure classes registered")
}

pub fn cell_class_id() -> ClassId {
    classes().cell
}

pub fn environment_class_id() -> ClassId {
    classes().environment
}

// ─── <cell> builders + accessors ──────────────────────────────────────────

/// Allocate a `<cell>` initialised with `value`. The resulting Word is
/// pointer-tagged and lives in the moveable heap.
pub fn make_cell(value: Word) -> Word {
    let md = classes().cell_md;
    // SAFETY: registered metadata; init-keyword name matches the slot.
    unsafe { rust_make(md, &[("value", value)]) }
}

/// Read the `value` slot from a `<cell>` Word.
///
/// # Safety
///
/// `cell_w` must be a pointer-tagged `<cell>` Word.
unsafe fn cell_value_ptr(cell_w: Word) -> *mut Word {
    let md = classes().cell_md;
    let p = cell_w
        .as_mut_ptr::<u8>()
        .expect("cell_value_ptr: argument is not a pointer-tagged Word");
    let offset = md
        .slot_offset("value")
        .expect("<cell> has a `value` slot");
    (p as usize + offset) as *mut Word
}

/// JIT-callable `%cell-get(c) -> <object>`. Loads the `value` slot.
///
/// # Safety
///
/// `cell_raw` must be a pointer-tagged `<cell>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_cell_get(cell_raw: u64) -> u64 {
    let cell = Word::from_raw(cell_raw);
    // SAFETY: caller's contract.
    let slot_ptr = unsafe { cell_value_ptr(cell) };
    // SAFETY: slot_ptr is inside the live <cell> allocation.
    unsafe { *slot_ptr }.raw()
}

/// JIT-callable `%cell-set!(value, c) -> value`. Writes the `value`
/// slot through the GC write barrier. Returns the new value (Dylan
/// setter convention).
///
/// # Safety
///
/// `cell_raw` must be a pointer-tagged `<cell>` Word; `value_raw` is
/// any Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_cell_set(value_raw: u64, cell_raw: u64) -> u64 {
    let cell = Word::from_raw(cell_raw);
    let value = Word::from_raw(value_raw);
    // SAFETY: caller's contract.
    let slot_ptr = unsafe { cell_value_ptr(cell) };
    // SAFETY: slot_ptr is inside the live <cell> allocation.
    unsafe { crate::write_barrier(slot_ptr, value) };
    value_raw
}

/// JIT-callable `%make-cell(v) -> <cell>`. Allocates a fresh cell.
///
/// # Safety
///
/// `value_raw` is any Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_make_cell(value_raw: u64) -> u64 {
    let v = Word::from_raw(value_raw);
    // Root `v` across the allocation in case `make_cell` triggers a
    // minor GC that evacuates the pointed-at object.
    let _g = crate::make::RootGuard::new(&v);
    make_cell(v).raw()
}

// ─── GAP-004 — `define variable` getter/setter shims by name ──────────────
//
// These are the runtime side of the variable-by-name lowering Option A
// in the GAP-004 design: getter and setter call sites pass the
// variable's source name as a Dylan `<byte-string>` Word. The runtime
// decodes the name, looks up the variable's cell-slot in the process-
// global table, loads the cell pointer, and delegates to nod_cell_get
// / nod_cell_set. Optimisation (slot pointer per call site) is a
// future sprint — see the GAP-004 design notes in COMPILER_GAPS.md.

/// JIT-callable `%var-get(name) -> <object>`. The name Word must be a
/// Dylan `<byte-string>` whose contents identify a previously-
/// registered `define variable`. Panics if the variable is not yet
/// initialised (cell slot still zero) — that indicates the AOT
/// resolver or JIT-side init driver failed to call
/// `nod_aot_register_variable` for this name before user code reached
/// the getter.
///
/// # Safety
///
/// `name_raw` must be a pointer-tagged `<byte-string>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_var_get_by_name(name_raw: u64) -> u64 {
    let name_w = Word::from_raw(name_raw);
    // SAFETY: the sema layer enforces the byte-string type at the call
    // site (it always passes an interned string literal).
    let Some(bs) =
        (unsafe { crate::try_byte_string(name_w, ClassId::BYTE_STRING) })
    else {
        panic!(
            "nod_var_get_by_name: expected <byte-string> name, got raw Word {:#x}",
            name_raw
        );
    };
    // SAFETY: `bs` borrows the live byte-string payload.
    let name = unsafe { std::str::from_utf8_unchecked(bs.bytes()) };
    let slot = crate::variable_cell_slot_addr(name);
    let cell_bits = slot.load(std::sync::atomic::Ordering::Acquire);
    if cell_bits == 0 {
        panic!(
            "nod_var_get_by_name: variable `{name}` referenced before \
             initialisation — the AOT resolver or JIT init driver did \
             not call nod_aot_register_variable for it"
        );
    }
    // Delegate to the cell-get path.
    // SAFETY: `cell_bits` came from a successful `nod_make_cell` call
    // at init time and the GC rewrites the slot when the cell moves,
    // so the bits are always a live pointer-tagged `<cell>` Word.
    unsafe { nod_cell_get(cell_bits) }
}

/// JIT-callable `%var-set!(value, name) -> value`. Stores `value`
/// into the variable's underlying `<cell>` and returns the new value
/// (Dylan setter convention). Panics if the variable is not yet
/// initialised.
///
/// # Safety
///
/// `name_raw` must be a pointer-tagged `<byte-string>` Word; `value_raw`
/// is any Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_var_set_by_name(
    value_raw: u64,
    name_raw: u64,
) -> u64 {
    let name_w = Word::from_raw(name_raw);
    let Some(bs) =
        (unsafe { crate::try_byte_string(name_w, ClassId::BYTE_STRING) })
    else {
        panic!(
            "nod_var_set_by_name: expected <byte-string> name, got raw Word {:#x}",
            name_raw
        );
    };
    let name = unsafe { std::str::from_utf8_unchecked(bs.bytes()) };
    let slot = crate::variable_cell_slot_addr(name);
    let cell_bits = slot.load(std::sync::atomic::Ordering::Acquire);
    if cell_bits == 0 {
        panic!(
            "nod_var_set_by_name: variable `{name}` written before \
             initialisation"
        );
    }
    // SAFETY: see nod_var_get_by_name.
    unsafe { nod_cell_set(value_raw, cell_bits) }
}

// ─── <environment> builders + accessors ───────────────────────────────────

/// Allocate a fresh `<environment>` whose `cells` slot holds a
/// `<simple-object-vector>` of `cell_words` (which must each be a
/// pointer-tagged `<cell>` Word). The cells vector is laid out in the
/// order given.
pub fn make_environment(cell_words: &[Word]) -> Word {
    let md = classes().environment_md;
    // Allocate the vector first. Root each cell Word across the
    // allocation so a minor GC during vector-alloc doesn't strand
    // them.
    let _cell_guards: Vec<crate::make::RootGuard> = cell_words
        .iter()
        .map(crate::make::RootGuard::new)
        .collect();
    let vec_word = crate::with_literal_pool(|pool| {
        pool.heap
            .alloc_simple_object_vector(cell_words.len(), &pool.classes)
    });
    // Fill the vector slots through the write barrier.
    // SAFETY: vec_word is a fresh <simple-object-vector> we just
    // allocated; no aliasing readers.
    if let Some(sov) = unsafe {
        crate::try_simple_object_vector_mut(vec_word, ClassId::SIMPLE_OBJECT_VECTOR)
    } {
        // SAFETY: same.
        let slots = unsafe { sov.slots_mut() };
        for (i, &w) in cell_words.iter().enumerate() {
            let slot_ptr = &mut slots[i] as *mut Word;
            // SAFETY: slot_ptr is inside the live SOV allocation.
            unsafe { crate::write_barrier(slot_ptr, w) };
        }
    }
    // Root the vector Word across the environment allocation.
    let _vec_guard = crate::make::RootGuard::new(&vec_word);
    // SAFETY: registered metadata; init-keyword name matches the slot.
    unsafe { rust_make(md, &[("cells", vec_word)]) }
}

/// Read the `cells` slot of an `<environment>` Word as the underlying
/// `<simple-object-vector>` Word.
///
/// # Safety
///
/// `env_w` must be a pointer-tagged `<environment>` Word.
pub unsafe fn env_cells_vector(env_w: Word) -> Word {
    let md = classes().environment_md;
    let p = env_w
        .as_ptr::<u8>()
        .expect("env_cells_vector: argument is not a pointer-tagged Word");
    let offset = md
        .slot_offset("cells")
        .expect("<environment> has a `cells` slot");
    // SAFETY: slot is inside the <environment> allocation.
    unsafe { *((p as usize + offset) as *const Word) }
}

/// JIT-callable `%env-cell(env, idx) -> <cell>`. Returns the cell Word
/// at the given index (NOT the cell's value — call `%cell-get` on the
/// result to read it).
///
/// # Safety
///
/// `env_raw` must be a pointer-tagged `<environment>` Word; `idx_raw`
/// must be a fixnum-tagged Word in `0 ..= cells.len() - 1`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_env_cell(env_raw: u64, idx_raw: u64) -> u64 {
    let env = Word::from_raw(env_raw);
    let idx = Word::from_raw(idx_raw).as_fixnum().unwrap_or(-1);
    // SAFETY: caller's contract.
    let cells_vec = unsafe { env_cells_vector(env) };
    // SAFETY: same.
    let sov = unsafe { crate::try_simple_object_vector(cells_vec, ClassId::SIMPLE_OBJECT_VECTOR) };
    let Some(sov) = sov else {
        panic!(
            "nod_env_cell: env's cells slot is not a <simple-object-vector> (env raw = {:#x})",
            env_raw
        );
    };
    let len = sov.len as i64;
    if idx < 0 || idx >= len {
        panic!(
            "nod_env_cell: index {idx} out of range for environment of size {len} \
             (env raw = {env_raw:#x})"
        );
    }
    // SAFETY: bounds checked.
    let slots = unsafe { sov.slots() };
    slots[idx as usize].raw()
}

/// JIT-callable `%make-environment(cells_vec) -> <environment>`. The
/// caller passes a pre-built `<simple-object-vector>` containing the
/// initial cell Words. The Sprint 24 lowerer uses this together with
/// `%make-sov` so that closure-creation sites can build the env from
/// already-promoted cell-Words.
///
/// # Safety
///
/// `cells_vec_raw` must be a pointer-tagged `<simple-object-vector>`
/// Word, every element of which is a pointer-tagged `<cell>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_make_environment(cells_vec_raw: u64) -> u64 {
    let cells_vec = Word::from_raw(cells_vec_raw);
    // Root the supplied vector across the env allocation.
    let _g = crate::make::RootGuard::new(&cells_vec);
    let md = classes().environment_md;
    // SAFETY: registered metadata; init-keyword matches.
    unsafe { rust_make(md, &[("cells", cells_vec)]) }.raw()
}

/// True iff `w` is a pointer-tagged `<environment>` Word.
pub fn is_environment(w: Word) -> bool {
    let Some(p) = w.as_ptr::<u8>() else {
        return false;
    };
    // SAFETY: pointer-tagged Word; first 8 bytes are the Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    crate::is_subclass(wrapper.class(), classes().environment)
}

/// True iff `w` is a pointer-tagged `<cell>` Word.
pub fn is_cell(w: Word) -> bool {
    let Some(p) = w.as_ptr::<u8>() else {
        return false;
    };
    // SAFETY: pointer-tagged Word; first 8 bytes are the Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    crate::is_subclass(wrapper.class(), classes().cell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_round_trips_a_fixnum() {
        ensure_registered();
        let initial = Word::from_fixnum(42).unwrap();
        let c = make_cell(initial);
        assert!(is_cell(c));
        // SAFETY: c is a real <cell>.
        let got = unsafe { nod_cell_get(c.raw()) };
        assert_eq!(Word::from_raw(got).as_fixnum(), Some(42));
        // Mutate.
        let new_val = Word::from_fixnum(99).unwrap();
        // SAFETY: c is a real <cell>, new_val is a tagged Word.
        let returned = unsafe { nod_cell_set(new_val.raw(), c.raw()) };
        assert_eq!(returned, new_val.raw());
        // SAFETY: same.
        let got2 = unsafe { nod_cell_get(c.raw()) };
        assert_eq!(Word::from_raw(got2).as_fixnum(), Some(99));
    }

    #[test]
    fn environment_indexed_access() {
        ensure_registered();
        let a = make_cell(Word::from_fixnum(10).unwrap());
        let b = make_cell(Word::from_fixnum(20).unwrap());
        let c = make_cell(Word::from_fixnum(30).unwrap());
        let env = make_environment(&[a, b, c]);
        assert!(is_environment(env));
        for (i, expected) in [10, 20, 30].iter().enumerate() {
            let idx = Word::from_fixnum(i as i64).unwrap();
            // SAFETY: env is a real <environment>, idx is a fixnum in range.
            let cell_raw = unsafe { nod_env_cell(env.raw(), idx.raw()) };
            // SAFETY: cell_raw is a real <cell> Word.
            let value_raw = unsafe { nod_cell_get(cell_raw) };
            assert_eq!(Word::from_raw(value_raw).as_fixnum(), Some(*expected));
        }
    }

    #[test]
    fn cell_aliasing_through_env() {
        // The outer scope can read/write a cell directly; the inner
        // closure reading through env-cell observes the same storage.
        ensure_registered();
        let a = make_cell(Word::from_fixnum(7).unwrap());
        let env = make_environment(&[a]);
        let idx0 = Word::from_fixnum(0).unwrap();
        // SAFETY: env + idx fresh.
        let cell_via_env = unsafe { nod_env_cell(env.raw(), idx0.raw()) };
        // Different Word values but same underlying cell.
        assert_eq!(cell_via_env, a.raw());
        // Mutate through `a` directly; observe through env.
        // SAFETY: a is a fresh cell.
        unsafe {
            nod_cell_set(Word::from_fixnum(11).unwrap().raw(), a.raw());
        }
        // SAFETY: cell_via_env points at the same cell.
        let got = unsafe { nod_cell_get(cell_via_env) };
        assert_eq!(Word::from_raw(got).as_fixnum(), Some(11));
    }
}
