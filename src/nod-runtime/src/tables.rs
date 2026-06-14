//! **Stdlib boundary**: new table APIs go in
//! `src/nod-dylan/dylan-sources/stdlib.dylan`, not here. The bucket-array
//! ops and hash-function inner loop are *frozen exceptions* per
//! `docs/STDLIB_BOUNDARY.md` (need tight GC-root coordination and
//! eventually SIMD). Higher-level table methods belong in Dylan.
//!
//! Sprint 22 — `<table>` + hashing.
//!
//! `<table>` is a Dylan `<explicit-key-collection>`: a hash table with
//! pluggable key types. Sprint 22 supports three key flavours out of
//! the box — `<integer>`, `<byte-string>`, `<symbol>` — and signals an
//! `<error>` for anything else. The implementation is open-addressing
//! with linear probing and tombstones; growth is at 70% load factor
//! by doubling capacity.
//!
//! ### Heap layout
//!
//! ```text
//!   <table>:
//!     wrapper, %capacity (int), %size (int), %tombstones (int),
//!     %buckets (-> <simple-object-vector> of length 3*capacity)
//! ```
//!
//! Each bucket occupies three consecutive Words in the backing SOV:
//!
//! ```text
//!   slot 3*i + 0  ← state fixnum: Empty=0, Occupied=1, Tombstone=2
//!   slot 3*i + 1  ← key Word (any tagged Dylan value)
//!   slot 3*i + 2  ← value Word (any tagged Dylan value)
//! ```
//!
//! The SOV's existing scanner walks every slot; the state fixnum is a
//! tagged Word so the GC scan visits it harmlessly (fixnum tags skip
//! the pointer-walk). Empty/Tombstone buckets retain the zero-filled
//! key/value slots — also fixnum-shaped — so the scanner is safe to
//! call before any insertion.
//!
//! ### Hash + equality contract (Sprint 22 shortcut)
//!
//! `object_hash` and `object_equal_p` dispatch by class id at the Rust
//! level. The full Dylan-side `object-hash` / `object-equal?` generics
//! with sealed methods are a follow-up — see DEFERRED.md. The runtime
//! fast-path stays even after the generic ships; it's the loop body
//! during probe.
//!
//! Sprint 22 hash methods:
//!   * `<integer>`     — MIX (multiplicative hash, see `mix_hash`)
//!   * `<byte-string>` — FNV-1a 64-bit over the byte payload
//!   * `<symbol>`      — FNV-1a 64-bit over the symbol's name string

use std::sync::OnceLock;

use crate::classes::{ClassId, ClassMetadata, SlotDefault, SlotInfo, SlotType, class_metadata_for};
use crate::make::rust_make;
use crate::word::Word;

// ─── Constants ─────────────────────────────────────────────────────────────

/// Initial bucket count. Must be a power of two so the modulo reduces
/// to a mask.
const INITIAL_CAPACITY: u64 = 8;

/// Load factor numerator. Grow when (size + tombstones + 1) /
/// capacity > NUM/DEN.
const LOAD_FACTOR_NUM: u64 = 7;
const LOAD_FACTOR_DEN: u64 = 10;

/// Bucket states encoded as fixnums in the state slot.
const STATE_EMPTY: i64 = 0;
const STATE_OCCUPIED: i64 = 1;
const STATE_TOMBSTONE: i64 = 2;

// ─── Class registration ────────────────────────────────────────────────────

struct TableClasses {
    table: ClassId,
    table_md: &'static ClassMetadata,
    not_hashable_error: ClassId,
    not_hashable_error_md: &'static ClassMetadata,
}

static TABLE_CLASSES: OnceLock<TableClasses> = OnceLock::new();

/// Idempotently register `<table>` and `<not-hashable-error>` against
/// the seed class registry. Called by every entry point below.
pub fn ensure_registered() {
    // Collections (parent `<explicit-key-collection>`) and conditions
    // (parent `<error>`) must be registered first.
    crate::collections::ensure_registered();
    crate::conditions::ensure_registered();
    let _ = TABLE_CLASSES.get_or_init(|| {
        let parent = crate::collections::explicit_key_collection_class_id();
        let (table, _) = crate::register_simple_user_class(
            "<table>",
            Some(parent),
            vec![
                slot_integer("%capacity", "capacity"),
                slot_integer("%size", "size"),
                slot_integer("%tombstones", "tombstones"),
                slot_object("%buckets", "buckets"),
            ],
        );
        let error_id = crate::conditions::error_class_id();
        let (not_hashable_error, _) = crate::register_simple_user_class(
            "<not-hashable-error>",
            Some(error_id),
            vec![
                slot_str("key-class-name", "key-class-name"),
                slot_str("message", "message"),
            ],
        );
        let table_md = class_metadata_for(table);
        let not_hashable_error_md = class_metadata_for(not_hashable_error);
        TableClasses {
            table,
            table_md,
            not_hashable_error,
            not_hashable_error_md,
        }
    });
}

fn classes() -> &'static TableClasses {
    ensure_registered();
    TABLE_CLASSES.get().expect("table classes registered")
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

pub fn table_class_id() -> ClassId {
    classes().table
}

pub fn not_hashable_error_class_id() -> ClassId {
    classes().not_hashable_error
}

// ─── Slot offset cache ─────────────────────────────────────────────────────

fn table_slot_offset(name: &str) -> usize {
    classes()
        .table_md
        .slot_offset(name)
        .unwrap_or_else(|| panic!("<table> missing slot `{name}`"))
}

fn read_table_slot(t: Word, offset: usize) -> Word {
    let p = t
        .as_ptr::<u8>()
        .expect("<table> is pointer-tagged");
    // SAFETY: caller asserts `t` is a `<table>` allocation; offset comes
    // from the registered metadata.
    unsafe { *((p as usize + offset) as *const Word) }
}

/// # Safety
///
/// `t` must be a pointer-tagged `<table>` Word. Offset must come from
/// `<table>`'s registered slot metadata.
unsafe fn write_table_slot(t: Word, offset: usize, value: Word) {
    let p = t
        .as_mut_ptr::<u8>()
        .expect("<table> is pointer-tagged");
    let slot_ptr = (p as usize + offset) as *mut Word;
    // SAFETY: caller's contract; offset is in bounds.
    unsafe { crate::write_barrier(slot_ptr, value) };
}

// ─── Hash + equality (Sprint 22 fast-path) ─────────────────────────────────

/// Mix hash for `<integer>` keys. Knuth-style multiplicative hash;
/// masked to 63 bits so the result fits in a fixnum.
fn mix_hash(x: i64) -> u64 {
    // Fibonacci-derived multiplier; same as the brief.
    let mixed = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mixed & 0x7FFF_FFFF_FFFF_FFFF
}

/// FNV-1a 64-bit over `bytes`. Mask to 63 bits so the result fits in
/// a fixnum.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h & 0x7FFF_FFFF_FFFF_FFFF
}

/// Compute the 63-bit hash for a Dylan Word. Returns `Err` if the
/// key's class isn't supported as a table key in Sprint 22.
fn object_hash(key: Word) -> Result<u64, &'static str> {
    if let Some(n) = key.as_fixnum() {
        return Ok(mix_hash(n));
    }
    // SAFETY: pointer-tagged Word; first 8 bytes are a Wrapper.
    let Some(p) = key.as_ptr::<u8>() else {
        return Err("<object>");
    };
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let cid = wrapper.class();
    if cid == ClassId::BYTE_STRING {
        // SAFETY: class match implies ByteString layout.
        let bs = unsafe { &*(p as *const crate::strings::ByteString) };
        // SAFETY: ByteString invariant.
        let bytes = unsafe { bs.bytes() };
        return Ok(fnv1a_64(bytes));
    }
    if cid == ClassId::SYMBOL {
        // SAFETY: class match implies Symbol layout.
        let sym = unsafe { &*(p as *const crate::symbols::Symbol) };
        let name_w = sym.name;
        // SAFETY: name slot is a <byte-string> Word.
        let np = name_w
            .as_ptr::<u8>()
            .ok_or("<symbol> with non-pointer name")?;
        let bs = unsafe { &*(np as *const crate::strings::ByteString) };
        // SAFETY: same.
        let bytes = unsafe { bs.bytes() };
        return Ok(fnv1a_64(bytes));
    }
    // Look up class name for diagnostic — best-effort.
    Err(class_name_for_diag(cid))
}

fn class_name_for_diag(cid: ClassId) -> &'static str {
    if cid == ClassId::OBJECT { return "<object>"; }
    if cid == ClassId::INTEGER { return "<integer>"; }
    if cid == ClassId::BOOLEAN { return "<boolean>"; }
    if cid == ClassId::CHARACTER { return "<character>"; }
    if cid == ClassId::SIMPLE_OBJECT_VECTOR { return "<simple-object-vector>"; }
    if cid == ClassId::EMPTY_LIST { return "<empty-list>"; }
    if cid == ClassId::PAIR { return "<pair>"; }
    "<unknown>"
}

/// Object equality used by table-probe hit confirmation. Falls back
/// to identity equality (`==`) for any class without a registered
/// content-equality method.
fn object_equal_p(a: Word, b: Word) -> bool {
    if a == b {
        return true;
    }
    // Both must be pointer-tagged with matching classes for content
    // comparison.
    let (Some(pa), Some(pb)) = (a.as_ptr::<u8>(), b.as_ptr::<u8>()) else {
        return false;
    };
    // SAFETY: pointer-tagged Word.
    let wa = unsafe { *(pa as *const crate::wrapper::Wrapper) };
    let wb = unsafe { *(pb as *const crate::wrapper::Wrapper) };
    let ca = wa.class();
    let cb = wb.class();
    if ca != cb {
        return false;
    }
    if ca == ClassId::BYTE_STRING {
        // SAFETY: class match.
        let bsa = unsafe { &*(pa as *const crate::strings::ByteString) };
        let bsb = unsafe { &*(pb as *const crate::strings::ByteString) };
        // SAFETY: ByteString invariant.
        let ba = unsafe { bsa.bytes() };
        let bb = unsafe { bsb.bytes() };
        return ba == bb;
    }
    if ca == ClassId::SYMBOL {
        // Symbols are interned — identity check above already covered
        // equal symbols; reaching here means distinct symbols, ergo
        // not equal.
        return false;
    }
    // Everything else: identity (already covered by the leading `a == b`).
    false
}

// ─── <not-hashable-error> builder ──────────────────────────────────────────

/// Allocate a `<not-hashable-error>` carrying the offending key's class
/// name. Mirrors the Sprint 19 condition builders (e.g.
/// `make_simple_error`).
pub fn make_not_hashable_error(key_class_name: &str) -> Word {
    let md = classes().not_hashable_error_md;
    let name_w = crate::intern_string_literal(key_class_name);
    let msg = format!("key class `{key_class_name}` is not hashable");
    let msg_w = crate::intern_string_literal(&msg);
    // SAFETY: registered metadata; init keywords match the registered
    // slot names.
    unsafe {
        rust_make(
            md,
            &[("key-class-name", name_w), ("message", msg_w)],
        )
    }
}

fn signal_not_hashable(key: Word) -> ! {
    let cid = match key.as_ptr::<u8>() {
        Some(p) => {
            // SAFETY: pointer-tagged.
            let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
            wrapper.class()
        }
        None => ClassId::INTEGER, // fixnum case; shouldn't reach here
    };
    let name = class_name_for_diag(cid);
    let cond = make_not_hashable_error(name);
    // The `nod_signal` extern wraps a `-> !` inner that panics through
    // the handler chain. Call it; it diverges.
    // SAFETY: cond is a pointer-tagged Dylan condition Word.
    unsafe {
        crate::nod_signal(cond.raw());
    }
    // The above call diverges, but the type system needs help.
    unreachable!("nod_signal diverges");
}

// ─── Bucket helpers ────────────────────────────────────────────────────────

/// Read the SOV-backed buckets array. Each bucket is 3 consecutive
/// slots: `[state, key, value]`. Returns the SOV reference.
///
/// # Safety
///
/// `buckets` must be a pointer-tagged `<simple-object-vector>` Word
/// laid out as `3 * capacity` slots.
unsafe fn buckets_slots(buckets: Word) -> &'static [Word] {
    // SAFETY: caller's contract — class match.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector(buckets, ClassId::SIMPLE_OBJECT_VECTOR)
            .expect("table buckets is a SOV")
    };
    // SAFETY: same.
    unsafe { sov.slots() }
}

/// Read the SOV-backed buckets array mutably.
///
/// # Safety
///
/// As `buckets_slots`, plus exclusive access.
unsafe fn buckets_slots_mut(buckets: Word) -> &'static mut [Word] {
    // SAFETY: caller's contract.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector_mut(buckets, ClassId::SIMPLE_OBJECT_VECTOR)
            .expect("table buckets is a SOV")
    };
    // SAFETY: same; mutator is single-threaded.
    unsafe { sov.slots_mut() }
}

/// Read the bucket state at index `i`.
///
/// # Safety
///
/// `buckets` must be the backing SOV; `i` must be in `0..capacity`.
unsafe fn bucket_state(buckets: Word, i: usize) -> i64 {
    // SAFETY: caller's contract.
    let slots = unsafe { buckets_slots(buckets) };
    slots[3 * i].as_fixnum().unwrap_or(STATE_EMPTY)
}

/// Read the key Word at bucket index `i`.
///
/// # Safety
///
/// As `bucket_state`.
unsafe fn bucket_key(buckets: Word, i: usize) -> Word {
    // SAFETY: caller's contract.
    let slots = unsafe { buckets_slots(buckets) };
    slots[3 * i + 1]
}

/// Read the value Word at bucket index `i`.
///
/// # Safety
///
/// As `bucket_state`.
unsafe fn bucket_value(buckets: Word, i: usize) -> Word {
    // SAFETY: caller's contract.
    let slots = unsafe { buckets_slots(buckets) };
    slots[3 * i + 2]
}

/// Install a bucket at index `i` with the given state, key, and value.
/// Goes through the write barrier for each Word store.
///
/// # Safety
///
/// `buckets` must be the backing SOV; `i` must be in `0..capacity`.
unsafe fn write_bucket(buckets: Word, i: usize, state: i64, key: Word, value: Word) {
    // SAFETY: caller's contract.
    let slots = unsafe { buckets_slots_mut(buckets) };
    let state_w = Word::from_fixnum(state).expect("bucket state fits");
    let s_ptr = &mut slots[3 * i] as *mut Word;
    let k_ptr = &mut slots[3 * i + 1] as *mut Word;
    let v_ptr = &mut slots[3 * i + 2] as *mut Word;
    // SAFETY: each pointer is inside the live SOV allocation.
    unsafe {
        crate::write_barrier(s_ptr, state_w);
        crate::write_barrier(k_ptr, key);
        crate::write_barrier(v_ptr, value);
    }
}

/// Mark the bucket at index `i` as Tombstone, leaving key/value
/// fixnum-shaped (we zero them so the scanner sees fixnum-tagged 0).
///
/// # Safety
///
/// As `write_bucket`.
unsafe fn tombstone_bucket(buckets: Word, i: usize) {
    // SAFETY: caller's contract.
    let slots = unsafe { buckets_slots_mut(buckets) };
    let tomb_w = Word::from_fixnum(STATE_TOMBSTONE).expect("tomb fits");
    let zero = Word::from_fixnum(0).expect("0 fits");
    let s_ptr = &mut slots[3 * i] as *mut Word;
    let k_ptr = &mut slots[3 * i + 1] as *mut Word;
    let v_ptr = &mut slots[3 * i + 2] as *mut Word;
    // SAFETY: each pointer is inside the live SOV allocation.
    unsafe {
        crate::write_barrier(s_ptr, tomb_w);
        crate::write_barrier(k_ptr, zero);
        crate::write_barrier(v_ptr, zero);
    }
}

// ─── <table> field accessors ───────────────────────────────────────────────

fn table_capacity(t: Word) -> u64 {
    read_table_slot(t, table_slot_offset("%capacity"))
        .as_fixnum()
        .unwrap_or(0) as u64
}

fn table_size_field(t: Word) -> u64 {
    read_table_slot(t, table_slot_offset("%size"))
        .as_fixnum()
        .unwrap_or(0) as u64
}

fn table_tombstones_field(t: Word) -> u64 {
    read_table_slot(t, table_slot_offset("%tombstones"))
        .as_fixnum()
        .unwrap_or(0) as u64
}

fn table_buckets_field(t: Word) -> Word {
    read_table_slot(t, table_slot_offset("%buckets"))
}

/// # Safety
///
/// `t` must be a pointer-tagged `<table>` Word.
unsafe fn set_table_size(t: Word, size: u64) {
    let w = Word::from_fixnum(size as i64).expect("size fits");
    // SAFETY: caller's contract.
    unsafe { write_table_slot(t, table_slot_offset("%size"), w) };
}

/// # Safety
///
/// As `set_table_size`.
unsafe fn set_table_tombstones(t: Word, tombs: u64) {
    let w = Word::from_fixnum(tombs as i64).expect("tombstones fit");
    // SAFETY: caller's contract.
    unsafe { write_table_slot(t, table_slot_offset("%tombstones"), w) };
}

/// # Safety
///
/// As `set_table_size`.
unsafe fn set_table_capacity(t: Word, cap: u64) {
    let w = Word::from_fixnum(cap as i64).expect("capacity fits");
    // SAFETY: caller's contract.
    unsafe { write_table_slot(t, table_slot_offset("%capacity"), w) };
}

/// # Safety
///
/// As `set_table_size`.
unsafe fn set_table_buckets(t: Word, buckets: Word) {
    // SAFETY: caller's contract.
    unsafe { write_table_slot(t, table_slot_offset("%buckets"), buckets) };
}

// ─── Probe + insert + grow ─────────────────────────────────────────────────

/// Result of a bucket probe.
#[derive(Clone, Copy, Debug)]
enum Probe {
    /// Found an Occupied bucket whose key equals the searched-for key.
    Hit(usize),
    /// No matching key; the supplied index is the first Empty bucket
    /// found, or — if a Tombstone was seen earlier — the first
    /// Tombstone (for insert reuse).
    Miss(usize),
}

/// Linear-probe the buckets array looking for `key`. Caller pre-grew
/// the table if needed; this is read-only.
///
/// # Safety
///
/// `t` must be a pointer-tagged `<table>` Word; the table must be
/// fully initialised.
unsafe fn probe(t: Word, key: Word) -> Probe {
    let cap = table_capacity(t);
    if cap == 0 {
        return Probe::Miss(0);
    }
    let buckets = table_buckets_field(t);
    let h = match object_hash(key) {
        Ok(h) => h,
        Err(_) => {
            signal_not_hashable(key);
        }
    };
    let mask = cap - 1;
    let mut idx = (h & mask) as usize;
    let mut first_tombstone: Option<usize> = None;
    for _ in 0..cap {
        // SAFETY: idx < capacity.
        let st = unsafe { bucket_state(buckets, idx) };
        match st {
            x if x == STATE_EMPTY => {
                return Probe::Miss(first_tombstone.unwrap_or(idx));
            }
            x if x == STATE_TOMBSTONE => {
                if first_tombstone.is_none() {
                    first_tombstone = Some(idx);
                }
            }
            x if x == STATE_OCCUPIED => {
                // SAFETY: bucket at idx is Occupied; key is laid out.
                let k = unsafe { bucket_key(buckets, idx) };
                if object_equal_p(k, key) {
                    return Probe::Hit(idx);
                }
            }
            _ => {
                // Unknown state — treat as Empty.
                return Probe::Miss(first_tombstone.unwrap_or(idx));
            }
        }
        idx = ((idx as u64 + 1) & mask) as usize;
    }
    // Table is full of tombstones + occupied (shouldn't happen if we
    // grew correctly). Fall back to the first tombstone or 0.
    Probe::Miss(first_tombstone.unwrap_or(0))
}

/// Decide whether the table needs to grow before inserting one more
/// entry. Triggers at 70% (size + tombstones + 1) / capacity.
fn should_grow(size: u64, tombs: u64, cap: u64) -> bool {
    cap == 0
        || (size + tombs + 1).saturating_mul(LOAD_FACTOR_DEN)
            > cap.saturating_mul(LOAD_FACTOR_NUM)
}

/// Allocate a fresh `<simple-object-vector>` of length `3 * cap`
/// initialised with state = Empty in every bucket. (Zero-init from
/// the heap allocator already gives Empty, since Word(0) is fixnum
/// 0 = STATE_EMPTY.)
fn make_buckets_storage(cap: u64) -> Word {
    let len = (3 * cap) as usize;
    crate::with_literal_pool(|pool| pool.heap.alloc_simple_object_vector(len, &pool.classes))
}

/// Grow the table to `new_cap` (must be a power of two greater than
/// the current). Rehashes every Occupied bucket; tombstones are
/// discarded.
///
/// # Safety
///
/// `t` must be a pointer-tagged `<table>` Word.
unsafe fn grow(t: Word, new_cap: u64) {
    let old_cap = table_capacity(t);
    let old_buckets = table_buckets_field(t);
    // Root the table across the SOV allocation.
    let t_local = t;
    let _t_guard = crate::make::RootGuard::new(&t_local);
    let old_buckets_local = old_buckets;
    let _ob_guard = crate::make::RootGuard::new(&old_buckets_local);

    let new_buckets = make_buckets_storage(new_cap);
    let new_buckets_local = new_buckets;
    let _nb_guard = crate::make::RootGuard::new(&new_buckets_local);

    // Refresh: GC during alloc may have moved t / old_buckets.
    let t = t_local;
    let old_buckets = table_buckets_field(t);

    // For every Occupied bucket in old_buckets, re-probe in new and
    // install. We can do this directly (without going through `probe`)
    // because each rehashed key is fresh w.r.t. the new array, so
    // no tombstones exist and Empty is the only stop condition.
    let mask = new_cap - 1;
    let mut count: u64 = 0;
    for i in 0..(old_cap as usize) {
        // SAFETY: i < old capacity; old_buckets is a live SOV.
        let st = unsafe { bucket_state(old_buckets, i) };
        if st != STATE_OCCUPIED {
            continue;
        }
        // SAFETY: same.
        let k = unsafe { bucket_key(old_buckets, i) };
        // SAFETY: same.
        let v = unsafe { bucket_value(old_buckets, i) };
        let h = object_hash(k).expect("rehashed key was hashable at insert time");
        let mut idx = (h & mask) as usize;
        loop {
            // SAFETY: idx < new capacity; new_buckets is live.
            let new_st = unsafe { bucket_state(new_buckets_local, idx) };
            if new_st == STATE_EMPTY {
                // SAFETY: idx in bounds.
                unsafe { write_bucket(new_buckets_local, idx, STATE_OCCUPIED, k, v) };
                count += 1;
                break;
            }
            idx = ((idx as u64 + 1) & mask) as usize;
        }
    }
    // Install new buckets + capacity; reset tombstones.
    // SAFETY: t is a <table>.
    unsafe {
        set_table_buckets(t, new_buckets_local);
        set_table_capacity(t, new_cap);
        set_table_size(t, count);
        set_table_tombstones(t, 0);
    }
}

// ─── Public API ────────────────────────────────────────────────────────────

/// Allocate a fresh `<table>` with the requested initial capacity
/// (rounded up to the next power of two, minimum `INITIAL_CAPACITY`).
pub fn make_table(capacity_hint: u64) -> Word {
    ensure_registered();
    let mut cap = INITIAL_CAPACITY;
    while cap < capacity_hint {
        cap = cap.saturating_mul(2);
        if cap == 0 {
            cap = INITIAL_CAPACITY;
            break;
        }
    }
    // Allocate the backing buckets storage first, root it across the
    // <table> allocation.
    let buckets = make_buckets_storage(cap);
    let buckets_local = buckets;
    let _b_guard = crate::make::RootGuard::new(&buckets_local);

    let md = classes().table_md;
    let cap_w = Word::from_fixnum(cap as i64).expect("capacity fits");
    let zero = Word::from_fixnum(0).expect("0 fits");
    // SAFETY: registered metadata, matching keyword names.
    unsafe {
        rust_make(
            md,
            &[
                ("capacity", cap_w),
                ("size", zero),
                ("tombstones", zero),
                ("buckets", buckets_local),
            ],
        )
    }
}

/// `size(t)` — number of Occupied buckets.
pub fn table_size(t: Word) -> u64 {
    ensure_registered();
    if !is_table(t) {
        return 0;
    }
    table_size_field(t)
}

/// `element(t, key, default)` — look up `key` and return the value,
/// or `default` if absent.
pub fn table_element(t: Word, key: Word, default: Word) -> Word {
    ensure_registered();
    if !is_table(t) {
        return default;
    }
    // SAFETY: t is a <table>.
    match unsafe { probe(t, key) } {
        Probe::Hit(i) => {
            let buckets = table_buckets_field(t);
            // SAFETY: i is in bounds of the buckets array.
            unsafe { bucket_value(buckets, i) }
        }
        Probe::Miss(_) => default,
    }
}

/// `element(t, key) := value` — install `value` under `key`. Grows if
/// the load factor would exceed 70%. Returns `value` for `:=`
/// value-propagation.
pub fn table_element_setter(value: Word, t: Word, key: Word) -> Word {
    ensure_registered();
    if !is_table(t) {
        return value;
    }
    // Root inputs across the potential growth allocation.
    let t_local = t;
    let value_local = value;
    let key_local = key;
    let _t_guard = crate::make::RootGuard::new(&t_local);
    let _v_guard = crate::make::RootGuard::new(&value_local);
    let _k_guard = crate::make::RootGuard::new(&key_local);

    let size = table_size_field(t_local);
    let tombs = table_tombstones_field(t_local);
    let cap = table_capacity(t_local);
    if should_grow(size, tombs, cap) {
        let new_cap = if cap == 0 {
            INITIAL_CAPACITY
        } else {
            cap.saturating_mul(2)
        };
        // SAFETY: t_local is a <table>.
        unsafe { grow(t_local, new_cap) };
    }
    // Re-read fields post-grow.
    let buckets = table_buckets_field(t_local);
    // SAFETY: t_local is a <table>.
    match unsafe { probe(t_local, key_local) } {
        Probe::Hit(i) => {
            // SAFETY: i in bounds.
            unsafe { write_bucket(buckets, i, STATE_OCCUPIED, key_local, value_local) };
        }
        Probe::Miss(i) => {
            // SAFETY: i in bounds.
            let was_tomb = unsafe { bucket_state(buckets, i) } == STATE_TOMBSTONE;
            // SAFETY: i in bounds.
            unsafe { write_bucket(buckets, i, STATE_OCCUPIED, key_local, value_local) };
            // SAFETY: t_local is a <table>.
            unsafe {
                set_table_size(t_local, size + 1);
                if was_tomb {
                    set_table_tombstones(t_local, tombs.saturating_sub(1));
                }
            }
        }
    }
    value
}

/// `remove-key!(t, key)` — delete the entry under `key`, if any.
/// Returns `t`.
pub fn table_remove_key(t: Word, key: Word) -> Word {
    ensure_registered();
    if !is_table(t) {
        return t;
    }
    let t_local = t;
    let key_local = key;
    let _t_guard = crate::make::RootGuard::new(&t_local);
    let _k_guard = crate::make::RootGuard::new(&key_local);

    // SAFETY: t_local is a <table>.
    if let Probe::Hit(i) = unsafe { probe(t_local, key_local) } {
        let buckets = table_buckets_field(t_local);
        // SAFETY: i in bounds.
        unsafe { tombstone_bucket(buckets, i) };
        let size = table_size_field(t_local);
        let tombs = table_tombstones_field(t_local);
        // SAFETY: t_local is a <table>.
        unsafe {
            set_table_size(t_local, size.saturating_sub(1));
            set_table_tombstones(t_local, tombs + 1);
        }
    }
    t
}

/// `keys(t)` — return a `<simple-object-vector>` of every Occupied
/// key in unspecified order.
pub fn table_keys(t: Word) -> Word {
    ensure_registered();
    if !is_table(t) {
        return crate::with_literal_pool(|pool| {
            pool.heap.alloc_simple_object_vector(0, &pool.classes)
        });
    }
    let size = table_size_field(t);
    let cap = table_capacity(t);
    let buckets = table_buckets_field(t);

    // Root the table + buckets across the SOV allocation.
    let t_local = t;
    let buckets_local = buckets;
    let _t_guard = crate::make::RootGuard::new(&t_local);
    let _b_guard = crate::make::RootGuard::new(&buckets_local);

    let out = crate::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(size as usize, &pool.classes)
    });
    let out_local = out;
    let _out_guard = crate::make::RootGuard::new(&out_local);

    let buckets = table_buckets_field(t_local);

    // SAFETY: out_local is the fresh SOV.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector_mut(
            out_local,
            ClassId::SIMPLE_OBJECT_VECTOR,
        )
        .expect("out is a SOV")
    };
    // SAFETY: same.
    let slots = unsafe { sov.slots_mut() };
    let mut j = 0usize;
    for i in 0..(cap as usize) {
        // SAFETY: i < cap; buckets is live.
        let st = unsafe { bucket_state(buckets, i) };
        if st != STATE_OCCUPIED {
            continue;
        }
        // SAFETY: same.
        let k = unsafe { bucket_key(buckets, i) };
        let slot_ptr = &mut slots[j] as *mut Word;
        // SAFETY: slot_ptr is inside the live SOV.
        unsafe { crate::write_barrier(slot_ptr, k) };
        j += 1;
    }
    out_local
}

/// `values(t)` — return a `<simple-object-vector>` of every Occupied
/// value in unspecified order (matching `keys(t)`'s ordering for the
/// same table snapshot).
pub fn table_values(t: Word) -> Word {
    ensure_registered();
    if !is_table(t) {
        return crate::with_literal_pool(|pool| {
            pool.heap.alloc_simple_object_vector(0, &pool.classes)
        });
    }
    let size = table_size_field(t);
    let cap = table_capacity(t);

    let t_local = t;
    let _t_guard = crate::make::RootGuard::new(&t_local);

    let out = crate::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(size as usize, &pool.classes)
    });
    let out_local = out;
    let _out_guard = crate::make::RootGuard::new(&out_local);

    let buckets = table_buckets_field(t_local);

    // SAFETY: out_local is the fresh SOV.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector_mut(
            out_local,
            ClassId::SIMPLE_OBJECT_VECTOR,
        )
        .expect("out is a SOV")
    };
    // SAFETY: same.
    let slots = unsafe { sov.slots_mut() };
    let mut j = 0usize;
    for i in 0..(cap as usize) {
        // SAFETY: i < cap.
        let st = unsafe { bucket_state(buckets, i) };
        if st != STATE_OCCUPIED {
            continue;
        }
        // SAFETY: same.
        let v = unsafe { bucket_value(buckets, i) };
        let slot_ptr = &mut slots[j] as *mut Word;
        // SAFETY: inside the live SOV.
        unsafe { crate::write_barrier(slot_ptr, v) };
        j += 1;
    }
    out_local
}

/// True if `w` is a pointer-tagged `<table>` instance.
pub fn is_table(w: Word) -> bool {
    let Some(p) = w.as_ptr::<u8>() else {
        return false;
    };
    // SAFETY: pointer-tagged Word.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    wrapper.class() == classes().table
}

// ─── JIT-callable shims ────────────────────────────────────────────────────

/// `%make-table(capacity_hint)` — allocate a fresh `<table>`.
///
/// # Safety
///
/// `capacity_hint_raw` is a fixnum-tagged Word; non-fixnum collapses
/// to the default initial capacity.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_make_table(capacity_hint_raw: u64) -> u64 {
    let hint = Word::from_raw(capacity_hint_raw)
        .as_fixnum()
        .unwrap_or(0)
        .max(0) as u64;
    make_table(hint).raw()
}

/// `%table-size(t)` — return the number of Occupied buckets.
///
/// # Safety
///
/// `t_raw` must be a pointer-tagged `<table>` Word; non-tables return 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_table_size(t_raw: u64) -> u64 {
    let t = Word::from_raw(t_raw);
    let n = table_size(t);
    Word::from_fixnum(n as i64).expect("size fits").raw()
}

/// `%table-element(t, key)` — look up `key`; returns the pinned `#f`
/// on miss. Use `%table-element-or-default` to plumb a custom default.
///
/// # Safety
///
/// `t_raw` is any Dylan Word; non-tables return `#f`. `key_raw` is any
/// Dylan Word — if its class isn't hashable, signals
/// `<not-hashable-error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_table_element(t_raw: u64, key_raw: u64) -> u64 {
    let t = Word::from_raw(t_raw);
    let key = Word::from_raw(key_raw);
    let imm = crate::literal_pool_immediates();
    table_element(t, key, imm.false_).raw()
}

/// `%table-element-or-default(t, key, default)` — look up `key`;
/// returns `default` on miss.
///
/// # Safety
///
/// As `nod_table_element`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_table_element_or_default(
    t_raw: u64,
    key_raw: u64,
    default_raw: u64,
) -> u64 {
    let t = Word::from_raw(t_raw);
    let key = Word::from_raw(key_raw);
    let default = Word::from_raw(default_raw);
    table_element(t, key, default).raw()
}

/// `%table-element-setter(value, t, key)` — install `value` under
/// `key`. Returns `value`.
///
/// # Safety
///
/// `t_raw` must be a pointer-tagged `<table>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_table_element_setter(
    value_raw: u64,
    t_raw: u64,
    key_raw: u64,
) -> u64 {
    let value = Word::from_raw(value_raw);
    let t = Word::from_raw(t_raw);
    let key = Word::from_raw(key_raw);
    table_element_setter(value, t, key).raw()
}

/// `%table-remove-key(t, key)` — delete the entry under `key`.
///
/// # Safety
///
/// As `nod_table_element_setter`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_table_remove_key(t_raw: u64, key_raw: u64) -> u64 {
    let t = Word::from_raw(t_raw);
    let key = Word::from_raw(key_raw);
    table_remove_key(t, key).raw()
}

/// `%table-keys(t)` — fresh SOV of keys.
///
/// # Safety
///
/// `t_raw` must be a pointer-tagged `<table>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_table_keys(t_raw: u64) -> u64 {
    let t = Word::from_raw(t_raw);
    table_keys(t).raw()
}

/// `%table-values(t)` — fresh SOV of values.
///
/// # Safety
///
/// As `nod_table_keys`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_table_values(t_raw: u64) -> u64 {
    let t = Word::from_raw(t_raw);
    table_values(t).raw()
}

/// `%object-hash(x)` — return the 63-bit hash as a fixnum. Signals
/// `<not-hashable-error>` for unsupported key classes.
///
/// # Safety
///
/// `x_raw` is any Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_object_hash(x_raw: u64) -> u64 {
    let x = Word::from_raw(x_raw);
    match object_hash(x) {
        Ok(h) => Word::from_fixnum((h & 0x7FFF_FFFF_FFFF_FFFF) as i64)
            .expect("hash fits in fixnum")
            .raw(),
        Err(_) => {
            signal_not_hashable(x);
        }
    }
}

/// `%object-equal?(a, b)` — content equality with the Sprint 22 fast
/// path. Returns the pinned `#t` / `#f`.
///
/// # Safety
///
/// Both args are any Dylan Words.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_object_equal_p(a_raw: u64, b_raw: u64) -> u64 {
    let a = Word::from_raw(a_raw);
    let b = Word::from_raw(b_raw);
    let imm = crate::literal_pool_immediates();
    if object_equal_p(a, b) {
        imm.true_.raw()
    } else {
        imm.false_.raw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::is_subclass;

    #[test]
    fn table_hierarchy_is_correct() {
        ensure_registered();
        let t = table_class_id();
        let eks = crate::collections::explicit_key_collection_class_id();
        let coll = crate::collections::collection_class_id();
        let obj = ClassId::OBJECT;
        assert!(is_subclass(t, eks));
        assert!(is_subclass(t, coll));
        assert!(is_subclass(t, obj));
    }

    #[test]
    fn make_table_size_zero() {
        ensure_registered();
        let t = make_table(0);
        assert_eq!(table_size(t), 0);
    }

    #[test]
    fn integer_key_round_trip() {
        ensure_registered();
        let t = make_table(0);
        let k = Word::from_fixnum(42).unwrap();
        let v = Word::from_fixnum(100).unwrap();
        table_element_setter(v, t, k);
        let got = table_element(t, k, Word::from_fixnum(-1).unwrap());
        assert_eq!(got.as_fixnum(), Some(100));
        assert_eq!(table_size(t), 1);
    }

    #[test]
    fn string_key_round_trip() {
        ensure_registered();
        let t = make_table(0);
        let k = crate::intern_string_literal("hello");
        let v = Word::from_fixnum(7).unwrap();
        table_element_setter(v, t, k);
        // Distinct string Word with same content — must hit.
        let k2 = crate::intern_string_literal("hello");
        let got = table_element(t, k2, Word::from_fixnum(-1).unwrap());
        assert_eq!(got.as_fixnum(), Some(7));
    }

    #[test]
    fn symbol_key_round_trip() {
        ensure_registered();
        let t = make_table(0);
        let k = crate::intern_symbol_literal("foo");
        let v = Word::from_fixnum(9).unwrap();
        table_element_setter(v, t, k);
        let k2 = crate::intern_symbol_literal("foo");
        let got = table_element(t, k2, Word::from_fixnum(-1).unwrap());
        assert_eq!(got.as_fixnum(), Some(9));
    }

    #[test]
    fn grow_past_initial_capacity() {
        ensure_registered();
        let t = make_table(0);
        for i in 0..100i64 {
            let k = Word::from_fixnum(i).unwrap();
            let v = Word::from_fixnum(i * 10).unwrap();
            table_element_setter(v, t, k);
        }
        assert_eq!(table_size(t), 100);
        for i in 0..100i64 {
            let k = Word::from_fixnum(i).unwrap();
            let got = table_element(t, k, Word::from_fixnum(-1).unwrap());
            assert_eq!(got.as_fixnum(), Some(i * 10), "lookup {i}");
        }
    }

    #[test]
    fn remove_key_decrements_size() {
        ensure_registered();
        let t = make_table(0);
        let k = Word::from_fixnum(1).unwrap();
        let v = Word::from_fixnum(2).unwrap();
        table_element_setter(v, t, k);
        assert_eq!(table_size(t), 1);
        table_remove_key(t, k);
        assert_eq!(table_size(t), 0);
        let got = table_element(t, k, Word::from_fixnum(-1).unwrap());
        assert_eq!(got.as_fixnum(), Some(-1));
    }

    #[test]
    fn overwrite_same_key() {
        ensure_registered();
        let t = make_table(0);
        let k = Word::from_fixnum(5).unwrap();
        table_element_setter(Word::from_fixnum(1).unwrap(), t, k);
        table_element_setter(Word::from_fixnum(2).unwrap(), t, k);
        table_element_setter(Word::from_fixnum(3).unwrap(), t, k);
        assert_eq!(table_size(t), 1);
        let got = table_element(t, k, Word::from_fixnum(-1).unwrap());
        assert_eq!(got.as_fixnum(), Some(3));
    }

    #[test]
    fn keys_and_values_round_trip() {
        ensure_registered();
        let t = make_table(0);
        for i in 1..=3i64 {
            let k = Word::from_fixnum(i).unwrap();
            let v = Word::from_fixnum(i * 100).unwrap();
            table_element_setter(v, t, k);
        }
        let ks = table_keys(t);
        let vs = table_values(t);
        // SAFETY: both are fresh SOVs.
        let ks_sov = unsafe {
            crate::vectors::try_simple_object_vector(ks, ClassId::SIMPLE_OBJECT_VECTOR)
        }
        .expect("keys returns SOV");
        let vs_sov = unsafe {
            crate::vectors::try_simple_object_vector(vs, ClassId::SIMPLE_OBJECT_VECTOR)
        }
        .expect("values returns SOV");
        assert_eq!(ks_sov.len, 3);
        assert_eq!(vs_sov.len, 3);
    }
}
