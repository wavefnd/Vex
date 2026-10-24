// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{parse, render, Lockfile, LOCKFILE_NAME};

pub fn read_lockfile() -> Result<Option<Lockfile>, diagnostic::Error> {
    let path = Path::new(LOCKFILE_NAME);
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }

    let raw = wson::read_document(path).map_err(|e| {
        diagnostic::Error::new(
            if e.kind() == std::io::ErrorKind::InvalidData {
                diagnostic::Category::Resolution
            } else {
                diagnostic::Category::Environment
            },
            format!("failed to read `{LOCKFILE_NAME}`: {e}"),
        )
    })?;
    parse::parse_lockfile(&raw)
        .map(Some)
        .map_err(diagnostic::Error::resolution)
}

pub fn write_lockfile(lockfile: &Lockfile) -> Result<(), String> {
    let content = render::render_lockfile(lockfile.clone().normalized());
    atomic_write(Path::new(LOCKFILE_NAME), |file| file.write_all(content.as_bytes()))
        .map_err(|e| format!("failed to write `{LOCKFILE_NAME}`: {e}\nhelp: check free disk space and directory permissions, then retry `vex fetch`; the previous lockfile has been preserved"))
}

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TemporaryPath(PathBuf);

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn atomic_write(
    destination: &Path,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let (temporary, mut file) = loop {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".vex.lock-{}-{id}.tmp", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => break (TemporaryPath(path), file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    write(&mut file)?;
    file.sync_all()?;
    drop(file);
    // Both paths share a directory/filesystem. std::fs::rename replaces the
    // destination atomically (MoveFileExW with REPLACE_EXISTING on Windows).
    // Never remove the destination first: failed replacement must preserve it.
    fs::rename(&temporary.0, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("vex-lock-store-{}-{id}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn failed_write_preserves_previous_lock_and_removes_temporary() {
        for existing in [false, true] {
            let dir = TestDir::new();
            let lock = dir.0.join(LOCKFILE_NAME);
            if existing {
                fs::write(&lock, "previous").unwrap();
            }
            let result = atomic_write(&lock, |file| {
                file.write_all(b"partial")?;
                Err(std::io::Error::other("injected write failure"))
            });
            assert!(result.is_err());
            assert_eq!(lock.exists(), existing);
            if existing {
                assert_eq!(fs::read(&lock).unwrap(), b"previous");
            }
            assert_eq!(fs::read_dir(&dir.0).unwrap().count(), usize::from(existing));
        }
    }

    #[test]
    fn failed_replacement_preserves_destination_and_removes_temporary() {
        let dir = TestDir::new();
        let lock = dir.0.join(LOCKFILE_NAME);
        fs::create_dir(&lock).unwrap();
        fs::write(lock.join("keep"), "previous").unwrap();
        assert!(atomic_write(&lock, |file| file.write_all(b"replacement")).is_err());
        assert_eq!(fs::read(lock.join("keep")).unwrap(), b"previous");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn successful_write_creates_and_replaces_complete_file() {
        let dir = TestDir::new();
        let lock = dir.0.join(LOCKFILE_NAME);
        for content in [b"first".as_slice(), b"replacement"] {
            atomic_write(&lock, |file| file.write_all(content)).unwrap();
            assert_eq!(fs::read(&lock).unwrap(), content);
            assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        }
    }
}
