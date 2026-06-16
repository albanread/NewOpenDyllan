//! Sprint 47 — thread-local secondary-values buffer for multi-value
//! return. See `docs/COMPILER_GAPS.md` GAP-003 for the motivating
//! problem (packing two integers into one via `line * 1_000_000 + col`).
//!
//! Common Lisp / SBCL / Open Dylan convention: callee writes extras to
//! a TLS buffer and returns the first value normally through the
//! ordinary single-value ABI; the caller, when it wants the extras,
//! clears `count = 0` BEFORE the call so polluted state from earlier
//! calls doesn't leak in, then reads `extras[0..count]` after the call
//! returns.
//!
//! Single-value receivers never touch the buffer — they pay zero
//! overhead. Multi-binder receivers (`let (a, b) = expr;`) clear the
//! count before the call and read missing extras as `#f` when the call
//! turned out to return fewer values than asked for. This matches the
//! standard CL discipline.
//!
//! # Buffer
//!
//! - Capacity: 8 `Word`s. Buffer overflow signals an error via
//!   `nod_error` (Sprint 47 ships with the limit; expansion is trivial
//!   if a future Dylan call ever needs more).
//! - Storage: thread-local; secondary values are per-thread state, just
//!   like in SBCL.
//!
//! # GC integration
//!
//! The buffer's first `count` entries are scanned as roots by
//! `snapshot_active_values_roots`, which `heap::snapshot_roots` calls
//! alongside the JIT and AOT root snapshots. The buffer Words can be
//! heap pointers (e.g. a `<byte-string>` returned as the second value
//! of a parse function) and must therefore be precise roots whenever
//! `count > 0`.
//!
//! # Layout — why `Cell<Word>` rather than `UnsafeCell<Word>`
//!
//! `Cell` gives the safe interior-mutability surface we want from JIT
//! code (one writer at a time on the owning thread). The GC root walk
//! reads through a `*const Word` derived from `Cell::as_ptr`; the
//! thread-local storage owns the cells and never moves them.

use std::cell::Cell;

use crate::word::Word;

/// Sprint 47 — buffer cap. 8 is comfortably larger than any realistic
/// Dylan multi-value return we have today (typical use is 2; three
/// values for `divmod-with-sign` and similar; the lexer fixture's
/// `offset-to-line-col` returns 2).
const VALUES_BUF_CAP: usize = 8;

thread_local! {
    /// Secondary-values storage. Entry `i` corresponds to the `(i+1)`-th
    /// return value of the most recent multi-value-returning call (entry
    /// 0 is the "second" value, since the "first" goes through the
    /// ordinary ABI return slot).
    static VALUES_BUF: [Cell<Word>; VALUES_BUF_CAP] =
        std::array::from_fn(|_| Cell::new(Word::from_raw(0)));

    /// Number of extras currently in `VALUES_BUF`. `0` means "single-value
    /// return", which is the default state.
    static VALUES_COUNT: Cell<usize> = const { Cell::new(0) };
}

fn imm_false_raw() -> u64 {
    crate::literal_pool_immediates().false_.raw()
}

/// Reset the secondary-values buffer to "no extras." Called by every
/// multi-binder `let` BEFORE the value expression is evaluated, so that
/// extras left in the buffer by an unrelated earlier call cannot leak
/// in if the new call happens to be single-valued.
#[unsafe(no_mangle)]
pub extern "C" fn nod_values_clear() -> u64 {
    VALUES_COUNT.with(|c| c.set(0));
    imm_false_raw()
}

/// Store `val` as the `(idx+1)`-th return value (i.e. index 0 = second
/// value, index 1 = third value, …). `idx` is fixnum-encoded; the
/// returned `Word` is the stored value (so `%values-set!` chains
/// naturally inside the natural lowering shape).
///
/// Bounds-checked: if `idx >= VALUES_BUF_CAP`, panics. Declared with
/// `extern "C-unwind"` (like `nod_error`) so the panic propagates out
/// through JIT-emitted call sites and the Sprint 45g crash dumper can
/// catch it.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn nod_values_set(idx_raw: u64, val_raw: u64) -> u64 {
    let idx_word = Word::from_raw(idx_raw);
    let idx = idx_word
        .as_fixnum()
        .expect("nod_values_set: idx must be a fixnum")
        as usize;
    assert!(
        idx < VALUES_BUF_CAP,
        "nod_values_set: index {idx} out of range for {VALUES_BUF_CAP}-slot \
         secondary-values buffer (Sprint 47 cap; raise VALUES_BUF_CAP if a \
         legitimate Dylan call needs more)"
    );
    let val = Word::from_raw(val_raw);
    VALUES_BUF.with(|buf| buf[idx].set(val));
    VALUES_COUNT.with(|c| {
        let next = idx + 1;
        if next > c.get() {
            c.set(next);
        }
    });
    val_raw
}

/// Read the `(idx+1)`-th return value of the most recent multi-value
/// call. If `idx >= count`, returns `#f` (the standard CL discipline:
/// missing extras default to false). `idx` is fixnum-encoded.
#[unsafe(no_mangle)]
pub extern "C" fn nod_values_get(idx_raw: u64) -> u64 {
    let idx_word = Word::from_raw(idx_raw);
    let idx = idx_word
        .as_fixnum()
        .expect("nod_values_get: idx must be a fixnum")
        as usize;
    let count = VALUES_COUNT.with(|c| c.get());
    if idx < count {
        VALUES_BUF.with(|buf| buf[idx].get().raw())
    } else {
        imm_false_raw()
    }
}

/// Current secondary-value count, fixnum-tagged. Useful for code that
/// wants to detect the "actually multi-valued?" case, though Dylan
/// itself rarely needs it — `let (a, b) = x` happily binds `b` to `#f`
/// when `x` was single-valued.
#[unsafe(no_mangle)]
pub extern "C" fn nod_values_count() -> u64 {
    let count = VALUES_COUNT.with(|c| c.get());
    // count <= VALUES_BUF_CAP = 8 always fits in a fixnum.
    Word::fixnum_unchecked(count as i64).raw()
}

/// Build the rest-sequence `<simple-object-vector>` for a multiple-value
/// `let (… #rest r) = form` binding.
///
/// `buf_start` is the first `VALUES_BUF` index to include (slots already
/// consumed by explicit binders are skipped). `include_primary != 0` puts
/// the form's primary return value (`primary`) as the FIRST element — the
/// `let (#rest r)` case where there are no explicit binders, so the primary
/// belongs in the rest sequence.
///
/// GC: `VALUES_BUF` entries are already GC roots
/// ([`snapshot_active_values_roots`]) and `primary` is protected with a
/// [`crate::make::RootGuard`], so both survive the SOV allocation and are
/// read back (post-collection) afterwards. No allocation happens after the
/// SOV is created, so the freshly-built vector stays valid while it fills.
///
/// # Safety
///
/// No preconditions beyond the standard tagged-Word ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_collect_rest_values(
    primary: u64,
    buf_start_raw: u64,
    include_primary_raw: u64,
) -> u64 {
    let buf_start = Word::from_raw(buf_start_raw).as_fixnum().unwrap_or(0).max(0) as usize;
    let include_primary =
        Word::from_raw(include_primary_raw).as_fixnum().unwrap_or(0) != 0;
    let count = VALUES_COUNT.with(|c| c.get());
    let n_extra = count.saturating_sub(buf_start);
    let total = n_extra + if include_primary { 1 } else { 0 };

    // Protect the primary across the (possibly collecting) SOV allocation.
    let primary_local = Word::from_raw(primary);
    let p_guard = crate::make::RootGuard::new(&primary_local);

    let sov = crate::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(total, &pool.classes)
    });
    let sov_class = crate::with_literal_pool(|pool| pool.classes.simple_object_vector());

    // Read the primary back through the guard (post-GC address) and fill.
    let primary_v = p_guard.reload();
    // SAFETY: `sov` is the SOV we just allocated; mutator is single-threaded
    // and no allocation happens between here and the end of the fill.
    let v = unsafe { crate::try_simple_object_vector_mut(sov, sov_class) }
        .expect("freshly allocated SOV");
    let slots = unsafe { v.slots_mut() };
    let mut w = 0usize;
    if include_primary {
        slots[w] = primary_v;
        w += 1;
    }
    for i in 0..n_extra {
        slots[w] = VALUES_BUF.with(|buf| buf[buf_start + i].get());
        w += 1;
    }
    drop(p_guard);
    sov.raw()
}

/// GC root scanner — return a `*const Word` for each currently-live
/// secondary-value entry. Called from `heap::snapshot_roots` so the
/// extras stay reachable across any safepoint that happens between the
/// multi-value-returning call returning and the receiver's
/// `%values-get` reads completing.
pub fn snapshot_active_values_roots() -> Vec<*const Word> {
    let count = VALUES_COUNT.with(|c| c.get());
    VALUES_BUF.with(|buf| {
        (0..count)
            .map(|i| buf[i].as_ptr() as *const Word)
            .collect()
    })
}

#[cfg(test)]
fn reset_values_for_tests() {
    VALUES_COUNT.with(|c| c.set(0));
    VALUES_BUF.with(|buf| {
        for cell in buf.iter() {
            cell.set(Word::from_raw(0));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn fix(n: i64) -> u64 {
        Word::from_fixnum(n).unwrap().raw()
    }

    #[test]
    #[serial]
    fn clear_resets_count_to_zero() {
        reset_values_for_tests();
        // Pretend there were two extras around.
        nod_values_set(fix(0), fix(11));
        nod_values_set(fix(1), fix(22));
        assert_eq!(Word::from_raw(nod_values_count()).as_fixnum(), Some(2));
        // Clear should drop count back to 0.
        nod_values_clear();
        assert_eq!(Word::from_raw(nod_values_count()).as_fixnum(), Some(0));
    }

    #[test]
    #[serial]
    fn set_then_get_round_trips() {
        reset_values_for_tests();
        nod_values_clear();
        nod_values_set(fix(0), fix(101));
        nod_values_set(fix(1), fix(202));
        let v0 = Word::from_raw(nod_values_get(fix(0))).as_fixnum();
        let v1 = Word::from_raw(nod_values_get(fix(1))).as_fixnum();
        assert_eq!(v0, Some(101));
        assert_eq!(v1, Some(202));
    }

    #[test]
    #[serial]
    fn count_grows_with_set_index() {
        reset_values_for_tests();
        nod_values_clear();
        nod_values_set(fix(0), fix(1));
        assert_eq!(Word::from_raw(nod_values_count()).as_fixnum(), Some(1));
        nod_values_set(fix(2), fix(3));
        assert_eq!(Word::from_raw(nod_values_count()).as_fixnum(), Some(3));
        nod_values_set(fix(1), fix(2));
        // Setting a lower index shouldn't shrink count.
        assert_eq!(Word::from_raw(nod_values_count()).as_fixnum(), Some(3));
    }

    #[test]
    #[serial]
    fn get_past_count_returns_false() {
        reset_values_for_tests();
        nod_values_clear();
        nod_values_set(fix(0), fix(42));
        // Only one extra; reading index 1 should give #f.
        let v1 = nod_values_get(fix(1));
        let imm_false = crate::literal_pool_immediates().false_.raw();
        assert_eq!(v1, imm_false);
    }

    #[test]
    #[serial]
    fn get_after_clear_returns_false() {
        reset_values_for_tests();
        nod_values_clear();
        nod_values_set(fix(0), fix(7));
        nod_values_clear();
        let v0 = nod_values_get(fix(0));
        let imm_false = crate::literal_pool_immediates().false_.raw();
        assert_eq!(v0, imm_false);
    }

    #[test]
    #[serial]
    #[should_panic(expected = "out of range")]
    fn set_at_cap_panics() {
        reset_values_for_tests();
        nod_values_clear();
        nod_values_set(fix(VALUES_BUF_CAP as i64), fix(99));
    }

    #[test]
    #[serial]
    fn snapshot_returns_count_pointers() {
        reset_values_for_tests();
        nod_values_clear();
        nod_values_set(fix(0), fix(11));
        nod_values_set(fix(1), fix(22));
        nod_values_set(fix(2), fix(33));
        let roots = snapshot_active_values_roots();
        assert_eq!(roots.len(), 3);
        // SAFETY: roots reference the thread-local buffer cells, which
        // outlive this test scope (thread-local destructors run at
        // thread exit; the test thread is alive).
        unsafe {
            assert_eq!((*roots[0]).as_fixnum(), Some(11));
            assert_eq!((*roots[1]).as_fixnum(), Some(22));
            assert_eq!((*roots[2]).as_fixnum(), Some(33));
        }
        // After clear, the snapshot is empty.
        nod_values_clear();
        assert!(snapshot_active_values_roots().is_empty());
    }

    #[test]
    #[serial]
    fn polluted_buffer_does_not_leak_when_caller_clears() {
        reset_values_for_tests();
        // Simulate call A returning two values.
        nod_values_clear();
        nod_values_set(fix(0), fix(111));
        nod_values_set(fix(1), fix(222));
        // Receiver of A consumes the extras (implicit; just leaves them).
        // Now call B is single-valued. A correct caller of B that does
        // multi-binder destructuring MUST clear first.
        nod_values_clear();
        // ... B returns its single value through the ordinary ABI; it
        // never touches the buffer. The "second binder" should now be #f.
        let v1 = nod_values_get(fix(0));
        let imm_false = crate::literal_pool_immediates().false_.raw();
        assert_eq!(
            v1, imm_false,
            "after clear, extras must default to #f even if the buffer's \
             backing bytes still hold A's old values"
        );
    }
}
