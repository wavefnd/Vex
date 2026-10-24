// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Explicit user workspace ownership, independent of Vex's Rust workspace.
use crate::Manifest;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use wson::WsonValue;

#[derive(Debug, Clone)]
pub struct Workspace {
    pub members: Vec<String>,
    pub exclude: Vec<String>,
    pub default_members: Option<Vec<String>>,
}
#[derive(Debug, Clone)]
pub struct WorkspaceContext {
    pub owner: PathBuf,
    pub members: Vec<PathBuf>,
    pub selected: Vec<PathBuf>,
}
fn paths(value: Option<&WsonValue>, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let WsonValue::Array(values) = value else {
        return Err(format!("workspace {field} must be an array"));
    };
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        let WsonValue::String(path) = value else {
            return Err(format!("workspace {field} paths must be strings"));
        };
        if path != "." {
            source::validate_subdir(path)?;
        }
        if path.contains(['*', '?', '[', ']']) {
            return Err("workspace paths are explicit; globs are unsupported".into());
        }
        if !seen.insert(path) {
            return Err(format!("duplicate workspace {field} path `{path}`"));
        }
        result.push(path.clone());
    }
    Ok(result)
}
pub(crate) fn parse(value: Option<&WsonValue>, format: u32) -> Result<Option<Workspace>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if format < 4 {
        return Err("workspace requires format = 4".into());
    }
    let WsonValue::Object(object) = value else {
        return Err("workspace must be an object".into());
    };
    crate::parse::reject_unknown_fields(
        object,
        &["members", "exclude", "default_members"],
        "workspace",
    )?;
    Ok(Some(Workspace {
        members: paths(object.get("members"), "members")?,
        exclude: paths(object.get("exclude"), "exclude")?,
        default_members: object
            .get("default_members")
            .map(|value| paths(Some(value), "default_members"))
            .transpose()?,
    }))
}

impl Workspace {
    pub fn excluded(&self, root: &Path, path: &Path) -> bool {
        self.exclude.iter().any(|entry| {
            let candidate = root.join(entry);
            let candidate = candidate.canonicalize().unwrap_or(candidate);
            path.starts_with(candidate)
        })
    }
    pub fn member_paths(&self, owner: &Manifest) -> Result<Vec<PathBuf>, diagnostic::Error> {
        let root = owner
            .source_path
            .parent()
            .unwrap()
            .canonicalize()
            .map_err(diagnostic::Error::environment)?;
        let mut seen = BTreeSet::new();
        let mut members = Vec::new();
        if !owner.virtual_workspace {
            members.push(owner.source_path.clone());
            seen.insert(root.clone());
        }
        for entry in &self.members {
            let directory = root.join(entry).canonicalize().map_err(|e| {
                diagnostic::Error::resolution(format!(
                    "workspace member `{entry}` is unavailable: {e}"
                ))
            })?;
            if !directory.starts_with(&root) {
                return Err(diagnostic::Error::resolution(
                    "workspace member escapes its workspace root",
                ));
            }
            if self.excluded(&root, &directory) {
                continue;
            }
            if entry == "." && !owner.virtual_workspace {
                continue;
            }
            if !seen.insert(directory.clone()) {
                return Err(diagnostic::Error::resolution(
                    "workspace contains duplicate physical member paths",
                ));
            }
            let manifest = directory.join(crate::MANIFEST_FILE);
            if !manifest.is_file() {
                return Err(diagnostic::Error::resolution(format!(
                    "workspace member has no manifest: {}",
                    manifest.display()
                )));
            }
            let manifest = manifest
                .canonicalize()
                .map_err(diagnostic::Error::environment)?;
            if manifest.parent() != Some(directory.as_path())
                || manifest
                    .file_name()
                    .is_none_or(|n| n != crate::MANIFEST_FILE)
            {
                return Err(diagnostic::Error::resolution(
                    "workspace member manifest must stay in its declared directory",
                ));
            }
            members.push(manifest);
        }
        if members.is_empty() {
            return Err(diagnostic::Error::resolution(
                "workspace has no package members",
            ));
        }
        Ok(members)
    }
    pub fn defaults(
        &self,
        owner: &Manifest,
        members: &[Manifest],
    ) -> Result<Vec<PathBuf>, diagnostic::Error> {
        let Some(defaults) = &self.default_members else {
            return Ok(members.iter().map(|m| m.source_path.clone()).collect());
        };
        let root = owner.source_path.parent().unwrap();
        let mut selected = Vec::new();
        for path in defaults {
            let path = root
                .join(path)
                .join(crate::MANIFEST_FILE)
                .canonicalize()
                .map_err(diagnostic::Error::resolution)?;
            if !members.iter().any(|m| m.source_path == path) {
                return Err(diagnostic::Error::resolution(
                    "default_members must name included workspace members",
                ));
            }
            selected.push(path);
        }
        if selected.is_empty() {
            return Err(diagnostic::Error::resolution(
                "default_members must not be empty",
            ));
        }
        Ok(selected)
    }
}

/// Apply only workspace-owned policy. Dependency tool settings still never
/// propagate into a root build. Member-specific binary settings remain local.
pub fn apply_policy(
    owner: &Manifest,
    member: &mut Manifest,
    context: &WorkspaceContext,
) -> Result<(), diagnostic::Error> {
    let root = owner.source_path.parent().unwrap();
    if owner.source_path != member.source_path {
        if member.workspace.is_some() {
            return Err(diagnostic::Error::resolution(
                "nested workspace members are unsupported; exclude the nested workspace",
            ));
        }
        if member.fields.contains("profiles") || member.fields.contains("build") {
            return Err(diagnostic::Error::resolution(
                "workspace profiles and build target belong in the workspace root",
            ));
        }
        if member
            .target_config
            .iter()
            .any(|c| c.sysroot.is_some() || c.linker.is_some())
        {
            return Err(diagnostic::Error::resolution(
                "workspace sysroot/linker settings belong in the workspace root",
            ));
        }
        if let Some(required) = &owner.compiler {
            if member
                .compiler
                .as_ref()
                .is_some_and(|value| value != required)
            {
                return Err(diagnostic::Error::resolution(
                    "member compiler requirement conflicts with the workspace root",
                ));
            }
            member.compiler = Some(required.clone());
        }
        member.profiles = owner.profiles.clone();
        member.build_target = owner.build_target.clone();
        let local = std::mem::take(&mut member.target_config);
        member.target_config = owner
            .target_config
            .iter()
            .map(|config| {
                let mut config = config.clone();
                config.bins.clear();
                if let Some(path) = &config.sysroot {
                    config.sysroot = Some(root.join(path));
                }
                if let Some(linker) = &config.linker {
                    if Path::new(linker).components().count() > 1 || linker.contains('\\') {
                        config.linker = Some(
                            root.join(linker)
                                .to_str()
                                .ok_or_else(|| {
                                    diagnostic::Error::resolution(
                                        "workspace linker path must be UTF-8",
                                    )
                                })?
                                .to_owned(),
                        );
                    }
                }
                Ok(config)
            })
            .collect::<Result<_, diagnostic::Error>>()?;
        for config in local {
            if let Some(existing) = member
                .target_config
                .iter_mut()
                .find(|c| c.triple == config.triple)
            {
                existing.bins = config.bins;
            } else {
                member.target_config.push(config);
            }
        }
    }
    let directory = member.source_path.parent().unwrap();
    for target in member.targets.iter_mut().chain(member.tests.iter_mut()) {
        target.path = directory.join(&target.path);
    }
    member.scope = Some(context.clone());
    Ok(())
}

pub fn load_context(context: &WorkspaceContext) -> Result<Vec<Manifest>, diagnostic::Error> {
    let owner = Manifest::load_from(&context.owner)?;
    let mut names = BTreeSet::new();
    let mut members = Vec::new();
    for path in &context.members {
        let mut member = Manifest::load_from(path)?;
        crate::validate_target_name(&member.name).map_err(diagnostic::Error::resolution)?;
        if !names.insert(member.name.to_ascii_lowercase()) {
            return Err(diagnostic::Error::resolution(
                "workspace package names must be unique, including ASCII case",
            ));
        }
        // An intermediate workspace would create two owners for this package.
        for directory in path
            .parent()
            .unwrap()
            .ancestors()
            .skip(1)
            .take_while(|_| path != &owner.source_path)
        {
            if directory == owner.source_path.parent().unwrap() {
                break;
            }
            let candidate = directory.join(crate::MANIFEST_FILE);
            if candidate.is_file() && Manifest::load_from(&candidate)?.workspace.is_some() {
                return Err(diagnostic::Error::resolution(
                    "workspace member is owned by an intermediate nested workspace",
                ));
            }
        }
        apply_policy(&owner, &mut member, context)?;
        members.push(member);
    }
    members.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(members)
}
