//! Source spans and the source map.
//!
//! Per `specs/01-lexer.md` §4: a `Span` is a `(file_id, lo, hi)` byte
//! range into a `SourceMap`-owned source buffer. Line/column information
//! is derived lazily — tokens stay small.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Interned identifier for a source file. 32 bits — supports up to 4 billion
/// distinct files per process, way more than enough.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct FileId(pub u32);

/// A byte range `[lo, hi)` inside a specific source file.
///
/// `lo` and `hi` are UTF-8 byte offsets, **not** char counts. Spans are
/// 32-bit; source files larger than 4 GiB are rejected at load time.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Span {
    pub file_id: FileId,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(file_id: FileId, lo: u32, hi: u32) -> Self {
        debug_assert!(lo <= hi, "span lo ({lo}) > hi ({hi})");
        Self { file_id, lo, hi }
    }

    pub fn len(&self) -> u32 {
        self.hi - self.lo
    }

    pub fn is_empty(&self) -> bool {
        self.lo == self.hi
    }
}

/// Owns the source text of each loaded file and caches line-offset tables
/// for fast `(line, col)` lookups.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

struct SourceFile {
    path: PathBuf,
    src: Arc<str>,
    /// Byte offsets of the *start* of each line. `line_starts[0] == 0`.
    /// Built lazily on first `line_col` lookup.
    line_starts: std::sync::OnceLock<Vec<u32>>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a source file. Returns its `FileId`. Source larger than
    /// 4 GiB is rejected (see `SourceMapError::TooLarge`).
    pub fn add(
        &mut self,
        path: impl Into<PathBuf>,
        src: impl Into<Arc<str>>,
    ) -> Result<FileId, SourceMapError> {
        let src: Arc<str> = src.into();
        if src.len() > u32::MAX as usize {
            return Err(SourceMapError::TooLarge(src.len()));
        }
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile {
            path: path.into(),
            src,
            line_starts: std::sync::OnceLock::new(),
        });
        Ok(id)
    }

    pub fn path(&self, id: FileId) -> &Path {
        &self.files[id.0 as usize].path
    }

    pub fn source(&self, id: FileId) -> &str {
        &self.files[id.0 as usize].src
    }

    /// Borrow the slice of source covered by a span.
    pub fn slice(&self, span: Span) -> &str {
        let src = self.source(span.file_id);
        &src[span.lo as usize..span.hi as usize]
    }

    /// 1-based line and column for a byte offset.
    pub fn line_col(&self, file_id: FileId, offset: u32) -> (u32, u32) {
        let file = &self.files[file_id.0 as usize];
        let line_starts = file
            .line_starts
            .get_or_init(|| compute_line_starts(&file.src));

        // Binary search: find the greatest line_start <= offset.
        let idx = match line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line = (idx as u32) + 1;
        let col = offset - line_starts[idx] + 1;
        (line, col)
    }
}

fn compute_line_starts(src: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            // Line starts at the byte after '\n'.
            starts.push((i + 1) as u32);
        }
    }
    starts
}

#[derive(Debug)]
pub enum SourceMapError {
    TooLarge(usize),
}

impl std::fmt::Display for SourceMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(n) => write!(f, "source file too large ({n} bytes); maximum 4 GiB"),
        }
    }
}

impl std::error::Error for SourceMapError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_basic() {
        let mut sm = SourceMap::new();
        let id = sm.add("test.dylan", "abc\ndef\n\nghi").unwrap();
        assert_eq!(sm.line_col(id, 0), (1, 1));
        assert_eq!(sm.line_col(id, 2), (1, 3));
        assert_eq!(sm.line_col(id, 4), (2, 1)); // first char of "def"
        assert_eq!(sm.line_col(id, 9), (4, 1)); // first char of "ghi"
    }

    #[test]
    fn slice_round_trips() {
        let mut sm = SourceMap::new();
        let id = sm.add("test.dylan", "foo bar baz").unwrap();
        let span = Span::new(id, 4, 7);
        assert_eq!(sm.slice(span), "bar");
    }
}
