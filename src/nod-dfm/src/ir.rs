//! Core IR types — `Function`, `Block`, `Computation`, `Temporary`.

use nod_reader::Span;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct FunctionId(pub u32);

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct BlockId(pub u32);

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct TempId(pub u32);

#[derive(Clone, Debug)]
pub struct Function {
    pub id: FunctionId,
    pub name: String,
    pub params: Vec<TempId>,
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    pub temps: Vec<Temporary>,
    pub return_type: TypeEstimate,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub id: BlockId,
    pub label: String,
    /// Block parameters (phi-style): values supplied by predecessor
    /// `Terminator::Jump { args }`. Function params live on the entry
    /// block only as `Function::params`; non-entry blocks use this for
    /// joins (e.g. `if`-expression results).
    pub params: Vec<TempId>,
    pub computations: Vec<Computation>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug)]
pub struct Temporary {
    pub id: TempId,
    pub type_estimate: TypeEstimate,
}

/// Planned physical location of a GC root at a safepoint.
///
/// Sprint 45c introduces this as the first step away from the
/// temp-identity-based safepoint contract. The active runtime path
/// still uses `safepoint_roots: Vec<TempId>` plus spill/reload shims,
/// but codegen planning and debug surfaces can now talk in terms of
/// *where* a root will live rather than only *which temp* is live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafepointLocation {
    /// Root lives in a compiler-owned frame slot.
    FrameSlot(u32),
    /// Root lives in a saved general-purpose register slot.
    SavedRegister(u8),
}

/// One root and its planned location at a safepoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafepointRootLocation {
    pub temp: TempId,
    pub location: SafepointLocation,
}

#[derive(Clone, Debug)]
pub enum Computation {
    Const {
        dst: TempId,
        value: ConstValue,
    },
    PrimOp {
        dst: TempId,
        op: PrimOp,
        args: Vec<TempId>,
    },
    /// Call a statically-known top-level name. Resolution happens during
    /// lowering against the surrounding module's `define function` set.
    ///
    /// `safepoint_roots` lists the pointer-shaped temps live across this
    /// call. Sprint 11b's spill-to-runtime-slots GC root protection:
    /// codegen brackets the call with `nod_register_root` /
    /// `nod_unregister_root` pairs for each entry, reloading the temp
    /// from the slot after the call returns so any GC-driven evacuation
    /// is observed by subsequent uses. Empty `Vec` means "no allocation
    /// possible at this call site" — codegen skips the bracketing.
    DirectCall {
        dst: TempId,
        callee: String,
        args: Vec<TempId>,
        safepoint_roots: Vec<TempId>,
        /// Sprint 48: when true, the callee is known not to allocate
        /// (no `nod_make` calls, no growth of any heap-backed structure,
        /// no condition signalling). Sourced from `LOWER_PRIMITIVE_TABLE`
        /// at lowering time for `%`-prefixed primitives; user-defined
        /// functions default to `false` unless a future fixed-point
        /// analysis (Sprint 48 Phase B) propagates it. When true,
        /// `is_potentially_allocating_call` returns false → the liveness
        /// pass leaves `safepoint_roots` empty → codegen skips the
        /// `nod_jit_begin_safepoint` / `nod_jit_end_safepoint` brackets
        /// (existing `!rented.is_empty()` guard).
        is_no_alloc: bool,
    },
    /// Call an evaluated callee value. Lowered for higher-order calls
    /// once the callee expression isn't a bare ident.
    ///
    /// See `DirectCall::safepoint_roots`.
    Call {
        dst: TempId,
        callee: TempId,
        args: Vec<TempId>,
        safepoint_roots: Vec<TempId>,
    },
    /// Tag / class membership test. Sprint 09 covers the two checks
    /// answerable from `Word` tag bits alone: `<integer>` (fixnum tag)
    /// and `<boolean>` (no heap allocation yet — provisional encoding
    /// detailed in `nod-llvm::codegen`). Other classes are stubbed
    /// out with a `Bool(false)` result until Sprint 12 wires real
    /// `<wrapper>`-based dispatch.
    TypeCheck {
        dst: TempId,
        value: TempId,
        class: ClassCheck,
    },
    /// Sprint 11 stub. Store `src` into `*dst` and mark the
    /// corresponding GC card. Sprint 12+ lowering paths (slot
    /// setters, vector-set) will emit this node; Sprint 11 codegen
    /// recognises it but no lowering emits it yet, so it's
    /// unreachable in practice.
    ///
    /// `dst` is a tagged-Word slot pointer (a Word whose decoded
    /// pointer is a `*mut Word` into a heap object's slot). `src` is
    /// the Word to store. The lowered code is equivalent to
    /// `nod_runtime::write_barrier(dst, src)`.
    WriteBarrier {
        /// Unused result temp (the store has no value). Kept for SSA
        /// uniformity — every Computation has a `dst()`.
        dst: TempId,
        /// Slot to write into. Must be a Word whose decoded pointer
        /// is a writable `*mut Word`.
        slot: TempId,
        /// Value to store.
        value: TempId,
    },
    /// Sprint 12 slot getter. Untag `instance`, read 8 bytes at
    /// `offset`. The dst temp gets the slot's tagged Word.
    LoadSlot {
        dst: TempId,
        instance: TempId,
        /// Byte offset from the start of the heap object (includes
        /// the leading `Wrapper`).
        offset: usize,
        slot_type: SlotTypeKind,
    },
    /// Sprint 12 slot setter. Untag `instance`, store `value` at
    /// `offset`, emit a card-marking write barrier. The dst temp is
    /// unused (set to `value` for SSA uniformity).
    StoreSlot {
        dst: TempId,
        instance: TempId,
        offset: usize,
        value: TempId,
        slot_type: SlotTypeKind,
    },
    /// Sprint 12 single-dispatch generic call. Codegen lowers this
    /// to a runtime call into `nod_dispatch_unary` (unary case) which
    /// walks the dispatch table, picks the most-specific method, and
    /// tail-calls into it. Sprint 13 grows this into the inline-cache
    /// shape.
    ///
    /// See `DirectCall::safepoint_roots`.
    Dispatch {
        dst: TempId,
        generic_name: String,
        args: Vec<TempId>,
        safepoint_roots: Vec<TempId>,
    },
    /// Sprint 15: sealed-direct multimethod call. Issued by the Sprint 15
    /// dispatch resolver when sealing facts let the compiler pick a
    /// single most-specific method at compile time, but additional
    /// applicable methods (less specific) remain in the chain so the
    /// resolved method body may legally call `next-method()`.
    ///
    /// Lowering emits a thread-local chain-frame push (carrying
    /// `args` and the `fallback_chain` symbols, walked via the
    /// runtime registry) before the call to `method`, and a matching
    /// pop afterwards. The method body's
    /// `extern "C" fn(u64, ..., u64) -> u64` ABI is preserved exactly
    /// — bodies don't gain implicit parameters; chain setup happens
    /// at the call site, matching the recommendation in spec 15 §9.7.
    ///
    /// When the resolver finds exactly ONE applicable method (no chain
    /// possible), it emits a plain `DirectCall` instead; this variant
    /// is reserved for the 2+-applicable case.
    ///
    /// See `DirectCall::safepoint_roots`.
    SealedDirectCall {
        dst: TempId,
        method: String,
        /// Less-specific applicable methods (in most-specific-first
        /// order) AFTER the chosen `method`. The codegen-level
        /// preamble pushes these onto the runtime's thread-local
        /// chain stack so `next-method()` inside `method`'s body walks
        /// through them. May be empty when the resolver couldn't
        /// prove uniqueness — in that case lowering falls back to
        /// plain Dispatch and this variant doesn't fire.
        fallback_chain: Vec<String>,
        /// Generic-function name used for diagnostics / dump-dispatch
        /// annotations. Not load-bearing for codegen.
        generic_name: String,
        args: Vec<TempId>,
        safepoint_roots: Vec<TempId>,
        /// Sprint 48 — see `DirectCall::is_no_alloc`. Practically
        /// always `false` for sealed-direct calls today (user-defined
        /// methods aren't analysed); reserved for the Phase B
        /// extension.
        is_no_alloc: bool,
    },
    // TODO(sprint-08+): `Values`, `BindExit`, `UnbindExit`, `Closure`,
    // `MakeEnvironment`. Verifier rejects them today because they aren't
    // emitted by the kernel-subset lowering; the comments stand in for
    // them so the SPRINTS.md §216 list is documented in the source.
}

/// Slot value kind for `LoadSlot` / `StoreSlot`. A subset of the
/// runtime's `SlotType` — codegen only needs to know whether the slot
/// is pointer-shaped (for the future write-barrier emission) and
/// whether to surface it as `<integer>` vs `<top>`.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum SlotTypeKind {
    Integer,
    Object,
}

/// Which class an `instance?` test resolves to. Sprint 09 only
/// materialised the cases answerable from tag bits; Sprint 10 adds
/// wrapper-tagged heap classes (`<byte-string>`, `<symbol>`,
/// `<simple-object-vector>`, `<character>`, `<empty-list>`) plus a
/// proper `<boolean>` test rooted at the boolean singleton's wrapper.
/// Sprint 12 extends this with user-defined classes carrying a runtime
/// `ClassId` payload.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum ClassCheck {
    /// Fixnum-tag test (bit 0 == 0).
    Integer,
    /// Wrapper-class test against `<boolean>` — true for the pinned
    /// `#t` / `#f` singletons.
    Boolean,
    /// Wrapper-class test against `<byte-string>`.
    String,
    /// Wrapper-class test against `<symbol>`.
    Symbol,
    /// Wrapper-class test against `<simple-object-vector>`.
    Vector,
    /// Wrapper-class test against `<character>` (placeholder — Sprint 10
    /// still lowers char literals as raw i32; this test currently always
    /// answers `#f`, which is consistent until char boxing lands).
    Character,
    /// Wrapper-class test against `<empty-list>` — true for the pinned
    /// `nil` singleton.
    EmptyList,
    /// User-defined class — tested via the runtime `nod_is_instance_of`
    /// helper (walks the target object's class CPL). Sprint 12 adds
    /// this; the `class_id` is the runtime ClassId, baked into the IR.
    UserClass { id: u32, name: String },
    /// Anything we can't yet test — codegen folds to a constant `#f`.
    /// `name` is preserved for diagnostics + DFM dumps.
    Unsupported { name: &'static str },
}

impl ClassCheck {
    pub fn name(&self) -> &str {
        match self {
            ClassCheck::Integer => "<integer>",
            ClassCheck::Boolean => "<boolean>",
            ClassCheck::String => "<byte-string>",
            ClassCheck::Symbol => "<symbol>",
            ClassCheck::Vector => "<simple-object-vector>",
            ClassCheck::Character => "<character>",
            ClassCheck::EmptyList => "<empty-list>",
            ClassCheck::UserClass { name, .. } => name.as_str(),
            ClassCheck::Unsupported { name } => name,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Return {
        value: Option<TempId>,
    },
    If {
        cond: TempId,
        then_block: BlockId,
        else_block: BlockId,
    },
    Jump {
        target: BlockId,
        args: Vec<TempId>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Integer(i128),
    Float(f64),
    Bool(bool),
    String(String),
    Char(char),
    Unit,
    /// Raw 64-bit pattern, used by Sprint 12 lowering to bake
    /// metadata-pointer Words and tagged singletons into the IR.
    /// Codegen lowers this to a literal `i64` constant — no
    /// shift, no encoding.
    WordBits(u64),
    /// Sprint 38c — a reference to a class's `ClassMetadata` pointer.
    /// Codegen lowers to a `load i64` through a per-module external
    /// global `@nod_class_md__<key>__<class_id>`. The JIT-link path
    /// binds the symbol to the address of a `u64` slot whose contents
    /// are `class_metadata_ptr(class_id) as u64` (the raw, untagged
    /// metadata pointer in the current process). If `tagged` is true,
    /// codegen ORs `| 1` after the load to materialise the
    /// pointer-tagged Word; if false, the loaded value is used as-is
    /// (`nod_make`'s class arg expects an untagged pointer).
    ClassMetadataPtr { class_id: u32, tagged: bool },
    /// Sprint 38c — a reference to an interned `<byte-string>` literal.
    /// Codegen lowers to a `load i64` through a per-module external
    /// global `@nod_strlit__<key>__<idx>`, where `idx` is a per-module
    /// counter assigned by codegen on first encounter and deduped by
    /// content. The slot's contents are
    /// `intern_string_literal(text).raw()` in the current process.
    StringLiteralRef(String),
    /// Sprint 38c — a reference to an interned `<symbol>` literal.
    /// Same shape as `StringLiteralRef` but for Dylan symbols.
    SymbolLiteralRef(String),
    /// Sprint 38d — a reference to a Win32 stub-entry pointer for the
    /// `(dll, symbol)` pair carrying the marshaling signature.
    ///
    /// Codegen lowers to `load i64, ptr @nod_stub__<key>__<idx>` through
    /// a per-module external global; the JIT-link path binds the
    /// symbol's address to a stable `u64` slot whose contents are the
    /// address of a freshly-allocated [`nod_runtime::ApiStubEntry`] in
    /// the current process. The loaded value is the entry pointer the
    /// trampoline (`nod_winffi_call_N`) takes as its first argument.
    ///
    /// `signature_bytes` is the bytewise-encoded
    /// [`nod_runtime::ApiCallSignature`] (`#[repr(C)] Copy`); the
    /// resolver round-trips it through `copy_nonoverlapping` before
    /// allocating the entry. The bytes are part of the IR's
    /// cache-key fingerprint, so two call sites with different
    /// marshaling signatures emit distinct external globals.
    StubEntryRef {
        dll: String,
        symbol: String,
        signature_bytes: Vec<u8>,
    },
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PrimOp {
    AddInt,
    SubInt,
    MulInt,
    DivInt,
    ModInt,
    RemInt,
    NegInt,
    AddFloat,
    SubFloat,
    MulFloat,
    DivFloat,
    NegFloat,
    EqInt,
    NeInt,
    LtInt,
    GtInt,
    LeInt,
    GeInt,
    EqFloat,
    LtFloat,
    GtFloat,
    LeFloat,
    GeFloat,
    BoolAnd,
    BoolOr,
    BoolNot,
}

impl PrimOp {
    pub fn name(self) -> &'static str {
        match self {
            PrimOp::AddInt => "AddInt",
            PrimOp::SubInt => "SubInt",
            PrimOp::MulInt => "MulInt",
            PrimOp::DivInt => "DivInt",
            PrimOp::ModInt => "ModInt",
            PrimOp::RemInt => "RemInt",
            PrimOp::NegInt => "NegInt",
            PrimOp::AddFloat => "AddFloat",
            PrimOp::SubFloat => "SubFloat",
            PrimOp::MulFloat => "MulFloat",
            PrimOp::DivFloat => "DivFloat",
            PrimOp::NegFloat => "NegFloat",
            PrimOp::EqInt => "EqInt",
            PrimOp::NeInt => "NeInt",
            PrimOp::LtInt => "LtInt",
            PrimOp::GtInt => "GtInt",
            PrimOp::LeInt => "LeInt",
            PrimOp::GeInt => "GeInt",
            PrimOp::EqFloat => "EqFloat",
            PrimOp::LtFloat => "LtFloat",
            PrimOp::GtFloat => "GtFloat",
            PrimOp::LeFloat => "LeFloat",
            PrimOp::GeFloat => "GeFloat",
            PrimOp::BoolAnd => "BoolAnd",
            PrimOp::BoolOr => "BoolOr",
            PrimOp::BoolNot => "BoolNot",
        }
    }

    /// The result type for a PrimOp given the operand-type lattice.
    /// Lowering uses this for the `dst` temporary's `type_estimate`.
    pub fn result_type(self) -> TypeEstimate {
        match self {
            PrimOp::AddInt
            | PrimOp::SubInt
            | PrimOp::MulInt
            | PrimOp::DivInt
            | PrimOp::ModInt
            | PrimOp::RemInt
            | PrimOp::NegInt => TypeEstimate::Integer,
            PrimOp::AddFloat
            | PrimOp::SubFloat
            | PrimOp::MulFloat
            | PrimOp::DivFloat
            | PrimOp::NegFloat => TypeEstimate::DoubleFloat,
            PrimOp::EqInt
            | PrimOp::NeInt
            | PrimOp::LtInt
            | PrimOp::GtInt
            | PrimOp::LeInt
            | PrimOp::GeInt
            | PrimOp::EqFloat
            | PrimOp::LtFloat
            | PrimOp::GtFloat
            | PrimOp::LeFloat
            | PrimOp::GeFloat
            | PrimOp::BoolAnd
            | PrimOp::BoolOr
            | PrimOp::BoolNot => TypeEstimate::Boolean,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum TypeEstimate {
    Top,
    Bottom,
    Integer,
    SingleFloat,
    DoubleFloat,
    Character,
    Boolean,
    String,
    Unit,
    /// Sprint 15: receiver is known to be an instance of this class or
    /// a subclass. The `u32` is a `nod_runtime::ClassId`'s raw value
    /// (kept as a `u32` here so `nod-dfm` doesn't have to depend on
    /// `nod-runtime`). Conversion at the dispatch resolver boundary
    /// uses `nod_runtime::ClassId(estimate.class_id().unwrap())`.
    ///
    /// `Class(c)` is the metaclass reference. `TypeEstimate::Integer`
    /// is the immediate-fixnum bit pattern; the two are distinct at
    /// the lattice level (an `Integer` is implicitly a `Class(<integer>)`
    /// at runtime, but the lattice treats them as separate so the
    /// narrowing pass can drive both fixnum codegen and class-based
    /// dispatch resolution from the same estimate set).
    Class(u32),
    /// Sprint 15 lattice variant — receiver is known to be EXACTLY this
    /// Word value (e.g. `#f` after an `if`-test branch). Carried but
    /// **not populated** by the Sprint 15 narrower; the conditions that
    /// would refine to a Singleton (`if x == #f then …`) need pattern
    /// recognition in the analyser. Reserved for Sprint 17 (macros)
    /// and Sprint 19 (conditions). The dispatch resolver downgrades any
    /// Singleton to `Top` for now.
    Singleton(u64),
}

impl TypeEstimate {
    pub fn name(self) -> &'static str {
        match self {
            TypeEstimate::Top => "<top>",
            TypeEstimate::Bottom => "<bottom>",
            TypeEstimate::Integer => "<integer>",
            TypeEstimate::SingleFloat => "<single-float>",
            TypeEstimate::DoubleFloat => "<double-float>",
            TypeEstimate::Character => "<character>",
            TypeEstimate::Boolean => "<boolean>",
            TypeEstimate::String => "<string>",
            TypeEstimate::Unit => "<unit>",
            TypeEstimate::Class(_) => "<class>",
            TypeEstimate::Singleton(_) => "<singleton>",
        }
    }

    /// If this estimate identifies a single class id, return it. Used
    /// at the `nod-sema`/`nod-runtime` boundary so the dispatch resolver
    /// can re-wrap the raw u32 in `nod_runtime::ClassId`.
    pub fn class_id(self) -> Option<u32> {
        match self {
            TypeEstimate::Class(id) => Some(id),
            _ => None,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, TypeEstimate::SingleFloat | TypeEstimate::DoubleFloat)
    }

    pub fn is_integer(self) -> bool {
        matches!(self, TypeEstimate::Integer)
    }

    /// Sprint 11b: does a temp of this type need GC root protection
    /// across a potentially-allocating call?
    ///
    /// **Immediate** estimates (`Integer`, `Boolean`, `Character`,
    /// `SingleFloat`, `DoubleFloat`) lower to non-pointer i64/f32/f64
    /// register values — GC can't relocate them. **Pointer-shaped**
    /// estimates (`String`, `Top`, `Bottom`) may carry a tagged heap
    /// pointer; if the target object moves, the slot's bit pattern
    /// must be rewritten. `Unit` never flows through SSA as a real
    /// value.
    ///
    /// Note that `Boolean` is currently a pinned heap singleton
    /// (`#t`/`#f` wrappers live in the static area), so its addresses
    /// also survive GC unchanged — no protection needed. Same for
    /// `Character` (Sprint 12 lowers chars as raw i32).
    ///
    /// This is over-conservative for `Top`/`Bottom` (a `Top`-typed
    /// fixnum still gets protected), but the cost is a few extra
    /// alloca + store + load instructions around each allocating
    /// call. Sprint 11c's `gc.statepoint` upgrade tightens this.
    pub fn needs_gc_protection(self) -> bool {
        match self {
            TypeEstimate::Integer
            | TypeEstimate::Boolean
            | TypeEstimate::Character
            | TypeEstimate::SingleFloat
            | TypeEstimate::DoubleFloat
            | TypeEstimate::Unit => false,
            TypeEstimate::String
            | TypeEstimate::Top
            | TypeEstimate::Bottom
            | TypeEstimate::Class(_)
            | TypeEstimate::Singleton(_) => true,
        }
    }

    /// Join two estimates on the lattice. Used at block-arg joins (e.g.
    /// `if` expressions where then/else produce different concrete types).
    ///
    /// For `Class(_)`-vs-`Class(_)` widening (Sprint 15 if-merge), the
    /// caller can compose this with `join_class_via_cpl` — `join` itself
    /// doesn't know about the class precedence list, so two distinct
    /// `Class(c1)` and `Class(c2)` estimates widen to `Top` here. The
    /// dispatch-resolver-side helper looks up both CPLs and produces the
    /// closest common ancestor.
    pub fn join(self, other: TypeEstimate) -> TypeEstimate {
        if self == other {
            return self;
        }
        match (self, other) {
            (TypeEstimate::Bottom, t) | (t, TypeEstimate::Bottom) => t,
            (TypeEstimate::SingleFloat, TypeEstimate::DoubleFloat)
            | (TypeEstimate::DoubleFloat, TypeEstimate::SingleFloat) => TypeEstimate::DoubleFloat,
            _ => TypeEstimate::Top,
        }
    }

    /// Sprint 15 meet — narrows. For two `Class(c1)` and `Class(c2)`
    /// estimates the meet is whichever is `<:` the other (more
    /// specific wins). When neither is a subclass of the other the
    /// meet is `Bottom` (no value can inhabit both). For other
    /// disjoint kinds the meet is also `Bottom`. `meet` with `Top` on
    /// either side returns the other side; `meet` with itself is the
    /// identity.
    ///
    /// `is_subclass` is the caller-supplied "is class A <: class B?"
    /// predicate — typically `nod_runtime::is_subclass`. We thread it
    /// in rather than depend on `nod-runtime` from `nod-dfm`.
    pub fn meet(self, other: TypeEstimate, is_subclass: &dyn Fn(u32, u32) -> bool) -> TypeEstimate {
        if self == other {
            return self;
        }
        match (self, other) {
            (TypeEstimate::Top, t) | (t, TypeEstimate::Top) => t,
            (TypeEstimate::Bottom, _) | (_, TypeEstimate::Bottom) => TypeEstimate::Bottom,
            (TypeEstimate::Class(a), TypeEstimate::Class(b)) => {
                // is_subclass is reflexive: `a == b` is handled by the
                // first branch via `is_subclass(a, b)` being true.
                if is_subclass(a, b) {
                    TypeEstimate::Class(a)
                } else if is_subclass(b, a) {
                    TypeEstimate::Class(b)
                } else {
                    TypeEstimate::Bottom
                }
            }
            _ => TypeEstimate::Bottom,
        }
    }

    /// Sprint 15 load-bearing predicate for the dispatch resolver. True
    /// iff every concrete instance compatible with this estimate is
    /// `<: target`. For `Class(c)`, equivalent to "is `target` in `c`'s
    /// CPL?" (the caller supplies the `is_subclass` walk). For
    /// `Singleton(_)`, conservatively returns false (Sprint 15 doesn't
    /// know which class the Word belongs to). For immediate-kind
    /// estimates (`Integer`, `Boolean`, ...) the conversion to a
    /// `ClassId` is fixed; the caller is responsible for matching the
    /// corresponding seed-class id (`ClassId::INTEGER` etc.).
    pub fn is_subtype_of_class(
        self,
        target: u32,
        is_subclass: &dyn Fn(u32, u32) -> bool,
    ) -> bool {
        match self {
            TypeEstimate::Class(c) => is_subclass(c, target),
            TypeEstimate::Bottom => true, // vacuously
            _ => false,
        }
    }
}

impl Function {
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }

    pub fn temp(&self, id: TempId) -> Option<&Temporary> {
        self.temps.iter().find(|t| t.id == id)
    }

    pub fn temp_type(&self, id: TempId) -> TypeEstimate {
        self.temp(id)
            .map(|t| t.type_estimate)
            .unwrap_or(TypeEstimate::Top)
    }
}

impl Computation {
    pub fn dst(&self) -> TempId {
        match self {
            Computation::Const { dst, .. }
            | Computation::PrimOp { dst, .. }
            | Computation::DirectCall { dst, .. }
            | Computation::Call { dst, .. }
            | Computation::TypeCheck { dst, .. }
            | Computation::WriteBarrier { dst, .. }
            | Computation::LoadSlot { dst, .. }
            | Computation::StoreSlot { dst, .. }
            | Computation::Dispatch { dst, .. }
            | Computation::SealedDirectCall { dst, .. } => *dst,
        }
    }

    /// Returns this computation's `safepoint_roots` if it's a call-shaped
    /// node, else `None`. Sprint 11b precise-roots metadata.
    pub fn safepoint_roots(&self) -> Option<&[TempId]> {
        match self {
            Computation::DirectCall { safepoint_roots, .. }
            | Computation::Call { safepoint_roots, .. }
            | Computation::Dispatch { safepoint_roots, .. }
            | Computation::SealedDirectCall { safepoint_roots, .. } => Some(safepoint_roots),
            _ => None,
        }
    }

    /// Mutable accessor for `safepoint_roots`. Used by the liveness
    /// post-pass to populate the field after the initial lowering.
    pub fn safepoint_roots_mut(&mut self) -> Option<&mut Vec<TempId>> {
        match self {
            Computation::DirectCall { safepoint_roots, .. }
            | Computation::Call { safepoint_roots, .. }
            | Computation::Dispatch { safepoint_roots, .. }
            | Computation::SealedDirectCall { safepoint_roots, .. } => Some(safepoint_roots),
            _ => None,
        }
    }

    /// Returns the call argument list for call-shaped computations.
    /// `None` for non-call computations. Used by the
    /// `NOD_DIAG_ARG_ROOT_COVERAGE` probe to detect GAP-011-style
    /// staleness: a GC-typed argument that's passed to a potentially
    /// allocating callee but isn't tracked in `safepoint_roots`
    /// (because liveness only sees it as dead-after-call).
    ///
    /// Note: the `callee` of `Computation::Call` is NOT included here;
    /// that variant's callee is a TempId, not an arg. Callers who need
    /// the callee should special-case it.
    pub fn call_args(&self) -> Option<&[TempId]> {
        match self {
            Computation::DirectCall { args, .. }
            | Computation::Call { args, .. }
            | Computation::Dispatch { args, .. }
            | Computation::SealedDirectCall { args, .. } => Some(args),
            _ => None,
        }
    }

    /// True if this computation is a potentially-allocating call that
    /// needs GC root protection bracketing. Sprint 11b: `DirectCall`,
    /// `Call`, `Dispatch`, and Sprint 15's `SealedDirectCall` are the
    /// nodes that can lead to heap allocation (via `nod_make`, JIT'd
    /// Dylan bodies that internally allocate, or method bodies behind
    /// a dispatch).
    /// Sprint 48: returns false for `DirectCall` / `SealedDirectCall`
    /// where `is_no_alloc` is set, even though they're call-shaped.
    /// `Call` (computed callee) and `Dispatch` (runtime dispatch) are
    /// always treated as potentially allocating — the resolved callee
    /// isn't known at IR-build time, so we can't safely assert
    /// no-allocation.
    pub fn is_potentially_allocating_call(&self) -> bool {
        match self {
            Computation::DirectCall { is_no_alloc, .. } => !is_no_alloc,
            Computation::SealedDirectCall { is_no_alloc, .. } => !is_no_alloc,
            Computation::Call { .. } | Computation::Dispatch { .. } => true,
            _ => false,
        }
    }
}
