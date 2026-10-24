// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::{Category, Error};
use lockfile::LockedSource;
use manifest::Manifest;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::{git, paths, resolve_guarded, ResolveOptions, UpdatePolicy};

const RECORD: &str = ".git/vex-managed.json";
const PROTECTED: &str = ".git/vex-prune-protected";
const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct PruneEntry {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct PruneReport {
    pub removable: Vec<PathBuf>,
    pub preserved: Vec<PruneEntry>,
    pub removed: usize,
}

/// Inspect the complete current graph and unused checkouts under one exclusive,
/// existing lease. Both preview and apply refuse pending recovery and never
/// discover dependencies over the network or rewrite the lockfile.
pub fn prune(
    manifest: &Manifest,
    apply: bool,
    status: impl FnMut(&str, String),
) -> Result<PruneReport, Error> {
    let guard = state::Guard::acquire_existing_exclusive(status).map_err(Error::resolution)?;
    let root = guard.root().to_path_buf();
    let resolution = resolve_guarded(
        manifest,
        ResolveOptions {
            requests: Vec::new(),
            update_bindings: None,
            force_bindings: false,
            additional_contexts: Vec::new(),
            selection: Default::default(),
            dry_run: true,
            locked: true,
            offline: true,
            update: UpdatePolicy::ReuseLocked,
        },
        |_, _| {},
        guard,
        &mut |_| Ok(()),
        true,
    )
    .map_err(|error| {
        error.context("cannot prune before the local graph is prepared; run `vex fetch`")
    })?;
    let used: Vec<_> = resolution
        .union_packages()
        .iter()
        .map(|package| {
            let (LockedSource::Path { resolved, .. } | LockedSource::Git { resolved, .. }) =
                &package.source;
            let path = root.join(resolved);
            path.canonicalize().unwrap_or(path)
        })
        .collect();
    let directory = root.join(".vex/deps");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PruneReport::default());
        }
        Err(error) => return Err(Error::environment(error)),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut report = PruneReport::default();
    for path in paths {
        check_interrupted()?;
        let reason = if used
            .iter()
            .any(|used| used.starts_with(&path) || path.starts_with(used))
        {
            Some("required by the current local graph".to_string())
        } else {
            trusted_checkout(&path).err()
        };
        if let Some(reason) = reason {
            report.preserved.push(PruneEntry {
                path: paths::relative_to_root(&path, &root),
                reason,
            });
        } else {
            report.removable.push(paths::relative_to_root(&path, &root));
        }
    }
    check_interrupted()?;
    if apply {
        for path in &report.removable {
            check_interrupted()?;
            let absolute = root.join(path);
            // Check immediately before deletion as well as during planning.
            // The project lease prevents build/update from changing any source.
            trusted_checkout(&absolute).map_err(|reason| {
                Error::resolution(format!(
                    "checkout `{}` changed during prune and was preserved: {reason}",
                    path.display()
                ))
            })?;
            fs::remove_dir_all(&absolute).map_err(|error| {
                Error::environment(error)
                    .with_field("operation", "prune unused checkout")
                    .with_field("path", path.display())
            })?;
            state::sync_dir(&directory)?;
            report.removed += 1;
        }
    }
    Ok(report)
}

fn check_interrupted() -> Result<(), Error> {
    if process::cancelled() {
        Err(Error::new(Category::Cancelled, "prune cancelled"))
    } else if process::failure_code() == 124 {
        Err(Error::new(Category::Timeout, "Git inspection timed out"))
    } else {
        Ok(())
    }
}

/// Full content evidence deliberately includes ignored files and all Git
/// metadata. A user-created branch, config edit, hook, loose object, ignored
/// output, or empty directory invalidates ownership for deletion purposes.
pub(crate) fn trusted_checkout(path: &Path) -> Result<(), String> {
    state::reject_link(path)?;
    paths::validate_managed_checkout_path(path).map_err(|error| error.to_string())?;
    match fs::symlink_metadata(path.join(PROTECTED)) {
        Ok(_) => {
            return Err("checkout was changed or unowned before a Vex update; preserved".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let record = read_record(path)?;
    let name = record["package"]
        .as_str()
        .ok_or("invalid ownership package")?;
    let origin = record["origin"]
        .as_str()
        .ok_or("invalid ownership origin")?;
    let commit = record["commit"]
        .as_str()
        .ok_or("invalid ownership commit")?;
    if path.file_name().and_then(|name| name.to_str()) != Some(&paths::checkout_name(name)) {
        return Err("legacy or unknown checkout name; no deletion ownership".into());
    }
    if source::identity(origin) != origin
        || !matches!(commit.len(), 40 | 64)
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("invalid checkout ownership identity".into());
    }
    if record["files"] != snapshot(path)? {
        return Err("checkout contents or Git metadata changed since Vex publication".into());
    }
    let manifest = Manifest::load_from(path.join(manifest::MANIFEST_FILE))
        .map_err(|error| error.to_string())?;
    if manifest.name != name || !manifest.lib {
        return Err("checkout manifest does not match Vex ownership".into());
    }
    git::verify_origin(path, origin).map_err(|error| error.to_string())?;
    if !git::is_detached(path).map_err(|error| error.to_string())? {
        return Err("checkout has a local branch checked out".into());
    }
    let head = git::stdout(
        git::command_in(path).args(["rev-parse", "HEAD"]),
        "inspect prune checkout HEAD",
    )
    .map_err(|error| error.to_string())?;
    if head != commit {
        return Err("checkout HEAD differs from Vex publication".into());
    }
    let dirty = git::stdout(
        git::command_in(path).args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
            "--ignore-submodules=none",
        ]),
        "inspect prune checkout files",
    )
    .map_err(|error| error.to_string())?;
    if !dirty.is_empty() {
        return Err("checkout contains dirty, untracked, or ignored files".into());
    }
    Ok(())
}

/// Keep an untrusted live checkout untrusted after copying normalizes any file
/// metadata. Preserve the old record, including user edits to it, verbatim.
pub(crate) fn protect_checkout(path: &Path) -> Result<(), Error> {
    let protection = path.join(PROTECTED);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&protection)
    {
        Ok(mut file) => {
            file.write_all(b"Vex prune preserves this checkout because its previous ownership record was absent or changed.\n")?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(Error::environment(error)),
    }
}

fn read_record(path: &Path) -> Result<Value, String> {
    let record = path.join(RECORD);
    state::reject_link(&record)?;
    let metadata = fs::symlink_metadata(&record)
        .map_err(|_| "no verified Vex ownership record; existing checkout preserved".to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err("invalid Vex ownership record".into());
    }
    let bytes = fs::read(&record).map_err(|error| error.to_string())?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid Vex ownership record".to_string())?;
    if value["schema_version"] != 1
        || value.as_object().is_none_or(|value| value.len() != 5)
        || !value["files"].is_object()
        || serde_json::to_vec(&value).map_err(|error| error.to_string())? != bytes
    {
        return Err("unsupported or modified Vex ownership record".into());
    }
    Ok(value)
}

/// Called only for unpublished transaction candidates. Unsupported trees stay
/// usable dependencies but receive no deletion ownership. Existing checkouts
/// without a previously verified record are never retroactively claimed.
pub(crate) fn record_checkout(
    path: &Path,
    name: &str,
    origin: &str,
    commit: &str,
    replace: bool,
) -> Result<(), Error> {
    match fs::symlink_metadata(path.join(PROTECTED)) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::environment(error)),
    }
    let marker = path.join(RECORD);
    if fs::symlink_metadata(&marker).is_ok() && !replace {
        return Ok(());
    }
    let files = match snapshot(path) {
        Ok(files) => files,
        Err(_) if process::cancelled() => {
            return Err(Error::new(
                Category::Cancelled,
                "checkout ownership recording cancelled",
            ));
        }
        Err(_) => return Ok(()),
    };
    let bytes = serde_json::to_vec(&json!({
        "schema_version": 1,
        "package": name,
        "origin": source::identity(origin),
        "commit": commit,
        "files": files,
    }))
    .map_err(Error::internal)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Ok(());
    }
    state::reject_link(&marker).map_err(Error::environment)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(!replace)
        .truncate(replace)
        .open(&marker)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn snapshot(root: &Path) -> Result<Value, String> {
    let mut files = Map::new();
    state::reject_link(root)?;
    let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err("checkout is not a directory".into());
    }
    files.insert(
        ".".into(),
        json!({"kind":"directory", "mode":mode(&metadata)}),
    );
    snapshot_directory(root, root, &mut files)?;
    Ok(Value::Object(files))
}

fn snapshot_directory(
    root: &Path,
    directory: &Path,
    files: &mut Map<String, Value>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        if process::cancelled() {
            return Err("operation cancelled".into());
        }
        let path = entry.map_err(|error| error.to_string())?.path();
        let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
        if relative == Path::new(RECORD) {
            continue;
        }
        let name = relative
            .components()
            .map(|part| part.as_os_str().to_str().ok_or("non-UTF-8 checkout path"))
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        state::reject_link(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        let mode = mode(&metadata);
        if metadata.is_dir() {
            files.insert(name, json!({"kind":"directory", "mode":mode}));
            snapshot_directory(root, &path, files)?;
        } else if metadata.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err("checkout contains a hard-linked file".into());
                }
            }
            let mut file = File::open(&path).map_err(|error| error.to_string())?;
            let mut digest = Sha256::new();
            let mut buffer = [0; 65536];
            loop {
                if process::cancelled() {
                    return Err("operation cancelled".into());
                }
                let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
            files.insert(
                name,
                json!({"kind":"file", "mode":mode, "sha256":format!("{:x}", digest.finalize())}),
            );
        } else {
            return Err("checkout contains an unsupported filesystem entry".into());
        }
    }
    Ok(())
}

fn mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        u32::from(metadata.permissions().readonly())
    }
}
