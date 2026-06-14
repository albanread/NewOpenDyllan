//! Precise root set for the tracer.
//!
//! Sprint 10 only models static roots — pointers to `Word` cells whose
//! values are GC roots for the duration of the session. Examples:
//! the boolean / nil immediates, the literal-pool entries baked into
//! JIT'd modules, REPL top-level bindings.
//!
//! Sprint 11 will extend `RootSet` with stack maps (precise frame
//! roots emitted via `gc.statepoint`); the collector reads both.

use crate::word::Word;

/// Append-only collection of static roots. Each entry is a raw pointer
/// to a `Word` slot; the tracer dereferences it to obtain the current
/// root value.
pub struct RootSet {
    /// Pointers to `Word` slots. Each pointee is a single tagged Dylan
    /// value the runtime considers always-live.
    pub statics: Vec<*const Word>,
}

// SAFETY: the raw pointers in `statics` are user-managed; `RootSet`
// itself doesn't dereference them. Sprint 10 single-threaded use is
// fine. Sprint 11 will revisit when the collector runs concurrently.
unsafe impl Send for RootSet {}
unsafe impl Sync for RootSet {}

impl RootSet {
    pub fn new() -> Self {
        Self { statics: Vec::new() }
    }

    /// Register a pointer to a `Word` slot as a root. The pointee must
    /// outlive the trace; in practice this is always a slot in a
    /// pinned static or in JIT-baked module data.
    pub fn add_static(&mut self, root: *const Word) {
        self.statics.push(root);
    }

    pub fn len(&self) -> usize {
        self.statics.len()
    }

    pub fn is_empty(&self) -> bool {
        self.statics.is_empty()
    }
}

impl Default for RootSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_static_grows_vec() {
        let mut rs = RootSet::new();
        let w = Word::from_fixnum(0).unwrap();
        rs.add_static(&w as *const Word);
        assert_eq!(rs.len(), 1);
    }
}
