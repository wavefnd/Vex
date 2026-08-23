use std::fs;
use std::path::{Path, PathBuf};

use wson_rs::{loads, WsonMap, WsonValue};

pub const LOCKFILE_NAME: &str = "vex.lock";
pub const LOCKFILE_VERSION: i64 = 2;
const URL_SEPARATOR_SENTINEL: &str = "__VEX_LOCK_URL_SEPARATOR__";

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LockedSource {
    Path {
        requested: String,
        resolved: PathBuf,
    },
    Git {
        url: String,
        branch: Option<String>,
        tag: Option<String>,
        rev: Option<String>,
        commit: String,
        resolved: PathBuf,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: LockedSource,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Lockfile {
    pub version: i64,
    pub packages: Vec<LockedPackage>,
}

impl Lockfile {
    pub fn empty() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            packages: Vec::new(),
        }
    }

    pub fn normalized(mut self) -> Self {
        for package in &mut self.packages {
            package.dependencies.sort();
            package.dependencies.dedup();
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
        self
    }

    pub fn package(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|package| package.name == name)
    }
}

pub fn read_lockfile() -> Result<Option<Lockfile>, String> {
    let path = Path::new(LOCKFILE_NAME);
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read_to_string(path).map_err(|e| format!("failed to read `{LOCKFILE_NAME}`: {e}"))?;
    parse_lockfile(&raw).map(Some)
}

pub fn write_lockfile(lockfile: &Lockfile) -> Result<(), String> {
    let content = render_lockfile(lockfile.clone().normalized());
    fs::write(LOCKFILE_NAME, content).map_err(|e| format!("failed to write `{LOCKFILE_NAME}`: {e}"))
}

fn parse_lockfile(raw: &str) -> Result<Lockfile, String> {
    let protected = raw.replace("://", URL_SEPARATOR_SENTINEL);
    let data = loads(&protected).map_err(|e| format!("failed to parse `{LOCKFILE_NAME}`: {e}"))?;
    let version = match data.get("version") {
        Some(WsonValue::Int(value)) => *value,
        _ => {
            return Err(format!(
                "`{LOCKFILE_NAME}` field `version` must be an integer"
            ))
        }
    };

    if version == 1 {
        // Version 1 only mirrored direct manifest entries and did not pin commits.
        return Ok(Lockfile {
            version,
            packages: Vec::new(),
        });
    }
    if version != LOCKFILE_VERSION {
        return Err(format!(
            "unsupported `{LOCKFILE_NAME}` version `{version}`; expected `{LOCKFILE_VERSION}`"
        ));
    }

    let packages = match data.get("package") {
        Some(WsonValue::Array(items)) => items
            .iter()
            .map(parse_package)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(format!(
                "`{LOCKFILE_NAME}` field `package` must be an array"
            ))
        }
        None => Vec::new(),
    };

    let normalized = Lockfile { version, packages }.normalized();
    for pair in normalized.packages.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(format!(
                "`{LOCKFILE_NAME}` contains duplicate package `{}`",
                pair[0].name
            ));
        }
    }
    Ok(normalized)
}

fn parse_package(value: &WsonValue) -> Result<LockedPackage, String> {
    let WsonValue::Object(object) = value else {
        return Err("lockfile package entry must be an object".to_string());
    };

    let name = required_string(object, "name")?;
    let version = required_string(object, "version")?;
    let source = required_string(object, "source")?;
    let dependencies = optional_string_array(object, "dependencies")?;

    let source = match source.as_str() {
        "path" => LockedSource::Path {
            requested: required_string(object, "path")?,
            resolved: PathBuf::from(required_string(object, "resolved")?),
        },
        "git" => {
            let commit = required_string(object, "commit")?;
            validate_commit(&commit, &name)?;
            LockedSource::Git {
                url: required_string(object, "git")?,
                branch: optional_string(object, "branch")?,
                tag: optional_string(object, "tag")?,
                rev: optional_string(object, "rev")?,
                commit,
                resolved: PathBuf::from(required_string(object, "resolved")?),
            }
        }
        other => {
            return Err(format!(
                "unknown lockfile source `{other}` for package `{name}`"
            ))
        }
    };

    Ok(LockedPackage {
        name,
        version,
        source,
        dependencies,
    })
}

fn validate_commit(commit: &str, package: &str) -> Result<(), String> {
    if matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(format!(
        "lockfile package `{package}` has invalid Git commit `{commit}`; expected a full hexadecimal object ID"
    ))
}

fn required_string(object: &WsonMap, key: &str) -> Result<String, String> {
    match object.get(key) {
        Some(WsonValue::String(value)) => Ok(restore_url(value)),
        _ => Err(format!("lockfile field `{key}` must be a string")),
    }
}

fn optional_string(object: &WsonMap, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        Some(WsonValue::String(value)) => Ok(Some(restore_url(value))),
        Some(_) => Err(format!("lockfile field `{key}` must be a string")),
        None => Ok(None),
    }
}

fn optional_string_array(object: &WsonMap, key: &str) -> Result<Vec<String>, String> {
    match object.get(key) {
        Some(WsonValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                WsonValue::String(value) => Ok(restore_url(value)),
                _ => Err(format!("lockfile field `{key}` must contain only strings")),
            })
            .collect(),
        Some(_) => Err(format!("lockfile field `{key}` must be an array")),
        None => Ok(Vec::new()),
    }
}

fn restore_url(value: &str) -> String {
    value.replace(URL_SEPARATOR_SENTINEL, "://")
}

fn render_lockfile(lockfile: Lockfile) -> String {
    let blocks = lockfile
        .packages
        .iter()
        .map(render_package)
        .collect::<Vec<_>>();

    if blocks.is_empty() {
        return format!("{{\n    version = {LOCKFILE_VERSION},\n    package = []\n}}\n");
    }

    format!(
        "{{\n    version = {LOCKFILE_VERSION},\n    package = [\n{}\n    ]\n}}\n",
        blocks.join(",\n")
    )
}

fn render_package(package: &LockedPackage) -> String {
    let mut fields = vec![
        render_field("name", &package.name),
        render_field("version", &package.version),
    ];

    match &package.source {
        LockedSource::Path {
            requested,
            resolved,
        } => {
            fields.push(render_field("source", "path"));
            fields.push(render_field("path", requested));
            fields.push(render_field("resolved", &resolved.to_string_lossy()));
        }
        LockedSource::Git {
            url,
            branch,
            tag,
            rev,
            commit,
            resolved,
        } => {
            fields.push(render_field("source", "git"));
            fields.push(render_field("git", url));
            if let Some(branch) = branch {
                fields.push(render_field("branch", branch));
            }
            if let Some(tag) = tag {
                fields.push(render_field("tag", tag));
            }
            if let Some(rev) = rev {
                fields.push(render_field("rev", rev));
            }
            fields.push(render_field("commit", commit));
            fields.push(render_field("resolved", &resolved.to_string_lossy()));
        }
    }

    let dependencies = package
        .dependencies
        .iter()
        .map(|name| format!("\"{}\"", escape(name)))
        .collect::<Vec<_>>()
        .join(", ");
    fields.push(format!("            dependencies = [{dependencies}]"));

    format!("        {{\n{}\n        }}", fields.join(",\n"))
}

fn render_field(key: &str, value: &str) -> String {
    format!("            {key} = \"{}\"", escape(value))
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_round_trip_preserves_git_commits_and_edges() {
        let lockfile = Lockfile {
            version: LOCKFILE_VERSION,
            packages: vec![LockedPackage {
                name: "math".to_string(),
                version: "1.2.3".to_string(),
                source: LockedSource::Git {
                    url: "https://example.com/math.git".to_string(),
                    branch: Some("main".to_string()),
                    tag: None,
                    rev: None,
                    commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                    resolved: PathBuf::from(".vex/deps/math"),
                },
                dependencies: vec!["core".to_string()],
            }],
        };

        let rendered = render_lockfile(lockfile.clone());
        let parsed = parse_lockfile(&rendered).expect("rendered lockfile must parse");
        assert_eq!(parsed, lockfile);
    }

    #[test]
    fn version_one_lockfile_is_treated_as_unresolved() {
        let parsed = parse_lockfile("{ version = 1, package = [] }")
            .expect("legacy lockfile should trigger regeneration");
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn rejects_non_object_id_git_commits() {
        let err = parse_lockfile(
            r#"{
                version = 2,
                package = [{
                    name = "bad",
                    version = "1.0.0",
                    source = "git",
                    git = "https://example.com/bad.git",
                    commit = "--help",
                    resolved = ".vex/deps/bad",
                    dependencies = []
                }]
            }"#,
        )
        .expect_err("Git commits must be full object IDs");
        assert!(err.contains("invalid Git commit"), "{err}");
    }
}
