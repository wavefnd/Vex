// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! A durable two-file edit. Uncommitted journals roll back, including after a crash.
use crate::{atomic_rename, reject_link, sync_dir, Guard};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};
const JOURNAL: &str = ".vex/edit.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    manifest: String,
    old: String,
    new: String,
    old_lock: Option<String>,
    new_lock: Option<String>,
    committed: bool,
}
fn read(root: &Path) -> Result<Option<Record>, String> {
    let path = root.join(JOURNAL);
    reject_link(&path)?;
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
pub fn replace(path: &Path, text: &str) -> Result<(), String> {
    reject_link(path)?;
    let temporary = path.with_file_name(format!(
        ".vex-edit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&temporary).map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        drop(f);
        atomic_rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn store(root: &Path, record: &Record) -> Result<(), String> {
    replace(
        &root.join(JOURNAL),
        &serde_json::to_string(record).map_err(|e| e.to_string())?,
    )
}
pub fn begin(guard: &Guard, manifest: &Path, new: String) -> Result<(), String> {
    let root = guard.root();
    if read(root)?.is_some() {
        return Err("dependency edit recovery is pending".into());
    }
    let relative = manifest
        .strip_prefix(root)
        .map_err(|_| "manifest is outside project")?;
    let manifest = relative
        .to_str()
        .ok_or("manifest path must be UTF-8")?
        .replace('\\', "/");
    validate_path(&manifest)?;
    let path = root.join(&manifest);
    reject_link(&path)?;
    reject_link(&root.join("vex.lock"))?;
    let old = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let old_lock = match fs::read_to_string(root.join("vex.lock")) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let record = Record {
        version: 1,
        manifest,
        old,
        new,
        new_lock: old_lock.clone(),
        old_lock,
        committed: false,
    };
    store(root, &record)?;
    replace(&path, &record.new)
}
pub fn expect_lock(root: &Path, content: String) -> Result<(), String> {
    if let Some(mut record) = read(root)? {
        record.new_lock = Some(content);
        store(root, &record)?;
    }
    Ok(())
}
pub fn commit(guard: &Guard) -> Result<(), String> {
    let root = guard.root();
    let mut record = read(root)?.ok_or("missing dependency edit journal")?;
    record.committed = true;
    store(root, &record)?;
    recover(root, false)
}
fn validate_path(path: &str) -> Result<(), String> {
    if path
        .split('/')
        .any(|v| v.is_empty() || v == "." || v == ".." || v.contains(['\\', ':']))
        || Path::new(path).file_name().is_none_or(|v| v != "vex.ws")
    {
        return Err("invalid dependency edit journal path".into());
    }
    Ok(())
}
pub fn recover(root: &Path, readonly: bool) -> Result<(), String> {
    let Some(record) = read(root)? else {
        return Ok(());
    };
    if readonly {
        return Err("dependency edit recovery pending; run vex fetch; read-only commands preserve the journal".into());
    }
    if record.version != 1 {
        return Err("unsupported dependency edit journal".into());
    }
    validate_path(&record.manifest)?;
    let path = root.join(&record.manifest);
    let canonical_parent = path
        .parent()
        .unwrap()
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !canonical_parent.starts_with(root) {
        return Err("edit journal escapes project".into());
    }
    reject_link(&path)?;
    reject_link(&root.join("vex.lock"))?;
    let manifest = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let lock = match fs::read_to_string(root.join("vex.lock")) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if (manifest != record.old && manifest != record.new)
        || (lock != record.old_lock && lock != record.new_lock)
    {
        return Err(
            "dependency edit files were modified; preserve .vex/edit.json for manual recovery"
                .into(),
        );
    }
    if !record.committed {
        replace(&path, &record.old)?;
        if let Some(lock) = &record.old_lock {
            replace(&root.join("vex.lock"), lock)?;
        } else if lock.is_some() {
            fs::remove_file(root.join("vex.lock")).map_err(|e| e.to_string())?;
            sync_dir(root).map_err(|e| e.to_string())?;
        }
    } else if manifest != record.new || lock != record.new_lock {
        return Err("committed dependency edit is incomplete; preserve journal".into());
    }
    fs::remove_file(root.join(JOURNAL)).map_err(|e| e.to_string())?;
    sync_dir(&root.join(".vex")).map_err(|e| e.to_string())
}
