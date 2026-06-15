//! **Stdlib boundary**: new collection APIs go in
//! `stdlib/*.dylan`, not here. This file is a
//! frozen-by-policy host for collection PRIMITIVES that genuinely need
//! Rust (allocation, GC root coordination, tag-aware iteration). See
//! `docs/STDLIB_BOUNDARY.md` — Rule 4 (pre-flight) is the gate.
//!
//! Sprint 20 — collection class hierarchy, forward iteration protocol,
//! and the core collection types (`<range>`, `<stretchy-vector>`,
//! plus collection-protocol attachments to `<list>` and
//! `<simple-object-vector>`).
//!
//! This module owns four things:
//!
//!   1. **Abstract collection class hierarchy** — `<collection>`,
//!      `<mutable-collection>`, `<sequence>`, `<mutable-sequence>`,
//!      `<explicit-key-collection>`, `<stretchy-collection>`,
//!      `<iteration-state>`, `<out-of-range-error>`. Registered as seed
//!      user classes via the same idempotent pattern Sprint 19 uses for
//!      conditions.
//!
//!   2. **Concrete collection classes** — `<range>` (from / to / by) and
//!      `<stretchy-vector>` (length / capacity / backing-vector).
//!      Existing concrete classes (`<simple-object-vector>`, `<pair>`,
//!      `<empty-list>`) keep their seed-class identity but participate in
//!      the FIP via the `collection_of_word` accessor below.
//!
//!   3. **Forward iteration protocol (FIP) state** — a heap-allocated
//!      `<iteration-state>` object bundling the seven DRM-defined
//!      values into one record. The Sprint 20 simplification: we don't
//!      have true multiple values, so the FIP returns a single Word
//!      pointing at an `<iteration-state>`; iteration drivers walk it.
//!      Heap allocations per iteration start are accepted; sealing-driven
//!      inlining (Sprint 22+) will retire most of them.
//!
//!   4. **Rust-side iteration driver + collection ops** —
//!      `size`/`element`/`element-setter`/`do`/`map`/`reduce`/
//!      `concatenate` exposed as Rust APIs. Sprint 20 makes the FIP
//!      machinery real and the headline acceptance tests pass; landing
//!      the equivalent Dylan-side generics (sealed on each concrete
//!      class) is a Sprint 22 follow-up — the spec's
//!      "Dylan-defined-by-macros" direction is honoured by keeping the
//!      operations small and primitive-driven so the macro engine can
//!      later wrap them.
//!
//! ### Deviation from the spec
//!
//! The spec asked for the FIP and the core collection generics to live
//! in `stdlib/*.dylan`. That file doesn't
//! exist yet (the directory is empty as of Sprint 19), and the loader
//! plumbing required to fold a stdlib file into the lowering pass
//! before user code lowers is itself a Sprint 22 task. To keep
//! Sprint 20 self-contained and the acceptance tests reachable, the
//! Sprint 20 collection ops live here as Rust APIs that mirror the
//! sealed-Dylan-generic shape; the Phase G tests exercise them
//! directly. When the Dylan-side stdlib loader is alive (DEFERRED
//! tracker item) the API surface can move into Dylan unchanged. See
//! DEFERRED.md → "Sprint 22 — Dylan-side stdlib for collections".

use std::sync::OnceLock;

use crate::classes::{
    ClassId, ClassMetadata, SlotDefault, SlotInfo, SlotType, class_metadata_for, is_subclass,
};
use crate::make::rust_make;
use crate::word::Word;

// ─── Class-id registration ─────────────────────────────────────────────────

#[allow(dead_code)] // Some accessors only fire from future-sprint Dylan code.
struct CollectionClassIds {
    collection: ClassId,
    mutable_collection: ClassId,
    sequence: ClassId,
    mutable_sequence: ClassId,
    explicit_key_collection: ClassId,
    stretchy_collection: ClassId,
    iteration_state: ClassId,
    range: ClassId,
    stretchy_vector: ClassId,
    out_of_range_error: ClassId,

    // Cached metadata pointers for hot paths.
    iteration_state_md: &'static ClassMetadata,
    range_md: &'static ClassMetadata,
    stretchy_vector_md: &'static ClassMetadata,
    out_of_range_error_md: &'static ClassMetadata,
}

static COLLECTION_CLASSES: OnceLock<CollectionClassIds> = OnceLock::new();

/// Register the Sprint 20 seed collection classes if they aren't already.
/// Idempotent. Called by the LiteralPool initialiser in `lib.rs` and by
/// every public accessor below.
pub fn ensure_registered() {
    // Conditions are a prerequisite: `<out-of-range-error>` inherits
    // from `<error>`.
    crate::conditions::ensure_registered();
    let _ = COLLECTION_CLASSES.get_or_init(|| {
        // Abstract hierarchy. All are slot-less marker classes.
        let (collection, _) =
            crate::register_simple_user_class("<collection>", None, Vec::new());
        let (mutable_collection, _) = crate::register_simple_user_class(
            "<mutable-collection>",
            Some(collection),
            Vec::new(),
        );
        let (sequence, _) =
            crate::register_simple_user_class("<sequence>", Some(collection), Vec::new());
        // `<mutable-sequence>` would in DRM inherit from BOTH
        // `<mutable-collection>` and `<sequence>`. Sprint 20 brief permits
        // falling back to SI parentage if MI registration trips
        // Sprint 14's slot-merge — both abstract parents are slot-less
        // here so the merge would actually succeed, but we keep SI to
        // dodge any latent MI bookkeeping risk (e.g. CPL ordering) and
        // tracker the full DRM shape as a DEFERRED.md item. See
        // README's Sprint 20 leftovers.
        let (mutable_sequence, _) = crate::register_simple_user_class(
            "<mutable-sequence>",
            Some(sequence),
            Vec::new(),
        );
        let (explicit_key_collection, _) = crate::register_simple_user_class(
            "<explicit-key-collection>",
            Some(collection),
            Vec::new(),
        );
        let (stretchy_collection, _) = crate::register_simple_user_class(
            "<stretchy-collection>",
            Some(mutable_collection),
            Vec::new(),
        );

        // `<iteration-state>` bundles the 7 DRM iteration values into
        // one heap record (Sprint 20 simplification — see top-level doc).
        let (iteration_state, _) = crate::register_simple_user_class(
            "<iteration-state>",
            None,
            vec![
                slot_object("state-object", "state-object"),
                slot_object("limit", "limit"),
                slot_object("next-state", "next-state"),
                slot_boolean("finished-state?", "finished-state?"),
                slot_object("current-key", "current-key"),
                slot_object("current-element", "current-element"),
                slot_object("current-element-setter", "current-element-setter"),
                // Tag identifies which concrete-collection FIP minted this
                // state. Used by the iteration driver to dispatch
                // `advance` and `current` to the right primitive without
                // a Dylan-side generic. Values are constants from
                // `FipKind`.
                slot_integer("%fip-kind", "fip-kind"),
            ],
        );

        // Concrete: `<range>` — three fixnum slots. Sprint 26: default
        // `by:` to `1` so the canonical Dylan spec form
        // `make(<range>, from: 1, to: 100)` works without the caller
        // having to spell out the step. The bare default of
        // `SlotDefault::Unbound` leaves `by` zero, which makes the
        // iterator never advance.
        let (range, _) = crate::register_simple_user_class(
            "<range>",
            Some(sequence),
            vec![
                slot_integer("range-from", "from"),
                slot_integer("range-to", "to"),
                slot_integer_default("range-by", "by", 1),
            ],
        );

        // Concrete: `<stretchy-vector>`. Layout:
        //
        //   - `%length`: logical fixnum length
        //   - `%capacity`: physical fixnum capacity
        //   - `%storage`: backing `<simple-object-vector>` Word
        //
        // Grow by 2x when push exceeds capacity. The storage SOV is a
        // heap object the GC scans normally; the `%storage` slot is a
        // pointer-shaped Vector slot so write_barrier-handled stores
        // are tracked.
        let (stretchy_vector, _) = crate::register_simple_user_class(
            "<stretchy-vector>",
            Some(mutable_sequence),
            vec![
                slot_integer("%length", "size"),
                slot_integer("%capacity", "capacity"),
                slot_vector("%storage", "storage"),
            ],
        );

        // `<out-of-range-error>` — Sprint 20 addition to the condition
        // hierarchy. Parent is `<error>` (from Sprint 19). Two slots:
        // `value` and `bounds` (the latter rendered as a fixnum
        // upper-bound for now; full DRM shape lands with the stdlib
        // port).
        let error_id = crate::conditions::error_class_id();
        let (out_of_range_error, _) = crate::register_simple_user_class(
            "<out-of-range-error>",
            Some(error_id),
            vec![
                slot_object("value", "value"),
                slot_integer("bounds", "bounds"),
                slot_str("message", "message"),
            ],
        );

        let iteration_state_md = class_metadata_for(iteration_state);
        let range_md = class_metadata_for(range);
        let stretchy_vector_md = class_metadata_for(stretchy_vector);
        let out_of_range_error_md = class_metadata_for(out_of_range_error);

        // Register generic dispatch methods for <stretchy-vector> so that
        // `element(sv, i)` / `size(sv)` resolve correctly when the compiler
        // cannot statically infer the primitive form.
        //
        // SAFETY: `nod_stretchy_vector_*` shims have the standard
        // `extern "C" fn(u64, ...) -> u64` ABI expected by the dispatcher.
        unsafe {
            crate::dispatch::add_method(
                "element",
                stretchy_vector,
                nod_stretchy_vector_element as *const u8,
                2,
            );
            crate::dispatch::add_method(
                "size",
                stretchy_vector,
                nod_stretchy_vector_size as *const u8,
                1,
            );
            // element-setter(val, sv :: <stretchy-vector>, idx) — receiver is
            // arg[1], so use add_method_full with the specialiser list.
            crate::dispatch::add_method_full(
                "element-setter",
                vec![
                    crate::classes::ClassId::OBJECT,
                    stretchy_vector,
                    crate::classes::ClassId::OBJECT,
                ],
                nod_stretchy_vector_element_setter as *const u8,
                3,
            );
        }

        CollectionClassIds {
            collection,
            mutable_collection,
            sequence,
            mutable_sequence,
            explicit_key_collection,
            stretchy_collection,
            iteration_state,
            range,
            stretchy_vector,
            out_of_range_error,
            iteration_state_md,
            range_md,
            stretchy_vector_md,
            out_of_range_error_md,
        }
    });
}

fn classes() -> &'static CollectionClassIds {
    ensure_registered();
    COLLECTION_CLASSES.get().expect("collections registered")
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

fn slot_integer(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::Integer,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: true,
    }
}

/// `slot_integer` variant that defaults to a fixnum literal when the
/// caller omits the init-keyword. Sprint 26: used for `<range>`'s
/// `range-by` slot so `make(<range>, from: 1, to: 100)` does not leave
/// `by` as zero (which would make the range degenerate).
fn slot_integer_default(name: &str, init_kw: &str, default: i64) -> SlotInfo {
    let default_word = crate::word::Word::from_fixnum(default)
        .expect("slot_integer_default: fixnum default fits");
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::Integer,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Value(default_word),
        has_setter: true,
    }
}

fn slot_boolean(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::Boolean,
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

fn slot_str(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::String,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: false,
    }
}

// ─── Public accessors ──────────────────────────────────────────────────────

pub fn collection_class_id() -> ClassId {
    classes().collection
}
pub fn mutable_collection_class_id() -> ClassId {
    classes().mutable_collection
}
pub fn sequence_class_id() -> ClassId {
    classes().sequence
}
pub fn mutable_sequence_class_id() -> ClassId {
    classes().mutable_sequence
}
pub fn explicit_key_collection_class_id() -> ClassId {
    classes().explicit_key_collection
}
pub fn stretchy_collection_class_id() -> ClassId {
    classes().stretchy_collection
}
pub fn iteration_state_class_id() -> ClassId {
    classes().iteration_state
}
pub fn range_class_id() -> ClassId {
    classes().range
}
pub fn stretchy_vector_class_id() -> ClassId {
    classes().stretchy_vector
}
pub fn out_of_range_error_class_id() -> ClassId {
    classes().out_of_range_error
}

// ─── FIP kind tags ─────────────────────────────────────────────────────────
//
// The 8th slot of `<iteration-state>` (`%fip-kind`) carries a small
// integer identifying which concrete-collection FIP minted the state.
// The iteration driver uses it to dispatch advance / current without a
// Dylan-side generic.

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
#[repr(u8)]
pub enum FipKind {
    /// Sentinel for an empty / finished iteration state.
    Empty = 0,
    /// `<simple-object-vector>` — `%state` is an integer index.
    SimpleObjectVector = 1,
    /// `<pair>` chain (proper list ending in `nil`).
    List = 2,
    /// `<range>` — `%state` is the current fixnum value; advance adds
    /// the `by` step.
    Range = 3,
    /// `<stretchy-vector>` — `%state` is an integer index.
    StretchyVector = 4,
    /// Sprint 42a — `<byte-string>` — `%state` is an integer byte
    /// index; current-element yields the byte as a fixnum-tagged
    /// `<integer>`.
    ByteString = 5,
}

impl FipKind {
    fn from_i64(v: i64) -> Option<Self> {
        match v {
            0 => Some(FipKind::Empty),
            1 => Some(FipKind::SimpleObjectVector),
            2 => Some(FipKind::List),
            3 => Some(FipKind::Range),
            4 => Some(FipKind::StretchyVector),
            5 => Some(FipKind::ByteString),
            _ => None,
        }
    }
}

// ─── Helpers to read/write `<iteration-state>` slots ───────────────────────

fn iter_state_slot_offset(name: &str) -> usize {
    classes()
        .iteration_state_md
        .slot_offset(name)
        .unwrap_or_else(|| panic!("<iteration-state> missing slot `{name}`"))
}

/// Read a slot of an `<iteration-state>` instance. Caller asserts that
/// `state` is a pointer-tagged `<iteration-state>` Word.
fn read_slot(instance: Word, offset: usize) -> Word {
    let p = instance.as_ptr::<u8>().expect("iteration-state is pointer-tagged");
    // SAFETY: caller asserts `instance` is an `<iteration-state>`
    // allocation; its slot at `offset` is a `Word`.
    unsafe { *((p as usize + offset) as *const Word) }
}

/// Write a slot of an `<iteration-state>` instance through the GC's
/// write barrier (the state may live in old gen after a minor GC).
///
/// # Safety
///
/// `instance` must be a pointer-tagged `<iteration-state>` Word.
unsafe fn write_slot(instance: Word, offset: usize, value: Word) {
    let p = instance
        .as_mut_ptr::<u8>()
        .expect("iteration-state is pointer-tagged");
    let slot_ptr = (p as usize + offset) as *mut Word;
    // SAFETY: caller asserts `instance` is an `<iteration-state>`
    // allocation; offset is in bounds (it came from
    // `classes().iteration_state_md.slot_offset`).
    unsafe { crate::write_barrier(slot_ptr, value) };
}

/// Snapshot of an `<iteration-state>`'s fields in Rust-side form. Useful
/// for tests and for the iteration driver, which reads the fields once
/// per loop iteration rather than chasing slot offsets four times.
#[derive(Clone, Copy, Debug)]
pub struct IterStateSnapshot {
    pub state_object: Word,
    pub limit: Word,
    pub finished: bool,
    pub current_key: Word,
    pub current_element: Word,
    pub fip_kind: FipKind,
}

/// Read all FIP slots of an `<iteration-state>` Word into a snapshot.
/// Returns `None` if `state` is not an `<iteration-state>` instance.
pub fn iter_state_snapshot(state: Word) -> Option<IterStateSnapshot> {
    let cls = classes();
    let p = state.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged Word; first 8 bytes are the Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    if wrapper.class() != cls.iteration_state {
        return None;
    }
    let state_object = read_slot(state, iter_state_slot_offset("state-object"));
    let limit = read_slot(state, iter_state_slot_offset("limit"));
    let finished_w = read_slot(state, iter_state_slot_offset("finished-state?"));
    let imm = crate::literal_pool_immediates();
    let finished = finished_w == imm.true_;
    let current_key = read_slot(state, iter_state_slot_offset("current-key"));
    let current_element = read_slot(state, iter_state_slot_offset("current-element"));
    let kind_w = read_slot(state, iter_state_slot_offset("%fip-kind"));
    let fip_kind =
        FipKind::from_i64(kind_w.as_fixnum().unwrap_or(0)).unwrap_or(FipKind::Empty);
    Some(IterStateSnapshot {
        state_object,
        limit,
        finished,
        current_key,
        current_element,
        fip_kind,
    })
}

// ─── forward-iteration-protocol(<collection>) ─────────────────────────────
//
// Allocate a fresh `<iteration-state>` initialised for `coll`. Returns
// the state's Word (pointer-tagged). The shape of the state depends on
// the collection's concrete class.

/// Allocate a fresh `<iteration-state>` whose initial slot values are
/// the supplied fields. Used by every concrete FIP method below.
fn make_iter_state(
    state_object: Word,
    limit: Word,
    finished: bool,
    current_key: Word,
    current_element: Word,
    kind: FipKind,
) -> Word {
    let md = classes().iteration_state_md;
    let imm = crate::literal_pool_immediates();
    let finished_word = if finished { imm.true_ } else { imm.false_ };
    let kind_word = Word::from_fixnum(kind as i64).expect("fip-kind fits in fixnum");
    let nil = imm.nil;
    // SAFETY: registered metadata; init-keywords match registered slot
    // names; values are valid Dylan Words.
    unsafe {
        rust_make(
            md,
            &[
                ("state-object", state_object),
                ("limit", limit),
                ("next-state", nil),
                ("finished-state?", finished_word),
                ("current-key", current_key),
                ("current-element", current_element),
                ("current-element-setter", nil),
                ("fip-kind", kind_word),
            ],
        )
    }
}

/// `forward-iteration-protocol(c :: <collection>) => <iteration-state>`.
/// Dispatches on the concrete class id and allocates the appropriate
/// initial state. Returns `None` if `c`'s class isn't a recognised
/// concrete collection.
pub fn forward_iteration_protocol(c: Word) -> Option<Word> {
    ensure_registered();
    let p = c.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged Dylan Word; first 8 bytes are the Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let cid = wrapper.class();
    let cls = classes();
    let imm = crate::literal_pool_immediates();
    if cid == ClassId::SIMPLE_OBJECT_VECTOR {
        // SAFETY: class match implies SOV layout.
        let sov = unsafe {
            crate::vectors::try_simple_object_vector(c, ClassId::SIMPLE_OBJECT_VECTOR)
                .expect("class match")
        };
        let len = sov.len as i64;
        let zero = Word::from_fixnum(0).expect("0 fits");
        let limit = Word::from_fixnum(len).expect("len fits");
        let finished = len == 0;
        let first_element = if finished {
            imm.nil
        } else {
            // SAFETY: bounds 0 < len, slot is laid out.
            let s = unsafe { sov.slots() };
            s[0]
        };
        let current_key = zero;
        return Some(make_iter_state(
            c,
            limit,
            finished,
            current_key,
            first_element,
            FipKind::SimpleObjectVector,
        ));
    }
    if cid == cls.range {
        let (from, to, by) = range_fields(c)?;
        // Empty range: by > 0 && from > to, or by < 0 && from < to.
        let finished = (by > 0 && from > to) || (by < 0 && from < to) || by == 0;
        let from_w = Word::from_fixnum(from).expect("range from fits");
        let to_w = Word::from_fixnum(to).expect("range to fits");
        let zero = Word::from_fixnum(0).expect("0 fits");
        return Some(make_iter_state(
            from_w,    // state-object = current value
            to_w,      // limit = to
            finished,
            zero,      // current-key = 0
            from_w,    // current-element = current value
            FipKind::Range,
        ));
    }
    if cid == cls.stretchy_vector {
        let (length, _, storage) = stretchy_vector_fields(c)?;
        let zero = Word::from_fixnum(0).expect("0 fits");
        let limit = Word::from_fixnum(length as i64).expect("len fits");
        let finished = length == 0;
        let first_element = if finished {
            imm.nil
        } else {
            // SAFETY: storage is the backing SOV; bounds 0 < length.
            let sov = unsafe {
                crate::vectors::try_simple_object_vector(storage, ClassId::SIMPLE_OBJECT_VECTOR)
                    .expect("stretchy-vector storage is a SOV")
            };
            // SAFETY: same.
            let s = unsafe { sov.slots() };
            s[0]
        };
        return Some(make_iter_state(
            c, // state-object = the stretchy itself
            limit,
            finished,
            zero,
            first_element,
            FipKind::StretchyVector,
        ));
    }
    if cid == ClassId::PAIR {
        // Non-empty list. state-object = current pair; current-key =
        // index 0; current-element = head of first pair.
        // SAFETY: class match.
        let pair = unsafe {
            crate::lists::try_pair(c, ClassId::PAIR).expect("class match")
        };
        let zero = Word::from_fixnum(0).expect("0 fits");
        // limit is unused for lists; stash nil.
        return Some(make_iter_state(
            c,
            imm.nil,
            false,
            zero,
            pair.head,
            FipKind::List,
        ));
    }
    if cid == ClassId::EMPTY_LIST {
        let zero = Word::from_fixnum(0).expect("0 fits");
        return Some(make_iter_state(
            imm.nil,
            imm.nil,
            true, // finished
            zero,
            imm.nil,
            FipKind::List,
        ));
    }
    // Sprint 42a — `<byte-string>` FIP. state-object = the byte-string,
    // limit = byte count, current-key = byte index (starts at 0),
    // current-element = first byte as a fixnum-tagged `<integer>`.
    if cid == ClassId::BYTE_STRING {
        // SAFETY: class match guarantees the layout.
        let bs = unsafe {
            crate::strings::try_byte_string(c, ClassId::BYTE_STRING)
                .expect("class match")
        };
        let len = bs.len as i64;
        let zero = Word::from_fixnum(0).expect("0 fits");
        let limit = Word::from_fixnum(len).expect("len fits");
        let finished = len == 0;
        let first_element = if finished {
            imm.nil
        } else {
            // SAFETY: bounds 0 < len.
            let b = unsafe { bs.bytes() }[0];
            Word::from_fixnum(b as i64).expect("byte fits fixnum")
        };
        return Some(make_iter_state(
            c,
            limit,
            finished,
            zero,
            first_element,
            FipKind::ByteString,
        ));
    }
    None
}

/// Advance an iteration state in-place. After calling, the state's
/// `finished-state?` reflects whether iteration is complete and (if
/// not) `current-element` / `current-key` carry the next item. The
/// state Word itself doesn't change identity.
///
/// Returns the (possibly-updated) state Word so call sites can chain.
pub fn iter_state_advance(state: Word) -> Word {
    ensure_registered();
    let snap = match iter_state_snapshot(state) {
        Some(s) => s,
        None => return state,
    };
    if snap.finished {
        return state;
    }
    let imm = crate::literal_pool_immediates();
    match snap.fip_kind {
        FipKind::Empty => {
            // SAFETY: state is an <iteration-state> (snapshot succeeded).
            unsafe {
                write_slot(state, iter_state_slot_offset("finished-state?"), imm.true_);
            }
        }
        FipKind::SimpleObjectVector => {
            let cur_idx = snap.current_key.as_fixnum().unwrap_or(0);
            let len = snap.limit.as_fixnum().unwrap_or(0);
            let next_idx = cur_idx + 1;
            if next_idx >= len {
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("finished-state?"), imm.true_);
                }
            } else {
                let next_key = Word::from_fixnum(next_idx).expect("idx fits");
                // SAFETY: state_object is the SOV.
                let sov = unsafe {
                    crate::vectors::try_simple_object_vector(
                        snap.state_object,
                        ClassId::SIMPLE_OBJECT_VECTOR,
                    )
                    .expect("FIP state-object is a SOV")
                };
                // SAFETY: bounds checked above; slot is laid out.
                let next_elem = unsafe { sov.slots() }[next_idx as usize];
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("current-key"), next_key);
                    write_slot(
                        state,
                        iter_state_slot_offset("current-element"),
                        next_elem,
                    );
                }
            }
        }
        FipKind::StretchyVector => {
            let cur_idx = snap.current_key.as_fixnum().unwrap_or(0);
            let len = snap.limit.as_fixnum().unwrap_or(0);
            let next_idx = cur_idx + 1;
            if next_idx >= len {
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("finished-state?"), imm.true_);
                }
            } else {
                let next_key = Word::from_fixnum(next_idx).expect("idx fits");
                let (_, _, storage) = stretchy_vector_fields(snap.state_object)
                    .expect("FIP state-object is a <stretchy-vector>");
                // SAFETY: storage is the backing SOV.
                let sov = unsafe {
                    crate::vectors::try_simple_object_vector(storage, ClassId::SIMPLE_OBJECT_VECTOR)
                        .expect("stretchy-vector storage is a SOV")
                };
                // SAFETY: bounds checked above.
                let next_elem = unsafe { sov.slots() }[next_idx as usize];
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("current-key"), next_key);
                    write_slot(
                        state,
                        iter_state_slot_offset("current-element"),
                        next_elem,
                    );
                }
            }
        }
        FipKind::Range => {
            // state-object holds the current value as a fixnum Word;
            // limit holds the `to` bound. We recover `by` from the range
            // object — but the iteration state doesn't carry it. The
            // range FIP stashes `by` in the `next-state` slot.
            let cur = snap.state_object.as_fixnum().unwrap_or(0);
            let to = snap.limit.as_fixnum().unwrap_or(0);
            let next_state_w =
                read_slot(state, iter_state_slot_offset("next-state"));
            let by = next_state_w.as_fixnum().unwrap_or(1);
            let by = if by == 0 { 1 } else { by };
            let next = cur + by;
            let done = (by > 0 && next > to) || (by < 0 && next < to);
            let next_key_idx = snap.current_key.as_fixnum().unwrap_or(0) + 1;
            let next_key = Word::from_fixnum(next_key_idx).expect("idx fits");
            // SAFETY: state is an <iteration-state>.
            unsafe {
                write_slot(state, iter_state_slot_offset("current-key"), next_key);
                if done {
                    write_slot(state, iter_state_slot_offset("finished-state?"), imm.true_);
                } else {
                    let next_w = Word::from_fixnum(next).expect("range val fits");
                    write_slot(state, iter_state_slot_offset("state-object"), next_w);
                    write_slot(state, iter_state_slot_offset("current-element"), next_w);
                }
            }
        }
        FipKind::ByteString => {
            // Sprint 42a — state-object = the byte-string itself;
            // current-key = byte index; limit = byte count; element =
            // current byte (fixnum).
            let cur_idx = snap.current_key.as_fixnum().unwrap_or(0);
            let len = snap.limit.as_fixnum().unwrap_or(0);
            let next_idx = cur_idx + 1;
            if next_idx >= len {
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("finished-state?"), imm.true_);
                }
            } else {
                let next_key = Word::from_fixnum(next_idx).expect("idx fits");
                // SAFETY: state_object is a <byte-string> (FIP guarantees).
                let bs = unsafe {
                    crate::strings::try_byte_string(snap.state_object, ClassId::BYTE_STRING)
                        .expect("FIP state-object is a <byte-string>")
                };
                // SAFETY: bounds checked above; bytes are inline.
                let next_byte = unsafe { bs.bytes() }[next_idx as usize];
                let next_elem =
                    Word::from_fixnum(next_byte as i64).expect("byte fits fixnum");
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("current-key"), next_key);
                    write_slot(
                        state,
                        iter_state_slot_offset("current-element"),
                        next_elem,
                    );
                }
            }
        }
        FipKind::List => {
            // state-object is the current pair. Advance to tail; if tail
            // is nil, finished.
            // SAFETY: state-object is a <pair> (FIP kind guarantees it).
            let pair = match unsafe {
                crate::lists::try_pair(snap.state_object, ClassId::PAIR)
            } {
                Some(p) => p,
                None => {
                    // SAFETY: state is an <iteration-state>.
                    unsafe {
                        write_slot(
                            state,
                            iter_state_slot_offset("finished-state?"),
                            imm.true_,
                        );
                    }
                    return state;
                }
            };
            let tail = pair.tail;
            if tail == imm.nil {
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("finished-state?"), imm.true_);
                }
            } else {
                // tail must be another <pair>; defensively re-check.
                // SAFETY: pointer-tagged inspection.
                let next_pair = match unsafe { crate::lists::try_pair(tail, ClassId::PAIR) } {
                    Some(p) => p,
                    None => {
                        // Improper list ending in a non-nil non-pair —
                        // treat as finished. (DRM: improper lists fail
                        // iteration; we don't yet model the error class.)
                        // SAFETY: state is an <iteration-state>.
                        unsafe {
                            write_slot(
                                state,
                                iter_state_slot_offset("finished-state?"),
                                imm.true_,
                            );
                        }
                        return state;
                    }
                };
                let next_key_idx =
                    snap.current_key.as_fixnum().unwrap_or(0) + 1;
                let next_key = Word::from_fixnum(next_key_idx).expect("idx fits");
                // SAFETY: state is an <iteration-state>.
                unsafe {
                    write_slot(state, iter_state_slot_offset("state-object"), tail);
                    write_slot(state, iter_state_slot_offset("current-key"), next_key);
                    write_slot(
                        state,
                        iter_state_slot_offset("current-element"),
                        next_pair.head,
                    );
                }
            }
        }
    }
    state
}

// Special-case the range-FIP so we can stash `by` in `next-state` after
// initial state construction. (`make_iter_state` always writes `nil` to
// next-state; we overwrite it here for ranges.)
fn install_range_by(state: Word, by: i64) {
    let by_w = Word::from_fixnum(by).expect("by fits");
    // SAFETY: state is an <iteration-state>.
    unsafe {
        write_slot(state, iter_state_slot_offset("next-state"), by_w);
    }
}

// ─── <range> field accessors ───────────────────────────────────────────────

fn range_slot_offset(name: &str) -> usize {
    classes()
        .range_md
        .slot_offset(name)
        .unwrap_or_else(|| panic!("<range> missing slot `{name}`"))
}

/// Read the (from, to, by) fields of a `<range>`. Returns `None` if
/// `r` is not a `<range>` instance.
pub fn range_fields(r: Word) -> Option<(i64, i64, i64)> {
    let cls = classes();
    let p = r.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    if wrapper.class() != cls.range {
        return None;
    }
    let from = read_slot(r, range_slot_offset("range-from"))
        .as_fixnum()
        .unwrap_or(0);
    let to = read_slot(r, range_slot_offset("range-to"))
        .as_fixnum()
        .unwrap_or(0);
    let by = read_slot(r, range_slot_offset("range-by"))
        .as_fixnum()
        .unwrap_or(1);
    Some((from, to, by))
}

/// Allocate a `<range>` instance.
pub fn make_range(from: i64, to: i64, by: i64) -> Word {
    let md = classes().range_md;
    let from_w = Word::from_fixnum(from).expect("range from fits");
    let to_w = Word::from_fixnum(to).expect("range to fits");
    let by_w = Word::from_fixnum(by).expect("range by fits");
    // SAFETY: registered metadata, matching keyword names, valid Words.
    unsafe {
        rust_make(
            md,
            &[("from", from_w), ("to", to_w), ("by", by_w)],
        )
    }
}

/// Sprint 20 JIT-callable `make-range(from, to, by)` shim. Mirrors the
/// keyword-init `make(<range>, from: f, to: t, by: b)` site that Dylan
/// code will eventually drive through `make`; this direct entry point is
/// what the Rust-side tests use today.
///
/// # Safety
///
/// Each fixnum-tagged Word is unwrapped; non-fixnum inputs collapse to
/// 0 (the wrapping `make_range` will still produce a valid `<range>`,
/// it'll just hold zeroes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_make_range(from_raw: u64, to_raw: u64, by_raw: u64) -> u64 {
    let f = Word::from_raw(from_raw).as_fixnum().unwrap_or(0);
    let t = Word::from_raw(to_raw).as_fixnum().unwrap_or(0);
    let b = Word::from_raw(by_raw).as_fixnum().unwrap_or(1);
    make_range(f, t, b).raw()
}

// ─── <stretchy-vector> ─────────────────────────────────────────────────────

fn stretchy_vector_slot_offset(name: &str) -> usize {
    classes()
        .stretchy_vector_md
        .slot_offset(name)
        .unwrap_or_else(|| panic!("<stretchy-vector> missing slot `{name}`"))
}

/// Read the (length, capacity, storage_word) fields of a
/// `<stretchy-vector>`. Returns `None` if `sv` isn't one.
pub fn stretchy_vector_fields(sv: Word) -> Option<(usize, usize, Word)> {
    let cls = classes();
    let p = sv.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    if wrapper.class() != cls.stretchy_vector {
        return None;
    }
    let length = read_slot(sv, stretchy_vector_slot_offset("%length"))
        .as_fixnum()
        .unwrap_or(0) as usize;
    let capacity = read_slot(sv, stretchy_vector_slot_offset("%capacity"))
        .as_fixnum()
        .unwrap_or(0) as usize;
    let storage = read_slot(sv, stretchy_vector_slot_offset("%storage"));
    Some((length, capacity, storage))
}

/// Allocate a fresh `<stretchy-vector>` with the requested initial
/// capacity (the backing SOV's length). The logical length starts at
/// zero. Capacity must be at least 1.
pub fn make_stretchy_vector(initial_capacity: usize) -> Word {
    let cap = initial_capacity.max(1);
    // First allocate the backing SOV. The SOV's slots are zero-filled
    // by `alloc_simple_object_vector`, which is fine — we never read
    // past `%length`.
    let storage = crate::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(cap, &pool.classes)
    });
    // Root the storage Word across the next allocation in case a minor
    // GC fires during the stretchy-vector allocation.
    let storage_local = storage;
    let _guard = crate::make::RootGuard::new(&storage_local);
    let md = classes().stretchy_vector_md;
    let len_w = Word::from_fixnum(0).expect("0 fits");
    let cap_w = Word::from_fixnum(cap as i64).expect("cap fits");
    // SAFETY: registered metadata, matching keyword names.
    unsafe {
        rust_make(
            md,
            &[("size", len_w), ("capacity", cap_w), ("storage", storage_local)],
        )
    }
}

/// Push a value onto the end of a `<stretchy-vector>`. Grows the backing
/// SOV by 2x when capacity is exhausted.
pub fn stretchy_vector_push(sv: Word, value: Word) {
    let (length, capacity, storage) = match stretchy_vector_fields(sv) {
        Some(fields) => fields,
        None => {
            // GAP-011: the caller handed us a stale/dead `<stretchy-vector>`.
            // Print the raw `sv` so a `NOD_GC_TRACE` log can be grepped for
            // this address — to see whether it was ever a registered root and
            // whether the collector rewrote that slot across each cycle.
            eprintln!(
                "[GAP-011] stretchy_vector_push: not a <stretchy-vector>: \
                 sv=0x{:016x} ptr=0x{:016x}",
                sv.raw(),
                sv.raw() & !1,
            );
            // GAP-011: capture the AOT call chain that led here, so we can
            // map the immediate caller's return address back to the Dylan
            // function whose frame holds the unregistered stale-vector slot.
            // `force_capture` ignores `RUST_BACKTRACE` and always captures.
            // GAP-011: capture raw stack-frame IPs via Win32
            // RtlCaptureStackBackTrace — std::backtrace::Backtrace's
            // Display/Debug show only `<unknown>` for AOT EXE frames
            // (no PDB), but the raw IPs are what we actually need to
            // resolve against the EXE's `.map` file.
            #[cfg(windows)]
            {
                unsafe extern "system" {
                    fn RtlCaptureStackBackTrace(
                        frames_to_skip: u32,
                        frames_to_capture: u32,
                        back_trace: *mut *mut core::ffi::c_void,
                        back_trace_hash: *mut u32,
                    ) -> u16;
                    fn GetModuleHandleW(lp: *const u16) -> *mut core::ffi::c_void;
                }
                // EXE base needed to compute the ASLR slide against the
                // `.map`'s preferred load address. `nod-driver symbolicate
                // --runtime-base <hex>` consumes it.
                let exe_base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
                eprintln!("[GAP-011] EXE base (GetModuleHandle NULL): 0x{exe_base:016x}");
                const MAX_FRAMES: usize = 64;
                let mut frames: [*mut core::ffi::c_void; MAX_FRAMES] =
                    [core::ptr::null_mut(); MAX_FRAMES];
                let n = unsafe {
                    RtlCaptureStackBackTrace(
                        0,
                        MAX_FRAMES as u32,
                        frames.as_mut_ptr(),
                        core::ptr::null_mut(),
                    )
                } as usize;
                eprintln!("[GAP-011] push caller backtrace ({n} frames):");
                for (i, &ip) in frames.iter().take(n).enumerate() {
                    eprintln!("  frame {i:>2}: 0x{:016x}", ip as usize);
                }
                eprintln!(
                    "[GAP-011] hint: symbolicate with `nod-driver symbolicate \
                     --map <exe>.map --runtime-base 0x{exe_base:016x} < this-stderr`"
                );
            }
            panic!("stretchy_vector_push: not a <stretchy-vector>");
        }
    };
    // Root the value + vector across any allocations. The guards are
    // NAMED (not `_`-discarded) so the grow path can `reload()` them: the
    // grow allocation can trigger a moving GC that evacuates `sv` and/or
    // `value`, and the collector rewrites the registered root *slots* — but
    // a plain read of the `sv`/`value` locals may reuse a pre-GC register
    // copy (GAP-011). `reload()` forces a fresh read of the slot the
    // collector rewrote.
    let value_local = value;
    let value_guard = crate::make::RootGuard::new(&value_local);
    let sv_local = sv;
    let sv_guard = crate::make::RootGuard::new(&sv_local);

    let storage = if length >= capacity {
        // Grow: allocate a new SOV with 2x capacity, copy elements.
        let new_cap = (capacity.max(1)) * 2;
        let new_storage = crate::with_literal_pool(|pool| {
            pool.heap.alloc_simple_object_vector(new_cap, &pool.classes)
        });
        // Root the new storage across the slot-copy loop (write_barrier
        // is benign on young-gen targets, but the roots keep both
        // storages live across any minor GC the per-slot write triggers
        // — though stores themselves don't allocate, this is belt + braces).
        let new_storage_local = new_storage;
        let _new_storage_guard = crate::make::RootGuard::new(&new_storage_local);
        // The grow alloc above may have evacuated `sv`. Reload it from its
        // registered root slot (a fresh memory read, NOT the possibly-cached
        // `sv_local` register) so we read the post-GC address. Everything
        // below uses `sv_fresh`.
        let sv_fresh = sv_guard.reload();
        let (_, _, fresh_storage) = stretchy_vector_fields(sv_fresh)
            .expect("stretchy_vector_push: sv evacuated mid-grow");
        // SAFETY: fresh_storage is the live backing SOV.
        let src = unsafe {
            crate::vectors::try_simple_object_vector(
                fresh_storage,
                ClassId::SIMPLE_OBJECT_VECTOR,
            )
            .expect("stretchy storage is a SOV")
        };
        // SAFETY: same.
        let src_slots = unsafe { src.slots() };
        // SAFETY: new_storage_local is the freshly-allocated SOV (still rooted).
        let dst = unsafe {
            crate::vectors::try_simple_object_vector_mut(
                new_storage_local,
                ClassId::SIMPLE_OBJECT_VECTOR,
            )
            .expect("new storage is a SOV")
        };
        // SAFETY: same.
        let dst_slots = unsafe { dst.slots_mut() };
        dst_slots[..length].copy_from_slice(&src_slots[..length]);
        // Install new storage + new capacity via write barrier.
        // SAFETY: sv_fresh is the post-GC <stretchy-vector> address.
        unsafe {
            write_slot(
                sv_fresh,
                stretchy_vector_slot_offset("%storage"),
                new_storage_local,
            );
            write_slot(
                sv_fresh,
                stretchy_vector_slot_offset("%capacity"),
                Word::from_fixnum(new_cap as i64).expect("cap fits"),
            );
        }
        new_storage_local
    } else {
        storage
    };

    // Write `value` at index `length` of the storage SOV. Then bump the
    // logical length.
    // SAFETY: storage is the live backing SOV.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector_mut(storage, ClassId::SIMPLE_OBJECT_VECTOR)
            .expect("stretchy storage is a SOV")
    };
    // SAFETY: same.
    let slots = unsafe { sov.slots_mut() };
    // Reload `value` and `sv` from their root slots: if we took the grow
    // path above, the allocation there may have evacuated either one, and
    // the collector rewrote the registered slots — but the `value_local` /
    // `sv_local` registers may be stale (GAP-011). On the non-grow path no
    // GC fired, so `reload()` returns the original value unchanged.
    let value_now = value_guard.reload();
    let sv_now = sv_guard.reload();
    // Use write_barrier for the slot write so card-marking is recorded.
    let slot_ptr = &mut slots[length] as *mut Word;
    // SAFETY: slot_ptr is inside the live SOV allocation.
    unsafe { crate::write_barrier(slot_ptr, value_now) };
    // Bump length.
    // SAFETY: sv_now is the post-GC <stretchy-vector> address.
    unsafe {
        write_slot(
            sv_now,
            stretchy_vector_slot_offset("%length"),
            Word::from_fixnum((length + 1) as i64).expect("len fits"),
        );
    }
}

/// JIT-callable `make(<stretchy-vector>, size:)` shim. Sprint 20 keyword
/// initialiser; allocates with the requested initial capacity.
///
/// # Safety
///
/// `capacity_raw` is a fixnum-tagged Word; non-fixnum collapses to 1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_make_stretchy_vector(capacity_raw: u64) -> u64 {
    let cap = Word::from_raw(capacity_raw).as_fixnum().unwrap_or(0).max(0) as usize;
    make_stretchy_vector(cap).raw()
}

/// JIT-callable `push!(sv, value)` shim.
///
/// # Safety
///
/// `sv_raw` must be a pointer-tagged `<stretchy-vector>` Word; `value`
/// is any Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_stretchy_vector_push(sv_raw: u64, value_raw: u64) -> u64 {
    let sv = Word::from_raw(sv_raw);
    let v = Word::from_raw(value_raw);
    stretchy_vector_push(sv, v);
    sv_raw
}

// ─── <out-of-range-error> builder ──────────────────────────────────────────

/// Allocate an `<out-of-range-error>` instance with the supplied
/// rendered message.
pub fn make_out_of_range_error(value: Word, bounds: i64, message: &str) -> Word {
    let md = classes().out_of_range_error_md;
    let msg = crate::intern_string_literal(message);
    let bounds_w = Word::from_fixnum(bounds).expect("bounds fits");
    // SAFETY: registered metadata, matching keywords, valid Words.
    unsafe {
        rust_make(
            md,
            &[("value", value), ("bounds", bounds_w), ("message", msg)],
        )
    }
}

// ─── `size`, `element`, `element-setter` (Rust-side generic shapes) ───────
//
// These wrap the FIP machinery for callers that need direct access.
// Sprint 22's stdlib port replaces them with `define sealed method`
// dispatches; the API surface stays compatible because the operations
// are pure functions of the input Word.

/// `size(c :: <collection>) => <integer>`. Returns the element count.
/// Returns `None` if `c` is not a recognised collection.
pub fn collection_size(c: Word) -> Option<i64> {
    ensure_registered();
    let p = c.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let cid = wrapper.class();
    let cls = classes();
    if cid == ClassId::SIMPLE_OBJECT_VECTOR {
        // SAFETY: class match.
        let sov = unsafe {
            crate::vectors::try_simple_object_vector(c, ClassId::SIMPLE_OBJECT_VECTOR)
                .expect("class match")
        };
        return Some(sov.len as i64);
    }
    if cid == cls.range {
        let (from, to, by) = range_fields(c)?;
        if by == 0 {
            return Some(0);
        }
        if (by > 0 && from > to) || (by < 0 && from < to) {
            return Some(0);
        }
        // Inclusive count: (to - from) / by + 1, computed in the
        // direction of `by` to handle negative steps.
        let span = to - from;
        let count = span / by + 1;
        return Some(count.max(0));
    }
    if cid == cls.stretchy_vector {
        let (length, _, _) = stretchy_vector_fields(c)?;
        return Some(length as i64);
    }
    if cid == ClassId::EMPTY_LIST {
        return Some(0);
    }
    if cid == ClassId::PAIR {
        // Walk the spine.
        let mut count: i64 = 0;
        let imm = crate::literal_pool_immediates();
        let mut cur = c;
        loop {
            if cur == imm.nil {
                break;
            }
            // SAFETY: cur is a pointer-tagged Word; ask if it's a pair.
            match unsafe { crate::lists::try_pair(cur, ClassId::PAIR) } {
                Some(p) => {
                    count += 1;
                    cur = p.tail;
                }
                None => break, // improper list
            }
        }
        return Some(count);
    }
    // Sprint 42a — `<byte-string>` returns its byte count.
    if cid == ClassId::BYTE_STRING {
        // SAFETY: class match.
        let bs = unsafe {
            crate::strings::try_byte_string(c, ClassId::BYTE_STRING)
                .expect("class match")
        };
        return Some(bs.len as i64);
    }
    None
}

/// `element(c, key, default)`. For sequences `key` is a `<integer>`
/// index. Returns `Err(())` when the index is out of bounds AND no
/// default is supplied. (Sprint 22's stdlib port will turn `Err` into
/// a signalled `<out-of-range-error>`; the runtime API returns `Result`
/// so call sites can decide.)
pub fn collection_element(c: Word, key: i64, default: Option<Word>) -> Result<Word, OutOfRange> {
    ensure_registered();
    let imm = crate::literal_pool_immediates();
    let p = c
        .as_ptr::<u8>()
        .ok_or(OutOfRange { value: c, bounds: 0 })?;
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let cid = wrapper.class();
    let cls = classes();
    if cid == ClassId::SIMPLE_OBJECT_VECTOR {
        // SAFETY: class match.
        let sov = unsafe {
            crate::vectors::try_simple_object_vector(c, ClassId::SIMPLE_OBJECT_VECTOR)
                .expect("class match")
        };
        let len = sov.len as i64;
        if key < 0 || key >= len {
            return match default {
                Some(d) => Ok(d),
                None => Err(OutOfRange { value: c, bounds: len }),
            };
        }
        // SAFETY: bounds checked.
        let slots = unsafe { sov.slots() };
        return Ok(slots[key as usize]);
    }
    if cid == cls.stretchy_vector {
        let (length, _, storage) = stretchy_vector_fields(c).expect("class match");
        let len = length as i64;
        if key < 0 || key >= len {
            return match default {
                Some(d) => Ok(d),
                None => Err(OutOfRange { value: c, bounds: len }),
            };
        }
        // SAFETY: storage is the live backing SOV.
        let sov = unsafe {
            crate::vectors::try_simple_object_vector(storage, ClassId::SIMPLE_OBJECT_VECTOR)
                .expect("stretchy storage is a SOV")
        };
        // SAFETY: bounds checked.
        let slots = unsafe { sov.slots() };
        return Ok(slots[key as usize]);
    }
    if cid == cls.range {
        let (from, to, by) = range_fields(c).expect("class match");
        let size = match (by, from, to) {
            (0, _, _) => 0,
            (b, f, t) if b > 0 && f > t => 0,
            (b, f, t) if b < 0 && f < t => 0,
            (b, f, t) => (t - f) / b + 1,
        };
        if key < 0 || key >= size {
            return match default {
                Some(d) => Ok(d),
                None => Err(OutOfRange { value: c, bounds: size }),
            };
        }
        let v = from + key * by;
        return Ok(Word::from_fixnum(v).unwrap_or(imm.nil));
    }
    if cid == ClassId::PAIR || cid == ClassId::EMPTY_LIST {
        // Walk the spine.
        let mut cur = c;
        let mut idx: i64 = 0;
        loop {
            if cur == imm.nil {
                let total = idx;
                return match default {
                    Some(d) => Ok(d),
                    None => Err(OutOfRange { value: c, bounds: total }),
                };
            }
            // SAFETY: cur is a pointer-tagged Word.
            match unsafe { crate::lists::try_pair(cur, ClassId::PAIR) } {
                Some(p) => {
                    if idx == key {
                        return Ok(p.head);
                    }
                    cur = p.tail;
                    idx += 1;
                }
                None => {
                    return match default {
                        Some(d) => Ok(d),
                        None => Err(OutOfRange { value: c, bounds: idx }),
                    };
                }
            }
        }
    }
    // Sprint 42a — `<byte-string>` returns the byte at `key` as a
    // fixnum-tagged `<integer>` (0..255).
    if cid == ClassId::BYTE_STRING {
        // SAFETY: class match.
        let bs = unsafe {
            crate::strings::try_byte_string(c, ClassId::BYTE_STRING)
                .expect("class match")
        };
        let len = bs.len as i64;
        if key < 0 || key >= len {
            return match default {
                Some(d) => Ok(d),
                None => Err(OutOfRange { value: c, bounds: len }),
            };
        }
        // SAFETY: bounds checked above.
        let b = unsafe { bs.bytes() }[key as usize];
        return Ok(Word::from_fixnum(b as i64).expect("byte fits fixnum"));
    }
    // Unrecognised collection.
    Err(OutOfRange { value: c, bounds: 0 })
}

/// Mutating `element(c, key) := value`. Only the mutable collection
/// types succeed; immutable collections return `Err`.
pub fn collection_element_setter(c: Word, key: i64, value: Word) -> Result<(), OutOfRange> {
    ensure_registered();
    let p = c.as_ptr::<u8>().ok_or(OutOfRange {
        value: c,
        bounds: 0,
    })?;
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let cid = wrapper.class();
    let cls = classes();
    if cid == ClassId::SIMPLE_OBJECT_VECTOR {
        // SAFETY: class match.
        let sov = unsafe {
            crate::vectors::try_simple_object_vector_mut(c, ClassId::SIMPLE_OBJECT_VECTOR)
                .expect("class match")
        };
        let len = sov.len as i64;
        if key < 0 || key >= len {
            return Err(OutOfRange { value: c, bounds: len });
        }
        // SAFETY: bounds checked.
        let slots = unsafe { sov.slots_mut() };
        let slot_ptr = &mut slots[key as usize] as *mut Word;
        // SAFETY: slot_ptr is inside the live SOV allocation.
        unsafe { crate::write_barrier(slot_ptr, value) };
        return Ok(());
    }
    if cid == cls.stretchy_vector {
        let (length, _, storage) = stretchy_vector_fields(c).expect("class match");
        let len = length as i64;
        if key < 0 || key >= len {
            return Err(OutOfRange { value: c, bounds: len });
        }
        // SAFETY: storage is the live backing SOV.
        let sov = unsafe {
            crate::vectors::try_simple_object_vector_mut(
                storage,
                ClassId::SIMPLE_OBJECT_VECTOR,
            )
            .expect("stretchy storage is a SOV")
        };
        // SAFETY: bounds checked.
        let slots = unsafe { sov.slots_mut() };
        let slot_ptr = &mut slots[key as usize] as *mut Word;
        // SAFETY: slot_ptr is inside the live SOV allocation.
        unsafe { crate::write_barrier(slot_ptr, value) };
        return Ok(());
    }
    // Pairs are immutable in DRM (head / tail are slots, but
    // <list> is officially a `<sequence>` not a `<mutable-sequence>`).
    Err(OutOfRange { value: c, bounds: 0 })
}

/// Result of an out-of-range element access. Sprint 22's stdlib turns
/// these into signalled `<out-of-range-error>` conditions; the runtime
/// API leaves the decision to the caller.
#[derive(Clone, Copy, Debug)]
pub struct OutOfRange {
    pub value: Word,
    pub bounds: i64,
}

impl std::fmt::Display for OutOfRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "element out of range: key bounds = 0..{} (exclusive)",
            self.bounds
        )
    }
}

impl std::error::Error for OutOfRange {}

// ─── do / map / reduce drivers (Rust-side) ─────────────────────────────────

/// `do(fn, c)` — drive the FIP, calling `fn(current-element)` each
/// step. The closure's return value is discarded. Returns `()`.
pub fn collection_do(c: Word, mut f: impl FnMut(Word)) {
    let state = match forward_iteration_protocol_init(c) {
        Some(s) => s,
        None => return,
    };
    let _g = crate::make::RootGuard::new(&state);
    while let Some(snap) = iter_state_snapshot(state) {
        if snap.finished {
            break;
        }
        f(snap.current_element);
        iter_state_advance(state);
    }
}

/// `reduce(fn, init, c)` — left-fold over the collection.
pub fn collection_reduce<F: FnMut(Word, Word) -> Word>(c: Word, init: Word, mut f: F) -> Word {
    // `acc_slot` lives on the stack for the duration of the loop. We
    // register its address as a GC root so that if the closure
    // allocates (which it almost always does — `+` boxes its sum), a
    // minor GC fires, and the accumulator's target gets evacuated, the
    // collector can rewrite our slot to the new address. The closure
    // reads/writes through `acc_slot`, not a local — the local copy
    // semantics would defeat the rooting.
    let mut acc_slot: Word = init;
    let _acc_guard = crate::make::RootGuard::new(&acc_slot);
    let state = match forward_iteration_protocol_init(c) {
        Some(s) => s,
        None => return acc_slot,
    };
    let _g = crate::make::RootGuard::new(&state);
    while let Some(snap) = iter_state_snapshot(state) {
        if snap.finished {
            break;
        }
        acc_slot = f(acc_slot, snap.current_element);
        iter_state_advance(state);
    }
    acc_slot
}

/// `map(fn, c) => <collection>` — allocate a fresh same-shaped
/// collection holding the mapped elements.
///
/// For a `<simple-object-vector>` input, returns a `<simple-object-vector>`
/// of the same length. For a `<pair>` / `<empty-list>` input, returns a
/// list. For a `<range>` input, returns a `<simple-object-vector>` (the
/// DRM allows this fallback when the input is immutable and lacks a
/// mutator). For a `<stretchy-vector>` input, returns another
/// `<stretchy-vector>`.
pub fn collection_map<F: FnMut(Word) -> Word>(c: Word, mut f: F) -> Word {
    ensure_registered();
    let imm = crate::literal_pool_immediates();
    let p = match c.as_ptr::<u8>() {
        Some(p) => p,
        None => return imm.nil,
    };
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let cid = wrapper.class();
    let cls = classes();
    if cid == ClassId::SIMPLE_OBJECT_VECTOR || cid == cls.range {
        let len = collection_size(c).unwrap_or(0);
        let out = crate::with_literal_pool(|pool| {
            pool.heap
                .alloc_simple_object_vector(len as usize, &pool.classes)
        });
        let out_local = out;
        let _g = crate::make::RootGuard::new(&out_local);
        let mut idx: i64 = 0;
        collection_do(c, |elem| {
            let mapped = f(elem);
            // Set out[idx] = mapped.
            let _ = collection_element_setter(out_local, idx, mapped);
            idx += 1;
        });
        return out_local;
    }
    if cid == cls.stretchy_vector {
        let len = collection_size(c).unwrap_or(0).max(1) as usize;
        let out = make_stretchy_vector(len);
        let out_local = out;
        let _g = crate::make::RootGuard::new(&out_local);
        collection_do(c, |elem| {
            stretchy_vector_push(out_local, f(elem));
        });
        return out_local;
    }
    if cid == ClassId::EMPTY_LIST {
        return imm.nil;
    }
    if cid == ClassId::PAIR {
        // Build a list by accumulating mapped values, then reverse.
        let mut acc: Vec<Word> = Vec::new();
        collection_do(c, |elem| {
            acc.push(f(elem));
        });
        // Build (a0 . (a1 . (a2 . nil))) right-to-left.
        let mut tail = imm.nil;
        for v in acc.into_iter().rev() {
            tail = crate::with_literal_pool(|pool| {
                pool.heap.alloc_pair(v, tail, &pool.classes)
            });
        }
        return tail;
    }
    // Unrecognised — return nil.
    imm.nil
}

/// `concatenate(c1, c2)` — binary concatenation. For Sprint 20 the
/// result has the class of `c1` when both inputs are the same class;
/// otherwise we widen to `<simple-object-vector>`. (DRM allows
/// `concatenate` to return a class compatible with both arguments; SOV
/// is the safe pick.)
pub fn collection_concatenate(c1: Word, c2: Word) -> Word {
    ensure_registered();
    let imm = crate::literal_pool_immediates();
    let s1 = collection_size(c1).unwrap_or(0);
    let s2 = collection_size(c2).unwrap_or(0);
    let total = s1 + s2;
    let p1 = c1.as_ptr::<u8>();
    let p2 = c2.as_ptr::<u8>();
    let cid1 = p1.map(|p|
        // SAFETY: pointer-tagged.
        unsafe { *(p as *const crate::wrapper::Wrapper) }.class());
    let cid2 = p2.map(|p|
        // SAFETY: pointer-tagged.
        unsafe { *(p as *const crate::wrapper::Wrapper) }.class());

    // Both lists? Build a fresh list.
    let list_class = |c: Option<ClassId>| {
        matches!(c, Some(id) if id == ClassId::PAIR || id == ClassId::EMPTY_LIST)
    };
    if list_class(cid1) && list_class(cid2) {
        let mut elems: Vec<Word> = Vec::with_capacity(total as usize);
        collection_do(c1, |e| elems.push(e));
        collection_do(c2, |e| elems.push(e));
        let mut tail = imm.nil;
        for v in elems.into_iter().rev() {
            tail = crate::with_literal_pool(|pool| {
                pool.heap.alloc_pair(v, tail, &pool.classes)
            });
        }
        return tail;
    }

    // Otherwise build a SOV.
    let out = crate::with_literal_pool(|pool| {
        pool.heap
            .alloc_simple_object_vector(total as usize, &pool.classes)
    });
    let out_local = out;
    let _g = crate::make::RootGuard::new(&out_local);
    let mut idx: i64 = 0;
    collection_do(c1, |e| {
        let _ = collection_element_setter(out_local, idx, e);
        idx += 1;
    });
    collection_do(c2, |e| {
        let _ = collection_element_setter(out_local, idx, e);
        idx += 1;
    });
    out_local
}

// ─── Internal: forward-iteration-protocol with `by`-installation ───────────

pub fn forward_iteration_protocol_init(c: Word) -> Option<Word> {
    let state = forward_iteration_protocol(c)?;
    // For ranges, stash `by` in next-state so advance can read it.
    let snap = iter_state_snapshot(state)?;
    if snap.fip_kind == FipKind::Range
        && let Some((_, _, by)) = range_fields(c)
    {
        install_range_by(state, by);
    }
    Some(state)
}

// ─── instance? checks ──────────────────────────────────────────────────────

/// True iff `w` is an instance of `<collection>` (or any subclass).
pub fn is_collection(w: Word) -> bool {
    ensure_registered();
    let Some(p) = w.as_ptr::<u8>() else {
        return false;
    };
    // SAFETY: pointer-tagged.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let cid = wrapper.class();
    // Concrete seed-classes that participate in the protocol but aren't
    // registered as subclasses of `<collection>` in the seed table.
    if cid == ClassId::SIMPLE_OBJECT_VECTOR
        || cid == ClassId::PAIR
        || cid == ClassId::EMPTY_LIST
        // Sprint 42a — byte-strings participate in size / element / FIP.
        || cid == ClassId::BYTE_STRING
    {
        return true;
    }
    is_subclass(cid, classes().collection)
}

// ─── Sprint 20b — JIT-callable extern shims for stdlib primitives ──────────
//
// Each shim mirrors the same-named Rust API above. The lowering pass in
// `nod-sema/src/lower.rs` recognises `%`-prefixed primitive callees
// (`%range-from`, `%fip-init`, …) and emits `DirectCall` against the
// canonical symbol below. Codegen declares the extern with the matching
// `(u64, …) -> u64` ABI, and `nod_llvm/src/jit.rs` binds it at engine
// creation via `LLVMAddGlobalMapping`.

/// `%range-from(r) -> <integer>` — the `from` field of a `<range>`.
///
/// # Safety
/// `r_raw` must be a pointer-tagged `<range>` Word; non-range inputs
/// return `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_range_from(r_raw: u64) -> u64 {
    let r = Word::from_raw(r_raw);
    let Some((from, _, _)) = range_fields(r) else {
        return Word::from_fixnum(0).expect("0 fits").raw();
    };
    Word::from_fixnum(from).expect("range from fits").raw()
}

/// `%range-to(r) -> <integer>` — the `to` field of a `<range>`.
///
/// # Safety
/// `r_raw` must be a pointer-tagged `<range>` Word; non-range inputs
/// return `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_range_to(r_raw: u64) -> u64 {
    let r = Word::from_raw(r_raw);
    let Some((_, to, _)) = range_fields(r) else {
        return Word::from_fixnum(0).expect("0 fits").raw();
    };
    Word::from_fixnum(to).expect("range to fits").raw()
}

/// `%range-by(r) -> <integer>` — the `by` field of a `<range>`.
///
/// # Safety
/// `r_raw` must be a pointer-tagged `<range>` Word; non-range inputs
/// return `1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_range_by(r_raw: u64) -> u64 {
    let r = Word::from_raw(r_raw);
    let Some((_, _, by)) = range_fields(r) else {
        return Word::from_fixnum(1).expect("1 fits").raw();
    };
    Word::from_fixnum(by).expect("range by fits").raw()
}

/// `%stretchy-vector-size(sv) -> <integer>` — logical length.
///
/// # Safety
/// `sv_raw` must be a pointer-tagged `<stretchy-vector>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_stretchy_vector_size(sv_raw: u64) -> u64 {
    let sv = Word::from_raw(sv_raw);
    let Some((length, _, _)) = stretchy_vector_fields(sv) else {
        return Word::from_fixnum(0).expect("0 fits").raw();
    };
    Word::from_fixnum(length as i64).expect("len fits").raw()
}

/// `%stretchy-vector-element(sv, i) -> <object>` — read index `i`.
/// Returns the pinned `nil` on out-of-range (Sprint 20b doesn't yet
/// surface `<out-of-range-error>` from primitive ops).
///
/// # Safety
/// `sv_raw` must be a pointer-tagged `<stretchy-vector>` Word; `i_raw`
/// is a fixnum-tagged Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_stretchy_vector_element(sv_raw: u64, i_raw: u64) -> u64 {
    let sv = Word::from_raw(sv_raw);
    let i = Word::from_raw(i_raw).as_fixnum().unwrap_or(0);
    match collection_element(sv, i, None) {
        Ok(w) => w.raw(),
        Err(_) => crate::literal_pool_immediates().nil.raw(),
    }
}

/// `%stretchy-vector-element-setter(value, sv, i)` — write index `i`.
/// Returns `value` for `:=` value-propagation. Silently ignores
/// out-of-range writes (matches the existing `collection_element_setter`
/// Err path; signalled errors arrive when Sprint 21 wires them).
///
/// # Safety
/// `sv_raw` must be a pointer-tagged `<stretchy-vector>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_stretchy_vector_element_setter(
    value_raw: u64,
    sv_raw: u64,
    i_raw: u64,
) -> u64 {
    let sv = Word::from_raw(sv_raw);
    let i = Word::from_raw(i_raw).as_fixnum().unwrap_or(0);
    let value = Word::from_raw(value_raw);
    let _ = collection_element_setter(sv, i, value);
    value_raw
}

/// `%collection-size(c) -> <integer>` — dispatches on the concrete class.
///
/// # Safety
/// `c_raw` is any Dylan Word; non-collection inputs return `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_collection_size(c_raw: u64) -> u64 {
    let c = Word::from_raw(c_raw);
    let n = collection_size(c).unwrap_or(0);
    Word::from_fixnum(n).expect("size fits").raw()
}

/// `%collection-concatenate(c1, c2) -> <collection>` — binary concat.
///
/// # Safety
/// Both args are Dylan Words; non-collection inputs collapse to
/// `nil`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_collection_concatenate(c1_raw: u64, c2_raw: u64) -> u64 {
    let c1 = Word::from_raw(c1_raw);
    let c2 = Word::from_raw(c2_raw);
    collection_concatenate(c1, c2).raw()
}

/// `%fip-init(c) -> <iteration-state>` — initialise an iteration state.
/// Returns the pinned `nil` if `c` isn't a recognised collection.
///
/// # Safety
/// `c_raw` is any Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_fip_init(c_raw: u64) -> u64 {
    let c = Word::from_raw(c_raw);
    match forward_iteration_protocol_init(c) {
        Some(s) => s.raw(),
        None => crate::literal_pool_immediates().nil.raw(),
    }
}

/// `%fip-finished?(state) -> <boolean>` — true iff iteration is
/// complete. Returns `#t` if `state` isn't an `<iteration-state>`
/// (defensive — treat unknown states as finished).
///
/// # Safety
/// `state_raw` must be a pointer-tagged `<iteration-state>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_fip_finished_p(state_raw: u64) -> u64 {
    let state = Word::from_raw(state_raw);
    let imm = crate::literal_pool_immediates();
    match iter_state_snapshot(state) {
        Some(s) if !s.finished => imm.false_.raw(),
        _ => imm.true_.raw(),
    }
}

/// `%fip-current-element(state) -> <object>` — current element. Pins
/// `nil` if the state isn't valid.
///
/// # Safety
/// `state_raw` must be a pointer-tagged `<iteration-state>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_fip_current_element(state_raw: u64) -> u64 {
    let state = Word::from_raw(state_raw);
    match iter_state_snapshot(state) {
        Some(s) => s.current_element.raw(),
        None => crate::literal_pool_immediates().nil.raw(),
    }
}

/// `%fip-advance!(state) -> <iteration-state>` — advance in place.
/// Returns the same Word identity (chaining is preserved).
///
/// # Safety
/// `state_raw` must be a pointer-tagged `<iteration-state>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_fip_advance(state_raw: u64) -> u64 {
    let state = Word::from_raw(state_raw);
    iter_state_advance(state).raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_register_with_expected_cpl() {
        ensure_registered();
        let seq = sequence_class_id();
        let coll = collection_class_id();
        let mut_seq = mutable_sequence_class_id();
        let stretchy = stretchy_vector_class_id();
        let range = range_class_id();
        let oore = out_of_range_error_class_id();
        let err = crate::conditions::error_class_id();
        assert!(is_subclass(seq, coll));
        assert!(is_subclass(mut_seq, seq));
        assert!(is_subclass(stretchy, mut_seq));
        assert!(is_subclass(range, seq));
        assert!(is_subclass(oore, err));
    }

    #[test]
    fn range_fip_walks_one_to_three() {
        ensure_registered();
        let r = make_range(1, 3, 1);
        let mut collected: Vec<i64> = Vec::new();
        collection_do(r, |w| {
            collected.push(w.as_fixnum().unwrap_or(-1));
        });
        assert_eq!(collected, vec![1, 2, 3]);
    }

    #[test]
    fn range_reduce_sum() {
        ensure_registered();
        let r = make_range(1, 100, 1);
        let zero = Word::from_fixnum(0).unwrap();
        let result = collection_reduce(r, zero, |acc, x| {
            let a = acc.as_fixnum().unwrap_or(0);
            let b = x.as_fixnum().unwrap_or(0);
            Word::from_fixnum(a + b).unwrap()
        });
        assert_eq!(result.as_fixnum(), Some(5050));
    }

    #[test]
    fn stretchy_vector_push_and_size() {
        ensure_registered();
        let sv = make_stretchy_vector(2);
        for i in 0..5 {
            stretchy_vector_push(sv, Word::from_fixnum(i * 10).unwrap());
        }
        assert_eq!(collection_size(sv), Some(5));
        let mut seen: Vec<i64> = Vec::new();
        collection_do(sv, |w| seen.push(w.as_fixnum().unwrap()));
        assert_eq!(seen, vec![0, 10, 20, 30, 40]);
    }

    #[test]
    fn element_out_of_range_returns_err() {
        ensure_registered();
        let v = crate::with_literal_pool(|pool| {
            pool.heap.alloc_simple_object_vector(3, &pool.classes)
        });
        let result = collection_element(v, 10, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.bounds, 3);
    }

    #[test]
    fn old_stretchy_vector_keeps_user_object_alive_across_gc() {
        ensure_registered();

        let (tokenish_id, _) = crate::register_simple_user_class(
            "<gc-tokenish>",
            Some(crate::ClassId::OBJECT),
            Vec::new(),
        );
        let md = crate::class_metadata_for(tokenish_id);

        let sv = make_stretchy_vector(4);
        let _sv_guard = crate::RootGuard::new(&sv);

        for _ in 0..3 {
            crate::with_literal_pool(|pool| pool.heap.collect_minor());
        }
        crate::with_literal_pool(|pool| pool.heap.collect_full());

        let obj = unsafe { crate::rust_make(md, &[]) };
        stretchy_vector_push(sv, obj);

        for _ in 0..8 {
            crate::with_literal_pool(|pool| pool.heap.collect_minor());
        }
        for _ in 0..2 {
            crate::with_literal_pool(|pool| pool.heap.collect_full());
        }

        let stored = collection_element(sv, 0, None).expect("stored element");
        assert!(crate::nod_is_instance_of_word(stored, tokenish_id));
        let wrapper = crate::with_literal_pool(|pool| {
            pool.heap.wrapper_of(stored).expect("stored object still in heap")
        });
        assert_eq!(wrapper.class(), tokenish_id);
    }
}
