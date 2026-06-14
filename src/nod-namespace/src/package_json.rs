//! `dylan-package.json` parser per `specs/05-library-module-graph.md` §4.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDep {
    pub name: String,
    pub version_spec: String,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub path: PathBuf,
    pub name: String,
    pub version: String,
    pub dependencies: Vec<PackageDep>,
    pub dev_dependencies: Vec<PackageDep>,
    pub category: Option<String>,
    pub contact: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub license: Option<String>,
    pub license_url: Option<String>,
}

pub fn parse_package_json(path: &Path) -> io::Result<Package> {
    let text = fs::read_to_string(path)?;
    parse_package_json_str(&text, path.to_path_buf())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn parse_package_json_str(text: &str, path: PathBuf) -> Result<Package, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;

    let name = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let version = v
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let dependencies = collect_deps(v.get("dependencies"));
    let dev_dependencies = collect_deps(v.get("dev-dependencies"));

    let opt = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);

    Ok(Package {
        path,
        name,
        version,
        dependencies,
        dev_dependencies,
        category: opt("category"),
        contact: opt("contact"),
        description: opt("description"),
        url: opt("url"),
        license: opt("license"),
        license_url: opt("license-url"),
    })
}

fn collect_deps(v: Option<&Value>) -> Vec<PackageDep> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(Value::as_str)
        .map(parse_dep)
        .collect()
}

fn parse_dep(spec: &str) -> PackageDep {
    match spec.split_once('@') {
        Some((n, v)) => PackageDep {
            name: n.to_string(),
            version_spec: v.to_string(),
        },
        None => PackageDep {
            name: spec.to_string(),
            version_spec: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal() {
        let src = r#"{"name":"x","version":"1.0"}"#;
        let p = parse_package_json_str(src, PathBuf::from("p.json")).unwrap();
        assert_eq!(p.name, "x");
        assert_eq!(p.version, "1.0");
        assert!(p.dependencies.is_empty());
    }

    #[test]
    fn deps_split_at_first_at() {
        let p = parse_package_json_str(
            r#"{"name":"x","version":"1","dependencies":["a@1.2","b"]}"#,
            PathBuf::from("p.json"),
        )
        .unwrap();
        assert_eq!(p.dependencies.len(), 2);
        assert_eq!(p.dependencies[0].name, "a");
        assert_eq!(p.dependencies[0].version_spec, "1.2");
        assert_eq!(p.dependencies[1].name, "b");
        assert_eq!(p.dependencies[1].version_spec, "");
    }

    #[test]
    fn tolerates_extra_fields() {
        let p = parse_package_json_str(
            r#"{"name":"x","version":"1","extra":42,"nested":{"a":1}}"#,
            PathBuf::from("p.json"),
        )
        .unwrap();
        assert_eq!(p.name, "x");
    }
}
