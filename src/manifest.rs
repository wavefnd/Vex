use std::fs;
use std::path::{Path, PathBuf};

use wson_rs::{loads, WsonValue};

pub const MANIFEST_FILE: &str = "vex.ws";
const URL_SEPARATOR_SENTINEL: &str = "__VEX_WSON_URL_SEPARATOR__";

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DependencySource {
    Path {
        path: String,
    },
    Git {
        url: String,
        branch: Option<String>,
        tag: Option<String>,
        rev: Option<String>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub version: Option<String>,
    pub source: DependencySource,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub lib: bool,
    pub description: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub dependencies: Vec<Dependency>,
    pub source_path: PathBuf,
}

impl Manifest {
    pub fn load() -> Result<Self, String> {
        let source_path = Path::new(MANIFEST_FILE);
        if !source_path.is_file() {
            let directory = std::env::current_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            return Err(format!(
                "could not find `{MANIFEST_FILE}` in `{directory}`\nhelp: run `vex init` to create a package"
            ));
        }
        Self::load_from(source_path)
    }

    pub fn load_from(source_path: impl AsRef<Path>) -> Result<Self, String> {
        let source_path = source_path.as_ref().to_path_buf();
        if !source_path.is_file() {
            return Err(format!(
                "manifest not found at `{}`",
                source_path.to_string_lossy()
            ));
        }

        let raw = fs::read_to_string(&source_path)
            .map_err(|e| format!("failed to read `{}`: {e}", source_path.to_string_lossy()))?;

        parse_manifest(&raw, source_path.clone()).map_err(|err| {
            format!(
                "failed to load manifest `{}`: {err}",
                source_path.to_string_lossy()
            )
        })
    }

    pub fn default_entry_path(&self) -> PathBuf {
        if self.lib {
            PathBuf::from("src/lib.wave")
        } else {
            PathBuf::from("src/main.wave")
        }
    }
}

pub fn render_new_manifest(project_name: &str, author: &str, is_lib: bool) -> String {
    format!(
        r#"{{
    name = "{name}",
    version = 0.1.0,
    lib = {is_lib},
    description = "{name} Project",
    author = "{author}",
    license = "Unknown",
    dependencies = []
}}
"#,
        name = escape_wson_string(project_name),
        author = escape_wson_string(author)
    )
}

fn parse_manifest(raw: &str, source_path: PathBuf) -> Result<Manifest, String> {
    let protected_raw = protect_url_separators(raw);
    let data = loads(&protected_raw).map_err(|e| format!("failed to parse manifest: {e}"))?;

    let name = match data.get("name") {
        Some(WsonValue::String(value)) => restore_url_separators(value),
        _ => return Err("manifest field `name` must be a string".to_string()),
    };

    let version = match data.get("version") {
        Some(value) => parse_version_string(value)
            .ok_or_else(|| "manifest field `version` must be a version or string".to_string())?,
        None => "0.1.0".to_string(),
    };

    let lib = match data.get("lib") {
        Some(WsonValue::Bool(value)) => *value,
        None => false,
        _ => return Err("manifest field `lib` must be a bool".to_string()),
    };

    let description = parse_optional_string(data.get("description"))?;
    let author = parse_optional_string(data.get("author"))?;
    let license = parse_optional_string(data.get("license"))?;
    let dependencies = parse_dependencies(data.get("dependencies"))?;

    Ok(Manifest {
        name,
        version,
        lib,
        description,
        author,
        license,
        dependencies,
        source_path,
    })
}

fn parse_optional_string(value: Option<&WsonValue>) -> Result<Option<String>, String> {
    match value {
        Some(WsonValue::String(s)) => Ok(Some(restore_url_separators(s))),
        Some(_) => Err("manifest optional text field must be a string".to_string()),
        None => Ok(None),
    }
}

fn parse_dependencies(value: Option<&WsonValue>) -> Result<Vec<Dependency>, String> {
    let mut dependencies = Vec::new();

    let Some(WsonValue::Array(items)) = value else {
        if value.is_some() {
            return Err("manifest field `dependencies` must be an array".to_string());
        }
        return Ok(dependencies);
    };

    for item in items {
        let WsonValue::Object(obj) = item else {
            return Err("dependency entry must be an object".to_string());
        };

        let name = required_string(obj.get("name"), "dependency field `name`")?;
        validate_dependency_name(&name)?;

        let path = optional_string(obj.get("path"), "dependency field `path`")?;
        let git = optional_string(obj.get("git"), "dependency field `git`")?;
        let branch = optional_string(obj.get("branch"), "dependency field `branch`")?;
        let tag = optional_string(obj.get("tag"), "dependency field `tag`")?;
        let rev = optional_string(obj.get("rev"), "dependency field `rev`")?;
        let version = match obj.get("version") {
            Some(value) => Some(parse_version_string(value).ok_or_else(|| {
                format!("dependency `{name}` field `version` must be a version or string")
            })?),
            None => None,
        };

        let git_ref_count = branch.is_some() as u8 + tag.is_some() as u8 + rev.is_some() as u8;
        if git_ref_count > 1 {
            return Err(format!(
                "dependency `{name}` can specify only one of `branch`, `tag`, or `rev`"
            ));
        }

        let source = match (path, git) {
            (Some(path), None) => DependencySource::Path { path },
            (None, Some(url)) => DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            },
            (Some(_), Some(_)) => {
                return Err(format!(
                    "dependency `{name}` must specify only one of `path` or `git`"
                ));
            }
            (None, None) => {
                return Err(format!(
                    "dependency `{name}` must specify either `path` or `git`"
                ));
            }
        };

        dependencies.push(Dependency {
            name,
            version,
            source,
        });
    }

    Ok(dependencies)
}

fn required_string(value: Option<&WsonValue>, field: &str) -> Result<String, String> {
    match value {
        Some(WsonValue::String(value)) => Ok(restore_url_separators(value)),
        _ => Err(format!("{field} must be a string")),
    }
}

fn optional_string(value: Option<&WsonValue>, field: &str) -> Result<Option<String>, String> {
    match value {
        Some(WsonValue::String(value)) => Ok(Some(restore_url_separators(value))),
        Some(_) => Err(format!("{field} must be a string")),
        None => Ok(None),
    }
}

fn protect_url_separators(raw: &str) -> String {
    raw.replace("://", URL_SEPARATOR_SENTINEL)
}

fn restore_url_separators(value: &str) -> String {
    value.replace(URL_SEPARATOR_SENTINEL, "://")
}

fn parse_version_string(value: &WsonValue) -> Option<String> {
    match value {
        WsonValue::Version(v) => Some(
            v.iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("."),
        ),
        WsonValue::String(v) => Some(restore_url_separators(v)),
        _ => None,
    }
}

fn validate_dependency_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("dependency name cannot be empty".to_string());
    };

    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!(
            "invalid dependency name `{name}`: use [A-Za-z_][A-Za-z0-9_]*"
        ));
    }

    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(format!(
            "invalid dependency name `{name}`: use [A-Za-z_][A-Za-z0-9_]*"
        ));
    }

    Ok(())
}

fn escape_wson_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_path_and_git_dependencies() {
        let manifest = parse_manifest(
            r#"{
                name = "app",
                version = 0.1.0,
                dependencies = [
                    { name = "local", path = "../local" },
                    { name = "remote", git = "https://example.com/remote.git", tag = "v1" }
                ]
            }"#,
            PathBuf::from("vex.ws"),
        )
        .expect("manifest must parse");

        assert_eq!(manifest.dependencies.len(), 2);
        assert!(matches!(
            manifest.dependencies[0].source,
            DependencySource::Path { .. }
        ));
        assert!(matches!(
            manifest.dependencies[1].source,
            DependencySource::Git { .. }
        ));
    }

    #[test]
    fn rejects_ambiguous_dependency_sources() {
        let err = parse_manifest(
            r#"{
                name = "app",
                dependencies = [
                    { name = "bad", path = "../bad", git = "https://example.com/bad.git" }
                ]
            }"#,
            PathBuf::from("vex.ws"),
        )
        .expect_err("path and git cannot be combined");

        assert!(err.contains("path") && err.contains("git"), "{err}");
    }
}
