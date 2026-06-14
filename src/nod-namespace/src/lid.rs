//! LID-file parser. Grammar per `specs/05-library-module-graph.md` §3.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    Dll,
    Executable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct Lid {
    pub path: PathBuf,
    pub library: Option<String>,
    pub files: Vec<String>,
    pub target_type: Option<TargetType>,
    pub executable: Option<String>,
    pub start_function: Option<String>,
    pub major_version: Option<u32>,
    pub minor_version: Option<u32>,
    pub include: Option<String>,
    pub platforms: Vec<String>,
    pub base_address: Option<String>,
    pub other: Vec<(String, String)>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse_lid(path: &Path) -> io::Result<Lid> {
    let text = fs::read_to_string(path)?;
    Ok(parse_lid_str(&text, path.to_path_buf()))
}

pub fn parse_lid_str(text: &str, path: PathBuf) -> Lid {
    let mut lid = Lid {
        path,
        ..Default::default()
    };

    // First pass: collapse continuation lines into (line_no, key, value)
    // records. A continuation line is one whose first character is whitespace
    // (any whitespace counts).
    let mut records: Vec<(usize, String, String)> = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let starts_with_ws = raw.starts_with(|c: char| c.is_whitespace());
        if starts_with_ws {
            if let Some(last) = records.last_mut() {
                last.2.push(' ');
                last.2.push_str(raw.trim());
            } else {
                lid.diagnostics.push(Diagnostic {
                    line: line_no,
                    message: "continuation line before any header".into(),
                });
            }
            continue;
        }
        match raw.split_once(':') {
            Some((k, v)) => {
                records.push((line_no, k.trim().to_string(), v.trim().to_string()));
            }
            None => {
                lid.diagnostics.push(Diagnostic {
                    line: line_no,
                    message: format!("not a header line: {raw:?}"),
                });
            }
        }
    }

    for (line_no, key, value) in records {
        apply_field(&mut lid, line_no, &key, &value);
    }

    lid
}

fn apply_field(lid: &mut Lid, line_no: usize, key: &str, value: &str) {
    let k = key.to_ascii_lowercase();
    match k.as_str() {
        "library" => lid.library = Some(value.to_string()),
        "files" => {
            for tok in value.split_whitespace() {
                lid.files.push(tok.to_string());
            }
        }
        "target-type" => {
            let v = value.to_ascii_lowercase();
            match v.as_str() {
                "dll" => lid.target_type = Some(TargetType::Dll),
                "executable" => lid.target_type = Some(TargetType::Executable),
                _ => lid.diagnostics.push(Diagnostic {
                    line: line_no,
                    message: format!("unknown target-type: {value:?}"),
                }),
            }
        }
        "executable" => lid.executable = Some(value.to_string()),
        "start-function" => lid.start_function = Some(value.to_string()),
        "major-version" => match value.parse() {
            Ok(n) => lid.major_version = Some(n),
            Err(_) => lid.diagnostics.push(Diagnostic {
                line: line_no,
                message: format!("major-version: not a u32: {value:?}"),
            }),
        },
        "minor-version" => match value.parse() {
            Ok(n) => lid.minor_version = Some(n),
            Err(_) => lid.diagnostics.push(Diagnostic {
                line: line_no,
                message: format!("minor-version: not a u32: {value:?}"),
            }),
        },
        "lid" => lid.include = Some(value.to_string()),
        "platforms" => {
            for tok in value.split_whitespace() {
                lid.platforms.push(tok.to_string());
            }
        }
        "base-address" => lid.base_address = Some(value.to_string()),
        _ => lid.other.push((key.to_string(), value.to_string())),
    }
}

/// Resolve `LID:` includes and merge per spec §3 "LID inheritance".
pub fn load_lid_chain(path: &Path) -> io::Result<Lid> {
    let child = parse_lid(path)?;
    let Some(ref include) = child.include else {
        return Ok(child);
    };
    let parent_path = path
        .parent()
        .map(|p| p.join(include))
        .unwrap_or_else(|| PathBuf::from(include));
    let parent = load_lid_chain(&parent_path)?;
    Ok(merge_lid(parent, child))
}

fn merge_lid(parent: Lid, child: Lid) -> Lid {
    // Child shadows parent for single-valued fields; child extends parent for
    // list-valued fields.
    let mut files = parent.files;
    files.extend(child.files);
    let mut platforms = parent.platforms;
    platforms.extend(child.platforms);
    let mut other = parent.other;
    other.extend(child.other);
    let mut diagnostics = parent.diagnostics;
    diagnostics.extend(child.diagnostics);
    Lid {
        path: child.path,
        library: child.library.or(parent.library),
        files,
        target_type: child.target_type.or(parent.target_type),
        executable: child.executable.or(parent.executable),
        start_function: child.start_function.or(parent.start_function),
        major_version: child.major_version.or(parent.major_version),
        minor_version: child.minor_version.or(parent.minor_version),
        include: child.include,
        platforms,
        base_address: child.base_address.or(parent.base_address),
        other,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_keys() {
        let src = "Library: foo\nfiles: a b\nFILES: c\n";
        let lid = parse_lid_str(src, PathBuf::from("x.lid"));
        assert_eq!(lid.library.as_deref(), Some("foo"));
        assert_eq!(lid.files, vec!["a", "b", "c"]);
    }

    #[test]
    fn continuation_lines() {
        let src = "Files: one\n       two\n\tthree\nLibrary: l\n";
        let lid = parse_lid_str(src, PathBuf::from("x.lid"));
        assert_eq!(lid.files, vec!["one", "two", "three"]);
        assert_eq!(lid.library.as_deref(), Some("l"));
    }

    #[test]
    fn target_type_parsing() {
        let src = "library: x\ntarget-type: executable\n";
        let lid = parse_lid_str(src, PathBuf::from("x.lid"));
        assert_eq!(lid.target_type, Some(TargetType::Executable));
    }

    #[test]
    fn unknown_keys_become_other() {
        let src = "Library: l\nCopyright: 2026\nSynopsis: hi\n";
        let lid = parse_lid_str(src, PathBuf::from("x.lid"));
        assert_eq!(lid.other.len(), 2);
        assert!(lid.diagnostics.is_empty());
    }
}
