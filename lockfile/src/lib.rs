// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::path::PathBuf;

mod parse;
mod render;
mod store;

pub use parse::parse_lockfile as decode;
pub use render::render_lockfile as encode;
pub use store::{read_lockfile, write_lockfile};

pub const LOCKFILE_NAME: &str = "vex.lock";
pub const LOCKFILE_VERSION: i64 = 4;

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
    pub import_name: Option<String>,
    pub edges: Vec<source::graph::Edge>,
    pub features: source::graph::Features,
    /// Import binding, distinct from the declared package name.
    pub package: Option<String>,
    pub id: Option<String>,
    pub subdir: Option<String>,
    pub name: String,
    pub version: String,
    pub source: LockedSource,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Lockfile {
    pub roots: Vec<source::graph::Root>,
    pub version: i64,
    pub packages: Vec<LockedPackage>,
}

impl LockedPackage {
    pub fn import_name(&self) -> &str {
        self.import_name.as_deref().unwrap_or(&self.name)
    }
    pub fn package_name(&self) -> &str {
        self.package.as_deref().unwrap_or(&self.name)
    }
    pub fn directory(&self) -> PathBuf {
        let (LockedSource::Path { resolved, .. } | LockedSource::Git { resolved, .. }) =
            &self.source;
        let physical = if resolved.starts_with(".vex") {
            state::layout::dependency(&std::env::current_dir().unwrap_or_default(), resolved)
        } else {
            resolved.clone()
        };
        self.subdir
            .as_ref()
            .map_or_else(|| physical.clone(), |subdir| physical.join(subdir))
    }
}

impl Lockfile {
    pub fn empty() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            packages: Vec::new(),
            roots: Vec::new(),
        }
    }

    pub fn normalized(mut self) -> Self {
        self.roots.sort_by(|a, b| a.manifest.cmp(&b.manifest));
        for root in &mut self.roots {
            normalize_activation(&mut root.edges, &mut root.features);
        }
        for package in &mut self.packages {
            normalize_activation(&mut package.edges, &mut package.features);
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

fn normalize_activation(edges: &mut [source::graph::Edge], features: &mut source::graph::Features) {
    for edge in edges.iter_mut() {
        edge.features.sort();
    }
    edges.sort_by_cached_key(|edge| serde_json::to_string(edge).unwrap());
    for members in features.values_mut() {
        members.sort();
    }
}

pub fn package_identity(
    name: &str,
    version: &str,
    source: &LockedSource,
    subdir: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};
    // Concrete source identity excludes selectors and import aliases.
    let identity = match source {
        LockedSource::Path { resolved, .. } => {
            #[cfg(windows)]
            let path = resolved.to_string_lossy().replace('\\', "/");
            #[cfg(not(windows))]
            let path = resolved.to_string_lossy();
            format!("path:{path}")
        }
        LockedSource::Git { url, commit, .. } => format!("git:{}@{commit}", source::identity(url)),
    };
    hex_digest(Sha256::digest(
        format!("{name:?}|{version:?}|{identity:?}|{subdir:?}").as_bytes(),
    ))
}

// Keep persisted IDs/checksums as zero-padded lowercase SHA-256 hex. The
// digest output type no longer implements LowerHex in sha2 0.11.
fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .as_ref()
        .iter()
        .flat_map(|&byte| {
            [
                char::from(DIGITS[(byte >> 4) as usize]),
                char::from(DIGITS[(byte & 15) as usize]),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_identity_preserves_legacy_sha256_hex() {
        // A pre-upgrade identity with leading zeros must remain byte-identical.
        let source = LockedSource::Git {
            url: "https://example.com/dep.git".to_string(),
            branch: None,
            tag: None,
            rev: None,
            commit: "a".repeat(40),
            resolved: PathBuf::from("unused"),
        };
        assert_eq!(
            package_identity("dep211", "1.2.3", &source, None),
            "008cd81a9108de700a972998edf1a0fc6db4072fb68a1234a0f6986638738dd6"
        );
    }

    #[test]
    fn writing_a_legacy_lock_uses_an_escape_aware_format() {
        let old = crate::decode(r#"{version=2,package=[{name="dep",version="1.0.0",source="path",path="../new\name",resolved="../new\name",dependencies=[]}]}"#).unwrap();
        let rendered = crate::encode(old.clone());
        let new = crate::decode(&rendered).unwrap();
        assert_eq!(new.version, 3);
        assert_eq!(new.packages, old.packages);
    }

    #[test]
    fn lockfile_round_trip_preserves_git_commits_and_edges() {
        let lockfile = Lockfile {
            version: LOCKFILE_VERSION,
            roots: Vec::new(),
            packages: vec![
                LockedPackage {
                    package: None,
                    id: None,
                    import_name: None,
                    edges: Vec::new(),
                    features: Default::default(),
                    subdir: None,
                    name: "core".to_string(),
                    version: "1.0.0".to_string(),
                    source: LockedSource::Path {
                        requested: "../core".to_string(),
                        resolved: PathBuf::from("../core"),
                    },
                    dependencies: vec![],
                },
                LockedPackage {
                    package: None,
                    id: None,
                    import_name: None,
                    edges: Vec::new(),
                    features: Default::default(),
                    subdir: None,
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
                },
            ],
        };

        let rendered = crate::render::render_lockfile(lockfile.clone());
        let parsed = crate::parse::parse_lockfile(&rendered).expect("rendered lockfile must parse");
        assert_eq!(parsed, lockfile);
    }
}
