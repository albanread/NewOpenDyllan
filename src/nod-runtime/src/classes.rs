//! Class metadata — the per-class records the `<wrapper>` header points at.
//!
//! Sprint 12 grows the Sprint 09/11 metadata into a full slot + CPL +
//! init-keyword record, plus a `UserClasses` registry that mints fresh
//! `ClassId`s for user-defined classes. Every metadata entry — seed or
//! user — lives forever in the `StaticArea` so the address can be baked
//! into JIT-emitted constants.
//!
//! The class-driven scanning design from Sprint 11 generalises: a user
//! class's `scan` walks its pointer-typed slots; its `size_of` returns
//! a constant (fixed-offset layout is mandatory in Sprint 12 per the
//! brief — MI + indirect lookup land in Sprint 14).

use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::lists::Pair;
use crate::strings::ByteString;
use crate::symbols::Symbol;
use crate::vectors::SimpleObjectVector;
use crate::word::Word;
use crate::wrapper::Wrapper;

/// Stable identifier for a class. Wrapped so the `<wrapper>` header
/// can carry it in 32 bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ClassId(pub u32);

impl ClassId {
    pub const OBJECT: ClassId = ClassId(0);
    pub const INTEGER: ClassId = ClassId(1);
    pub const SINGLE_FLOAT: ClassId = ClassId(2);
    pub const DOUBLE_FLOAT: ClassId = ClassId(3);
    pub const BOOLEAN: ClassId = ClassId(4);
    pub const CHARACTER: ClassId = ClassId(5);
    pub const SYMBOL: ClassId = ClassId(6);
    pub const STRING: ClassId = ClassId(7);
    pub const BYTE_STRING: ClassId = ClassId(8);
    pub const SIMPLE_OBJECT_VECTOR: ClassId = ClassId(9);
    pub const EMPTY_LIST: ClassId = ClassId(10);
    /// Sprint 16: `<pair>` — Dylan cons cell. Two pointer-shaped slots
    /// (`head`, `tail`) at offsets 8 and 16; scanned by the user-class
    /// scanner via its registered slot list.
    pub const PAIR: ClassId = ClassId(11);
    /// First id minted for user-defined classes.
    pub const FIRST_USER: u32 = 1024;
    /// Sprint 51e — first id minted for **front-end shim** classes.
    ///
    /// A statically-linked Dylan front-end shim (the migrated
    /// lexer/parser, and later the macro expander / sema / lowering)
    /// carries its OWN `define class`es (`<token>`, `<ast-*>`, …). When
    /// such a shim's AOT resolver fires INSIDE a host process (e.g.
    /// `nod-driver` building a user program with `--parse-with-dylan`),
    /// those classes must NOT consume ids from the `FIRST_USER..` range
    /// that stdlib + the user program share — otherwise the host's
    /// `allocate_user_class_id` is bumped by the shim's class count and
    /// every subsequently-registered USER class lands at a higher id
    /// than the shim-free user EXE will allocate, tripping the AOT
    /// class-id-drift assert (`aot.rs`).
    ///
    /// Putting shim classes in a disjoint high band keeps the
    /// `FIRST_USER..` sequence byte-identical whether or not a shim is
    /// active, so a user program's baked class ids always match what its
    /// own (shim-free) EXE allocates. The band is far above any
    /// plausible user-class count; ids stay within `u32` and the
    /// `<wrapper>` header's 32-bit class-id field.
    pub const FIRST_SHIM: u32 = 0x4000_0000;
}

impl std::fmt::Debug for ClassId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClassId({})", self.0)
    }
}

/// Visit each tagged-`Word` slot inside the object whose first cell is
/// at `addr`.
///
/// # Safety
///
/// `addr` must point at a live heap object whose first 8 bytes are a
/// `Wrapper` whose class matches the metadata this function pointer
/// is registered against.
pub type ScanFn = unsafe fn(addr: usize, visit: &mut dyn FnMut(*mut Word));

/// Total byte footprint (header + payload, padded to 8-byte alignment).
///
/// # Safety
///
/// Same precondition as `ScanFn`.
pub type SizeFn = unsafe fn(addr: usize) -> usize;

/// Sprint 23: pointer-cell range for the NewGC `HeapLayout` binding.
///
/// Reports `(total_cells, pointer_cells_start, pointer_cells_end)` — the
/// same shape as `newgc_core::ObjectLayout`. `pointer_cells_start ==
/// pointer_cells_end` means "no pointer cells" (opaque payload).
///
/// **Safe over-scanning**: contiguous ranges that include non-pointer
/// fixnum slots (e.g. `<table>`'s capacity/size/tombstones) are fine —
/// the GC's `classify` shunt skips immediates and gates obvious-junk
/// pointers via the page-of-reservation check. The range only needs to
/// be *correct as a superset* of the real pointer-bearing slots.
///
/// # Safety
///
/// Same precondition as `ScanFn`.
pub type LayoutFn = unsafe fn(addr: usize) -> (usize, usize, usize);

// ─── Slot model (Sprint 12) ─────────────────────────────────────────────────

/// Slot value-type estimate. Drives the GC scan decision (pointer-shaped
/// slots are visited; immediate slots aren't) and the type-check at
/// store time (Sprint 13 will start enforcing it; Sprint 12 records).
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum SlotType {
    /// Fixnum (`<integer>` slot type). Stored as a tagged Word; not a
    /// pointer, not scanned.
    Integer,
    /// `<single-float>` / `<double-float>` slot. Sprint 12 has no float
    /// boxing — float-typed slots are conservatively treated as Top
    /// (any tagged Word); GC scans them like pointer slots. Documented
    /// in DEFERRED.md.
    DoubleFloat,
    /// `<boolean>` — pinned immediate; pointer-shaped but the targets
    /// are in the static area (not in the heap). The scanner walks the
    /// slot anyway in case it ever holds a non-immediate; the collector
    /// short-circuits on static-area addresses.
    Boolean,
    /// `<character>` — Sprint 12 still encodes characters as raw i32
    /// (see DEFERRED Sprint 10 #8). Treated as a fixnum slot — not
    /// scanned.
    Character,
    /// `<string>` / `<byte-string>` — pointer-tagged Word.
    String,
    /// `<symbol>` — pointer-tagged Word.
    Symbol,
    /// `<simple-object-vector>` / `<vector>` — pointer-tagged Word.
    Vector,
    /// `<object>` — any tagged Word.
    Object,
    /// Narrowed to a specific class. Pointer-shaped.
    Class(ClassId),
    /// `<top>` / unannotated — any tagged Word.
    Top,
}

impl SlotType {
    /// True if the slot may carry a pointer-tagged Word; the GC scan
    /// function follows these slots.
    pub fn is_pointer_shaped(self) -> bool {
        matches!(
            self,
            SlotType::String
                | SlotType::Symbol
                | SlotType::Vector
                | SlotType::Object
                | SlotType::Class(_)
                | SlotType::Top
                | SlotType::DoubleFloat
                | SlotType::Boolean
        )
    }
}

/// Default-init for a slot. Sprint 12 supports only a literal value or
/// "unbound" (caller must provide via init-keyword). Function-defaults
/// land in Sprint 13.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum SlotDefault {
    /// No default — slot is left zero-filled if neither init-keyword
    /// nor required-init-keyword fires. (`#f` after Sprint 13 once
    /// `<unbound>` lands.)
    Unbound,
    /// Use this literal value if no init-keyword is supplied.
    Value(Word),
}

/// Information about a single instance slot. Names live in the
/// metadata struct itself (not interned separately); they survive as
/// long as the metadata does — i.e. forever.
#[derive(Clone, Debug)]
pub struct SlotInfo {
    pub name: String,
    /// Byte offset from the start of the heap object (including the
    /// leading 8-byte Wrapper). For instance slots: `8 + slot_index*8`.
    pub offset: usize,
    pub type_kind: SlotType,
    pub init_keyword: Option<String>,
    pub required_init_keyword: bool,
    pub default_init: SlotDefault,
    pub has_setter: bool,
}

/// Per-class record. Address-stable: built once in the static area and
/// never moved or freed.
pub struct ClassMetadata {
    pub id: ClassId,
    /// Class name. For seed classes this is a `&'static` string baked
    /// in at startup; for user classes it's a `String` owned by the
    /// pinned metadata.
    pub name: String,
    /// First direct superclass — back-compat convenience accessor for
    /// Sprint 12 callers. `None` only for `<object>`. For multi-parent
    /// classes (Sprint 14 MI), this is `parents[0]`; consult `parents`
    /// or `cpl` for the full picture.
    pub parent: Option<ClassId>,
    /// Direct superclasses in declaration order. Sprint 14: MI classes
    /// have more than one entry; SI classes have exactly one (or empty
    /// for `<object>`). The CPL is the canonical source for inheritance
    /// walks; `parents` records what the source actually said.
    pub parents: Vec<ClassId>,
    /// Class precedence list: [self, …direct parents in C3 order…,
    /// `<object>`]. Built by `nod-sema::c3`; the vector includes the
    /// class itself at index 0.
    pub cpl: Vec<ClassId>,
    /// All instance slots, own + inherited, in layout order. Sprint 14:
    /// for MI subclasses, slots merged from every parent's slot list in
    /// most-specific-first append order. Inherited slot whose offset
    /// shifts vs. its defining parent's layout requires an override
    /// accessor method (lower.rs emits one); see `slot_origin` to find
    /// out which class introduced each slot.
    pub slots: Vec<SlotInfo>,
    /// How many slots are introduced by this class (i.e. not inherited
    /// from any parent).
    pub own_slot_count: usize,
    /// How many slots are inherited from the parent chain.
    pub inherited_slot_count: usize,
    /// For each slot in `slots`, the class id that first introduces
    /// (defines) it. For own slots this is the class's own id; for
    /// inherited slots it's the nearest ancestor that defined it. Used
    /// by Sprint 14's `dump_classes` and by the override-detection
    /// logic in lower.rs.
    pub slot_origin: Vec<ClassId>,
    /// Total instance size = `size_of::<Wrapper>() + 8 * slots.len()`.
    pub instance_size: usize,
    pub scan: ScanFn,
    pub size_of: SizeFn,
    /// Sprint 23 NewGC binding. Returns the same
    /// `(total_cells, pointer_cells_start, pointer_cells_end)` tuple
    /// that `newgc_core::ObjectLayout` expects, but expressed as a
    /// per-class function pointer so the GC can scan without going
    /// through the trait-object-shaped `ScanFn` callback. Defaults to
    /// a wrapper-derived layout for seed classes; user classes get a
    /// generated `user_class_layout` that walks the slot list.
    pub layout: LayoutFn,
    /// Sprint 23: when `true`, the payload after the wrapper is raw
    /// bytes (UTF-8 for `<byte-string>`, opaque for any future byte-
    /// vector classes), not tagged Words. The GC's `header_layout`
    /// reports an opaque payload so it doesn't try to interpret the
    /// bytes as pointers. The existing `scan: ScanFn` is independent
    /// and stays a no-op for byte-payload classes.
    pub is_byte_payload: bool,
    /// Sprint 15: this class is sealed against subclassing across
    /// library boundaries. `AtomicBool` because Sprint 15 sets this
    /// AFTER the metadata is pinned in the static area — the lowering
    /// pass registers the class with `sealed = false`, then flips this
    /// flag once it sees the `Modifier::Sealed` on the parsed
    /// `Item::DefineClass`. Reading from the dispatch resolver path
    /// uses `Ordering::Acquire` to pair with the lowering-side
    /// `Ordering::Release` store.
    ///
    /// `false` is the safe default (open class). The compiler's
    /// soundness rule is "when in doubt, treat as open" — a wrongly-set
    /// `true` would resolve dispatches that must not be resolved.
    pub sealed: AtomicBool,
    /// In-library subclasses known at this class's registration time
    /// (and updated as later `define class`es land). The dispatch
    /// resolver reads this to enumerate "every possible subclass of
    /// `<C>`" when `<C>` is sealed. Held under `RwLock` because subclass
    /// registration runs on the lowering thread while resolvers may
    /// read from JIT'd callers.
    pub direct_subclasses: RwLock<Vec<ClassId>>,
}

impl ClassMetadata {
    /// Slot offset for a named slot, or `None` if the class doesn't
    /// have one. The single source of truth for layout — both
    /// `LoadSlot` codegen and the `nod_make` runtime go through this.
    pub fn slot_offset(&self, name: &str) -> Option<usize> {
        self.slots.iter().find(|s| s.name == name).map(|s| s.offset)
    }

    /// Find a slot by init-keyword name (e.g. `"x"` for `x:`).
    pub fn slot_by_init_keyword(&self, kw: &str) -> Option<&SlotInfo> {
        self.slots
            .iter()
            .find(|s| s.init_keyword.as_deref() == Some(kw))
    }

    /// Sprint 15: is this class sealed against subclassing across
    /// library boundaries? Read with `Ordering::Acquire` to pair with
    /// `mark_sealed`'s release store. The default for every class is
    /// `false`; lowering flips it at registration time when the
    /// `sealed` modifier is present.
    pub fn is_sealed(&self) -> bool {
        self.sealed.load(Ordering::Acquire)
    }

    /// Sprint 15: mark this class as sealed. Called by the lowering
    /// pass when it sees `Modifier::Sealed` on a `define class`. Idempotent.
    pub fn mark_sealed(&self) {
        self.sealed.store(true, Ordering::Release);
    }

    /// Sprint 15: snapshot the direct-subclass list. Returns a Vec so
    /// the caller doesn't hold the lock; the slice can be stale by
    /// the time it's read but that's fine — the dispatch resolver runs
    /// inside the single-threaded lowering pass.
    pub fn direct_subclasses_snapshot(&self) -> Vec<ClassId> {
        self.direct_subclasses
            .read()
            .expect("direct_subclasses rwlock poisoned")
            .clone()
    }

    /// Sprint 15: append `child` to this class's direct-subclass list.
    /// No-op if `child` is already present (registration is idempotent).
    pub fn register_subclass(&self, child: ClassId) {
        let mut guard = self
            .direct_subclasses
            .write()
            .expect("direct_subclasses rwlock poisoned");
        if !guard.contains(&child) {
            guard.push(child);
        }
    }
}

impl std::fmt::Debug for ClassMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClassMetadata")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("parent", &self.parent)
            .field("slot_count", &self.slots.len())
            .field("instance_size", &self.instance_size)
            .finish()
    }
}

// ─── ScanFn / SizeFn implementations for seed classes ──────────────────────

/// # Safety
///
/// Trivially safe — never reads or writes memory.
unsafe fn noop_scan(_addr: usize, _visit: &mut dyn FnMut(*mut Word)) {}

/// # Safety
///
/// `addr` must point at a live `Wrapper`-headed object.
unsafe fn wrapper_only_size(_addr: usize) -> usize {
    size_of::<Wrapper>()
}

// ── Sprint 23 layout fns (NewGC HeapLayout binding) ─────────────────────────
//
// One per shape. All take a heap object's start address; return
// `(total_cells, pointer_cells_start, pointer_cells_end)`.

/// `<integer>`, `<single-float>`, `<double-float>`, `<boolean>`,
/// `<character>`, `<empty-list>` — single-cell wrapper, no payload.
///
/// # Safety
///
/// `addr` must point at a live `Wrapper`-headed object.
unsafe fn wrapper_only_layout(_addr: usize) -> (usize, usize, usize) {
    // 1 cell total, no pointer cells.
    (1, 0, 0)
}

/// `<byte-string>` — wrapper + len-word + N bytes padded to 8.
///
/// # Safety
///
/// `addr` must point at a live `<byte-string>`.
unsafe fn byte_string_layout(addr: usize) -> (usize, usize, usize) {
    // SAFETY: caller's precondition.
    let bs = unsafe { &*(addr as *const ByteString) };
    let total_bytes =
        (size_of::<ByteString>() + bs.len as usize).next_multiple_of(8);
    let total_cells = total_bytes / 8;
    // Opaque payload — GC must not scan the byte run as Words.
    (total_cells, 0, 0)
}

/// `<symbol>` — wrapper(0) + hash/pad(1) + name Word(2) = 3 cells.
///
/// # Safety
///
/// `addr` must point at a live `<symbol>`.
unsafe fn symbol_layout(_addr: usize) -> (usize, usize, usize) {
    // Only cell 2 (`name`) is a heap-pointer Word; cell 1 is
    // hash/pad and must NOT be over-scanned (a 32-bit hash could
    // land on bit 0 = 1 and fool classify into thinking it's a
    // pointer; the page_of gate would catch it but precise is
    // better).
    (3, 2, 3)
}

/// `<simple-object-vector>` — wrapper(0) + len(1) + N Word slots.
///
/// # Safety
///
/// `addr` must point at a live `<simple-object-vector>`.
unsafe fn vector_layout(addr: usize) -> (usize, usize, usize) {
    // SAFETY: caller's precondition.
    let v = unsafe { &*(addr as *const SimpleObjectVector) };
    let n = v.len as usize;
    // Payload starts at cell 2 (cell 1 is the length u64, not a Word).
    (2 + n, 2, 2 + n)
}

/// `<pair>` — wrapper(0) + head(1) + tail(2) = 3 cells, both Words.
///
/// # Safety
///
/// `addr` must point at a live `<pair>`.
unsafe fn pair_layout(_addr: usize) -> (usize, usize, usize) {
    (3, 1, 3)
}

/// User-class instance. All slots laid out contiguously at offsets
/// 8, 16, 24, … (one cell each). We report `(total, 1, total)` —
/// i.e. *every* payload cell is scanned. Non-pointer slots (`Integer`,
/// `Character`) hold tagged fixnums whose low bit is 0, so the GC's
/// classify returns `Immediate` and the cell is skipped harmlessly.
/// This is the "safe over-scanning" policy from Sprint 23's brief.
///
/// # Safety
///
/// `addr` must point at a live user-class instance whose wrapper's
/// `ClassId` is registered.
unsafe fn user_class_layout(addr: usize) -> (usize, usize, usize) {
    // SAFETY: caller asserts wrapper-first layout.
    let wrapper = unsafe { *(addr as *const Wrapper) };
    if wrapper.is_forwarded() {
        // Forwarded wrappers shouldn't be classified through layout —
        // the GC's `classify` short-circuits on Forwarded before
        // calling header_layout. Defensive fallback: pretend the
        // object is just the wrapper.
        return (1, 0, 0);
    }
    let metadata_ptr = class_metadata_ptr(wrapper.class());
    if metadata_ptr.is_null() {
        return (1, 0, 0);
    }
    // SAFETY: metadata is in the static area and lives forever.
    let metadata = unsafe { &*metadata_ptr };
    let total_cells = metadata.instance_size / 8;
    if total_cells <= 1 {
        (total_cells, 0, 0)
    } else {
        (total_cells, 1, total_cells)
    }
}

/// # Safety
///
/// `addr` must point at a live `<byte-string>` heap object.
unsafe fn byte_string_size(addr: usize) -> usize {
    // SAFETY: caller's precondition.
    let bs = unsafe { &*(addr as *const ByteString) };
    let total = size_of::<ByteString>() + bs.len as usize;
    total.next_multiple_of(8)
}

/// # Safety
///
/// `addr` must point at a live `<symbol>` heap object.
unsafe fn symbol_size(_addr: usize) -> usize {
    size_of::<Symbol>().next_multiple_of(8)
}

/// # Safety
///
/// `addr` must point at a live `<symbol>` heap object.
unsafe fn symbol_scan(addr: usize, visit: &mut dyn FnMut(*mut Word)) {
    // SAFETY: caller's precondition.
    let sym = unsafe { &mut *(addr as *mut Symbol) };
    visit(&mut sym.name as *mut Word);
}

/// # Safety
///
/// `addr` must point at a live `<simple-object-vector>` heap object.
unsafe fn vector_size(addr: usize) -> usize {
    // SAFETY: caller's precondition.
    let v = unsafe { &*(addr as *const SimpleObjectVector) };
    let total = size_of::<SimpleObjectVector>() + (v.len as usize) * size_of::<Word>();
    total.next_multiple_of(8)
}

/// # Safety
///
/// `addr` must point at a live `<simple-object-vector>` heap object.
unsafe fn vector_scan(addr: usize, visit: &mut dyn FnMut(*mut Word)) {
    // SAFETY: caller's precondition.
    let v = unsafe { &mut *(addr as *mut SimpleObjectVector) };
    let len = v.len as usize;
    let slots_base = (addr + size_of::<SimpleObjectVector>()) as *mut Word;
    for i in 0..len {
        // SAFETY: slots are laid out contiguously after the header.
        let slot_ptr = unsafe { slots_base.add(i) };
        visit(slot_ptr);
    }
}

/// # Safety
///
/// `addr` must point at a live `<pair>` heap object.
unsafe fn pair_size(_addr: usize) -> usize {
    size_of::<Pair>()
}

/// # Safety
///
/// `addr` must point at a live `<pair>` heap object.
unsafe fn pair_scan(addr: usize, visit: &mut dyn FnMut(*mut Word)) {
    // SAFETY: caller's precondition; layout is wrapper-first then
    // head Word (offset 8) and tail Word (offset 16).
    let pair = unsafe { &mut *(addr as *mut Pair) };
    visit(&mut pair.head as *mut Word);
    visit(&mut pair.tail as *mut Word);
}

// ─── User-class scan/size: read slot metadata + walk pointer slots ─────────

/// Scan a user-class instance by walking its `slots` from the registry.
///
/// # Safety
///
/// `addr` must point at a live user-class instance whose wrapper class
/// id is registered in `UserClasses`. The layout is: Wrapper at offset
/// 0, then 8-byte Words at slot offsets per `ClassMetadata`.
unsafe fn user_class_scan(addr: usize, visit: &mut dyn FnMut(*mut Word)) {
    // SAFETY: caller asserts wrapper-first layout.
    let wrapper = unsafe { *(addr as *const Wrapper) };
    if wrapper.is_forwarded() {
        return;
    }
    let class_id = wrapper.class();
    let metadata_ptr = class_metadata_ptr(class_id);
    if metadata_ptr.is_null() {
        return;
    }
    // SAFETY: metadata is in the static area and lives forever.
    let metadata = unsafe { &*metadata_ptr };
    for slot in &metadata.slots {
        if !slot.type_kind.is_pointer_shaped() {
            continue;
        }
        let slot_ptr = (addr + slot.offset) as *mut Word;
        visit(slot_ptr);
    }
}

/// Size of a user-class instance — constant from registered metadata.
///
/// # Safety
///
/// `addr` must point at a live user-class instance whose wrapper class
/// id is registered.
unsafe fn user_class_size(addr: usize) -> usize {
    // SAFETY: caller asserts wrapper-first layout.
    let wrapper = unsafe { *(addr as *const Wrapper) };
    if wrapper.is_forwarded() {
        return size_of::<Wrapper>();
    }
    let class_id = wrapper.class();
    let metadata_ptr = class_metadata_ptr(class_id);
    if metadata_ptr.is_null() {
        return size_of::<Wrapper>();
    }
    // SAFETY: metadata is in the static area and lives forever.
    let metadata = unsafe { &*metadata_ptr };
    metadata.instance_size
}

// ─── Seed table ────────────────────────────────────────────────────────────
//
// IDs in this slice MUST match the `ClassId::*` constants above.

struct SeedSpec {
    id: ClassId,
    name: &'static str,
    parent: Option<ClassId>,
    scan: ScanFn,
    size_of_fn: SizeFn,
    layout_fn: LayoutFn,
    instance_size: usize,
    is_byte_payload: bool,
}

fn seed_specs() -> [SeedSpec; 12] {
    [
        SeedSpec { id: ClassId::OBJECT, name: "<object>", parent: None, scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::INTEGER, name: "<integer>", parent: Some(ClassId::OBJECT), scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::SINGLE_FLOAT, name: "<single-float>", parent: Some(ClassId::OBJECT), scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::DOUBLE_FLOAT, name: "<double-float>", parent: Some(ClassId::OBJECT), scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::BOOLEAN, name: "<boolean>", parent: Some(ClassId::OBJECT), scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::CHARACTER, name: "<character>", parent: Some(ClassId::OBJECT), scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::SYMBOL, name: "<symbol>", parent: Some(ClassId::OBJECT), scan: symbol_scan, size_of_fn: symbol_size, layout_fn: symbol_layout, instance_size: size_of::<Symbol>(), is_byte_payload: false },
        SeedSpec { id: ClassId::STRING, name: "<string>", parent: Some(ClassId::OBJECT), scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::BYTE_STRING, name: "<byte-string>", parent: Some(ClassId::STRING), scan: noop_scan, size_of_fn: byte_string_size, layout_fn: byte_string_layout, instance_size: size_of::<ByteString>(), is_byte_payload: true },
        SeedSpec { id: ClassId::SIMPLE_OBJECT_VECTOR, name: "<simple-object-vector>", parent: Some(ClassId::OBJECT), scan: vector_scan, size_of_fn: vector_size, layout_fn: vector_layout, instance_size: size_of::<SimpleObjectVector>(), is_byte_payload: false },
        SeedSpec { id: ClassId::EMPTY_LIST, name: "<empty-list>", parent: Some(ClassId::OBJECT), scan: noop_scan, size_of_fn: wrapper_only_size, layout_fn: wrapper_only_layout, instance_size: size_of::<Wrapper>(), is_byte_payload: false },
        SeedSpec { id: ClassId::PAIR, name: "<pair>", parent: Some(ClassId::OBJECT), scan: pair_scan, size_of_fn: pair_size, layout_fn: pair_layout, instance_size: size_of::<Pair>(), is_byte_payload: false },
    ]
}

fn build_seed_cpl(id: ClassId, parent: Option<ClassId>, specs: &[SeedSpec]) -> Vec<ClassId> {
    let mut cpl = vec![id];
    let mut cur = parent;
    while let Some(c) = cur {
        cpl.push(c);
        cur = specs.iter().find(|s| s.id == c).and_then(|s| s.parent);
    }
    cpl
}

// ─── Process-global registry ────────────────────────────────────────────────
//
// Holds pointers to the seed metadata (static lifetime, built once at
// startup) plus user-defined entries (pinned in StaticArea via the
// `LiteralPool::user_classes` glue in lib.rs).

struct Registry {
    /// Indexed by class id (lookups use linear scan because counts stay
    /// small in v1; switch to HashMap if user classes ever number in
    /// the thousands).
    entries: Vec<*const ClassMetadata>,
    next_user_id: u32,
    /// Sprint 51e — monotonic id source for the front-end shim band
    /// (`ClassId::FIRST_SHIM..`). Bumped only while
    /// [`shim_class_band_active`] is set (the shim BUILD lowering its own
    /// source) or when the AOT resolver registers a shim-band class
    /// (`expected_class_id >= FIRST_SHIM`). Kept entirely separate from
    /// `next_user_id` so shim registrations never perturb the user
    /// id sequence. See `FIRST_SHIM`'s doc for why.
    next_shim_id: u32,
}

// SAFETY: entries are pointers into the static area (pinned and
// process-lived); the registry's `Mutex` guards mutation.
unsafe impl Send for Registry {}
unsafe impl Sync for Registry {}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut guard = REGISTRY.lock().expect("class registry mutex poisoned");
    if guard.is_none() {
        let specs = seed_specs();
        let mut entries: Vec<*const ClassMetadata> = Vec::with_capacity(specs.len());
        for spec in &specs {
            let cpl = build_seed_cpl(spec.id, spec.parent, &specs);
            let parents: Vec<ClassId> = spec.parent.into_iter().collect();
            let md = Box::leak(Box::new(ClassMetadata {
                id: spec.id,
                name: spec.name.to_string(),
                parent: spec.parent,
                parents,
                cpl,
                slots: Vec::new(),
                own_slot_count: 0,
                inherited_slot_count: 0,
                slot_origin: Vec::new(),
                instance_size: spec.instance_size,
                scan: spec.scan,
                size_of: spec.size_of_fn,
                layout: spec.layout_fn,
                is_byte_payload: spec.is_byte_payload,
                sealed: AtomicBool::new(false),
                direct_subclasses: RwLock::new(Vec::new()),
            }));
            entries.push(md as *const ClassMetadata);
        }
        *guard = Some(Registry {
            entries,
            // User (per-program `define class`) ids begin ABOVE the pinned
            // library band so they don't depend on the library class count.
            next_user_id: crate::class_pins::PIN_CEILING,
            next_shim_id: ClassId::FIRST_SHIM,
        });
    }
    f(guard.as_mut().expect("registry initialised"))
}

/// Sprint 51e — process-global toggle: when set, [`allocate_user_class_id`]
/// mints from the front-end-shim band (`ClassId::FIRST_SHIM..`) instead
/// of the normal user band (`ClassId::FIRST_USER..`).
///
/// The shim BUILD flips this ON after the stdlib has loaded (so stdlib
/// classes keep their canonical `FIRST_USER..` ids) and before lowering
/// the shim's OWN `define class`es, so the shim's classes are minted —
/// and thus baked into the shim `.obj` — in the high band. It stays OFF
/// for every other build (user programs, the JIT path), so their classes
/// allocate from `FIRST_USER..` exactly as before.
static SHIM_CLASS_BAND_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Sprint 51e — enter/leave the shim class-id band for subsequent
/// [`allocate_user_class_id`] calls on THIS process. See
/// [`ClassId::FIRST_SHIM`].
pub fn set_shim_class_band_active(active: bool) {
    SHIM_CLASS_BAND_ACTIVE.store(active, Ordering::SeqCst);
}

/// Sprint 51e — is the shim class-id band currently active?
pub fn shim_class_band_active() -> bool {
    SHIM_CLASS_BAND_ACTIVE.load(Ordering::SeqCst)
}

/// Raw pointer to the metadata for `id`, or null if unregistered. Used
/// by the GC's scan/size paths and by codegen-baked addresses.
pub fn class_metadata_ptr(id: ClassId) -> *const ClassMetadata {
    with_registry(|reg| {
        reg.entries
            .iter()
            .copied()
            .find(|p| {
                // SAFETY: pointer is to static-area metadata, always live.
                !p.is_null() && unsafe { (**p).id } == id
            })
            .unwrap_or(std::ptr::null())
    })
}

/// Look up metadata for a class id. Panics if the id is unknown — the
/// GC and tracer assume the registry is populated.
pub fn class_metadata_for(id: ClassId) -> &'static ClassMetadata {
    let p = class_metadata_ptr(id);
    assert!(!p.is_null(), "class id {} not registered", id.0);
    // SAFETY: pointer is to static-area metadata, lives for process.
    unsafe { &*p }
}

/// Register a user class. Returns its assigned `ClassId`. The metadata
/// must already be allocated in the static area (caller supplies a
/// `&'static ClassMetadata`).
///
/// # Safety
///
/// `metadata` must outlive the process — i.e. live in the static area.
pub unsafe fn register_user_class(metadata: &'static ClassMetadata) -> ClassId {
    with_registry(|reg| {
        reg.entries.push(metadata as *const ClassMetadata);
        metadata.id
    })
}

/// Reserve the next class id and bump the appropriate counter.
///
/// Mints from the front-end-shim band (`FIRST_SHIM..`) when
/// [`shim_class_band_active`] is set, otherwise from the normal user
/// band (`FIRST_USER..`). The two counters are independent so a shim
/// build's own classes never perturb the user id sequence — see
/// [`ClassId::FIRST_SHIM`].
pub fn allocate_user_class_id() -> ClassId {
    let shim_band = shim_class_band_active();
    with_registry(|reg| {
        if shim_band {
            let id = ClassId(reg.next_shim_id);
            reg.next_shim_id += 1;
            id
        } else {
            let id = ClassId(reg.next_user_id);
            reg.next_user_id += 1;
            id
        }
    })
}

/// Name-keyed class-id allocation — the stable replacement for
/// [`allocate_user_class_id`] at the class-registration funnel.
///
/// A LIBRARY class (runtime-seeded `ensure_*` or stdlib `define class`) gets
/// its STABLE id from the name->id pin table ([`crate::class_pins`]),
/// independent of registration order. A per-program USER class (not pinned)
/// allocates from the user band, which begins at `PIN_CEILING` (above every
/// pinned library id) so user ids do not depend on the library class count.
/// The shim band is unchanged.
///
/// This is what makes adding/removing/reordering a stdlib class NOT renumber
/// any other class — eliminating the recurring AOT/shim class-id drift.
pub fn allocate_user_class_id_named(name: &str) -> ClassId {
    if shim_class_band_active() {
        return with_registry(|reg| {
            let id = ClassId(reg.next_shim_id);
            reg.next_shim_id += 1;
            id
        });
    }
    if let Some(pid) = crate::class_pins::pinned_id(name) {
        return ClassId(pid);
    }
    with_registry(|reg| {
        let id = ClassId(reg.next_user_id);
        reg.next_user_id += 1;
        id
    })
}

/// Find a user-class id by name. Searches the seed table first, then
/// user classes. Returns `None` if not found.
pub fn find_class_id_by_name(name: &str) -> Option<ClassId> {
    with_registry(|reg| {
        for p in &reg.entries {
            if p.is_null() {
                continue;
            }
            // SAFETY: pointer is to static-area metadata.
            let md = unsafe { &**p };
            if md.name == name {
                return Some(md.id);
            }
        }
        None
    })
}

/// Sprint 51e — find a class id by name, **ignoring front-end-shim-band
/// classes** (`ClassId::FIRST_SHIM..`).
///
/// The shim's internal classes (`<token>`, `<ast-*>`, …) get registered
/// in a host process when a statically-linked front-end shim's resolver
/// fires (e.g. `nod-driver` parsing with `--parse-with-dylan`). They are
/// an implementation detail of the compiler's front-end and form a
/// namespace DISJOINT from the user program's classes — a user program
/// may legitimately `define class <token>` of its own. Name resolution
/// during USER-class lowering (`register_class`'s redefinition refusal
/// and superclass resolution) therefore consults only the user/seed
/// bands via this function, so a shim class never shadows or blocks a
/// same-named user class. (The shim's OWN build resolves its own classes
/// through the unfiltered [`find_class_id_by_name`], because there the
/// shim classes ARE the program's classes.)
pub fn find_class_id_by_name_excluding_shim_band(name: &str) -> Option<ClassId> {
    with_registry(|reg| {
        for p in &reg.entries {
            if p.is_null() {
                continue;
            }
            // SAFETY: pointer is to static-area metadata.
            let md = unsafe { &**p };
            if md.id.0 >= ClassId::FIRST_SHIM {
                continue;
            }
            if md.name == name {
                return Some(md.id);
            }
        }
        None
    })
}

/// Iterate every registered class metadata (seed + user). Used by
/// `dump_classes()`.
pub fn for_each_class(mut f: impl FnMut(&'static ClassMetadata)) {
    let snapshot: Vec<*const ClassMetadata> = with_registry(|reg| reg.entries.clone());
    for p in snapshot {
        if !p.is_null() {
            // SAFETY: pointer is to static-area metadata, lives for process.
            f(unsafe { &*p });
        }
    }
}

/// Sprint 16 test helper: drop every user-defined class from the
/// registry and reset the user-class id counter. Seed classes (ids
/// `0..FIRST_USER`) survive. Used by `#[serial]` tests that re-lower
/// fixtures with the same class names across runs — without this,
/// Sprint 12's redefinition refusal fires on the second pass.
///
/// **Not** safe to call while JIT'd code holding baked class-metadata
/// addresses is on the stack; the metadata stays pinned in the static
/// area (we don't free it, just drop the registry entry), so existing
/// JIT references still resolve, but a fresh lowering will mint new
/// class ids and the old ones will become orphaned.
pub fn _reset_user_classes_for_tests() {
    let mut guard = REGISTRY.lock().expect("class registry mutex poisoned");
    if let Some(reg) = guard.as_mut() {
        // SAFETY: every non-null entry points at a `ClassMetadata` pinned
        // in the static area; reading `id` is sound for the lifetime of
        // the process.
        reg.entries
            .retain(|p| !p.is_null() && unsafe { (**p).id.0 } < ClassId::FIRST_USER);
        reg.next_user_id = crate::class_pins::PIN_CEILING;
        // Sprint 51e — the retain above already drops shim-band entries
        // (their ids are `>= FIRST_SHIM > FIRST_USER`); reset the shim
        // counter too so a re-lower starts both bands from a clean seed.
        reg.next_shim_id = ClassId::FIRST_SHIM;
    }
}

/// Scan function used by every user-class metadata entry. Re-exported
/// so the registry-glue in `lib.rs` (`register_class`) can install it
/// without naming a private symbol.
pub fn user_class_scan_fn() -> ScanFn {
    user_class_scan
}

/// Size function for user classes.
pub fn user_class_size_fn() -> SizeFn {
    user_class_size
}

/// Sprint 23: layout function for user classes — over-scans every
/// slot from cell 1 to `instance_size / 8`. Fixnum-tagged Words skip
/// at `classify` time so the over-scan is safe.
pub fn user_class_layout_fn() -> LayoutFn {
    user_class_layout
}

// ─── `ClassTable`: the seed-only convenience handle ────────────────────────
//
// Sprint 09/10/11 callers expect a `ClassTable` they can ask
// `.integer()`, `.byte_string()`, etc. Sprint 12 keeps the type around
// as a thin facade over the registry — `find_by_name` now also resolves
// user classes registered after `ClassTable::new()`.

pub struct ClassTable;

impl ClassTable {
    pub fn new() -> Self {
        // Trigger registry init.
        let _ = class_metadata_ptr(ClassId::OBJECT);
        ClassTable
    }

    pub fn get(&self, id: ClassId) -> &'static ClassMetadata {
        class_metadata_for(id)
    }

    pub fn find_by_name(&self, name: &str) -> Option<ClassId> {
        find_class_id_by_name(name)
    }

    pub fn object(&self) -> ClassId { ClassId::OBJECT }
    pub fn integer(&self) -> ClassId { ClassId::INTEGER }
    pub fn single_float(&self) -> ClassId { ClassId::SINGLE_FLOAT }
    pub fn double_float(&self) -> ClassId { ClassId::DOUBLE_FLOAT }
    pub fn boolean(&self) -> ClassId { ClassId::BOOLEAN }
    pub fn character(&self) -> ClassId { ClassId::CHARACTER }
    pub fn symbol(&self) -> ClassId { ClassId::SYMBOL }
    pub fn string(&self) -> ClassId { ClassId::STRING }
    pub fn byte_string(&self) -> ClassId { ClassId::BYTE_STRING }
    pub fn simple_object_vector(&self) -> ClassId { ClassId::SIMPLE_OBJECT_VECTOR }
    pub fn empty_list(&self) -> ClassId { ClassId::EMPTY_LIST }
    pub fn pair(&self) -> ClassId { ClassId::PAIR }
}

impl Default for ClassTable {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Subclass test (walks CPL) ─────────────────────────────────────────────

/// True iff `sub`'s CPL contains `super_id`. Used by `instance?` for
/// user classes and by single-dispatch method lookup.
pub fn is_subclass(sub: ClassId, super_id: ClassId) -> bool {
    if sub == super_id {
        return true;
    }
    let p = class_metadata_ptr(sub);
    if p.is_null() {
        return false;
    }
    // SAFETY: pointer is to static-area metadata.
    unsafe { (*p).cpl.contains(&super_id) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_ids_are_stable_across_instances() {
        let a = ClassTable::new();
        let b = ClassTable::new();
        assert_eq!(a.integer(), b.integer());
        assert_eq!(a.string(), b.string());
        assert_eq!(a.boolean(), b.boolean());
    }

    #[test]
    fn seed_table_has_expected_names() {
        let ct = ClassTable::new();
        assert_eq!(ct.get(ct.integer()).name, "<integer>");
        assert_eq!(ct.get(ct.string()).name, "<string>");
        assert_eq!(ct.get(ct.object()).name, "<object>");
    }

    #[test]
    fn find_by_name_round_trip() {
        let ct = ClassTable::new();
        assert_eq!(ct.find_by_name("<integer>"), Some(ct.integer()));
        assert_eq!(ct.find_by_name("<boolean>"), Some(ct.boolean()));
        assert_eq!(ct.find_by_name("<byte-string>"), Some(ct.byte_string()));
        assert_eq!(ct.find_by_name("<simple-object-vector>"), Some(ct.simple_object_vector()));
        assert_eq!(ct.find_by_name("<empty-list>"), Some(ct.empty_list()));
        assert_eq!(ct.find_by_name("<pair>"), Some(ct.pair()));
        assert_eq!(ct.find_by_name("<no-such-class>"), None);
    }

    #[test]
    fn seed_metadata_address_stable() {
        let a = class_metadata_ptr(ClassId::BYTE_STRING);
        let b = class_metadata_ptr(ClassId::BYTE_STRING);
        assert_eq!(a, b);
        assert!(!a.is_null());
    }

    #[test]
    fn seed_cpl_chain_is_correct() {
        let bs = class_metadata_for(ClassId::BYTE_STRING);
        // CPL: <byte-string>, <string>, <object>.
        assert_eq!(bs.cpl[0], ClassId::BYTE_STRING);
        assert_eq!(bs.cpl[1], ClassId::STRING);
        assert_eq!(bs.cpl[2], ClassId::OBJECT);
    }

    #[test]
    fn is_subclass_walks_cpl() {
        assert!(is_subclass(ClassId::BYTE_STRING, ClassId::STRING));
        assert!(is_subclass(ClassId::BYTE_STRING, ClassId::OBJECT));
        assert!(!is_subclass(ClassId::BYTE_STRING, ClassId::INTEGER));
        assert!(is_subclass(ClassId::INTEGER, ClassId::OBJECT));
        assert!(is_subclass(ClassId::INTEGER, ClassId::INTEGER));
    }
}
