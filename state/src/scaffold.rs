// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Initialization publishes complete files without replacing existing entries.
//! The manifest is the commit point; a durable journal owns only the new files.
use crate::{atomic_rename, reject_link, sync_dir, Guard};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const JOURNAL: &str = ".vex/init.json";
const STAGE: &str = ".vex/init-stage";

fn error(e: impl std::fmt::Display) -> String {
    format!("project initialization: {e}")
}

pub fn initialize(guard: &Guard, files: &[(&str, String)]) -> Result<(), String> {
    let root = guard.root();
    if files.last().map(|f| f.0) != Some("vex.ws") {
        return Err(error("manifest must be published last"));
    }
    reject_link(&root.join("src"))?;
    let new_src = !root.join("src").exists();
    if !new_src && !root.join("src").is_dir() {
        return Err(error("src is not a directory"));
    }
    for (name, _) in files {
        valid_name(name)?;
        if fs::symlink_metadata(root.join(name)).is_ok() {
            return Err(error(format!("`{name}` already exists; preserving it")));
        }
    }
    let stage = root.join(STAGE);
    // Refuse to discard a leftover candidate whose journal was never published.
    fs::create_dir(&stage).map_err(|e| error(format!("cannot create staging at {}: {e}; preserve and inspect an existing staging directory before retrying", stage.display())))?;
    let prepare = (|| {
        for (index, (_, content)) in files.iter().enumerate() {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stage.join(index.to_string()))
                .map_err(error)?;
            file.write_all(content.as_bytes()).map_err(error)?;
            file.sync_all().map_err(error)?;
        }
        sync_dir(&stage).map_err(error)?;
        let record = json!({"version":1,"new_src":new_src,"files":files});
        let temporary = stage.join("journal");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(error)?;
        file.write_all(record.to_string().as_bytes())
            .map_err(error)?;
        file.sync_all().map_err(error)?;
        drop(file);
        atomic_rename(&temporary, &root.join(JOURNAL)).map_err(error)
    })();
    if let Err(failure) = prepare {
        if !root.join(JOURNAL).exists() {
            let _ = fs::remove_dir_all(&stage);
        }
        return Err(failure);
    }
    let publish = (|| {
        if new_src {
            fs::create_dir(root.join("src")).map_err(error)?;
        }
        for (index, (name, _)) in files.iter().enumerate() {
            // Hard-link publication is atomic and fails if an entry appeared in
            // the meantime. All candidates are on this project's filesystem.
            fs::hard_link(stage.join(index.to_string()), root.join(name)).map_err(error)?;
            sync_dir(root.join(name).parent().unwrap()).map_err(error)?;
            #[cfg(debug_assertions)]
            if std::env::var("VEX_TEST_INIT_CRASH_AFTER").ok().as_deref() == Some(*name) {
                std::process::exit(99);
            }
            #[cfg(debug_assertions)]
            if std::env::var("VEX_TEST_INIT_FAIL_AFTER").ok().as_deref() == Some(*name) {
                return Err(error("injected publication failure"));
            }
        }
        Ok(())
    })();
    let recovery = recover(root, false);
    match (publish, recovery) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) => Err(e),
        (_, Err(e)) => Err(format!("{e}; initialization recovery remains pending")),
    }
}

fn valid_name(name: &str) -> Result<(), String> {
    if matches!(
        name,
        "src/main.wave" | "src/lib.wave" | "vex.lock" | ".gitignore" | "vex.ws"
    ) {
        Ok(())
    } else {
        Err(error("invalid initialization journal path"))
    }
}

pub(crate) fn recover(root: &Path, dry_run: bool) -> Result<(), String> {
    let path = root.join(JOURNAL);
    reject_link(&path)?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(error(e)),
    };
    if dry_run {
        return Err(error(
            "recovery pending; dry-run cannot repair state; run vex init or vex fetch",
        ));
    }
    reject_link(&root.join(STAGE))?;
    reject_link(&root.join("src"))?;
    let record: Value = serde_json::from_str(&raw).map_err(error)?;
    if record["version"].as_u64() != Some(1) {
        return Err(error("unsupported journal version"));
    }
    let new_src = record["new_src"]
        .as_bool()
        .ok_or_else(|| error("missing source directory ownership"))?;
    let files = record["files"]
        .as_array()
        .ok_or_else(|| error("missing journal files"))?;
    let mut names = std::collections::BTreeSet::new();
    // Validate everything before any removal; user edits leave recovery pending.
    for (index, entry) in files.iter().enumerate() {
        let name = entry[0]
            .as_str()
            .ok_or_else(|| error("invalid journal filename"))?;
        let content = entry[1]
            .as_str()
            .ok_or_else(|| error("invalid journal content"))?;
        valid_name(name)?;
        if !names.insert(name) {
            return Err(error("duplicate journal path"));
        }
        let target = root.join(name);
        reject_link(&target)?;
        match fs::read(&target) {
            Ok(bytes) if bytes == content.as_bytes() => {
                let candidate = root.join(STAGE).join(index.to_string());
                reject_link(&candidate)?;
                if !same_file(&target, &candidate)? {
                    return Err(error(format!(
                        "`{name}` is not owned by initialization; preserve it for manual recovery"
                    )));
                }
            }
            Ok(_) => {
                return Err(error(format!(
                    "`{name}` was edited; preserve .vex/init-stage for recovery"
                )))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(error(e)),
        }
    }
    if files.last().and_then(|f| f[0].as_str()) != Some("vex.ws") {
        return Err(error("missing final manifest"));
    }
    let committed = root.join("vex.ws").is_file();
    if committed && names.iter().any(|n| !root.join(n).is_file()) {
        return Err(error("committed initialization has missing files"));
    }
    if !committed {
        for name in names {
            match fs::remove_file(root.join(name)) {
                Ok(()) => sync_dir(root.join(name).parent().unwrap()).map_err(error)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(error(e)),
            }
        }
        if new_src && root.join("src").exists() {
            // Never recursively remove a directory: unrelated files may exist.
            match fs::remove_dir(root.join("src")) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => (),
                Err(e) => return Err(error(e)),
            }
        }
    }
    fs::remove_file(&path).map_err(error)?;
    sync_dir(path.parent().unwrap()).map_err(error)?;
    fs::remove_dir_all(root.join(STAGE)).map_err(error)?;
    sync_dir(&root.join(".vex")).map_err(error)
}

fn same_file(first: &Path, second: &Path) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let first = fs::metadata(first).map_err(error)?;
        let second = fs::metadata(second).map_err(error)?;
        Ok(first.dev() == second.dev() && first.ino() == second.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                information: *mut u32,
            ) -> i32;
        }
        let identity = |path: &Path| -> Result<_, String> {
            let file = fs::File::open(path).map_err(error)?;
            // BY_HANDLE_FILE_INFORMATION consists of thirteen DWORDs, including
            // three FILETIME pairs. Volume serial and file index identify links.
            let mut info = [0u32; 13];
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
                return Err(error(std::io::Error::last_os_error()));
            }
            Ok((info[7], info[11], info[12]))
        };
        Ok(identity(first)? == identity(second)?)
    }
}
