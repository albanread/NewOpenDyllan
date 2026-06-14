//! `make` runtime — allocate a fresh user-class instance, initialise
//! slots from init keywords + defaults, and (optionally) dispatch to a
//! user `initialize` method.
//!
//! The JIT-callable entry point is fixed-arity (`MAKE_MAX_KW_PAIRS`
//! keyword pairs) to match the calling-convention shape of
//! `nod_format_out`. Sprint 23+ (c-ffi) brings true variadic calls.
//!
//! Keyword arguments are passed as `(name_word, value_word)` pairs.
//! Each `name_word` is a symbol pointer (pinned in the literal pool's
//! static area); `value_word` is whatever value the user supplied.
//!
//! Initialisation order:
//!   1. Allocate the instance in the moveable heap; install wrapper.
//!   2. Zero-fill payload (alloc_object already does this).
//!   3. For each slot with a literal default, write the default.
//!   4. For each supplied keyword whose name matches a slot's
//!      init-keyword, write the supplied value at that slot's offset.
//!      (Late binding wins — caller's value overrides the default.)
//!   5. If `dispatch::lookup_method("initialize", class)` returns a
//!      user method, tail-call into it. Otherwise return the new
//!      instance directly.
//!
//! A slot marked `required-init-keyword` whose keyword wasn't supplied
//! triggers a diagnostic to stderr and returns the partly-initialised
//! instance. Sprint 19 will replace the diagnostic with a real
//! `<missing-required-init-keyword>` signal.

use std::io::Write;

use crate::classes::{ClassId, ClassMetadata, SlotDefault};
use crate::dispatch::{find_initialize_method, invoke_method_with_self};
use crate::word::Word;
use crate::with_literal_pool;

/// Maximum number of `(keyword, value)` pairs the fixed-arity JIT shim
/// accepts. Sprint 23+ lifts the limit via c-ffi.
pub const MAKE_MAX_KW_PAIRS: usize = 8;

/// JIT-callable `make`. The class-metadata pointer is baked into the
/// IR as an `i64` constant (codegen does this via
/// `class_metadata_ptr`). Each keyword pair is two `u64` arguments;
/// unused pairs MUST be zero.
///
/// Returns the new instance as a pointer-tagged `Word::raw`.
///
/// # Safety
///
/// `class_metadata_ptr_raw` must be the address of a `ClassMetadata`
/// in the static area (i.e. returned by
/// `classes::class_metadata_ptr`). Each `(kw_name_N, value_N)` must
/// be one of:
///   - both zero (unused slot), OR
///   - `kw_name_N` is a pointer-tagged `<symbol>` Word and `value_N` is
///     any valid Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_make(
    class_metadata_ptr_raw: u64,
    kw_count: u64,
    kw_name_0: u64,
    value_0: u64,
    kw_name_1: u64,
    value_1: u64,
    kw_name_2: u64,
    value_2: u64,
    kw_name_3: u64,
    value_3: u64,
    kw_name_4: u64,
    value_4: u64,
    kw_name_5: u64,
    value_5: u64,
    kw_name_6: u64,
    value_6: u64,
    kw_name_7: u64,
    value_7: u64,
) -> u64 {
    if class_metadata_ptr_raw == 0 {
        diag("make: class metadata pointer is null");
        return 0;
    }
    // SAFETY: caller's contract.
    let metadata: &'static ClassMetadata =
        unsafe { &*(class_metadata_ptr_raw as *const ClassMetadata) };

    // Special-case <stretchy-vector>: its backing storage SOV must be
    // allocated before the outer object, which generic slot-init can't do.
    if metadata.id == crate::collections::stretchy_vector_class_id() {
        return crate::collections::make_stretchy_vector(4).raw();
    }

    // Special-case <byte-string>: backed by inline bytes, not slots.
    // Dylan: make(<byte-string>, size: N) → alloc_byte_string_uninit(N).
    if metadata.id == crate::classes::ClassId::BYTE_STRING {
        // Decode the `size:` keyword if present.
        let kw_count_local = (kw_count as usize).min(MAKE_MAX_KW_PAIRS);
        let kw_name_raw = [
            kw_name_0, kw_name_1, kw_name_2, kw_name_3,
            kw_name_4, kw_name_5, kw_name_6, kw_name_7,
        ];
        let kw_value_raw = [
            value_0, value_1, value_2, value_3,
            value_4, value_5, value_6, value_7,
        ];
        let mut size_n: usize = 0;
        for i in 0..kw_count_local {
            if let Some(name) = decode_keyword_name(crate::word::Word::from_raw(kw_name_raw[i])) {
                if name == "size" {
                    size_n = crate::word::Word::from_raw(kw_value_raw[i])
                        .as_fixnum()
                        .unwrap_or(0)
                        .max(0) as usize;
                }
            }
        }
        return with_literal_pool(|pool| pool.heap.alloc_byte_string_uninit(size_n, &pool.classes)).raw();
    }

    let kw_count = (kw_count as usize).min(MAKE_MAX_KW_PAIRS);

    // Sprint 11b: stable-stack-bind each `(name, value)` pair and
    // register the bindings as GC roots before the `alloc_object` call
    // can fire a minor GC. If `alloc_object` evacuates an object that
    // a user `value` Word points at, the collector rewrites the
    // rooted slot; subsequent writes through the slot use the new
    // address. Without this, a `make(<point>, x: 1, y: 2)` followed
    // by an immediate second `make` could land the first instance in
    // a stale-pointer state — exactly the latent bug from Sprint 11.
    //
    // `name_words` / `value_words` live for the rest of this function;
    // `_name_guards` / `_value_guards` register on construction and
    // unregister on drop (end of function scope). The kw_name decode
    // path reads the **rooted** Word (post-GC address) via
    // `name_words[i]` rather than the original `kw_name_N` arg.
    let kw_name_raw = [
        kw_name_0, kw_name_1, kw_name_2, kw_name_3, kw_name_4, kw_name_5, kw_name_6, kw_name_7,
    ];
    let kw_value_raw = [
        value_0, value_1, value_2, value_3, value_4, value_5, value_6, value_7,
    ];
    let mut name_words: [Word; MAKE_MAX_KW_PAIRS] =
        [Word::from_raw(0); MAKE_MAX_KW_PAIRS];
    let mut value_words: [Word; MAKE_MAX_KW_PAIRS] =
        [Word::from_raw(0); MAKE_MAX_KW_PAIRS];
    for i in 0..MAKE_MAX_KW_PAIRS {
        name_words[i] = Word::from_raw(kw_name_raw[i]);
        value_words[i] = Word::from_raw(kw_value_raw[i]);
    }
    let _name_guards: Vec<crate::make::RootGuard> = name_words
        .iter()
        .take(kw_count)
        .map(crate::make::RootGuard::new)
        .collect();
    let _value_guards: Vec<crate::make::RootGuard> = value_words
        .iter()
        .take(kw_count)
        .map(crate::make::RootGuard::new)
        .collect();

    // 1. Allocate instance through the moveable heap. Payload size is
    //    `instance_size - size_of::<Wrapper>()` = `8 * slot_count`.
    //    Any minor GC triggered here observes `name_words`/`value_words`
    //    as registered roots and updates the slots if it evacuates.
    let payload = metadata.instance_size.saturating_sub(8);
    let instance_word = with_literal_pool(|pool| pool.heap.alloc_object(metadata.id, payload));
    let instance_addr = match instance_word.as_mut_ptr::<u8>() {
        Some(p) => p as usize,
        None => {
            diag("make: heap returned non-pointer Word");
            return 0;
        }
    };

    // 2. Default-init each slot from the registered default value.
    for slot in &metadata.slots {
        if let SlotDefault::Value(default_word) = slot.default_init {
            // SAFETY: instance_addr is freshly allocated; offset is in
            // bounds per the metadata's instance_size.
            unsafe {
                let slot_ptr = (instance_addr + slot.offset) as *mut Word;
                *slot_ptr = default_word;
            }
        }
    }

    // 3. Apply user-supplied init-keywords. Read the **rooted** Words
    //    (potentially rewritten by GC) rather than the original arg
    //    registers, so the slot writes carry the post-evac addresses.
    for i in 0..kw_count {
        let name_w = name_words[i];
        let val_w = value_words[i];
        if name_w.raw() == 0 {
            continue;
        }
        let kw_name = match decode_keyword_name(name_w) {
            Some(n) => n,
            None => {
                diag("make: init-keyword name is not a <symbol> or <byte-string>");
                continue;
            }
        };
        let slot = match metadata.slot_by_init_keyword(&kw_name) {
            Some(s) => s,
            None => {
                diag(&format!(
                    "make: no slot in `{}` accepts init-keyword `{}`",
                    metadata.name, kw_name
                ));
                continue;
            }
        };
        // SAFETY: instance is live; slot.offset is within instance_size.
        unsafe {
            let slot_ptr = (instance_addr + slot.offset) as *mut Word;
            *slot_ptr = val_w;
        }
    }

    // 4. Diagnose any required-init-keyword that wasn't supplied.
    for slot in &metadata.slots {
        if !slot.required_init_keyword {
            continue;
        }
        let Some(kw_name) = slot.init_keyword.as_deref() else {
            continue;
        };
        let supplied = (0..kw_count).any(|i| {
            decode_keyword_name(name_words[i])
                .map(|s| s == kw_name)
                .unwrap_or(false)
        });
        if !supplied {
            diag(&format!(
                "make: required init-keyword `{kw_name}:` missing for `{}`",
                metadata.name
            ));
        }
    }

    // 5. If the user defined `initialize` for this class, call it. The
    //    method takes one argument (the new instance) plus whatever
    //    keyword pass-through the user wants — Sprint 12 only forwards
    //    the receiver. Keyword pass-through is Sprint 13.
    if let Some(method_ptr) = find_initialize_method(metadata.id) {
        // SAFETY: the method's signature is `(u64) -> u64`; the result
        // (the new instance) is discarded — `initialize` returns the
        // initialised object per Dylan convention, but `make` is what
        // surfaces it.
        unsafe {
            invoke_method_with_self(method_ptr, instance_word);
        }
    }

    instance_word.raw()
}

fn decode_keyword_name(w: Word) -> Option<String> {
    if !w.is_pointer() {
        return None;
    }
    let p = w.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged Word; first 8 bytes are a Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let class = wrapper.class();
    if class == ClassId::SYMBOL {
        // SAFETY: class match implies Symbol layout.
        let sym = unsafe { &*(p as *const crate::symbols::Symbol) };
        let name_word = sym.name;
        // SAFETY: symbol's name slot is a <byte-string> Word.
        return unsafe { decode_byte_string_to_string(name_word) };
    }
    if class == ClassId::BYTE_STRING {
        return unsafe { decode_byte_string_to_string(w) };
    }
    None
}

/// # Safety
///
/// `w` must be a pointer-tagged `<byte-string>` Word.
unsafe fn decode_byte_string_to_string(w: Word) -> Option<String> {
    let p = w.as_ptr::<u8>()?;
    // SAFETY: caller asserts class match.
    let bs = unsafe { &*(p as *const crate::strings::ByteString) };
    // SAFETY: ByteString invariant — inline bytes past header.
    let bytes = unsafe { bs.bytes() };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

fn diag(msg: &str) {
    let stderr = std::io::stderr();
    let mut h = stderr.lock();
    let _ = writeln!(h, "{msg}");
}

/// Convenience non-JIT make: from Rust, allocate a user-class instance
/// and apply init-keywords. Used by tests.
///
/// # Safety
///
/// `metadata` must be a valid registered class. Each keyword pair's
/// value Word must be a valid Dylan Word.
pub unsafe fn rust_make(
    metadata: &'static ClassMetadata,
    init_keywords: &[(&str, Word)],
) -> Word {
    // Sprint 11b: stable-stack-bind every init-keyword Word and
    // register them as GC roots before the heap allocation can trigger
    // a minor GC. See `nod_make` for the rationale.
    let mut kw_values: Vec<Word> = init_keywords.iter().map(|(_, v)| *v).collect();
    let _kw_guards: Vec<RootGuard> = kw_values.iter().map(RootGuard::new).collect();
    let payload = metadata.instance_size.saturating_sub(8);
    let instance_word = with_literal_pool(|pool| pool.heap.alloc_object(metadata.id, payload));
    let Some(p) = instance_word.as_mut_ptr::<u8>() else {
        return instance_word;
    };
    let addr = p as usize;
    for slot in &metadata.slots {
        if let SlotDefault::Value(default_word) = slot.default_init {
            // SAFETY: instance just allocated, offset in bounds.
            unsafe {
                let slot_ptr = (addr + slot.offset) as *mut Word;
                *slot_ptr = default_word;
            }
        }
    }
    // Apply user-supplied init-keywords, reading the **rooted** Word
    // (which the collector will have updated if it evacuated the
    // pointed-at object during `alloc_object`).
    for (idx, (kw, _)) in init_keywords.iter().enumerate() {
        if let Some(slot) = metadata.slot_by_init_keyword(kw) {
            // SAFETY: see above.
            unsafe {
                let slot_ptr = (addr + slot.offset) as *mut Word;
                *slot_ptr = kw_values[idx];
            }
        }
    }
    // Touch `kw_values` post-allocation so the optimiser can't elide
    // the rooted bindings.
    let _ = &mut kw_values;
    // Optional initialize.
    if let Some(method_ptr) = find_initialize_method(metadata.id) {
        // SAFETY: trusted method ptr; signature one Word in/out.
        unsafe {
            invoke_method_with_self(method_ptr, instance_word);
        }
    }
    instance_word
}

/// JIT-callable card-mark shim. Takes the raw address of a slot
/// (untagged pointer in `i64`) and marks the corresponding card in
/// the heap's write-barrier table. No-op if the slot isn't in old.
///
/// # Safety
///
/// `slot_addr` must be a valid heap-resident slot pointer. The runtime
/// handles the "not in old" case gracefully (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_card_mark(slot_addr: u64) {
    let ptr = slot_addr as *const Word;
    with_literal_pool(|pool| pool.heap.mark_card_for(ptr));
}

/// Sprint 11b JIT shim: register a stack-allocated `Word` slot as a GC
/// root for the duration of the next potentially-allocating call.
/// Codegen brackets every allocating call with one register/unregister
/// pair per live pointer-shaped temp; the GC walks the registered
/// slots and rewrites them if the targeted object gets evacuated.
///
/// # Safety
///
/// `slot` must be a writable, 8-byte-aligned `*mut Word` slot whose
/// lifetime spans the entire call site. The collector reads `*slot`
/// and may overwrite `*slot` if it relocates the targeted object. A
/// matching call to `nod_unregister_root(slot)` must follow before
/// `slot` becomes invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_register_root(slot: *mut Word) {
    // Sprint 11c: bypass `with_literal_pool` — the root registry is a
    // thread-local now, no shared state to lock. The Sprint 11b shim
    // took both the literal-pool mutex AND the roots mutex on every
    // call (hundreds of millions of acquisitions in Sprint 16's bench);
    // both are gone.
    crate::heap::register_root(slot as *const Word);
}

/// Sprint 11b JIT shim: companion to `nod_register_root`. Drop the
/// slot from the heap's root list. The collector will no longer touch
/// it on subsequent collections.
///
/// # Safety
///
/// `slot` must be the same pointer passed to a prior
/// `nod_register_root(slot)` call on the same thread, with no
/// intervening collection that would have moved the slot itself.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_unregister_root(slot: *mut Word) {
    // Sprint 11c: bypass the literal pool — see `nod_register_root`.
    crate::heap::unregister_root(slot as *const Word);
}

/// Sprint 11b: RAII helper for Rust-side allocating shims that need
/// to keep arg-Words alive across a `Heap::alloc_object` call. Drop
/// is implemented to call `unregister_root`. Construct with a `&Word`
/// in a stable stack frame slot; the borrow checker prevents
/// premature drop of the slot itself.
///
/// # Safety
///
/// The `&Word` reference's address must remain valid for the lifetime
/// of the `RootGuard`. In practice this means stack-binding the value
/// (`let val: Word = arg;`) and passing `&val` — the local is stable
/// for the rest of the enclosing scope.
pub struct RootGuard {
    slot: *const Word,
}

impl RootGuard {
    pub fn new(slot: &Word) -> Self {
        // Sprint 11c: bypass the literal-pool mutex — see
        // `nod_register_root` for the rationale.
        let slot_ptr = slot as *const Word;
        crate::heap::register_root(slot_ptr);
        Self { slot: slot_ptr }
    }

    /// GAP-011: read the current value of the rooted slot with a
    /// **volatile** load.
    ///
    /// The evacuator rewrites `*slot` *through the registered root
    /// pointer* whenever it moves the pointee (see
    /// `heap::minor_forward_word`). But the slot is the address of a
    /// `&Word`-shared local; the compiler is entitled to assume that a
    /// value behind a shared reference does not change, and at `-O2`/`-O3`
    /// it reuses the pre-collection register copy of that local. A caller
    /// that USES a rooted value *after* a potentially-collecting
    /// allocation then sees the stale (pre-evacuation) address — which now
    /// holds a forwarding pointer, not the live object. That is the
    /// "evacuated mid-grow" / "not a `<stretchy-vector>`" crash class.
    ///
    /// Reloading through this method forces a fresh memory read of the
    /// slot the collector actually rewrote, so the caller observes the
    /// post-GC address. Use it for every rooted value read back across an
    /// allocation (vector growth, table rehash, list cons, …).
    #[inline]
    pub fn reload(&self) -> Word {
        // SAFETY: `slot` is the address of a live, 8-aligned stack-bound
        // `Word` that outlives this guard (construction contract). The
        // volatile load prevents the compiler from substituting a cached
        // register value for the collector's in-memory rewrite.
        unsafe { core::ptr::read_volatile(self.slot) }
    }
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        // Sprint 11c: bypass the literal-pool mutex.
        crate::heap::unregister_root(self.slot);
    }
}

/// Test if `value` (a Word) is an instance of `target_class`. Walks
/// the target object's class CPL. Fixnums match only `<integer>` /
/// `<object>` (and `<top>` once we have it). Boolean / nil immediates
/// route through their wrapper's class.
pub fn nod_is_instance_of_word(value: Word, target_class: ClassId) -> bool {
    if value.is_fixnum() {
        return target_class == ClassId::INTEGER
            || target_class == ClassId::OBJECT;
    }
    // SAFETY: pointer-tagged Word; first 8 bytes are a Wrapper.
    let Some(p) = value.as_ptr::<u8>() else {
        return false;
    };
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    let class = wrapper.class();
    crate::classes::is_subclass(class, target_class)
}

/// JIT-callable `instance?` shim. Takes a tagged Word and a class id
/// (as a `u32` packed into a `u64`). Returns the pinned `#t` / `#f`
/// Word.
///
/// # Safety
///
/// `value` is any Dylan Word; `class_id` must be a registered class id.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_is_instance_of(value: u64, class_id: u64) -> u64 {
    let v = Word::from_raw(value);
    let cid = ClassId(class_id as u32);
    let result = nod_is_instance_of_word(v, cid);
    let imm = crate::literal_pool_immediates();
    if result { imm.true_.raw() } else { imm.false_.raw() }
}
