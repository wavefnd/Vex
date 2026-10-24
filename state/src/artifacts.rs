// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Conservative ownership of compiler outputs. All registry access is made
//! while the caller owns the project guard; run leases outlive that guard.
use crate::{atomic_rename, ensure_dir, inherit, reject_link, Guard};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

const REGISTRY: &str = ".vex/artifacts.json";

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    files: BTreeMap<String, String>,
    directories: BTreeSet<String>,
    generations: BTreeSet<String>,
}

fn relative(path: &Path) -> Result<String, String> {
    let parts: Vec<_> = path.components().collect();
    if parts.len() < 2
        || parts[0] != Component::Normal("target".as_ref())
        || parts.iter().any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err("owned outputs must be strictly inside target/".into());
    }
    parts
        .iter()
        .map(|part| {
            part.as_os_str()
                .to_str()
                .ok_or_else(|| "output path is not UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

fn checked(root: &Path, name: &str) -> Result<PathBuf, String> {
    if relative(Path::new(name))? != name || name.contains('\\') {
        return Err("invalid artifact ownership path".into());
    }
    let mut path = root.to_path_buf();
    for component in Path::new(name).components() {
        path.push(component);
        reject_link(&path)?;
    }
    Ok(path)
}

fn generation(name: &str) -> Result<String, String> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err("invalid run generation identity".into());
    }
    Ok(format!("target/.vex-run/{name}"))
}

fn open_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

fn read(root: &Path) -> Result<Registry, String> {
    let path = root.join(REGISTRY);
    reject_link(&path)?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if !metadata.is_file() {
            return Err("artifact registry must be a regular file".into());
        }
    }
    let mut registry: Registry = match open_read(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Registry {
                version: 1,
                ..Registry::default()
            })
        }
        Err(e) => return Err(format!("cannot read artifact ownership: {e}")),
        Ok(file) => {
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("artifact registry must be a regular file".into());
            }
            let mut bytes = Vec::new();
            file.take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 8 * 1024 * 1024 {
                return Err("artifact registry exceeds 8 MiB".into());
            }
            serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid artifact ownership: {e}"))?
        }
    };
    if registry.version != 1 {
        return Err("unsupported artifact ownership version".into());
    }
    for name in registry.files.keys().chain(registry.directories.iter()) {
        if relative(Path::new(name))? != *name || name.contains('\\') {
            return Err("invalid artifact ownership path".into());
        }
    }
    for (name, hash) in &registry.files {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("invalid artifact fingerprint for {name}"));
        }
    }
    for name in &registry.generations {
        generation(name)?;
    }
    // Files under .vex-run need a separately registered, leased generation.
    for name in registry.files.keys().chain(registry.directories.iter()) {
        if let Some(tail) = name.strip_prefix("target/.vex-run/") {
            let id = tail.split('/').next().unwrap();
            if !registry.generations.contains(id) {
                return Err("artifact has no owned run generation".into());
            }
        }
    }
    registry.version = 1;
    Ok(registry)
}

fn write(root: &Path, registry: &Registry) -> Result<(), String> {
    reject_link(&root.join(REGISTRY))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary = root.join(format!(".vex/artifacts-{}-{nonce}.tmp", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, registry).map_err(|e| e.to_string())?;
        file.write_all(b"\n")
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        atomic_rename(&temporary, &root.join(REGISTRY)).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn fingerprint(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Err("artifact is not a regular file".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        _ => {}
    }
    let file = match open_read(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("artifact is not a regular file".into());
    }
    let mut reader = io::BufReader::new(file);
    let mut hash = Sha256::new();
    let metadata = reader.get_ref().metadata().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        hash.update(metadata.permissions().mode().to_le_bytes());
    }
    #[cfg(windows)]
    hash.update([u8::from(metadata.permissions().readonly())]);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if process::cancelled() {
            return Err("cleanup cancelled".into());
        }
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}

/// Kept alive during compilation and after releasing the project guard, through
/// the user process. Inherited handles also protect orphaned child processes.
pub struct RunLease {
    _file: File,
}
impl RunLease {
    pub fn create(root: &Path, path: &Path) -> Result<Self, String> {
        let name = path
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or("invalid run generation")?;
        let expected = generation(name)?;
        if relative(path)? != expected {
            return Err("invalid run generation path".into());
        }
        checked(root, &expected)?;
        let directory = root.join(".vex/run-locks");
        ensure_dir(&directory)?;
        let path = directory.join(format!("{name}.lock"));
        reject_link(&path)?;
        let file = crate::acquire_file(&path, false, true, || {}).map_err(|e| e.to_string())?;
        inherit(&file).map_err(|e| e.to_string())?;
        let mut registry = read(root)?;
        registry.generations.insert(name.into());
        registry.directories.insert(expected);
        write(root, &registry)?;
        Ok(Self { _file: file })
    }
}

// Never create or unlink a lease while probing it: a surviving file is not a
// live lease, and removing it could split coordination between two inodes.
fn try_lease(root: &Path, name: &str) -> Result<Option<File>, String> {
    let directory = root.join(".vex/run-locks");
    reject_link(&directory)?;
    let path = directory.join(format!("{name}.lock"));
    reject_link(&path)?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if !metadata.is_file() {
            return Err("run lease is not a regular file".into());
        }
    }
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .and_then(|file| match file.try_lock() {
                Ok(()) => Ok(Some(file)),
                Err(fs::TryLockError::WouldBlock) => Ok(None),
                Err(fs::TryLockError::Error(e)) => Err(e),
            })
    };
    #[cfg(windows)]
    let result = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
            .map(Some)
            .or_else(|e| {
                if e.raw_os_error() == Some(32) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
    };
    match result {
        Ok(Some(file)) if !file.metadata().map_err(|e| e.to_string())?.is_file() => {
            Err("run lease is not a regular file".into())
        }
        Ok(value) => Ok(value),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Check every declared destination before the compiler can write anything.
/// This does not create state, and is also used for dry-run plans.
pub fn validate_output_paths(root: &Path, outputs: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut identities = BTreeSet::<String>::new();
    for output in outputs {
        let path = if output.is_absolute() {
            output
                .strip_prefix(root)
                .map_err(|_| "compiler output is outside the project")?
        } else {
            output
        };
        let name = relative(path)?;
        #[cfg(windows)]
        for component in name.split('/') {
            if component.ends_with(['.', ' ']) || component.contains(':') {
                return Err(format!("ambiguous Windows compiler output `{name}`"));
            }
        }
        let identity = if cfg!(any(windows, target_os = "macos")) {
            name.to_lowercase()
        } else {
            name.clone()
        };
        if identities.iter().any(|previous| {
            previous == &identity
                || identity.starts_with(&format!("{previous}/"))
                || previous.starts_with(&format!("{identity}/"))
        }) {
            return Err(format!("compiler output destinations overlap at `{name}`"));
        }
        checked(root, &name)?;
        identities.insert(identity);
        names.push(name);
    }
    Ok(names)
}

fn reject_hardlinked_output(path: &Path) -> Result<(), String> {
    let file = match open_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    #[cfg(unix)]
    let links = {
        use std::os::unix::fs::MetadataExt;
        file.metadata().map_err(|error| error.to_string())?.nlink()
    };
    #[cfg(windows)]
    let links = {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut u32) -> i32;
        }
        // BY_HANDLE_FILE_INFORMATION is thirteen DWORDs; number of links is
        // DWORD 10. The handle remains open throughout this query.
        let mut info = [0u32; 13];
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        u64::from(info[10])
    };
    if links > 1 {
        return Err(format!(
            "refusing to overwrite hard-linked compiler output `{}`",
            path.display()
        ));
    }
    Ok(())
}

/// Reserve compiler destinations. Only absent files or unchanged, previously
/// recorded artifacts may be replaced; user data must survive every build.
pub struct Batch {
    root: PathBuf,
    registry: Registry,
    eligible: BTreeSet<String>,
    required: BTreeSet<String>,
}
impl Batch {
    pub fn prepare(
        root: &Path,
        outputs: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, String> {
        Self::prepare_with_optional(root, outputs, [])
    }

    /// Linkers can produce optional companions that still need overwrite and
    /// ownership protection, even when a particular build does not emit them.
    pub fn prepare_with_optional(
        root: &Path,
        outputs: impl IntoIterator<Item = PathBuf>,
        optional: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, String> {
        let mut registry = read(root)?;
        let mut eligible = BTreeSet::new();
        let mut outputs: Vec<_> = outputs.into_iter().collect();
        let required = validate_output_paths(root, &outputs)?.into_iter().collect();
        outputs.extend(optional);
        for name in validate_output_paths(root, &outputs)? {
            if let Some(tail) = name.strip_prefix("target/.vex-run/") {
                if !registry
                    .generations
                    .contains(tail.split('/').next().unwrap())
                {
                    return Err("compiler output has no owned run generation".into());
                }
            }
            let absolute = checked(root, &name)?;
            let current = fingerprint(&absolute)?;
            let owned = registry.files.get(&name);
            if current.is_some() && current.as_ref() != owned {
                return Err(format!(
                    "refusing to overwrite unowned or modified compiler output `{name}`; move it before rebuilding"
                ));
            }
            reject_hardlinked_output(&absolute)?;
            for parent in Path::new(&name)
                .ancestors()
                .skip(1)
                .take_while(|p| p.components().count() > 1)
            {
                let name = relative(parent)?;
                if !root.join(parent).exists() {
                    registry.directories.insert(name);
                }
            }
            eligible.insert(name);
        }
        Ok(Self {
            root: root.to_owned(),
            registry,
            eligible,
            required,
        })
    }

    /// A successful compiler must actually produce every promised artifact.
    pub fn verify_outputs(&self) -> Result<(), String> {
        for name in &self.eligible {
            let path = checked(&self.root, name)?;
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && !self.required.contains(name) =>
                {
                    continue
                }
                Err(error) => return Err(format!("compiler did not produce `{name}`: {error}")),
            };
            if !metadata.is_file() {
                return Err(format!("compiler output `{name}` is not a regular file"));
            }
            reject_hardlinked_output(&path)?;
        }
        Ok(())
    }

    /// Record partial outputs too, so a failed compile can be cleaned safely.
    pub fn finish(mut self) -> Result<(), String> {
        for name in self.eligible {
            let path = checked(&self.root, &name)?;
            if let Some(hash) = fingerprint(&path)? {
                self.registry.files.insert(name, hash);
            } else {
                self.registry.files.remove(&name);
            }
        }
        if !self.registry.files.is_empty()
            || !self.registry.directories.is_empty()
            || self.root.join(REGISTRY).exists()
        {
            write(&self.root, &self.registry)?;
        }
        Ok(())
    }
}

pub fn has_records(root: &Path) -> Result<bool, String> {
    reject_link(&root.join(".vex"))?;
    reject_link(&root.join(REGISTRY))?;
    Ok(root.join(REGISTRY).exists())
}

/// The caller chooses a shared guard for previews and an exclusive guard for
/// deletion. Unknown files and directories are never recursively removed.
pub fn clean(
    guard: &Guard,
    apply: bool,
    mut report: impl FnMut(&str, String),
) -> Result<(), String> {
    let root = guard.root();
    for journal in [".vex/transaction.json", ".vex/init.json"] {
        reject_link(&root.join(journal))?;
        if root.join(journal).exists() {
            return Err(
                "recovery pending; cleanup preserves the transaction; run vex fetch first".into(),
            );
        }
    }
    let mut registry = read(root)?;
    let mut leases = Vec::new();
    let mut protected = BTreeSet::new();
    for name in &registry.generations {
        match try_lease(root, name)? {
            Some(file) => leases.push(file),
            None => {
                protected.insert(generation(name)?);
                report(
                    "Preserving",
                    format!("{} (active or missing lease)", generation(name)?),
                );
            }
        }
    }
    let protected_path = |name: &str| {
        protected
            .iter()
            .any(|prefix| name == prefix || name.starts_with(&format!("{prefix}/")))
    };
    let mut scheduled = BTreeSet::new();
    for (name, hash) in registry.files.clone() {
        if process::cancelled() {
            return Err("clean cancelled".into());
        }
        if protected_path(&name) {
            continue;
        }
        let path = match checked(root, &name) {
            Ok(path) => path,
            Err(error) => {
                report("Preserving", format!("{name}: {error}"));
                continue;
            }
        };
        match fingerprint(&path) {
            Ok(Some(current)) if current == hash => {
                scheduled.insert(path.clone());
                report(
                    if apply { "Removing" } else { "Would remove" },
                    name.clone(),
                );
                if apply {
                    fs::remove_file(&path).map_err(|e| e.to_string())?;
                    registry.files.remove(&name);
                }
            }
            Ok(None) => {
                if apply {
                    registry.files.remove(&name);
                }
            }
            _ => report(
                "Preserving",
                format!("{name} (modified or not a regular file)"),
            ),
        }
    }
    let mut directories: Vec<_> = registry.directories.iter().cloned().collect();
    directories.sort_by_key(|name| std::cmp::Reverse(name.len()));
    for name in directories {
        if process::cancelled() {
            return Err("clean cancelled".into());
        }
        if protected_path(&name) {
            continue;
        }
        let Ok(path) = checked(root, &name) else {
            continue;
        };
        if apply {
            match fs::remove_dir(&path) {
                Ok(()) => {
                    report("Removing", name.clone());
                    registry.directories.remove(&name);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    registry.directories.remove(&name);
                }
                Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
                    report("Preserving", format!("{name} (contains untracked files)"));
                }
                Err(e) => return Err(format!("cannot clean {name}: {e}")),
            }
        } else if let Ok(entries) = fs::read_dir(&path) {
            let remaining = entries
                .map(|entry| entry.map(|entry| !scheduled.contains(&entry.path())))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            if remaining.iter().all(|remaining| !remaining) {
                report("Would remove", name);
                scheduled.insert(path);
            } else {
                report(
                    "Preserving",
                    format!("{name} (contains untracked or protected files)"),
                );
            }
        }
    }
    if apply {
        registry.generations.retain(|id| {
            let prefix = generation(id).unwrap();
            root.join(&prefix).exists()
                || registry
                    .files
                    .keys()
                    .chain(registry.directories.iter())
                    .any(|name| name == &prefix || name.starts_with(&format!("{prefix}/")))
        });
        write(root, &registry)?;
    }
    Ok(())
}
