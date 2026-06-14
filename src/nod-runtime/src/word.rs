//! Tagged 64-bit Dylan value — the ABI between JIT'd code and the runtime.
//!
//! Tag scheme (committed in `PLAN.md` §1.11):
//!
//! ```text
//!   bit 0: 0  → fixnum     — upper 63 bits hold the signed integer
//!                            already shifted left by 1.
//!   bit 0: 1  → pointer    — bits [63:1] form an 8-byte-aligned heap
//!                            pointer; bit 0 carries the tag.
//! ```
//!
//! Encoding:   fixnum 5    →  (5 << 1) | 0     = 0x0A
//!             ptr 0x1000  →  0x1000 | 1       = 0x1001
//!
//! Untagging:  fixnum  →  (word as i64) >> 1   (arithmetic, sign-preserving)
//!             pointer →  word & !1            (clear bit 0)
//!
//! Tag check:  is_fixnum  →  (word & 1) == 0
//!             is_pointer →  (word & 1) == 1
//!
//! Fixnum range is 63 bits signed: `-2^62 ..= 2^62 - 1`. Out-of-range
//! integers earn a `FixnumOverflow` from `from_fixnum`; Sprint 12+
//! upgrades them to `<big-integer>` / `<double-integer>`.
//!
//! Arithmetic intent: `(a<<1) + (b<<1) = (a+b)<<1`, so tagged-fixnum
//! `add` / `sub` / `neg` lower to straight LLVM `add` / `sub`. `mul`
//! needs one operand right-shifted first; `div` needs a left-shift on
//! the quotient. See `nod-llvm::codegen` for the LLVM-side details.

use std::fmt;

/// Inclusive bounds for a fixnum payload (63-bit signed).
pub const FIXNUM_MIN: i64 = -(1_i64 << 62);
pub const FIXNUM_MAX: i64 = (1_i64 << 62) - 1;

/// Returned by `Word::from_fixnum` when the integer doesn't fit in 63
/// signed bits. Sprint 09 surfaces this as a "not yet supported"
/// diagnostic; Sprint 12 lights up `<big-integer>` / `<double-integer>`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct FixnumOverflow {
    pub value: i64,
}

impl fmt::Display for FixnumOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "integer {} out of fixnum range [{}, {}]; \
             <big-integer> / <double-integer> not yet supported",
            self.value, FIXNUM_MIN, FIXNUM_MAX
        )
    }
}

impl std::error::Error for FixnumOverflow {}

/// A tagged Dylan value. Carries either a 63-bit fixnum or an 8-byte
/// aligned heap pointer with bit 0 set.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Word(pub u64);

impl Word {
    /// Construct a fixnum if `n` fits in 63 signed bits.
    pub const fn from_fixnum(n: i64) -> Result<Word, FixnumOverflow> {
        if n < FIXNUM_MIN || n > FIXNUM_MAX {
            Err(FixnumOverflow { value: n })
        } else {
            Ok(Word((n as u64) << 1))
        }
    }

    /// Construct a fixnum without bounds-checking. Debug-asserts in
    /// range; release builds wrap silently. Used by codegen where the
    /// parser has already rejected out-of-range literals.
    pub const fn fixnum_unchecked(n: i64) -> Word {
        debug_assert!(
            n >= FIXNUM_MIN && n <= FIXNUM_MAX,
            "fixnum out of range"
        );
        Word((n as u64) << 1)
    }

    /// Tag an 8-byte-aligned heap pointer.
    pub fn from_ptr<T>(p: *const T) -> Word {
        let raw = p as u64;
        debug_assert!(raw & 0b111 == 0, "heap pointer must be 8-byte aligned");
        Word(raw | 1)
    }

    /// Raw 64-bit pattern. For diagnostics and the JIT ABI bridge.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Wrap a raw 64-bit pattern. For reading values back out of the
    /// JIT's i64 return slot.
    pub const fn from_raw(bits: u64) -> Word {
        Word(bits)
    }

    pub const fn is_fixnum(self) -> bool {
        (self.0 & 1) == 0
    }

    pub const fn is_pointer(self) -> bool {
        (self.0 & 1) == 1
    }

    /// Decode a fixnum payload. Returns `None` for pointer-tagged words.
    pub fn as_fixnum(self) -> Option<i64> {
        if self.is_fixnum() {
            // Arithmetic shift right preserves sign on signed casts.
            Some((self.0 as i64) >> 1)
        } else {
            None
        }
    }

    /// Decode a pointer payload. Returns `None` for fixnum-tagged words
    /// AND for pointer-tagged words whose post-mask address isn't
    /// 8-byte aligned.
    ///
    /// The alignment check is structurally guaranteed for any
    /// `Word` produced by `from_ptr` (the constructor `debug_assert`s
    /// it), but a `Word::from_raw` round-trip from JIT'd code can
    /// surface a tagged-pointer-shaped value that doesn't actually
    /// point at a heap object — e.g. when 8 consecutive bytes of a
    /// `<byte-string>` payload happen to look like a tagged pointer
    /// and a runtime probe (`collection_size`, `try_simple_object_vector`,
    /// …) calls `as_ptr` on them while walking conservatively. Before
    /// this check, the misidentified pointer would deref into raw
    /// payload bytes and trip an alignment assertion deep inside the
    /// `Wrapper`-load. After this check, the runtime probe simply
    /// gets `None` and short-circuits to "not that class".
    ///
    /// This is a defensive guardrail, not a real fix — the proper
    /// solution is Sprint 11d (`gc.statepoint` precise roots so the
    /// runtime never has to guess what is a pointer). Until then,
    /// alignment validation here turns a class of crashes into
    /// silent fall-through.
    pub fn as_ptr<T>(self) -> Option<*const T> {
        if self.is_pointer() {
            let addr = self.0 & !1;
            if addr & 0b111 != 0 {
                diag_misaligned("as_ptr", self.0);
                return None;
            }
            Some(addr as *const T)
        } else {
            None
        }
    }

    /// Same as `as_ptr` but mutable.
    pub fn as_mut_ptr<T>(self) -> Option<*mut T> {
        if self.is_pointer() {
            let addr = self.0 & !1;
            if addr & 0b111 != 0 {
                diag_misaligned("as_mut_ptr", self.0);
                return None;
            }
            Some(addr as *mut T)
        } else {
            None
        }
    }
}

/// Sprint 11d diagnostic: when the alignment guard in `as_ptr` /
/// `as_mut_ptr` fires, dump the raw Word and a Rust backtrace iff the
/// env var `NOD_GC_DIAG` is set. Silent in production. The backtrace
/// shows which runtime probe called us with a tagged-pointer-shaped
/// Word that wasn't a real heap pointer — the call chain across the
/// JIT boundary is the missing piece we need to fix the upstream
/// arithmetic bug.
///
/// Decoding tip: ASCII bytes (e.g. source-text payload) show up as
/// printable characters when read little-endian, so `0x212f2f0a74692066`
/// reads as " it\n//!f".
fn diag_misaligned(site: &str, raw: u64) {
    if std::env::var_os("NOD_GC_DIAG").is_none() {
        return;
    }
    let addr = raw & !1;
    // Decode the raw bytes as ASCII for the common "byte-string payload
    // pretending to be a pointer" case. Non-printable bytes show as '.'.
    let mut ascii = [b'.'; 8];
    for i in 0..8 {
        let b = ((raw >> (i * 8)) & 0xff) as u8;
        ascii[i] = if (0x20..0x7f).contains(&b) { b } else { b'.' };
    }
    let ascii_s = std::str::from_utf8(&ascii).unwrap_or("????????");
    eprintln!(
        "[NOD_GC_DIAG] Word::{site} alignment guard: raw=0x{raw:016x} \
         addr=0x{addr:016x} ascii=\"{ascii_s}\"\n{}",
        std::backtrace::Backtrace::capture()
    );
}

impl fmt::Debug for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(n) = self.as_fixnum() {
            write!(f, "Word::fixnum({n})")
        } else {
            // SAFETY: is_pointer() implies bit 0 is set; we untag and
            // print only the address, never dereferencing.
            write!(f, "Word::ptr({:#x})", self.0 & !1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_size_and_align() {
        assert_eq!(std::mem::size_of::<Word>(), 8);
        assert_eq!(std::mem::align_of::<Word>(), 8);
    }

    #[test]
    fn zero_round_trip() {
        let w = Word::from_fixnum(0).unwrap();
        assert!(w.is_fixnum());
        assert_eq!(w.as_fixnum(), Some(0));
        assert_eq!(w.raw(), 0);
    }

    #[test]
    fn positive_fixnum_round_trip() {
        for n in [1_i64, 5, 42, 1_000_000, FIXNUM_MAX] {
            let w = Word::from_fixnum(n).expect("in range");
            assert!(w.is_fixnum(), "{n}");
            assert!(!w.is_pointer(), "{n}");
            assert_eq!(w.as_fixnum(), Some(n));
        }
    }

    #[test]
    fn negative_fixnum_round_trip() {
        // Negative values exercise the arithmetic-shift sign extension.
        for n in [-1_i64, -42, -1_000_000, FIXNUM_MIN] {
            let w = Word::from_fixnum(n).expect("in range");
            assert!(w.is_fixnum(), "{n}");
            assert_eq!(w.as_fixnum(), Some(n));
        }
    }

    #[test]
    fn fixnum_bounds() {
        assert!(Word::from_fixnum(FIXNUM_MAX).is_ok());
        assert!(Word::from_fixnum(FIXNUM_MIN).is_ok());
        assert!(Word::from_fixnum(FIXNUM_MAX + 1).is_err());
        assert!(Word::from_fixnum(FIXNUM_MIN - 1).is_err());
        assert!(Word::from_fixnum(i64::MAX).is_err());
        assert!(Word::from_fixnum(i64::MIN).is_err());
    }

    #[test]
    fn fixnum_arithmetic_stable() {
        // (a << 1) + (b << 1) = (a + b) << 1 — no untag needed.
        let a = Word::from_fixnum(123).unwrap();
        let b = Word::from_fixnum(456).unwrap();
        let sum = Word(a.raw().wrapping_add(b.raw()));
        assert_eq!(sum.as_fixnum(), Some(579));
    }

    #[test]
    fn fixnum_neg_stable() {
        // -x on a tagged value: 0 - x. Both operands have bit 0 = 0,
        // and the result has bit 0 = 0 too.
        let a = Word::from_fixnum(7).unwrap();
        let neg = Word(0u64.wrapping_sub(a.raw()));
        assert_eq!(neg.as_fixnum(), Some(-7));
    }

    #[test]
    fn pointer_round_trip() {
        let dummy: u64 = 0;
        let p = (&dummy as *const u64) as *const u8;
        let w = Word::from_ptr(p);
        assert!(w.is_pointer());
        assert!(!w.is_fixnum());
        assert_eq!(w.as_ptr::<u8>(), Some(p));
        assert_eq!(w.as_fixnum(), None);
    }

    #[test]
    fn fixnum_overflow_carries_value() {
        let err = Word::from_fixnum(i64::MAX).unwrap_err();
        assert_eq!(err.value, i64::MAX);
    }
}
