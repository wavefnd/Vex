// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod model;
mod native;
mod parse;
mod render;
pub mod workspace;
pub use workspace::{Workspace, WorkspaceContext};

pub use model::{
    BinaryOverride, BuildProfile, PackageTarget, PlatformMode, PlatformSettings, Startup,
    TargetConfig, TargetKind,
};
pub use native::{NativeConfig, NativeLibrary, NativeOutput, NativeSearchPath};
pub use render::render_new_manifest;

/// Parse a manifest without filesystem access or project-state mutation.
pub fn decode(raw: &str, source_path: PathBuf) -> Result<Manifest, String> {
    parse::parse_manifest(raw, source_path)
}

pub const MANIFEST_FILE: &str = "vex.ws";

/// Validate a target name before writing an explicit-target manifest.
pub fn validate_target_name(name: &str) -> Result<(), String> {
    model::validate_component(name, "target name")
}

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
    pub test_only: bool,
    pub binding_alias: Option<String>,
    pub target: Option<source::graph::Predicate>,
    pub optional: bool,
    pub features: Vec<String>,
    pub default_features: bool,
    pub package: Option<String>,
    pub version_req: Option<String>,
    pub modern: bool,
    pub subdir: Option<String>,
    pub name: String,
    pub version: Option<String>,
    pub source: DependencySource,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub tests: Vec<PackageTarget>,
    pub workspace: Option<Workspace>,
    pub virtual_workspace: bool,
    pub scope: Option<WorkspaceContext>,
    pub fields: std::collections::BTreeSet<String>,
    pub format: u32,
    pub compiler: Option<String>,
    pub name: String,
    pub version: String,
    pub lib: bool,
    pub native: Option<NativeConfig>,
    pub targets: Vec<PackageTarget>,
    pub default_run: Option<String>,
    pub profiles: BTreeMap<String, BuildProfile>,
    pub build_target: Option<String>,
    pub target_config: Vec<TargetConfig>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub dependencies: Vec<Dependency>,
    pub features: source::graph::Features,
    pub source_path: PathBuf,
}

impl Dependency {
    pub fn import_name(&self) -> &str {
        self.binding_alias.as_deref().unwrap_or(&self.name)
    }
    pub fn edge(&self, key: String) -> source::graph::Edge {
        source::graph::Edge {
            development: self.test_only,
            key,
            alias: self.import_name().into(),
            optional: self.optional,
            default_features: self.default_features,
            features: self.features.clone(),
            target: self.target.clone(),
        }
    }

    pub fn package_name(&self) -> &str {
        self.package.as_deref().unwrap_or(&self.name)
    }
    pub fn validate_version(&self, found: &str) -> Result<(), String> {
        if self
            .version
            .as_ref()
            .is_some_and(|required| required != found)
        {
            return Err(format!(
                "dependency `{}` requires version `{}` but source contains version `{found}`",
                self.import_name(),
                self.version.as_ref().unwrap()
            ));
        }
        if let Some(required) = &self.version_req {
            let version = semver::Version::parse(found).map_err(|_| {
                format!("dependency `{}` version `{found}` is not SemVer", self.name)
            })?;
            let requirement = semver::VersionReq::parse(required).map_err(|e| e.to_string())?;
            if !requirement.matches(&version) {
                return Err(format!(
                    "dependency `{}` requires `{required}` but selected source contains `{found}`",
                    self.name
                ));
            }
        }
        Ok(())
    }
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
        self.targets
            .iter()
            .find(|target| target.kind == TargetKind::Library)
            .or_else(|| self.targets.first())
            .expect("validated manifests have targets")
            .path
            .clone()
    }

    pub fn profile(&self, name: &str) -> Result<&BuildProfile, String> {
        self.profiles
            .get(name)
            .ok_or_else(|| format!("unknown build profile `{name}`"))
    }

    pub fn platform_for(&self, target: &PackageTarget, triple: &str) -> PlatformSettings {
        let mut platform = target.platform.clone();
        if let Some(overrides) = self
            .target_config
            .iter()
            .find(|config| config.triple == triple)
            .and_then(|config| {
                config
                    .bins
                    .iter()
                    .find(|bin| bin.name == target.name && target.kind == TargetKind::Binary)
            })
        {
            platform.overlay(&overrides.platform);
        }
        platform
    }
}

mod versions;
