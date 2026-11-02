// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Command-local physical storage, separate from portable lockfile paths.
use std::{
    cell::RefCell,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
struct Layout {
    project: PathBuf,
    build: PathBuf,
    dependencies: PathBuf,
}
thread_local! { static CURRENT: RefCell<Option<Layout>> = const { RefCell::new(None) }; }
pub struct Scope(Option<Layout>);
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

fn normalize(path: &Path) -> Result<PathBuf, String> {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return Err("storage path escapes filesystem root".into());
                }
            }
            part => result.push(part),
        }
    }
    let mut ancestor = result.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or("storage path has no existing parent")?;
    }
    let real = ancestor.canonicalize().map_err(|e| e.to_string())?;
    Ok(real.join(result.strip_prefix(ancestor).map_err(|e| e.to_string())?))
}

pub fn configure(
    project: &Path,
    build: Option<&Path>,
    dependencies: Option<&Path>,
) -> Result<Scope, String> {
    let build_root = build
        .map(normalize)
        .transpose()?
        .unwrap_or_else(|| project.to_owned());
    if build.is_some()
        && ((project != build_root && project.starts_with(&build_root))
            || ["src", ".git", ".vex"]
                .iter()
                .any(|p| build_root.starts_with(project.join(p))))
    {
        return Err("external build directory overlaps protected project sources".into());
    }
    let dependencies = dependencies.map(normalize).transpose()?.unwrap_or_else(|| {
        if build.is_some() {
            build_root.join(".vex/dependencies")
        } else {
            project.join(".vex")
        }
    });
    if project.starts_with(&dependencies)
        || ["src", ".git", "target"]
            .iter()
            .any(|p| dependencies.starts_with(project.join(p)))
        || dependencies.starts_with(build_root.join("target"))
        || dependencies == build_root
    {
        return Err("dependency state directory overlaps protected project sources".into());
    }
    validate_owner(&build_root.join(".vex/build-owner.json"), project, false)?;
    validate_owner(&dependencies.join("store-owner.json"), project, false)?;
    Ok(CURRENT.with(|slot| {
        Scope(slot.replace(Some(Layout {
            project: project.into(),
            build: build_root,
            dependencies,
        })))
    }))
}

pub fn build_root(project: &Path) -> PathBuf {
    CURRENT.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|l| l.project == project)
            .map_or_else(|| project.into(), |l| l.build.clone())
    })
}
pub fn dependency(project: &Path, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    CURRENT.with(|slot| {
        if let Some(layout) = slot.borrow().as_ref().filter(|l| l.project == project) {
            if let Ok(tail) = path.strip_prefix(".vex") {
                return layout.dependencies.join(tail);
            }
        }
        project.join(path)
    })
}
pub fn output(project: &Path, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let root = build_root(project);
    if root == project {
        path.to_owned()
    } else {
        root.join(path)
    }
}
pub fn external(project: &Path) -> bool {
    build_root(project) != project || dependency(project, ".vex") != project.join(".vex")
}

/// Claims only Vex's state, never the surrounding Meson directory or its files.
pub fn prepare_build(project: &Path) -> Result<(), String> {
    let root = build_root(project);
    crate::ensure_tree(&root)?;
    let managed = root.join(".vex");
    crate::ensure_tree(&managed)?;
    validate_owner(&managed.join("build-owner.json"), project, true)
}

/// Persistent identities are coordination metadata, never claims on user files.
pub(crate) fn validate_owner(path: &Path, project: &Path, create: bool) -> Result<(), String> {
    use std::io::{Read, Write};
    crate::reject_link(path)?;
    let project_identity = match project.to_str() {
        Some(text) => serde_json::json!(text),
        None => serde_json::json!({"native_path_sha256":identity_digest(project)}),
    };
    let identity = serde_json::json!({"schema_version":1,"project":project_identity});
    match std::fs::File::open(path) {
        Ok(file) => {
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("storage owner must be a regular file".into());
            }
            let mut bytes = Vec::new();
            file.take(65537)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 65536
                || serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| e.to_string())?
                    != identity
            {
                return Err(
                    "storage directory belongs to a different project or has invalid ownership"
                        .into(),
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| e.to_string())?;
            file.write_all(identity.to_string().as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            crate::sync_dir(path.parent().unwrap()).map_err(|e| e.to_string())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

/// Persistent coordination is separate from the shared Git object cache.
pub fn coordination(project: &Path) -> Result<PathBuf, String> {
    let selected = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|p| (PathBuf::from(p), "Vex/state"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(|p| (PathBuf::from(p), "vex"))
            .or_else(|| {
                std::env::var_os("HOME").map(|p| (PathBuf::from(p).join(".local/state"), "vex"))
            })
    };
    let (base, owned) = selected.unwrap_or_else(|| (std::env::temp_dir(), "vex-state"));
    if !base.is_absolute() {
        return Err("OS state directory must be absolute".into());
    }
    // OS-selected roots may have physical aliases (macOS /var, redirected
    // homes). Resolve that boundary before appending Vex-owned components;
    // links within the owned coordination tree remain rejected by ensure_tree.
    let base = normalize(&base)?.join(owned);
    let digest = identity_digest(project);
    Ok(base.join("projects").join(digest).join("state.lock"))
}

fn identity_digest(project: &Path) -> String {
    use sha2::{Digest, Sha256};
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        project.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let bytes = {
        use std::os::windows::ffi::OsStrExt;
        project
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Match filesystem paths across Windows normal/verbatim spellings without
/// resolving links or parent components. All containment checks remain lexical.
pub fn path_form(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return path.into();
        };
        let mut normalized = match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                PathBuf::from(format!("{}:\\", (drive as char).to_ascii_uppercase()))
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                let mut result = PathBuf::from(r"\\");
                result.push(server);
                result.push(share);
                result
            }
            _ => return path.into(),
        };
        for component in components {
            if !matches!(component, Component::RootDir) {
                normalized.push(component.as_os_str());
            }
        }
        normalized
    }
    #[cfg(not(windows))]
    {
        path.into()
    }
}
pub fn relative_output(root: &Path, path: &Path) -> Result<PathBuf, String> {
    path_form(path)
        .strip_prefix(path_form(root))
        .map(Path::to_path_buf)
        .map_err(|_| {
            format!(
                "compiler output `{}` is outside build root `{}`",
                path.display(),
                root.display()
            )
        })
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    #[test]
    fn normal_verbatim_drives_and_unc_paths_have_one_lexical_boundary() {
        assert_eq!(
            relative_output(
                Path::new(r"\\?\C:\build"),
                Path::new(r"c:\build\target\app.o")
            )
            .unwrap(),
            Path::new(r"target\app.o")
        );
        assert_eq!(
            relative_output(
                Path::new(r"\\?\UNC\server\share\build"),
                Path::new(r"\\server\share\build\target\app.o")
            )
            .unwrap(),
            Path::new(r"target\app.o")
        );
        assert!(relative_output(
            Path::new(r"C:\build"),
            Path::new(r"C:\build-other\target\app.o")
        )
        .is_err());
        assert!(crate::artifacts::validate_output_paths(
            Path::new(r"C:\build"),
            &[r"C:\build\target\..\escape".into()]
        )
        .is_err());
    }
}
