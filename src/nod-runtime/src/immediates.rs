//! Boolean + nil singletons.
//!
//! Sprint 09 encoded `#t` / `#f` as tagged fixnums `2` / `0`, which is
//! indistinguishable from integers `1` / `0`. Sprint 10 promotes them
//! to pinned heap-shape objects whose `Wrapper` carries `<boolean>`,
//! so `instance?(#t, <boolean>)` reads the wrapper and finds the right
//! class. `nil` follows the same pattern with `<empty-list>`.
//!
//! Storage: pinned in a `StaticArea`. Address-stable for the process
//! lifetime; the tracer treats their addresses as roots that always
//! resolve, not as heap-managed allocations.

use crate::classes::ClassTable;
use crate::static_area::StaticArea;
use crate::word::Word;
use crate::wrapper::Wrapper;

/// A pinned wrapper-shape cell. The whole struct is exactly 8 bytes
/// (one `Wrapper`) so that taking its address and tagging bit 0 yields
/// a valid Dylan pointer-tagged `Word`.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct WrapperCell {
    pub wrapper: Wrapper,
}

/// The three pinned immediates: `#t`, `#f`, `nil`. Each carries a
/// `Wrapper` whose class is the right Dylan class, and a `Word` that
/// is the pointer-tagged Dylan value JIT'd code uses.
#[derive(Clone, Copy)]
pub struct Immediates {
    /// Tagged pointer to a pinned `<boolean>` wrapper representing `#t`.
    pub true_: Word,
    /// Tagged pointer to a pinned `<boolean>` wrapper representing `#f`.
    pub false_: Word,
    /// Tagged pointer to a pinned `<empty-list>` wrapper representing `nil`.
    pub nil: Word,
}

impl Immediates {
    /// Build the immediates by pinning three wrapper cells in the
    /// `StaticArea`. The returned `Word`s are stable for the process
    /// lifetime (i.e. `static_area`'s lifetime).
    pub fn new(static_area: &StaticArea, classes: &ClassTable) -> Self {
        let t_cell = static_area.alloc(WrapperCell {
            wrapper: Wrapper::new(classes.boolean()),
        });
        let f_cell = static_area.alloc(WrapperCell {
            wrapper: Wrapper::new(classes.boolean()),
        });
        let nil_cell = static_area.alloc(WrapperCell {
            wrapper: Wrapper::new(classes.empty_list()),
        });
        Self {
            true_: Word::from_ptr(t_cell as *const WrapperCell),
            false_: Word::from_ptr(f_cell as *const WrapperCell),
            nil: Word::from_ptr(nil_cell as *const WrapperCell),
        }
    }
}

/// Read the `Wrapper` of a pointer-tagged `Word` directly. Unlike
/// `Heap::wrapper_of`, this does no heap-membership check — it trusts
/// the caller to only pass valid heap pointers or immediates.
///
/// # Safety
///
/// `w` must be either a fixnum (in which case `None` is returned) or
/// a pointer-tagged `Word` whose target's first 8 bytes are a valid
/// `Wrapper`. Sprint 10 guarantees this for: (a) `Heap`-allocated
/// objects, (b) `Immediates` cells.
pub unsafe fn wrapper_of_unchecked(w: Word) -> Option<Wrapper> {
    let p = w.as_ptr::<Wrapper>()?;
    // SAFETY: documented above.
    Some(unsafe { *p })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediates_distinct() {
        let area = StaticArea::new();
        let ct = ClassTable::new();
        let imm = Immediates::new(&area, &ct);
        assert_ne!(imm.true_, imm.false_);
        assert_ne!(imm.true_, imm.nil);
        assert_ne!(imm.false_, imm.nil);
    }

    #[test]
    fn immediates_are_pointer_tagged() {
        let area = StaticArea::new();
        let ct = ClassTable::new();
        let imm = Immediates::new(&area, &ct);
        assert!(imm.true_.is_pointer());
        assert!(imm.false_.is_pointer());
        assert!(imm.nil.is_pointer());
    }

    #[test]
    fn immediates_wrappers_carry_right_classes() {
        let area = StaticArea::new();
        let ct = ClassTable::new();
        let imm = Immediates::new(&area, &ct);
        // SAFETY: cells came from StaticArea — addresses stable, layout
        // is `WrapperCell` which is wire-compatible with a wrapper-first
        // heap object.
        let wt = unsafe { wrapper_of_unchecked(imm.true_) }.unwrap();
        let wf = unsafe { wrapper_of_unchecked(imm.false_) }.unwrap();
        let wn = unsafe { wrapper_of_unchecked(imm.nil) }.unwrap();
        assert_eq!(wt.class(), ct.boolean());
        assert_eq!(wf.class(), ct.boolean());
        assert_eq!(wn.class(), ct.empty_list());
    }
}
