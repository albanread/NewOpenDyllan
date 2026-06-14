//! Sprint 29 — Win32 constants generator.
//!
//! Reads `data/win32_constants.txt` (the human-curated source of
//! truth — Sprint 29 Phase A established that `windows_api.db`
//! schema v5 carries enum-type rows but NOT the integer values of
//! their members) and emits
//! `src/nod-dylan/dylan-sources/stdlib/win32-constants.dylan`. The
//! generated file is auto-loaded by the stdlib loader (see
//! `nod-sema/src/stdlib.rs`) so user-code expressions like
//! `$MB-OK` resolve to the integer value at lowering time.
//!
//! Usage from the workspace root:
//!
//! ```text
//! cargo run --quiet -p nod-winapi --bin generate_constants
//! ```
//!
//! The script is idempotent — re-running over an unchanged source
//! produces a byte-identical output. CI doesn't need to invoke
//! this; the generated `.dylan` file is checked in. We re-run by
//! hand when `data/win32_constants.txt` is updated.
//!
//! ## Naming convention
//!
//! Win32 spelling                 →  Dylan spelling
//! ---------------------------------+----------------------------
//! `MB_OK`                        →  `$MB-OK`
//! `WM_PAINT`                     →  `$WM-PAINT`
//! `WS_OVERLAPPEDWINDOW`          →  `$WS-OVERLAPPEDWINDOW`
//! `GWL_STYLE` (value `-16`)      →  `$GWL-STYLE`, value `-16`
//!
//! The `$` prefix is Dylan's marker for module-level constants;
//! underscores become hyphens; case is preserved (Dylan is
//! case-insensitive, but Win32 spelling stays UPPER for grep-ability
//! and consistency with C/C++ source).
//!
//! ## Value formatting
//!
//! Small magnitudes (`|v| < 256`) emit as decimal. Larger values
//! emit as `#xHEX` (Dylan hex literal syntax) so flag bits stay
//! visually obvious. Negative values emit as `-DEC` (rare — the
//! curated file uses them for `GWL_*` and `STD_*_HANDLE` offsets).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let workspace_root = workspace_root_from_cwd_or_env();
    let src_path = workspace_root.join("data").join("win32_constants.txt");
    let out_path = workspace_root
        .join("src")
        .join("nod-dylan")
        .join("dylan-sources")
        .join("stdlib")
        .join("win32-constants.dylan");

    let src = fs::read_to_string(&src_path).unwrap_or_else(|e| {
        eprintln!(
            "generate_constants: cannot read {} — {e}",
            src_path.display()
        );
        std::process::exit(2);
    });

    let entries = parse_source(&src, &src_path);
    let total = entries.iter().filter(|e| matches!(e, Entry::Constant { .. })).count();
    let dylan = emit_dylan(&entries, total);
    fs::write(&out_path, &dylan).unwrap_or_else(|e| {
        eprintln!(
            "generate_constants: cannot write {} — {e}",
            out_path.display()
        );
        std::process::exit(3);
    });
    eprintln!(
        "generate_constants: wrote {} ({total} constants) to {}",
        out_path.display(),
        out_path.display()
    );
}

/// Resolve the workspace root. The binary runs under `cargo run`
/// from the workspace root in normal usage, but tolerate being
/// launched from `src/nod-winapi/` (e.g. an IDE run-action) by
/// walking upward until we find the workspace `Cargo.toml`.
fn workspace_root_from_cwd_or_env() -> PathBuf {
    if let Ok(s) = env::var("CARGO_WORKSPACE_DIR") {
        return PathBuf::from(s);
    }
    let cwd = env::current_dir().expect("cwd");
    let mut p: &Path = &cwd;
    loop {
        if p.join("Cargo.toml").is_file() && p.join("src").is_dir() && p.join("data").is_dir() {
            return p.to_path_buf();
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => {
                eprintln!(
                    "generate_constants: cannot locate workspace root from {}",
                    cwd.display()
                );
                std::process::exit(2);
            }
        }
    }
}

/// A line of the input file. We keep category headers so the
/// generated Dylan file is grouped the same way as the source.
#[derive(Debug)]
enum Entry {
    Category(String),
    Constant {
        name: String,
        value: i64,
        source_dll: Option<String>,
    },
}

fn parse_source(src: &str, path: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for (lineno, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Category header: lines of the form `# category: NAME`.
        if let Some(rest) = line.strip_prefix("# category:") {
            out.push(Entry::Category(rest.trim().to_string()));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (lhs, trailer) = match line.find(';') {
            Some(i) => (line[..i].trim_end(), Some(line[i + 1..].trim().to_string())),
            None => (line, None),
        };
        let Some(eq) = lhs.find('=') else {
            eprintln!(
                "generate_constants: bad line {} in {} — missing `=`",
                lineno + 1,
                path.display()
            );
            std::process::exit(2);
        };
        let name = lhs[..eq].trim().to_string();
        let value_str = lhs[eq + 1..].trim();
        let Some(value) = parse_int(value_str) else {
            eprintln!(
                "generate_constants: bad value on line {} in {} — {value_str:?}",
                lineno + 1,
                path.display()
            );
            std::process::exit(2);
        };
        out.push(Entry::Constant {
            name,
            value,
            source_dll: trailer,
        });
    }
    out
}

fn parse_int(s: &str) -> Option<i64> {
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r.trim())
    } else {
        (false, s)
    };
    let (radix, body) = if let Some(r) = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))
    {
        (16, r)
    } else {
        (10, rest)
    };
    let unsigned = u64::from_str_radix(body, radix).ok()?;
    if neg {
        let magnitude_i64 = i64::try_from(unsigned).ok()?;
        Some(-magnitude_i64)
    } else {
        Some(unsigned as i64)
    }
}

/// `MB_OK` → `$MB-OK`. Win32 spelling stays uppercase; underscores
/// become hyphens; `$` prefix marks a module-level constant.
fn dylan_name(win32: &str) -> String {
    let mut s = String::with_capacity(win32.len() + 1);
    s.push('$');
    for c in win32.chars() {
        if c == '_' {
            s.push('-');
        } else {
            s.push(c);
        }
    }
    s
}

/// Format an integer as Dylan source. Small magnitudes get decimal
/// (`0`, `1`, `-16`); larger values get `#x…` hex so flag bits stay
/// visually clear (`#x80000000`, `#xCC0020`).
fn dylan_int(v: i64) -> String {
    if v == i64::MIN {
        // No bare magnitude — emit the hex bit pattern.
        return format!("#x{:X}", v as u64);
    }
    let abs = v.unsigned_abs();
    if abs < 256 {
        format!("{v}")
    } else if v < 0 {
        // Negative-but-large: rare in our curated set. Emit as
        // signed decimal — easier to read than two's-complement hex.
        format!("{v}")
    } else {
        format!("#x{:X}", v as u64)
    }
}

fn emit_dylan(entries: &[Entry], total: usize) -> String {
    let mut out = String::new();
    out.push_str("Module: dylan\n");
    out.push_str("Author: NewOpenDylan Sprint 29 — generated bindings, do not edit by hand.\n");
    out.push('\n');
    out.push_str(&format!(
        "// Sprint 29 — Win32 integer constants ({total} total).\n"
    ));
    out.push_str("//\n");
    out.push_str("// Regenerate via:\n");
    out.push_str("//     cargo run --quiet -p nod-winapi --bin generate_constants\n");
    out.push_str("//\n");
    out.push_str("// Source of truth: data/win32_constants.txt. The vendored\n");
    out.push_str("// windows_api.db (schema v5) carries enum *type* declarations\n");
    out.push_str("// but NOT the integer values of their members, so Sprint 29\n");
    out.push_str("// curates the most-used Win32 constants by hand. A future\n");
    out.push_str("// sprint that extends the upstream DB with an enum-members\n");
    out.push_str("// table can add a DB-extraction pass to build.rs alongside\n");
    out.push_str("// the curated set; this file's layout doesn't change.\n");
    out.push('\n');
    for e in entries {
        match e {
            Entry::Category(name) => {
                // Pad section headers to a consistent ~74-char width so
                // they scan as visual separators in the generated file.
                let header = format!("// ─── {name} ");
                let width = 74usize.saturating_sub(header.chars().count());
                out.push('\n');
                out.push_str(&header);
                for _ in 0..width {
                    out.push('─');
                }
                out.push('\n');
                out.push('\n');
            }
            Entry::Constant { name, value, source_dll } => {
                let lhs = dylan_name(name);
                let rhs = dylan_int(*value);
                match source_dll {
                    Some(dll) => out.push_str(&format!(
                        "define constant {lhs} = {rhs};  // {dll}\n"
                    )),
                    None => out.push_str(&format!("define constant {lhs} = {rhs};\n")),
                }
            }
        }
    }
    out
}
