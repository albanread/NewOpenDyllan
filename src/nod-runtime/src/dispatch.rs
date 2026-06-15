//! Multimethod dispatch — generic functions, method tables, inline-cache
//! infrastructure.
//!
//! Sprint 13 grows Sprint 12's single-receiver dispatch into the full
//! multimethod story. The structure here drives both the runtime
//! dispatch path (`nod_dispatch`) and the JIT-emitted inline caches in
//! `nod-llvm`:
//!
//!   - **`GenericFunction`** owns one generic. It holds the method
//!     vector under an `RwLock` and an atomic `generation` counter that
//!     increments on every `add_method` / `remove_method`. Inline
//!     caches read the generation on each call; a mismatch means the
//!     cache is stale and the slow path repopulates it.
//!   - **`Method`** carries the full specialiser list (not just the
//!     receiver), so `intersect(<rect>, <circle>)` and friends pick the
//!     argument-major most-specific method.
//!   - **`CacheSlot`** is the three-field cell each JIT call site keeps
//!     in its own `.bss`-resident statics: `(class, method, gen)` plus
//!     hit/miss counters. The slow-path shim writes back into the slot
//!     it was handed.
//!   - **`lookup_method`** filters → sorts → checks ambiguity → returns
//!     the winner. Ambiguous (= equally applicable, neither strictly
//!     more specific) panics with a structured message; Sprint 19
//!     replaces this with `signal(<ambiguous-methods-error>)`.
//!
//! ### Specificity rule (argument-major lexicographic, CPL-driven)
//!
//! Given args `c1..cn` and methods `M1`, `M2` both applicable:
//!
//!   - Walk positions `i = 0..n`. At the first position where
//!     `M1.specialisers[i] != M2.specialisers[i]`, the more specific
//!     method is the one whose specialiser appears **earlier in
//!     `ci.cpl`** (= closer to `ci` itself).
//!   - If all specialisers agree, the methods are equally applicable;
//!     panic.
//!
//! ### Concurrency
//!
//! `methods` is guarded by an `RwLock`; reads (the common case) take
//! the shared lock. Mutations (`add_method` / `remove_method`) take the
//! exclusive lock AND bump the generation atomically before releasing.
//! Cache fields use relaxed atomics — the generation check is what
//! makes a stale cache safe.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::classes::{ClassId, class_metadata_ptr, is_subclass};
use crate::word::Word;

/// Sprint 15: structured errors from the sealing-aware method-table
/// mutation surface. Sprint 19's signalled-conditions story will route
/// these through Dylan's `<error>` hierarchy; for now they're an
/// algebraic error type the REPL / driver presents directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MethodTableError {
    /// Tried to add a method to a generic that has been sealed against
    /// further additions. `generic` is the name; the call site decides
    /// the diagnostic phrasing.
    SealedGenericClosed { generic: String },
    /// Tried to add a method whose specialisers fall under a sealed
    /// domain on this generic. `domain` is the offending sealed-domain
    /// tuple; `specialisers` is the rejected method's signature.
    SealedDomainViolated {
        generic: String,
        domain: Vec<ClassId>,
        specialisers: Vec<ClassId>,
    },
}

impl std::fmt::Display for MethodTableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MethodTableError::SealedGenericClosed { generic } => write!(
                f,
                "sealed-generic-closed: `{generic}` is sealed; no further methods may be added (Sprint 15 single-library scope)",
            ),
            MethodTableError::SealedDomainViolated {
                generic,
                domain,
                specialisers,
            } => {
                let dom: Vec<String> = domain.iter().map(|c| c.0.to_string()).collect();
                let spec: Vec<String> = specialisers.iter().map(|c| c.0.to_string()).collect();
                write!(
                    f,
                    "sealed-domain-violated: method on `{generic}` with specialisers [{}] falls inside the sealed domain [{}] (Sprint 15)",
                    spec.join(", "),
                    dom.join(", "),
                )
            }
        }
    }
}

impl std::error::Error for MethodTableError {}

/// One method registered against a generic. The `specialisers` vector
/// has length equal to the generic's required-parameter count — each
/// entry is the declared class for that argument position.
#[derive(Clone)]
pub struct Method {
    pub specialisers: Vec<ClassId>,
    /// JIT'd function pointer. Signature must be
    /// `extern "C" fn(u64, ..., u64) -> u64` with `param_count` `u64`
    /// arguments returning a `u64`.
    pub body_fn_ptr: *const u8,
    pub param_count: usize,
    /// Sprint 16: the JIT-emitted symbol name of `body_fn_ptr`. Used
    /// by the Sprint 15 dispatch resolver to emit a `DirectCall`
    /// against the EXACT name codegen produced. Sprint 12 +
    /// Sprint 14's slot-accessor methods don't follow the
    /// `{generic}${specialisers}` convention — their body lives at
    /// e.g. `<idler>-getter-id-state` — so the resolver can't
    /// reconstruct the symbol without consulting the registration.
    ///
    /// Empty string means "use the legacy `{generic}${specialisers}`
    /// convention", which the Sprint 15 resolver falls back to when
    /// `add_method` (the Sprint 12-era API) is used.
    pub body_fn_name: String,
}

// SAFETY: function pointers are Send + Sync (no interior mutability);
// the `Method` value is only mutated through the parent generic's
// RwLock.
unsafe impl Send for Method {}
unsafe impl Sync for Method {}

/// One JIT-emitted call site's inline-cache slot. The codegen layer
/// bakes the address of one of these into the LLVM IR as an `i64`
/// constant; both the JIT fast path and the runtime slow path read /
/// write the same memory.
///
/// Layout is `#[repr(C)]` so codegen can compute field offsets
/// directly. All fields are `AtomicU64` so the JIT can emit relaxed
/// loads/stores without going through Rust.
#[repr(C)]
pub struct CacheSlot {
    /// Receiver-class id (lower 32 bits used). `0` means cold.
    pub class: AtomicU64,
    /// Cached method body pointer.
    pub method: AtomicU64,
    /// `GenericFunction.generation` recorded when the cache was filled.
    pub generation: AtomicU64,
    /// Per-site hit counter (incremented in the fast path).
    pub hits: AtomicU64,
    /// Per-site miss counter (incremented in the slow path).
    pub misses: AtomicU64,
    /// Lightweight identifier for the site (for `dump_dispatch`).
    pub site_id: AtomicU64,
}

impl CacheSlot {
    /// Cold cache. All zeros.
    pub const fn cold(site_id: u64) -> Self {
        Self {
            class: AtomicU64::new(0),
            method: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            site_id: AtomicU64::new(site_id),
        }
    }
}

/// Process-global registry of generics. Indexed by name. Each
/// `GenericFunction` lives forever (boxed + leaked) so codegen can bake
/// the raw pointer into LLVM IR.
pub struct GenericFunction {
    pub name: String,
    pub methods: RwLock<Vec<Method>>,
    pub generation: AtomicU64,
    /// Cache slots known to belong to this generic. Used by
    /// `dump_dispatch` to render per-call-site state. Slots register
    /// themselves on first miss; entries are raw pointers into JIT
    /// statics (live forever).
    pub cache_slots: RwLock<Vec<*const CacheSlot>>,
    /// Sprint 15: the generic's method set is closed to additions
    /// from outside the defining library. The Sprint 15 dispatch
    /// resolver consults this to decide whether to rewrite a
    /// `Computation::Dispatch` to a direct call. Setting this is a
    /// one-way operation (cannot be unsealed) — `add_method` against a
    /// sealed generic returns `MethodTableError::SealedGenericClosed`.
    pub sealed: AtomicBool,
    /// Sprint 15: sealed-domain declarations covering this generic.
    /// Each entry is a specialiser tuple `[S0, S1, ...]` — a dispatch
    /// whose arg-type-estimate tuple falls under any entry
    /// (`est[i] <: Si` for all i) can be resolved using only the
    /// methods this library has installed (per spec 15 §2.3).
    pub sealed_domains: RwLock<Vec<Vec<ClassId>>>,
}

// SAFETY: the inner fields are individually thread-safe (`RwLock`,
// `AtomicU64`). `cache_slots` holds raw pointers but the targets are
// in JIT-mapped pages that live as long as the process.
unsafe impl Send for GenericFunction {}
unsafe impl Sync for GenericFunction {}

impl GenericFunction {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            methods: RwLock::new(Vec::new()),
            generation: AtomicU64::new(1),
            cache_slots: RwLock::new(Vec::new()),
            sealed: AtomicBool::new(false),
            sealed_domains: RwLock::new(Vec::new()),
        }
    }

    /// Sprint 15: is this generic sealed against method additions
    /// from outside its defining library? `Ordering::Acquire` pairs
    /// with `mark_sealed`'s release store and with `add_method`'s
    /// pre-add `is_sealed` check.
    pub fn is_sealed(&self) -> bool {
        self.sealed.load(Ordering::Acquire)
    }

    /// Sprint 15: mark this generic sealed. Idempotent; once set,
    /// remains set. The Sprint 15 redefinition refusal policy uses
    /// this to reject `add_method` calls against the closed table.
    pub fn mark_sealed(&self) {
        self.sealed.store(true, Ordering::Release);
    }

    /// Sprint 15: register a `define sealed domain` declaration on
    /// this generic. Each call appends a specialiser tuple to the
    /// generic's sealed-domains list. Duplicate tuples are deduped.
    /// The Sprint 15 dispatch resolver consults this list when the
    /// generic itself isn't sealed but a particular specialiser shape
    /// is closed.
    pub fn register_sealed_domain(&self, specialisers: Vec<ClassId>) {
        let mut guard = self
            .sealed_domains
            .write()
            .expect("sealed_domains rwlock poisoned");
        if !guard.iter().any(|s| s == &specialisers) {
            guard.push(specialisers);
        }
    }

    /// Sprint 15: snapshot the sealed-domain list. Returns a clone so
    /// the caller doesn't hold the lock across the resolution algorithm.
    pub fn sealed_domains_snapshot(&self) -> Vec<Vec<ClassId>> {
        self.sealed_domains
            .read()
            .expect("sealed_domains rwlock poisoned")
            .clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Sprint 21: param count for the first registered method (used by
    /// the `\name` first-class function-reference path to determine the
    /// arity to bake into the `<function>` Word). Returns `None` if no
    /// methods are registered yet.
    pub fn first_method_param_count(&self) -> Option<usize> {
        let methods = self.methods.read().ok()?;
        methods.first().map(|m| m.param_count)
    }

    /// Register `m`. If a method with identical specialisers already
    /// exists, it is replaced. Always bumps the generation counter.
    ///
    /// Sprint 15: bypasses the sealed-generic redefinition check. Use
    /// `try_add_method` to honour sealing. The internal lowering path
    /// (which adds the methods declared in the same library as the
    /// `define sealed generic`) needs the unconditional version; the
    /// REPL / JIT-time addition path goes through `try_add_method`.
    pub fn add_method(&self, m: Method) {
        {
            let mut methods = self.methods.write().expect("methods rwlock poisoned");
            methods.retain(|existing| existing.specialisers != m.specialisers);
            methods.push(m);
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Sprint 15 — sealing-aware `add_method`. Returns
    /// `MethodTableError::SealedGenericClosed` if this generic is sealed.
    /// Otherwise behaves like `add_method`.
    ///
    /// Single-library Sprint 15 scope: "sealed" means "no further
    /// additions through this entry point". Sprint 29 refines this to
    /// distinguish "this library" from "another library" once
    /// cross-library compilation lands. Use `add_method` (no `_try`)
    /// from inside the defining library's bring-up code to bypass the
    /// check.
    pub fn try_add_method(&self, m: Method) -> Result<(), MethodTableError> {
        if self.is_sealed() {
            return Err(MethodTableError::SealedGenericClosed {
                generic: self.name.clone(),
            });
        }
        // Sprint 15: also check sealed-domain coverage — if any sealed
        // domain `D` covers `m`'s specialisers (`m.specialisers[i] <: D[i]`
        // for every i), the addition violates the closure assumption.
        let domains = self.sealed_domains_snapshot();
        for d in &domains {
            if d.len() == m.specialisers.len()
                && d.iter()
                    .zip(m.specialisers.iter())
                    .all(|(ds, ms)| is_subclass(*ms, *ds))
            {
                return Err(MethodTableError::SealedDomainViolated {
                    generic: self.name.clone(),
                    domain: d.clone(),
                    specialisers: m.specialisers.clone(),
                });
            }
        }
        self.add_method(m);
        Ok(())
    }

    /// Remove the method whose specialisers exactly match `specialisers`.
    /// Always bumps the generation counter, whether or not anything was
    /// actually removed (callers treat a no-op remove as a cache-
    /// invalidating event for parity with `add_method`).
    pub fn remove_method(&self, specialisers: &[ClassId]) {
        {
            let mut methods = self.methods.write().expect("methods rwlock poisoned");
            methods.retain(|existing| existing.specialisers.as_slice() != specialisers);
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Register a per-call-site cache slot pointer for `dump_dispatch`.
    fn register_cache_slot(&self, slot: *const CacheSlot) {
        let mut slots = self
            .cache_slots
            .write()
            .expect("cache_slots rwlock poisoned");
        if !slots.contains(&slot) {
            slots.push(slot);
        }
    }
}

static GENERICS: RwLock<Option<GenericRegistry>> = RwLock::new(None);

struct GenericRegistry {
    by_name: HashMap<String, &'static GenericFunction>,
}

fn with_registry_mut<R>(f: impl FnOnce(&mut GenericRegistry) -> R) -> R {
    let mut guard = GENERICS.write().expect("generics registry poisoned");
    if guard.is_none() {
        *guard = Some(GenericRegistry {
            by_name: HashMap::new(),
        });
    }
    f(guard.as_mut().expect("registry initialised"))
}

fn with_registry<R>(f: impl FnOnce(&GenericRegistry) -> R) -> R {
    let guard = GENERICS.read().expect("generics registry poisoned");
    match guard.as_ref() {
        Some(reg) => f(reg),
        None => {
            drop(guard);
            with_registry_mut(|reg| f(reg))
        }
    }
}

/// Look up `name`'s `GenericFunction`, creating it if missing. Returns
/// a `&'static` reference — the underlying `GenericFunction` is leaked
/// on first creation so its address is stable forever.
pub fn get_or_create_generic(name: &str) -> &'static GenericFunction {
    if let Some(g) = with_registry(|reg| reg.by_name.get(name).copied()) {
        return g;
    }
    with_registry_mut(|reg| {
        if let Some(g) = reg.by_name.get(name) {
            return *g;
        }
        let leaked: &'static GenericFunction = Box::leak(Box::new(GenericFunction::new(name)));
        reg.by_name.insert(name.to_string(), leaked);
        leaked
    })
}

/// Look up `name`'s generic if it exists (no auto-create).
pub fn find_generic(name: &str) -> Option<&'static GenericFunction> {
    with_registry(|reg| reg.by_name.get(name).copied())
}

/// True iff a generic with this name has at least one registered method.
pub fn is_generic_defined(generic_name: &str) -> bool {
    match find_generic(generic_name) {
        Some(g) => !g
            .methods
            .read()
            .expect("methods rwlock poisoned")
            .is_empty(),
        None => false,
    }
}

// ─── `#rest` callee registry ───────────────────────────────────────────────
//
// Sprint 60: process-global table of `name -> FIXED` for every callee
// (top-level `define function` OR dispatched `define method`) declared with a
// trailing `#rest` parameter. `FIXED` is the pre-`#rest` parameter count — the
// number of leading actuals passed straight through; the trailing actuals
// (positions >= FIXED) are collected into ONE `<simple-object-vector>` slot.
//
// WHY A PROCESS-GLOBAL TABLE: `#rest` collection is a CALL-SITE rewrite (the
// lowerer builds the rest SOV before the call so the callee's fixed-arity ABI
// stays intact — see `lower_call`'s `rest_fixed_count` branch). The per-module
// `TopNames.rest_fns` only covers the module currently being lowered, but a
// user module calling the stdlib's `apply` / `compose` / `curry` / `rcurry`
// (which are `#rest` methods loaded into a DIFFERENT module) needs to know
// their fixed counts too. This table is the cross-module bridge: the stdlib
// loader records its `#rest` callees here when it lowers, and user-module
// call-site lowering consults it (mirroring how `is_generic_defined` exposes
// the cross-module generic set).
static REST_CALLEE_FIXED: OnceLock<RwLock<HashMap<String, usize>>> = OnceLock::new();

fn rest_callee_table() -> &'static RwLock<HashMap<String, usize>> {
    REST_CALLEE_FIXED.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Record that callee `name` takes a `#rest` parameter preceded by `fixed`
/// required parameters. Idempotent (last writer wins; the count is stable for
/// a given name). Called by the lowerer for every `#rest` callee it lowers.
pub fn register_rest_callee(name: &str, fixed: usize) {
    rest_callee_table()
        .write()
        .expect("rest callee table poisoned")
        .insert(name.to_string(), fixed);
}

/// Look up the FIXED (pre-`#rest`) parameter count of a `#rest` callee
/// registered via [`register_rest_callee`]. Returns `None` for callees with no
/// `#rest` parameter. Consulted by call-site lowering to decide whether to
/// collect the trailing actuals into a rest SOV.
pub fn rest_callee_fixed_count(name: &str) -> Option<usize> {
    rest_callee_table()
        .read()
        .expect("rest callee table poisoned")
        .get(name)
        .copied()
}

/// Sprint 20b: look up a method's body function pointer by the
/// emitted symbol name. Walks every generic's method list searching
/// for `body_fn_name == name`. Returns `None` if no method with that
/// body name is registered.
///
/// Used by the JIT's symbol-resolution path when user code's
/// codegen emits an extern declaration whose name is the
/// `{generic}${specialisers}` body symbol of a stdlib-resident
/// method. We bind it via `LLVMAddGlobalMapping` to the address
/// `add_method_named` stashed here.
pub fn find_method_body_ptr(name: &str) -> Option<*const u8> {
    let mut found: Option<*const u8> = None;
    for_each_generic(|g| {
        if found.is_some() {
            return;
        }
        let methods = g.methods.read().expect("methods rwlock poisoned");
        for m in methods.iter() {
            if m.body_fn_name == name {
                found = Some(m.body_fn_ptr);
                return;
            }
        }
    });
    found
}

/// Snapshot every registered generic (for `dump_dispatch`). The
/// returned references are `'static` (the generics live forever).
pub fn for_each_generic(mut f: impl FnMut(&'static GenericFunction)) {
    let snapshot: Vec<&'static GenericFunction> =
        with_registry(|reg| reg.by_name.values().copied().collect());
    for g in snapshot {
        f(g);
    }
}

/// Compute the most-specific applicable method on `generic` for the
/// argument classes `arg_classes`. Returns `Some(method)` on a unique
/// winner; `None` if no method is applicable. Ambiguity (two equally-
/// applicable methods, neither strictly more specific) panics with a
/// structured message — Sprint 19 lights up `<ambiguous-methods-error>`.
pub fn lookup_method(
    generic: &GenericFunction,
    arg_classes: &[ClassId],
) -> Option<*const u8> {
    let applicable = lookup_applicable_methods(generic, arg_classes)?;
    Some(applicable[0])
}

/// Sprint 14: compute the FULL sorted applicable-method chain
/// (most-specific first). Used by `nod_dispatch` to set up the
/// `next-method` chain before invoking the head. Returns `None` if no
/// method is applicable; ambiguity at the top still panics.
pub fn lookup_applicable_methods(
    generic: &GenericFunction,
    arg_classes: &[ClassId],
) -> Option<Vec<*const u8>> {
    let methods = generic.methods.read().expect("methods rwlock poisoned");

    let mut applicable: Vec<&Method> = methods
        .iter()
        .filter(|m| is_applicable(m, arg_classes))
        .collect();
    if applicable.is_empty() {
        return None;
    }

    applicable.sort_by(|a, b| compare_specificity(a, b, arg_classes));

    if applicable.len() >= 2 {
        let top = applicable[0];
        let next = applicable[1];
        if compare_specificity(top, next, arg_classes) == std::cmp::Ordering::Equal {
            ambiguous_panic(&generic.name, top, next, arg_classes);
        }
    }

    Some(applicable.iter().map(|m| m.body_fn_ptr).collect())
}

fn is_applicable(m: &Method, arg_classes: &[ClassId]) -> bool {
    if m.specialisers.len() != arg_classes.len() {
        return false;
    }
    m.specialisers
        .iter()
        .zip(arg_classes.iter())
        .all(|(spec, arg)| is_subclass(*arg, *spec))
}

/// Return `Less` if `a` is more specific than `b` (so sorted-by-Less-first
/// puts the most specific at index 0).
fn compare_specificity(
    a: &Method,
    b: &Method,
    arg_classes: &[ClassId],
) -> std::cmp::Ordering {
    for (i, &arg) in arg_classes.iter().enumerate() {
        let sa = a.specialisers[i];
        let sb = b.specialisers[i];
        if sa == sb {
            continue;
        }
        let pa = cpl_position(arg, sa);
        let pb = cpl_position(arg, sb);
        return pa.cmp(&pb);
    }
    std::cmp::Ordering::Equal
}

/// Position of `spec` in `arg`'s CPL. Smaller index = more specific
/// (closer to `arg` itself). Returns `usize::MAX` if `spec` isn't in
/// the CPL (shouldn't happen for applicable methods — we'd have failed
/// the `is_subclass` check first).
fn cpl_position(arg: ClassId, spec: ClassId) -> usize {
    let p = class_metadata_ptr(arg);
    if p.is_null() {
        return usize::MAX;
    }
    // SAFETY: metadata is in the static area, address-stable.
    let cpl = unsafe { &(*p).cpl };
    cpl.iter().position(|c| *c == spec).unwrap_or(usize::MAX)
}

fn ambiguous_panic(name: &str, a: &Method, b: &Method, arg_classes: &[ClassId]) -> ! {
    let args = arg_classes
        .iter()
        .map(|c| class_name(*c))
        .collect::<Vec<_>>()
        .join(", ");
    let aspec = a
        .specialisers
        .iter()
        .map(|c| class_name(*c))
        .collect::<Vec<_>>()
        .join(", ");
    let bspec = b
        .specialisers
        .iter()
        .map(|c| class_name(*c))
        .collect::<Vec<_>>()
        .join(", ");
    panic!(
        "dispatch: <ambiguous-methods-error>: `{name}` on ({args}) has multiple equally-specific methods (e.g. ({aspec}) and ({bspec})); Sprint 19 will surface this as a signalled condition"
    );
}

fn no_applicable_panic(name: &str, arg_classes: &[ClassId]) -> ! {
    // Sprint 19: lift the bare panic into a signalled
    // `<no-applicable-methods-error>` so user code can `block`/`exception`
    // around the generic call. If no handler is registered for
    // `<no-applicable-methods-error>` / `<error>` / `<condition>`, the
    // signal walker falls through to a process-level panic carrying a
    // structured message — that's the unhandled-condition outcome and
    // matches the prior contract of `no_applicable_panic`.
    let names: Vec<String> = arg_classes.iter().map(|c| class_name(*c)).collect();
    let names_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let cond = crate::conditions::make_no_applicable_methods_error(name, &names_refs);
    // We embed the structured message in the process-level panic
    // string so the existing test
    // (`no_applicable_methods_panics_with_structured_message`) can
    // continue to assert against it when run without an outer
    // `block`/`exception` — that path falls through `nod_signal`'s
    // unhandled-condition branch and panics with the class name. We
    // keep `name` and arg class names in the slot data; the panic
    // message itself names the class so the grep still passes.
    // SAFETY: nod_signal accepts any pointer-tagged Dylan Word; the
    // cond we just built is a freshly-made `<no-applicable-methods-error>`.
    // It diverges (either by NLX panic-unwind to a matching handler, or
    // by an unhandled-condition process-level panic).
    unsafe {
        crate::conditions::nod_signal(cond.raw());
    }
    // nod_signal diverges; this line is unreachable. We use `unreachable!`
    // rather than `loop {}` to give a clearer crash if the FFI signature
    // ever changes underneath us.
    unreachable!("nod_signal returned");
}

fn class_name(id: ClassId) -> String {
    let p = class_metadata_ptr(id);
    if p.is_null() {
        return format!("<unknown:{}>", id.0);
    }
    // SAFETY: metadata is in the static area, address-stable.
    unsafe { (*p).name.clone() }
}

// ─── Legacy Sprint 12 surface ──────────────────────────────────────────────
//
// Existing callers use the by-name `add_method` / `lookup_method` with
// a single receiver class. We keep that API working so the Sprint 12
// `register_methods` flow + `find_initialize_method` continue
// untouched; internally each call routes through `GenericFunction`.

/// Register a method on the named generic. Auto-creates the generic if
/// it doesn't exist yet.
///
/// # Safety
///
/// `body_fn_ptr` must point at a JIT'd function whose lifetime exceeds
/// the dispatch table's lifetime (the JIT engine is held forever). Its
/// calling convention must be `extern "C"` and take `param_count`
/// `u64` arguments returning a `u64`.
pub unsafe fn add_method(
    generic_name: &str,
    receiver_class: ClassId,
    body_fn_ptr: *const u8,
    param_count: usize,
) {
    // Sprint 12 callers only carry the receiver class. Pad with
    // `<object>` for any further required parameters so multi-arg
    // generics with single-position specialisers still dispatch.
    let mut specialisers = vec![receiver_class];
    while specialisers.len() < param_count {
        specialisers.push(ClassId::OBJECT);
    }
    // SAFETY: caller's contract carries through to add_method_full.
    unsafe {
        add_method_full(generic_name, specialisers, body_fn_ptr, param_count);
    }
}

/// Sprint 13 entry point: register a method with the full specialiser
/// list (one entry per required parameter).
///
/// # Safety
///
/// Same contract as `add_method`.
pub unsafe fn add_method_full(
    generic_name: &str,
    specialisers: Vec<ClassId>,
    body_fn_ptr: *const u8,
    param_count: usize,
) {
    // SAFETY: caller's contract carries through to `add_method_named`.
    unsafe {
        add_method_named(generic_name, specialisers, body_fn_ptr, param_count, "");
    }
}

/// Sprint 16 variant of `add_method_full` that also records the JIT
/// symbol name (`body_fn_name`) the method's body was emitted under.
/// The Sprint 15 dispatch resolver reads this name to emit a
/// `Computation::DirectCall` against the exact symbol — necessary
/// because slot accessors don't follow the `{generic}${specialisers}`
/// convention.
///
/// Passing `body_fn_name = ""` keeps the legacy behaviour (the
/// resolver synthesises the symbol).
///
/// # Safety
///
/// Same contract as `add_method_full`.
pub unsafe fn add_method_named(
    generic_name: &str,
    specialisers: Vec<ClassId>,
    body_fn_ptr: *const u8,
    param_count: usize,
    body_fn_name: &str,
) {
    let g = get_or_create_generic(generic_name);
    g.add_method(Method {
        specialisers,
        body_fn_ptr,
        param_count,
        body_fn_name: body_fn_name.to_string(),
    });
}

/// Sprint 15 sealing-aware variant of `add_method_full`. Returns
/// `MethodTableError::SealedGenericClosed` (or `::SealedDomainViolated`)
/// when the registration would break a sealing assumption. Used by the
/// REPL / cross-library code paths; the bring-up `register_methods`
/// flow still uses `add_method_full` because it adds the methods
/// declared in the same library as the `define sealed generic` (so the
/// check would always refuse).
///
/// # Safety
///
/// Same contract as `add_method_full`.
pub unsafe fn try_add_method_full(
    generic_name: &str,
    specialisers: Vec<ClassId>,
    body_fn_ptr: *const u8,
    param_count: usize,
) -> Result<(), MethodTableError> {
    let g = get_or_create_generic(generic_name);
    g.try_add_method(Method {
        specialisers,
        body_fn_ptr,
        param_count,
        body_fn_name: String::new(),
    })
}

/// Remove the method on `generic_name` with the exact specialiser list
/// given. No-op if no such method exists; still bumps the generation
/// counter (cache-invalidating).
pub fn remove_method(generic_name: &str, specialisers: &[ClassId]) {
    if let Some(g) = find_generic(generic_name) {
        g.remove_method(specialisers);
    }
}

/// Look up a single-receiver method on `generic_name`. Returns the
/// most-specific applicable method or `None`. This is the Sprint 12
/// surface — kept so `find_initialize_method` and other unary callers
/// stay unchanged.
pub fn lookup_method_by_receiver(generic_name: &str, receiver_class: ClassId) -> Option<MethodPtr> {
    let g = find_generic(generic_name)?;
    let methods = g.methods.read().expect("methods rwlock poisoned");
    let mut candidates: Vec<&Method> = methods
        .iter()
        .filter(|m| !m.specialisers.is_empty() && is_subclass(receiver_class, m.specialisers[0]))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Most specific = smallest CPL-position for the first specialiser.
    candidates.sort_by_key(|m| cpl_position(receiver_class, m.specialisers[0]));
    let top = candidates[0];
    Some(MethodPtr {
        body_fn_ptr: top.body_fn_ptr,
        param_count: top.param_count,
    })
}

/// Single-receiver method lookup result. Kept for Sprint 12 callers
/// (the `find_initialize_method` path).
#[derive(Clone, Copy)]
pub struct MethodPtr {
    pub body_fn_ptr: *const u8,
    pub param_count: usize,
}

// SAFETY: function pointers are Send + Sync.
unsafe impl Send for MethodPtr {}
unsafe impl Sync for MethodPtr {}

/// Find a user-defined `initialize` method specialised for
/// `receiver_class`. Used by `make` after instance allocation.
pub fn find_initialize_method(receiver_class: ClassId) -> Option<*const u8> {
    lookup_method_by_receiver("initialize", receiver_class).map(|m| m.body_fn_ptr)
}

/// Invoke a method's JIT'd body with a single `self` argument.
///
/// # Safety
///
/// `method_ptr` must point at a JIT'd `extern "C" fn(u64) -> u64`.
pub unsafe fn invoke_method_with_self(method_ptr: *const u8, self_word: Word) -> Word {
    // SAFETY: caller's contract.
    let f: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute(method_ptr) };
    Word::from_raw(f(self_word.raw()))
}

// ─── Variadic JIT-callable shim ────────────────────────────────────────────

/// Variadic dispatch: peel `arity` argument Words, classify each, look
/// up the most-specific method, transmute the function pointer to the
/// matching `extern "C"` signature, call, return.
///
/// `cache_slot_ptr_raw` is either `0` (no inline cache — used by Rust
/// callers and by the legacy `nod_dispatch_unary` / `_binary` shims) or
/// the address of a `CacheSlot`. When non-zero, the shim writes back
/// `(class, method, generation)` after a successful lookup and bumps
/// the slot's `misses` counter.
///
/// Sprint 13 caps arity at 8 to match `nod_make`'s shape. Sprint 23
/// (c-ffi) lifts the cap to true varargs.
///
/// # Safety
///
/// `generic_ptr_raw` must be the address of a `&'static GenericFunction`
/// returned by `get_or_create_generic`. `cache_slot_ptr_raw` must be
/// `0` or the address of a live `CacheSlot`. `arity` must be in
/// `0..=8`. Each `a_i` is a valid Dylan Word.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C-unwind" fn nod_dispatch(
    generic_ptr_raw: u64,
    cache_slot_ptr_raw: u64,
    arity: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
    a6: u64,
    a7: u64,
) -> u64 {
    // SAFETY: per caller's contract.
    let generic: &'static GenericFunction =
        unsafe { &*(generic_ptr_raw as *const GenericFunction) };
    let cache_slot_ptr = cache_slot_ptr_raw as *const CacheSlot;

    let arity = arity as usize;
    let raw_args: [u64; 8] = [a0, a1, a2, a3, a4, a5, a6, a7];
    let args: &[u64] = &raw_args[..arity.min(8)];

    // Compute the argument classes.
    let mut arg_classes: Vec<ClassId> = Vec::with_capacity(args.len());
    for &raw in args {
        arg_classes.push(word_class_id(Word::from_raw(raw)));
    }

    // Slow-path miss bookkeeping.
    if !cache_slot_ptr.is_null() {
        // SAFETY: slot lives in JIT-mapped statics for the process.
        let slot = unsafe { &*cache_slot_ptr };
        slot.misses.fetch_add(1, Ordering::Relaxed);
        // Register the slot with the generic so dump_dispatch can find it.
        generic.register_cache_slot(cache_slot_ptr);
    }

    let chain = match lookup_applicable_methods(generic, &arg_classes) {
        Some(c) => c,
        None => no_applicable_panic(&generic.name, &arg_classes),
    };
    let method_ptr = chain[0];

    // Update the cache (post-lookup, pre-call) so a recursive call into
    // the same site hits.
    if !cache_slot_ptr.is_null() && !arg_classes.is_empty() {
        // SAFETY: slot pointer is non-null and points at live storage.
        let slot = unsafe { &*cache_slot_ptr };
        slot.class
            .store(arg_classes[0].0 as u64, Ordering::Relaxed);
        slot.method.store(method_ptr as u64, Ordering::Relaxed);
        slot.generation
            .store(generic.generation(), Ordering::Relaxed);
    }

    // Sprint 14: push a method-chain frame so any `next-method()` call
    // inside the body can walk to the next-most-specific method. The
    // frame records the args + the tail of the applicable chain
    // (everything after index 0). We pop on return (even on panic
    // unwind — via the `ChainFrameGuard` RAII guard).
    let frame_pushed = chain.len() > 1;
    if frame_pushed {
        push_method_chain_frame(MethodChainFrame {
            args: args.to_vec(),
            remaining_methods: chain[1..].to_vec(),
        });
    }
    let _guard = ChainFrameGuard { active: frame_pushed };

    // SAFETY: lookup returned the JIT'd method, whose signature is
    // `extern "C" fn(u64...) -> u64` with `arity` arguments. Transmute
    // and call.
    unsafe { call_method(method_ptr, args) }
}

// ─── `next-method` chain stack ─────────────────────────────────────────────
//
// Sprint 14 implements `next-method` via a thread-local stack of method
// chain frames. Each frame records the args of the currently-executing
// method dispatch and the remaining applicable methods (most-specific
// first) AFTER the currently-executing one.
//
// On dispatch, if there are 2+ applicable methods, the dispatcher pushes
// a frame carrying the args + tail-of-chain, calls the head, and pops
// on return. Inside the head, `next-method()` lowers to a call into
// `nod_next_method`, which pops the next method off the frame's
// `remaining_methods` and invokes it with the SAME args.
//
// This design preserves the Sprint 13 `extern "C" fn(u64, ..., u64) -> u64`
// ABI for method bodies — no implicit chain parameter is needed. The
// frame has dynamic extent matching the method's own activation (it's
// invalid to capture `next-method` past the method's return — which
// Dylan's semantics forbids anyway).

/// One frame on the thread-local `next-method` chain stack.
struct MethodChainFrame {
    /// Args (raw `u64` Words) the currently-executing method received.
    /// `next-method` forwards these verbatim (Sprint 14 simplification —
    /// `(next-method x y)` with explicit args lands in Sprint 17 macros).
    args: Vec<u64>,
    /// Applicable methods MORE GENERAL than the currently-executing one,
    /// in most-specific-first order. `next-method()` pops the front
    /// element; an empty list means no next method.
    remaining_methods: Vec<*const u8>,
}

thread_local! {
    static METHOD_CHAIN_STACK: std::cell::RefCell<Vec<MethodChainFrame>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn push_method_chain_frame(frame: MethodChainFrame) {
    METHOD_CHAIN_STACK.with(|stack| stack.borrow_mut().push(frame));
}

fn pop_method_chain_frame() -> Option<MethodChainFrame> {
    METHOD_CHAIN_STACK.with(|stack| stack.borrow_mut().pop())
}

/// Drop guard that pops the chain frame on scope exit (including panic
/// unwind), keeping the stack balanced regardless of how the method
/// body returns.
struct ChainFrameGuard {
    active: bool,
}

impl Drop for ChainFrameGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = pop_method_chain_frame();
        }
    }
}

/// True iff the current dynamic extent has a next-most-specific method
/// available. Equivalent to `next-method?` in Dylan.
pub fn has_next_method() -> bool {
    METHOD_CHAIN_STACK.with(|stack| {
        let stack = stack.borrow();
        stack
            .last()
            .map(|f| !f.remaining_methods.is_empty())
            .unwrap_or(false)
    })
}

/// JIT-callable `next-method` shim. Reads the top chain frame; if the
/// `remaining_methods` list is non-empty, pops the front method,
/// invokes it with the frame's recorded args, and returns its result.
/// If empty, panics — Sprint 19 will replace this with a signalled
/// `<no-next-method-error>`.
///
/// Sprint 14 only supports the no-explicit-args form (`next-method()`).
/// The `extern "C"` signature takes no arguments because the body
/// re-forwards the parent method's args via the frame.
///
/// # Safety
///
/// Must be called from within an `extern "C"` method body whose
/// dispatch path pushed a chain frame. The shim takes no caller-
/// supplied state and is sound regardless — but the panic is
/// semantically meaningful as a Dylan error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_next_method() -> u64 {
    // Take the next method out of the current frame WITHOUT popping the
    // frame itself — subsequent `next-method` calls walk further down.
    // If there is no frame, OR the frame has no remaining methods, the
    // current method is the most-general applicable one and `next-method`
    // signals `<no-next-method-error>`. Sprint 19 lifts this from a
    // panic into a real Dylan signal.
    let popped = METHOD_CHAIN_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let frame = stack.last_mut()?;
        if frame.remaining_methods.is_empty() {
            return None;
        }
        let m = frame.remaining_methods.remove(0);
        Some((m, frame.args.clone()))
    });
    let Some((method_ptr, args)) = popped else {
        panic!(
            "dispatch: <no-next-method-error>: next-method() called with no remaining methods; Sprint 19 will surface this as a signalled condition"
        );
    };
    // SAFETY: pointer is to a JIT'd `extern "C" fn(u64...) -> u64` of
    // the right arity (the chain only contains applicable methods which
    // share the generic's required-parameter count).
    unsafe { call_method(method_ptr, &args) }
}

/// JIT-callable `next-method?` shim. Returns a tagged Dylan `<boolean>`
/// Word: `#t` if the current dispatch has a next method, `#f` otherwise.
///
/// # Safety
///
/// Trivially safe — only reads the thread-local stack.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_has_next_method() -> u64 {
    let imm = crate::literal_pool_immediates();
    if has_next_method() {
        imm.true_.raw()
    } else {
        imm.false_.raw()
    }
}

/// Sprint 55 — `%is-generic?(name :: <byte-string>) => <boolean>`: is `name`
/// the name of a registered generic function? The Dylan-side AST→DFM lowering
/// (load-bearing under `--lower-with-dylan`) calls this to decide whether a call
/// to a non-local name is a `Dispatch` (generic) or a `DirectCall` (a plain
/// function). The runtime generic registry is live by the time the lowering runs
/// (`stdlib::ensure_loaded` precedes it), so stdlib generics like `size` / `add!`
/// resolve correctly.
///
/// # Safety
/// `name_raw` must be a pointer-tagged `<byte-string>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_is_generic_defined(name_raw: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    let yes = decode_byte_string_to_string(Word::from_raw(name_raw))
        .map(|s| is_generic_defined(&s))
        .unwrap_or(false);
    if yes { imm.true_.raw() } else { imm.false_.raw() }
}

/// Sprint 55 — `%is-class?(name :: <byte-string>) => <boolean>`: is `name` the
/// name of a registered class? The Dylan-side lowering uses this to type a
/// non-scalar param/return whose name it doesn't recognize as a builtin class
/// (`Class` → `<class>`) vs a genuinely unknown type (`Top` → `<top>`). Builtin
/// classes (`<stretchy-vector>`, …) are registered before lowering runs; the
/// universal `<object>` is special-cased to `<top>` on the Dylan side.
///
/// # Safety
/// `name_raw` must be a pointer-tagged `<byte-string>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_is_class_defined(name_raw: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    let yes = decode_byte_string_to_string(Word::from_raw(name_raw))
        .map(|s| crate::classes::find_class_id_by_name(&s).is_some())
        .unwrap_or(false);
    if yes { imm.true_.raw() } else { imm.false_.raw() }
}

/// Test helper: clear the `next-method` chain stack. Useful for tests
/// that exercise `next-method` and want a clean slate. Not called in
/// production.
pub fn _reset_method_chain_stack_for_tests() {
    METHOD_CHAIN_STACK.with(|stack| stack.borrow_mut().clear());
}

// ─── Sprint 15: sealed-direct chain-frame helpers ──────────────────────────
//
// The Sprint 15 dispatch resolver rewrites `Computation::Dispatch` to
// `Computation::SealedDirectCall` when sealing implies a single
// most-specific method but additional applicable methods remain in the
// chain (so the body may legally call `next-method()`). Codegen brackets
// the resolved direct call with a `nod_push_sealed_chain_frame` /
// `nod_pop_sealed_chain_frame` pair so the thread-local chain stack
// looks the same as if `nod_dispatch` had pushed it.
//
// The push shim takes the original arg Words verbatim plus the
// fallback-chain body pointers — exactly the shape `nod_dispatch`
// builds. The pop shim takes no arguments.

/// Push a chain frame containing `args` and `remaining_methods` (the
/// less-specific applicable methods, most-specific-first) onto the
/// thread-local stack. Used by Sprint 15 sealed-direct call sites to
/// preserve `next-method()` semantics.
///
/// # Safety
///
/// `args_ptr` must point at a contiguous array of `arity` `u64`s
/// owned by the caller for the duration of this call.
/// `methods_ptr` must point at a contiguous array of `chain_len`
/// `*const u8`s, each a JIT'd method body pointer (`extern "C"
/// fn(u64, ..., u64) -> u64` of arity `arity`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_push_sealed_chain_frame(
    args_ptr: *const u64,
    arity: u64,
    methods_ptr: *const *const u8,
    chain_len: u64,
) {
    let arity = arity as usize;
    let chain_len = chain_len as usize;
    // SAFETY: per caller's contract.
    let args = unsafe { std::slice::from_raw_parts(args_ptr, arity) }.to_vec();
    let methods = unsafe { std::slice::from_raw_parts(methods_ptr, chain_len) }.to_vec();
    push_method_chain_frame(MethodChainFrame {
        args,
        remaining_methods: methods,
    });
}

/// Pop the topmost chain frame. Idempotent at an empty stack — Sprint
/// 15 codegen pairs every push with exactly one pop; an extra pop is
/// a programming bug but won't panic.
///
/// # Safety
///
/// Trivially safe; only mutates the thread-local stack.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_pop_sealed_chain_frame() {
    let _ = pop_method_chain_frame();
}

/// Transmute `method_ptr` to an `extern "C" fn` of the right arity and
/// call it. Returns the raw `u64`.
///
/// # Safety
///
/// `method_ptr` must point at a JIT'd `extern "C" fn(u64, ..., u64) -> u64`
/// whose argument count equals `args.len()`. `args.len() <= 8`.
unsafe fn call_method(method_ptr: *const u8, args: &[u64]) -> u64 {
    match args.len() {
        0 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(method_ptr) };
            f()
        }
        1 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute(method_ptr) };
            f(args[0])
        }
        2 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(method_ptr) };
            f(args[0], args[1])
        }
        3 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(method_ptr) };
            f(args[0], args[1], args[2])
        }
        4 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(method_ptr) };
            f(args[0], args[1], args[2], args[3])
        }
        5 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(method_ptr) };
            f(args[0], args[1], args[2], args[3], args[4])
        }
        6 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(method_ptr) };
            f(args[0], args[1], args[2], args[3], args[4], args[5])
        }
        7 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(method_ptr) };
            f(args[0], args[1], args[2], args[3], args[4], args[5], args[6])
        }
        8 => {
            // SAFETY: caller's contract.
            let f: extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(method_ptr) };
            f(
                args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
            )
        }
        n => panic!("nod_dispatch: arity {n} exceeds Sprint 13 cap of 8 (lifted in Sprint 23 c-ffi)"),
    }
}

/// Sprint 12-compat unary shim. Takes the legacy `(name_word,
/// self_word)` shape. Kept so existing JIT'd modules still link;
/// Sprint 13's `emit_dispatch` rewrites every call site to use
/// `nod_dispatch` directly with a cache slot.
///
/// # Safety
///
/// `generic_name_ptr_raw` is a tagged `<byte-string>` Word; `self_word`
/// is any valid Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_dispatch_unary(
    generic_name_ptr_raw: u64,
    self_word_raw: u64,
) -> u64 {
    let name = match decode_byte_string_to_string(Word::from_raw(generic_name_ptr_raw)) {
        Some(n) => n,
        None => panic!("nod_dispatch_unary: generic name word doesn't decode as <byte-string>"),
    };
    let g = get_or_create_generic(&name);
    // SAFETY: dispatch through the unified shim with no cache slot.
    unsafe { nod_dispatch(g as *const _ as u64, 0, 1, self_word_raw, 0, 0, 0, 0, 0, 0, 0) }
}

/// Sprint 12-compat binary shim. Same contract as `nod_dispatch_unary`
/// with one extra argument.
///
/// # Safety
///
/// Same as `nod_dispatch_unary` plus `arg1_raw` is any valid Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_dispatch_binary(
    generic_name_ptr_raw: u64,
    self_word_raw: u64,
    arg1_raw: u64,
) -> u64 {
    let name = match decode_byte_string_to_string(Word::from_raw(generic_name_ptr_raw)) {
        Some(n) => n,
        None => panic!("nod_dispatch_binary: generic name word doesn't decode as <byte-string>"),
    };
    let g = get_or_create_generic(&name);
    // SAFETY: dispatch through the unified shim with no cache slot.
    unsafe {
        nod_dispatch(
            g as *const _ as u64,
            0,
            2,
            self_word_raw,
            arg1_raw,
            0,
            0,
            0,
            0,
            0,
            0,
        )
    }
}

/// JIT-callable `nod_add_method` shim. The lowering path uses this when
/// it wants to install a method from inside JIT'd code. Sprint 13's
/// production flow still goes through Rust-side `register_methods`;
/// this shim is present so the JIT can do it directly when a fixture
/// needs it.
///
/// `specN` carries either a `ClassId.0` value or 0 if unused.
///
/// # Safety
///
/// `generic_ptr_raw` is the address of a `&'static GenericFunction`.
/// `body_fn_ptr_raw` is a JIT'd `extern "C" fn(u64, ..., u64) -> u64`
/// of arity `arity`. Spec values are `ClassId.0` values for registered
/// classes.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn nod_add_method(
    generic_ptr_raw: u64,
    arity: u64,
    spec0: u64,
    spec1: u64,
    spec2: u64,
    spec3: u64,
    spec4: u64,
    spec5: u64,
    spec6: u64,
    spec7: u64,
    body_fn_ptr_raw: u64,
) -> u64 {
    // SAFETY: per caller's contract.
    let generic: &'static GenericFunction =
        unsafe { &*(generic_ptr_raw as *const GenericFunction) };
    let raw_specs: [u64; 8] = [spec0, spec1, spec2, spec3, spec4, spec5, spec6, spec7];
    let arity = (arity as usize).min(8);
    let mut specs = Vec::with_capacity(arity);
    for &raw in &raw_specs[..arity] {
        specs.push(ClassId(raw as u32));
    }
    generic.add_method(Method {
        specialisers: specs,
        body_fn_ptr: body_fn_ptr_raw as *const u8,
        param_count: arity,
        body_fn_name: String::new(),
    });
    0
}

// ─── Classification helper (extracted for reuse with inline caches) ────────

/// Read the class id of a Dylan Word. Fixnums short-circuit to
/// `<integer>`; pointer-tagged words read the wrapper's class field.
pub fn word_class_id(w: Word) -> ClassId {
    if w.is_fixnum() {
        return ClassId::INTEGER;
    }
    let Some(p) = w.as_ptr::<u8>() else {
        return ClassId::OBJECT;
    };
    // SAFETY: pointer-tagged word; first 8 bytes are a Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    wrapper.class()
}

fn decode_byte_string_to_string(w: Word) -> Option<String> {
    let p = w.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged word; first 8 bytes are a Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    if wrapper.class() != ClassId::BYTE_STRING {
        return None;
    }
    // SAFETY: class match implies <byte-string> layout.
    let bs = unsafe { &*(p as *const crate::strings::ByteString) };
    // SAFETY: invariant.
    let bytes = unsafe { bs.bytes() };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

// ─── dump-dispatch helper ─────────────────────────────────────────────────

/// Render every registered generic + its methods + every registered
/// cache slot. Stable-ish format for test assertions.
pub fn dump_dispatch() -> String {
    let mut out = String::new();
    let mut generics: Vec<&'static GenericFunction> = Vec::new();
    for_each_generic(|g| generics.push(g));
    generics.sort_by(|a, b| a.name.cmp(&b.name));

    for g in generics {
        let generation = g.generation();
        let methods = g.methods.read().expect("methods rwlock poisoned");
        out.push_str(&format!(
            "Generic {} (generation = {}, {} method{}):\n",
            g.name,
            generation,
            methods.len(),
            if methods.len() == 1 { "" } else { "s" }
        ));
        for m in methods.iter() {
            let specs = m
                .specialisers
                .iter()
                .map(|c| class_name(*c))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "  method ({}) -> {:#x}\n",
                specs, m.body_fn_ptr as usize
            ));
        }
        let slots = g.cache_slots.read().expect("cache_slots rwlock poisoned");
        if !slots.is_empty() {
            out.push_str("  Call sites:\n");
            for slot_ptr in slots.iter() {
                // SAFETY: slot pointers come from JIT statics that live forever.
                let slot = unsafe { &**slot_ptr };
                let class_raw = slot.class.load(Ordering::Relaxed);
                let method_raw = slot.method.load(Ordering::Relaxed);
                let slot_gen = slot.generation.load(Ordering::Relaxed);
                let hits = slot.hits.load(Ordering::Relaxed);
                let misses = slot.misses.load(Ordering::Relaxed);
                let site_id = slot.site_id.load(Ordering::Relaxed);
                let class_disp = if class_raw == 0 {
                    "<cold>".to_string()
                } else {
                    format!("{}({})", class_name(ClassId(class_raw as u32)), class_raw)
                };
                out.push_str(&format!(
                    "    site#{site_id}: cache class={class_disp} method={method_raw:#x} gen={slot_gen} - hits={hits} misses={misses}\n"
                ));
            }
        }
    }
    out
}

/// Byte offset of the `generation` `AtomicU64` inside `GenericFunction`.
/// Codegen bakes this offset into the IR so the fast-path cache check
/// can read the counter without going through a Rust function call.
///
/// Computed at runtime via address arithmetic on a temporary instance
/// — Rust's struct layout for non-`repr(C)` structs isn't stable
/// across versions, but it IS stable WITHIN a single compilation, and
/// codegen baking happens at the same compilation as the runtime.
pub fn generic_generation_offset() -> usize {
    // Build a transient instance and measure. The temporary lives only
    // for the duration of this function; we never expose it.
    let g = GenericFunction::new("__layout_probe__");
    let base = (&g as *const GenericFunction) as usize;
    let gen_ptr = (&g.generation as *const AtomicU64) as usize;
    gen_ptr - base
}

/// Test helper: wipe every registered generic. Used by `#[serial]`
/// tests that want a clean slate.
pub fn _reset_for_tests() {
    let mut guard = GENERICS.write().expect("generics registry poisoned");
    if let Some(reg) = guard.as_mut() {
        reg.by_name.clear();
    }
    // Sprint 15: also clear the resolved-dispatch index so each test
    // starts from an empty back-reference set.
    let mut idx = RESOLVED_DISPATCH_INDEX
        .write()
        .expect("resolved dispatch index poisoned");
    idx.clear();
}

// ─── Sprint 15: resolved-dispatch back-reference index ────────────────────
//
// The Sprint 15 dispatch resolver records `(call_site_id, generic_name,
// recorded_generation)` for every Dispatch it rewrites to a direct
// call. The index sits empty during Sprint 15 — the redefinition refusal
// policy guarantees no rewritten call site can be invalidated. Sprint 29
// (cross-library library-merge) consults this table to recompile sites
// whose generic's generation has advanced past the recorded value.
//
// The data structure goes here (not in `nod-sema`) because Sprint 29's
// invalidation cascade runs from the runtime side at method-add time.

/// One entry in the resolved-dispatch back-reference index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDispatchEntry {
    /// Lightweight identifier for the call site that was resolved.
    /// Codegen mints these the same way as `CacheSlot::site_id`.
    pub call_site_id: u64,
    /// Generic whose method table was consulted when resolving.
    pub generic_name: String,
    /// The generation counter at the time of resolution. Sprint 29
    /// compares this with `GenericFunction::generation()` on each
    /// method-table mutation; a divergence means the site needs
    /// recompilation.
    pub recorded_generation: u64,
    /// The method symbol the resolver picked. Carried for diagnostics
    /// and the eventual Sprint 29 invalidation tooling.
    pub resolved_method: String,
}

static RESOLVED_DISPATCH_INDEX: RwLock<Vec<ResolvedDispatchEntry>> = RwLock::new(Vec::new());

/// Sprint 15: record that a `Computation::Dispatch` at `call_site_id`
/// was statically resolved to `resolved_method` when `generic_name`'s
/// method table was at generation `recorded_generation`.
pub fn record_resolved_dispatch(entry: ResolvedDispatchEntry) {
    let mut guard = RESOLVED_DISPATCH_INDEX
        .write()
        .expect("resolved dispatch index poisoned");
    guard.push(entry);
}

/// Sprint 15: snapshot the resolved-dispatch index for inspection
/// (driver `dump-dispatch --sealed` and tests).
pub fn resolved_dispatch_snapshot() -> Vec<ResolvedDispatchEntry> {
    RESOLVED_DISPATCH_INDEX
        .read()
        .expect("resolved dispatch index poisoned")
        .clone()
}

// Compile-time-checked layout for CacheSlot. The codegen layer assumes
// these offsets; if Rust's repr(C) discipline ever changes, these
// asserts catch it before tests do.
#[cfg(test)]
mod layout_asserts {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn cache_slot_field_offsets() {
        assert_eq!(offset_of!(CacheSlot, class), 0);
        assert_eq!(offset_of!(CacheSlot, method), 8);
        assert_eq!(offset_of!(CacheSlot, generation), 16);
        assert_eq!(offset_of!(CacheSlot, hits), 24);
        assert_eq!(offset_of!(CacheSlot, misses), 32);
        assert_eq!(offset_of!(CacheSlot, site_id), 40);
    }

    #[test]
    fn generic_generation_offset_consistent() {
        // Two consecutive probes return the same offset (probes are
        // independent instances; layout is process-stable).
        let a = generic_generation_offset();
        let b = generic_generation_offset();
        assert_eq!(a, b);
    }
}
