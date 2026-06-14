//! Sprint 49 — `.prj` project files.
//!
//! A project file is a TOML document that describes a single AOT
//! build target. The driver's `build` command accepts either the
//! traditional positional file-list (`nod-driver build a.dylan
//! b.dylan -o foo.exe`) OR `--project foo.prj`, which expands into
//! the same internal state — file list + output path.
//!
//! # Why
//!
//! - The IDE needs to know "what files belong together." A `.prj`
//!   answers that with one path instead of N.
//! - Multi-file builds with the same module name across files (the
//!   Sprint 44 model) require listing every file every time. A
//!   project file factors that out.
//! - The `nod-driver` itself currently hardcodes the parser EXE's
//!   two-file source list (`dylan-lexer.dylan` + `dylan-parser.dylan`)
//!   inside `main.rs`. The intent is to migrate that to a `.prj` so
//!   `nod-driver parse-dylan` ships an EXE built from a real project
//!   file. Sprint 49 stays focused on the build path; the parse-dylan
//!   migration is a follow-up.
//!
//! # Minimum-viable schema
//!
//! ```toml
//! name    = "nod-ide"
//! sources = ["nod-ide.dylan", "ide-render.dylan", "ide-input.dylan"]
//! output  = "nod-ide.exe"   # optional; defaults to `<name>.exe`
//! ```
//!
//! That's everything. Future additions get optional fields with
//! defaults that preserve today's CLI behavior — `Option<Vec<String>>`
//! for `lib_deps`, etc.
//!
//! # Path semantics — non-negotiable
//!
//! Every relative path in a project file is resolved against the
//! **project file's parent directory**, NOT the caller's CWD.
//! Without that, `nod-driver build --project src/foo.prj` from the
//! repo root would silently look for `nod-ide.dylan` in the repo
//! root rather than in `src/`. Make-style "CWD-relative" semantics
//! would also break IDE navigation (the IDE opens a `.prj` from
//! anywhere, and the resolved file paths must match what's on
//! disk).
//!
//! All resolution happens in [`Project::load`]; consumers receive
//! a [`ResolvedProject`] with absolute paths.

use std::path::{Path, PathBuf};

/// Raw on-disk schema. Wire format only; consumers should use
/// [`ResolvedProject`] (the post-load value) instead — its paths are
/// already anchored to the project file's directory.
#[derive(Debug, serde::Deserialize)]
struct RawProject {
    name: String,
    sources: Vec<String>,
    output: Option<String>,
    /// Sprint 50d — name of the Dylan-source function that should
    /// serve as the program's entry point. Defaults to `"main"` for
    /// back-compat with every existing `.prj`. Set to something else
    /// when bundling files that all happen to define `main` (e.g.
    /// bundling `dylan-parser.dylan` — whose entry is `main` — with
    /// a smoke test that wants its own entry name).
    start_function: Option<String>,
}

/// Loaded + path-resolved project. Field semantics:
///
/// * `name` — display name, used as the LLVM module label and the
///   default output stem.
/// * `sources` — absolute paths to every `.dylan` source file, in
///   the order they appear in the TOML. Compilation order matters:
///   each file's lowered IR is appended to the merge buffer in this
///   order, so later files can reference items defined in earlier
///   ones.
/// * `output` — absolute path to the EXE to produce. Always
///   populated (either from the explicit `output =` field or the
///   default `<project_dir>/<name>.exe`).
/// * `project_path` — kept for diagnostic / IDE use ("what file did
///   this build come from?").
#[derive(Debug, Clone)]
pub struct ResolvedProject {
    pub name: String,
    pub sources: Vec<PathBuf>,
    pub output: PathBuf,
    pub project_path: PathBuf,
    /// Sprint 50d — Dylan-source function name that becomes the EXE
    /// entry. Always populated; defaults to `"main"` when the .prj
    /// omits the field. The AOT pipeline renames this LLVM-side
    /// function to `nod_user_main` (the symbol the runtime wrapper
    /// `extern`s); choosing a different value lets a bundle resolve
    /// duplicate-`main` collisions between source files.
    pub start_function: String,
}

#[derive(Debug)]
pub enum LoadError {
    /// IO error reading the project file itself.
    Io { path: PathBuf, err: std::io::Error },
    /// TOML parse error.
    Parse { path: PathBuf, err: toml::de::Error },
    /// Schema-level error: empty source list.
    EmptySources { path: PathBuf },
    /// Schema-level error: `name` is empty.
    EmptyName { path: PathBuf },
    /// Could not canonicalize the project file path.
    CanonicalizeProject { path: PathBuf, err: std::io::Error },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, err } => {
                write!(f, "reading project file {}: {err}", path.display())
            }
            Self::Parse { path, err } => {
                write!(f, "parsing project file {}: {err}", path.display())
            }
            Self::EmptySources { path } => write!(
                f,
                "project file {}: `sources` must list at least one .dylan path",
                path.display()
            ),
            Self::EmptyName { path } => write!(
                f,
                "project file {}: `name` must be a non-empty string",
                path.display()
            ),
            Self::CanonicalizeProject { path, err } => write!(
                f,
                "canonicalizing project file path {}: {err}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for LoadError {}

impl ResolvedProject {
    /// Read a project file from disk, validate it, and resolve every
    /// relative path against the project file's parent directory.
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        // Canonicalize the project file first so the anchor directory is
        // unambiguous. `canonicalize` on Windows yields a `\\?\` path —
        // that's fine for subsequent file IO and the absolute form is
        // what we want anyway.
        let canon = path
            .canonicalize()
            .map_err(|err| LoadError::CanonicalizeProject {
                path: path.to_path_buf(),
                err,
            })?;
        let text = std::fs::read_to_string(&canon).map_err(|err| LoadError::Io {
            path: canon.clone(),
            err,
        })?;
        let raw: RawProject = toml::from_str(&text).map_err(|err| LoadError::Parse {
            path: canon.clone(),
            err,
        })?;
        if raw.name.trim().is_empty() {
            return Err(LoadError::EmptyName { path: canon });
        }
        if raw.sources.is_empty() {
            return Err(LoadError::EmptySources { path: canon });
        }
        let anchor = canon
            .parent()
            .map(Path::to_path_buf)
            // A project file at filesystem root has no parent — treat
            // its anchor as ".".
            .unwrap_or_else(|| PathBuf::from("."));
        let sources: Vec<PathBuf> = raw
            .sources
            .iter()
            .map(|s| resolve(&anchor, s))
            .collect();
        let output = match raw.output {
            Some(o) if !o.trim().is_empty() => resolve(&anchor, &o),
            _ => anchor.join(format!("{}.exe", raw.name)),
        };
        let start_function = match raw.start_function {
            Some(s) if !s.trim().is_empty() => s,
            _ => "main".to_string(),
        };
        Ok(Self {
            name: raw.name,
            sources,
            output,
            project_path: canon,
            start_function,
        })
    }
}

/// Resolve `rel` against `anchor` if it's relative; pass through
/// absolute paths unchanged.
fn resolve(anchor: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        anchor.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_prj(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("test.prj");
        std::fs::write(&p, body).expect("write prj");
        p
    }

    #[test]
    fn minimal_project_loads() {
        let dir = tempdir();
        // Create the source so canonicalization works on Windows.
        std::fs::write(dir.path().join("foo.dylan"), "// stub").unwrap();
        let path = write_prj(
            dir.path(),
            r#"
name = "foo"
sources = ["foo.dylan"]
"#,
        );
        let p = ResolvedProject::load(&path).expect("load");
        assert_eq!(p.name, "foo");
        assert_eq!(p.sources.len(), 1);
        assert!(p.sources[0].ends_with("foo.dylan"));
        assert!(p.output.ends_with("foo.exe"));
    }

    #[test]
    fn start_function_defaults_to_main() {
        let dir = tempdir();
        std::fs::write(dir.path().join("foo.dylan"), "").unwrap();
        let path = write_prj(
            dir.path(),
            r#"
name = "foo"
sources = ["foo.dylan"]
"#,
        );
        let p = ResolvedProject::load(&path).expect("load");
        assert_eq!(p.start_function, "main");
    }

    #[test]
    fn explicit_start_function_honored() {
        let dir = tempdir();
        std::fs::write(dir.path().join("foo.dylan"), "").unwrap();
        let path = write_prj(
            dir.path(),
            r#"
name = "foo"
sources = ["foo.dylan"]
start_function = "my-entry"
"#,
        );
        let p = ResolvedProject::load(&path).expect("load");
        assert_eq!(p.start_function, "my-entry");
    }

    #[test]
    fn explicit_output_honored() {
        let dir = tempdir();
        std::fs::write(dir.path().join("foo.dylan"), "").unwrap();
        let path = write_prj(
            dir.path(),
            r#"
name = "foo"
sources = ["foo.dylan"]
output = "bin/special.exe"
"#,
        );
        let p = ResolvedProject::load(&path).expect("load");
        assert!(p.output.ends_with("bin/special.exe") || p.output.ends_with("bin\\special.exe"));
    }

    #[test]
    fn relative_paths_anchor_at_project_dir_not_cwd() {
        let dir = tempdir();
        let sub = dir.path().join("nested");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.dylan"), "").unwrap();
        let path = write_prj(
            &sub,
            r#"
name = "nested"
sources = ["a.dylan"]
"#,
        );
        let p = ResolvedProject::load(&path).expect("load");
        // The resolved source must live under `sub`, regardless of
        // what the test's CWD happens to be.
        assert!(p.sources[0].starts_with(&sub) || p.sources[0]
            .to_string_lossy()
            .contains("nested"));
    }

    #[test]
    fn empty_sources_is_an_error() {
        let dir = tempdir();
        let path = write_prj(
            dir.path(),
            r#"
name = "x"
sources = []
"#,
        );
        match ResolvedProject::load(&path) {
            Err(LoadError::EmptySources { .. }) => {}
            other => panic!("expected EmptySources, got {other:?}"),
        }
    }

    #[test]
    fn empty_name_is_an_error() {
        let dir = tempdir();
        std::fs::write(dir.path().join("a.dylan"), "").unwrap();
        let path = write_prj(
            dir.path(),
            r#"
name = ""
sources = ["a.dylan"]
"#,
        );
        match ResolvedProject::load(&path) {
            Err(LoadError::EmptyName { .. }) => {}
            other => panic!("expected EmptyName, got {other:?}"),
        }
    }

    #[test]
    fn parse_errors_surface_with_path() {
        let dir = tempdir();
        let path = write_prj(dir.path(), "this is not valid toml = = =");
        match ResolvedProject::load(&path) {
            Err(LoadError::Parse { path: p, .. }) => assert!(p.ends_with("test.prj")),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    // Plain unique-temp-dir helper. We don't take a `tempfile` dep just
    // for tests; `std::env::temp_dir()` plus a random component is
    // enough.
    fn tempdir() -> Tempdir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("nod-driver-prj-test-{pid}-{n}"));
        std::fs::create_dir_all(&p).expect("create tempdir");
        Tempdir(p)
    }

    struct Tempdir(PathBuf);
    impl Tempdir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tempdir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
