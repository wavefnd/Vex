use std::path::{Path, PathBuf};

mod parse;
mod render;

pub use render::render_new_manifest;

/// Parse a manifest without filesystem access or project-state mutation.
pub fn decode(raw: &str, source_path: PathBuf) -> Result<Manifest, String> {
    parse::parse_manifest(raw, source_path)
}

pub const MANIFEST_FILE: &str = "vex.ws";

/// Package names are Wave import identifiers. Never silently normalize a name.
pub fn validate_package_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(format!(
            "invalid package name `{name}`: use [A-Za-z_][A-Za-z0-9_]*"
        ));
    }
    Ok(())
}

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
    pub format: u32,
    pub compiler: Option<String>,
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
    pub fn load() -> Result<Self, diagnostic::Error> {
        let source_path = Path::new(MANIFEST_FILE);
        if !source_path.is_file() {
            let directory = std::env::current_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            return Err(diagnostic::Error::environment(format!(
                "could not find `{MANIFEST_FILE}` in `{directory}`\nhelp: run `vex init` to create a package"
            )));
        }
        Self::load_from(source_path)
    }

    pub fn load_from(source_path: impl AsRef<Path>) -> Result<Self, diagnostic::Error> {
        let source_path = source_path.as_ref().to_path_buf();
        if !source_path.is_file() {
            return Err(diagnostic::Error::environment(format!(
                "manifest not found at `{}`",
                source_path.to_string_lossy()
            )));
        }

        let raw = wson::read_document(&source_path).map_err(|e| {
            diagnostic::Error::new(
                if e.kind() == std::io::ErrorKind::InvalidData {
                    diagnostic::Category::Resolution
                } else {
                    diagnostic::Category::Environment
                },
                format!("failed to read `{}`: {e}", source_path.to_string_lossy()),
            )
        })?;

        parse::parse_manifest(&raw, source_path.clone()).map_err(|err| {
            diagnostic::Error::resolution(err)
                .with_field("manifest", source_path.display())
                .context(format!(
                    "failed to load manifest `{}`",
                    source_path.display()
                ))
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
