// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Project coordination. A guard is also an inheritable lease for synchronous
//! Git/compiler children. Close it before spawning the user's program.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub mod artifacts;
pub mod edit;
mod scaffold;
pub use scaffold::initialize;

#[derive(Debug)]
pub struct Guard {
    _file: File,
    root: PathBuf,
}

impl Guard {
    pub fn acquire(shared: bool, mut waiting: impl FnMut(&str, String)) -> Result<Self, String> {
        Self::acquire_mode(shared, true, &mut waiting)
    }

    /// Read an already coordinated project without creating or repairing state.
    pub fn acquire_existing(mut waiting: impl FnMut(&str, String)) -> Result<Self, String> {
        Self::acquire_mode(true, false, &mut waiting)
    }

    /// Mutate explicitly selected owned state, without creating coordination or
    /// recovering a transaction as a side effect of a cleanup command.
    pub fn acquire_existing_exclusive(
        mut waiting: impl FnMut(&str, String),
    ) -> Result<Self, String> {
        Self::acquire_mode(false, false, &mut waiting)
    }

    fn acquire_mode(
        shared: bool,
        create: bool,
        waiting: &mut impl FnMut(&str, String),
    ) -> Result<Self, String> {
        process::install_handlers()?;
        let root = std::env::current_dir()
            .and_then(|p| p.canonicalize())
            .map_err(|e| e.to_string())?;
        let managed = root.join(".vex");
        if create {
            ensure_dir(&managed)?;
        } else {
            reject_link(&managed)?;
        }
        let path = managed.join("state.lock");
        reject_link(&path)?;
        let file = acquire_file(&path, shared, create, || {
            waiting("Waiting", format!("project state in {}", root.display()))
        })
        .map_err(|e| format!("cannot coordinate project state: {e}; refusing unlocked access; run `vex fetch` first"))?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("project coordination path must be a regular file".into());
        }
        inherit(&file).map_err(|e| format!("cannot protect compiler/Git child lifetime: {e}"))?;
        let guard = Self { _file: file, root };
        scaffold::recover(guard.root(), shared || !create)?;
        Ok(guard)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// A persistent coordination inode, never an existence-based PID lock.
/// Callers acquire the project lease before a cache lease, never in reverse.
#[derive(Debug)]
pub struct FileGuard(File);
impl FileGuard {
    pub fn exclusive(path: &Path, waiting: impl FnOnce()) -> Result<Self, String> {
        reject_link(path)?;
        let file = acquire_file(path, false, true, waiting).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("coordination path must be a regular file".into());
        }
        inherit(&file).map_err(|e| e.to_string())?;
        Ok(Self(file))
    }
    pub fn sync(&self) -> Result<(), String> {
        self.0.sync_all().map_err(|e| e.to_string())
    }
}

pub fn reject_link(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            let link = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let link = metadata.file_type().is_symlink();
            if link {
                return Err(format!(
                    "managed path `{}` must not be a symbolic link or reparse point",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot inspect `{}`: {e}", path.display())),
    }
}

pub fn ensure_dir(path: &Path) -> Result<(), String> {
    reject_link(path)?;
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            reject_link(path)?;
            if path.is_dir() {
                Ok(())
            } else {
                Err(format!("`{}` is not a directory", path.display()))
            }
        }
        Err(e) => Err(format!("cannot create `{}`: {e}", path.display())),
    }
}

#[cfg(unix)]
fn acquire_file(
    path: &Path,
    shared: bool,
    create: bool,
    waiting: impl FnOnce(),
) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut waiting = Some(waiting);
    loop {
        if process::cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "operation cancelled",
            ));
        }
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) => {
                if let Some(waiting) = waiting.take() {
                    waiting();
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
    }
    Ok(file)
}

#[cfg(windows)]
fn acquire_file(
    path: &Path,
    shared: bool,
    create: bool,
    waiting: impl FnOnce(),
) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut waiting = Some(waiting);
    loop {
        if process::cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "operation cancelled",
            ));
        }
        let result = OpenOptions::new()
            .read(true)
            .write(!shared)
            .share_mode(if shared { 1 } else { 0 })
            .open(path);
        match result {
            Ok(file) => return Ok(file),
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .share_mode(0)
                    .open(path)
                {
                    Ok(file) if !shared => return Ok(file),
                    Ok(file) => drop(file),
                    Err(e)
                        if e.kind() == io::ErrorKind::AlreadyExists
                            || e.raw_os_error() == Some(32) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) if e.raw_os_error() == Some(32) => {
                if let Some(waiting) = waiting.take() {
                    waiting();
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
fn inherit(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: fcntl operates on this live owned fd. Vex spawns synchronous children
    // while the guard is alive and drops it before launching a user program.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn inherit(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    // Share restrictions follow the inherited open handle, unlike byte-range
    // locks owned by the exiting parent. Rust 1.96 Command inherits flagged handles.
    let result = unsafe { SetHandleInformation(file.as_raw_handle(), 1, 1) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Persist directory entry updates on platforms with a directory fsync API.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}

/// Replace only after the complete candidate has been synced. On Windows use
/// write-through publication; on Unix sync both containing directories.
pub fn atomic_rename(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::rename(from, to)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        let wide = |p: &Path| -> io::Result<Vec<u16>> {
            let mut value: Vec<_> = p.as_os_str().encode_wide().collect();
            if value.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "NUL in publication path",
                ));
            }
            value.push(0);
            Ok(value)
        };
        let from = wide(from)?;
        let to = wide(to)?;
        // REPLACE_EXISTING | WRITE_THROUGH. Paths are on the same project volume.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    sync_dir(from.parent().unwrap())?;
    sync_dir(to.parent().unwrap())
}
