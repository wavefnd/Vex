// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use manifest::{Manifest, WorkspaceContext, MANIFEST_FILE};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Selection {
    pub graph: source::graph::Selection,
    pub manifest_path: Option<PathBuf>,
    pub packages: Vec<String>,
    pub all_packages: bool,
}

pub struct Project {
    pub manifest_path: PathBuf,
    pub owner: Manifest,
    pub members: Vec<Manifest>,
    pub selected: Vec<Manifest>,
    pub workspace: bool,
    previous_directory: PathBuf,
}
impl Project {
    pub fn primary(&self) -> Manifest {
        self.selected[0].clone()
    }
}
impl Selection {
    pub fn enter(&self) -> Result<Project, Error> {
        let previous_directory = std::env::current_dir().map_err(Error::environment)?;
        let start = previous_directory
            .canonicalize()
            .map_err(Error::environment)?;
        let selected = match &self.manifest_path {
            Some(path) => {
                if path.file_name().is_none_or(|name| name != MANIFEST_FILE) {
                    return Err(Error::usage("--manifest-path must name a vex.ws file"));
                }
                let path = start.join(path);
                if !path.is_file() {
                    return Err(Error::environment(format!(
                        "manifest not found at `{}`",
                        path.display()
                    )));
                }
                path
            }
            None => discover(&start)?,
        }
        .canonicalize()
        .map_err(Error::environment)?;
        if selected
            .file_name()
            .is_none_or(|name| name != MANIFEST_FILE)
        {
            return Err(Error::usage(
                "the selected manifest must resolve to a vex.ws file",
            ));
        }
        let nearest = Manifest::load_from(&selected)
            .map_err(|error| error.context("invalid project manifest `vex.ws`"))?;
        let mut workspace_owner = None;
        for directory in selected.parent().unwrap().ancestors() {
            let path = directory.join(MANIFEST_FILE);
            let owner = if path == selected {
                nearest.clone()
            } else {
                if !path.is_file() {
                    continue;
                }
                // A malformed unrelated ancestor must not shadow a nearer project.
                let Ok(actual) = path.canonicalize() else {
                    continue;
                };
                if actual.parent() != Some(directory) {
                    continue;
                }
                let Ok(owner) = Manifest::load_from(&actual) else {
                    continue;
                };
                owner
            };
            let Some(spec) = &owner.workspace else {
                continue;
            };
            if spec.excluded(directory, selected.parent().unwrap()) && owner.source_path != selected
            {
                continue;
            }
            let paths = spec.member_paths(&owner)?;
            if owner.source_path != selected && !paths.contains(&selected) {
                return Err(Error::resolution(format!("package `{}` is inside a workspace but is not a member\nhelp: add its path to workspace.members or workspace.exclude in {}", nearest.name, owner.source_path.display())));
            }
            if workspace_owner.is_some() {
                return Err(Error::resolution("nested or overlapping workspace ownership is unsupported; exclude the independent workspace"));
            }
            workspace_owner = Some((owner, paths));
        }
        let workspace = workspace_owner.is_some();
        let (owner, mut members) = match workspace_owner {
            Some((owner, paths)) => {
                let context = WorkspaceContext {
                    owner: owner.source_path.clone(),
                    members: paths,
                    selected: Vec::new(),
                };
                let members = manifest::workspace::load_context(&context)?;
                (owner, members)
            }
            None => (nearest.clone(), vec![nearest]),
        };
        let defaults = if self.all_packages {
            members.iter().map(|m| m.source_path.clone()).collect()
        } else if !self.packages.is_empty() {
            let names: BTreeSet<_> = self.packages.iter().collect();
            for name in &names {
                if !members.iter().any(|member| &member.name == *name) {
                    return Err(Error::resolution(format!(
                        "unknown workspace package `{name}`"
                    )));
                }
            }
            members
                .iter()
                .filter(|m| names.contains(&m.name))
                .map(|m| m.source_path.clone())
                .collect()
        } else if workspace && selected == owner.source_path {
            owner
                .workspace
                .as_ref()
                .unwrap()
                .defaults(&owner, &members)?
        } else {
            vec![selected.clone()]
        };
        for member in &mut members {
            if let Some(scope) = &mut member.scope {
                scope.selected = defaults.clone();
            }
        }
        let selected_members = members
            .iter()
            .filter(|m| defaults.contains(&m.source_path))
            .cloned()
            .collect::<Vec<_>>();
        if selected_members.is_empty() {
            return Err(Error::resolution("no package selected"));
        }
        let root = owner.source_path.parent().unwrap();
        crate::ui::verbose(format!("project manifest: {}", selected.display()));
        if workspace {
            crate::ui::verbose(format!(
                "workspace root: {}; selected {} package(s)",
                root.display(),
                selected_members.len()
            ));
        }
        std::env::set_current_dir(root).map_err(Error::environment)?;
        Ok(Project {
            manifest_path: if selected_members.len() == 1 {
                selected_members[0].source_path.clone()
            } else {
                owner.source_path.clone()
            },
            owner,
            members,
            selected: selected_members,
            workspace,
            previous_directory,
        })
    }
}
fn discover(start: &Path) -> Result<PathBuf, Error> {
    for directory in start.ancestors() {
        let path = directory.join(MANIFEST_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                if !path.is_file() {
                    return Err(Error::environment(format!(
                        "selected manifest `{}` is not a file",
                        path.display()
                    )));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::environment(format!(
                    "cannot inspect `{}`: {error}",
                    path.display()
                )))
            }
        }
    }
    Err(Error::environment(format!("could not find `{MANIFEST_FILE}` in `{}` or its ancestors\nhelp: run `vex init` to create a package", start.display())))
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous_directory);
    }
}
