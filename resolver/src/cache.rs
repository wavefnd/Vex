// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Shared immutable Git objects; project worktrees never depend on cache lifetime.
use crate::{git, paths::git_cli_path};
use diagnostic::Error;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> Result<PathBuf, Error> {
    if let Some(path) = std::env::var_os("VEX_CACHE_HOME") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(Error::environment("VEX_CACHE_HOME must be absolute"));
        }
        return Ok(path);
    }
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Caches"));
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")));
    base.filter(|p| p.is_absolute())
        .map(|p| p.join("vex"))
        .ok_or_else(|| Error::environment("cannot locate user cache; set VEX_CACHE_HOME"))
}

struct Cache {
    path: PathBuf,
    _guard: state::FileGuard,
}
impl Cache {
    fn open(url: &str) -> Result<Self, Error> {
        // Credential contexts may expose different repositories at the same URL.
        // Hash the declaration, never persist or display its authentication.
        let key = format!("{:x}", Sha256::digest(url.as_bytes()));
        let root = home()?.join("git");
        fs::create_dir_all(&root).map_err(Error::environment)?;
        state::reject_link(&root).map_err(Error::environment)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(Error::environment)?;
        }
        let guard = state::FileGuard::exclusive(&root.join(format!("{key}.lock")), || {})
            .map_err(Error::environment)?;
        Ok(Self {
            path: root.join(format!("{key}.git")),
            _guard: guard,
        })
    }
    fn valid(&self, url: &str) -> Result<bool, Error> {
        state::reject_link(&self.path).map_err(Error::environment)?;
        if !self.path.exists() {
            return Ok(false);
        }
        // No source content is trusted until Git verifies its complete object graph.
        let checked = (|| {
            crate::transaction::reject_git_metadata_links(&self.path)
                .map_err(Error::environment)?;
            let origin = git::stdout(
                bare(&self.path).args(["config", "--get", "remote.origin.url"]),
                "read cache origin",
            )?;
            if origin.trim_end() != source::identity(url) {
                return Err(Error::resolution("cache source mismatch"));
            }
            git::run(
                bare(&self.path).args(["fsck", "--full", "--no-dangling"]),
                "verify cached objects",
            )
        })();
        if let Err(error) = checked {
            if process::cancelled() || process::failure_code() == 124 {
                return Err(error);
            }
            let quarantine = self.path.with_extension(format!("quarantine-{}", stamp()));
            fs::rename(&self.path, quarantine).map_err(Error::environment)?;
            state::sync_dir(self.path.parent().unwrap())?;
            return Ok(false);
        }
        Ok(true)
    }
}

fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
fn local() -> Command {
    let mut command = git::command();
    // Even a user's insteadOf rewrite cannot turn an offline import into HTTP/SSH.
    command.args([
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.file.allow=always",
    ]);
    command
}
fn bare(path: &Path) -> Command {
    let mut command = local();
    // Match project Git commands: let Win32 enter the repository and keep
    // Git's own object paths relative, including in index-pack children.
    command.current_dir(path).args(["--git-dir", "."]);
    command
}

pub(crate) fn restore(destination: &Path, url: &str, commit: &str) -> Result<bool, Error> {
    let cache = Cache::open(url)?;
    if !cache.valid(url)? {
        return Ok(false);
    }
    if !matches!(git::stdout(bare(&cache.path).args(["cat-file", "-t", commit]), "inspect cached commit"), Ok(value) if value == "commit")
    {
        return Ok(false);
    }
    if !destination.join(".git").exists() {
        fs::create_dir_all(destination.parent().unwrap()).map_err(Error::environment)?;
        if destination.as_os_str().len() > 200 {
            let format = git::stdout(
                bare(&cache.path).args(["rev-parse", "--show-object-format"]),
                "read cached object format",
            )?;
            if !matches!(format.as_str(), "sha1" | "sha256") {
                return Err(Error::resolution("unsupported cached object format"));
            }
            fs::create_dir(destination).map_err(Error::environment)?;
            git::run(
                local().current_dir(destination).args([
                    "init",
                    "--template=",
                    &format!("--object-format={format}"),
                    "--",
                    ".",
                ]),
                "initialize cached checkout",
            )?;
            let mut command = git::command_in(destination);
            command
                .args([
                    "-c",
                    "protocol.allow=never",
                    "-c",
                    "protocol.file.allow=always",
                    "fetch",
                    "--no-tags",
                    "--",
                ])
                .arg(git_cli_path(&cache.path).as_ref())
                .arg(commit);
            git::run(&mut command, "restore cached Git objects")?;
        } else {
            git::run(
                local()
                    .args([
                        "clone",
                        "--no-hardlinks",
                        "--dissociate",
                        "--no-checkout",
                        "--local",
                        "--",
                    ])
                    .arg(git_cli_path(&cache.path).as_ref())
                    .arg(git_cli_path(destination).as_ref()),
                "restore cached Git objects",
            )?;
        }
        git::run(
            git::command_in(destination).args([
                "config",
                "--replace-all",
                "remote.origin.url",
                &source::identity(url),
            ]),
            "store cached source origin",
        )?;
        git::run(
            git::command_in(destination).args(["checkout", "--detach", commit]),
            "populate cached checkout",
        )?;
    } else {
        git::verify_origin(destination, url)?;
        let mut command = git::command_in(destination);
        command
            .args([
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always",
                "fetch",
                "--no-tags",
                "--",
            ])
            .arg(git_cli_path(&cache.path).as_ref())
            .arg(commit);
        git::run(&mut command, "import cached Git objects")?;
    }
    git::has_commit(destination, commit)
}

pub(crate) fn store(checkout: &Path, url: &str, commit: &str) -> Result<(), Error> {
    let cache = Cache::open(url)?;
    if cache.valid(url)? {
        if matches!(git::stdout(bare(&cache.path).args(["cat-file", "-t", commit]), "inspect cached commit"), Ok(value) if value == "commit")
        {
            return Ok(());
        }
        // Do not serve a deep transactional checkout through upload-pack.
        // Git for Windows can select the wrong repository or exceed its loose
        // object path limit in that nested local transport. A self-contained
        // bundle is produced from the checkout's own cwd, then indexed locally;
        // no transport rewrites, repository refs, or alternates are involved.
        let bundle = cache.path.with_extension(format!(
            "incoming-{}-{}.bundle",
            std::process::id(),
            stamp()
        ));
        let result = (|| {
            git::run(
                git::command_in(checkout)
                    .args(["bundle", "create"])
                    .arg(git_cli_path(&bundle).as_ref())
                    .args(["HEAD", commit]),
                "bundle verified Git objects",
            )?;
            git::run(
                bare(&cache.path)
                    .args(["bundle", "verify"])
                    .arg(git_cli_path(&bundle).as_ref()),
                "verify Git object bundle",
            )?;
            git::run(
                bare(&cache.path)
                    .args(["bundle", "unbundle"])
                    .arg(git_cli_path(&bundle).as_ref()),
                "import verified Git objects",
            )?;
            git::run(
                bare(&cache.path).args(["update-ref", &format!("refs/vex/pins/{commit}"), commit]),
                "retain verified Git object",
            )?;
            git::run(
                bare(&cache.path).args(["fsck", "--full", "--no-dangling"]),
                "verify extended cache",
            )?;
            crate::transaction::sync_tree(&cache.path).map_err(Error::environment)
        })();
        if bundle.exists() {
            let _ = fs::remove_file(&bundle);
        }
        result?;
        return Ok(());
    }
    let stage = cache
        .path
        .with_extension(format!("stage-{}-{}", std::process::id(), stamp()));
    let result = (|| {
        git::run(
            local()
                .args([
                    "clone",
                    "--bare",
                    "--no-hardlinks",
                    "--dissociate",
                    "--local",
                    "--",
                ])
                .arg(git_cli_path(checkout).as_ref())
                .arg(git_cli_path(&stage).as_ref()),
            "cache verified Git objects",
        )?;
        git::run(
            bare(&stage).args([
                "config",
                "--replace-all",
                "remote.origin.url",
                &source::identity(url),
            ]),
            "store credential-free cache origin",
        )?;
        git::run(
            bare(&stage).args(["update-ref", &format!("refs/vex/pins/{commit}"), commit]),
            "retain locked cached object",
        )?;
        git::run(
            bare(&stage).args(["fsck", "--full", "--no-dangling"]),
            "verify new cache",
        )?;
        crate::transaction::sync_tree(&stage).map_err(Error::environment)?;
        fs::rename(&stage, &cache.path).map_err(Error::environment)?;
        state::sync_dir(cache.path.parent().unwrap())?;
        Ok(())
    })();
    if stage.exists() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

/// Explicit cache cleanup. Project checkouts own all their objects, so no project
/// liveness guessing is needed. Coordination inodes and quarantines are retained.
pub fn prune(apply: bool) -> Result<Vec<PathBuf>, Error> {
    let root = home()?.join("git");
    state::reject_link(&root).map_err(Error::environment)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut candidates = fs::read_dir(&root)
        .map_err(Error::environment)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    candidates.sort();
    let mut removed = Vec::new();
    for path in candidates {
        let Some(key) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().and_then(|s| s.to_str()) != Some("git")
            || key.len() != 64
            || !key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        let _guard = state::FileGuard::exclusive(&root.join(format!("{key}.lock")), || {})
            .map_err(Error::environment)?;
        state::reject_link(&path).map_err(Error::environment)?;
        if !path.is_dir() {
            continue;
        }
        // Only recognized bare repositories in the cache namespace are removed.
        if git::stdout(
            bare(&path).args(["rev-parse", "--is-bare-repository"]),
            "inspect cache repository",
        )? != "true"
        {
            continue;
        }
        if apply {
            fs::remove_dir_all(&path).map_err(Error::environment)?;
            state::sync_dir(&root)?;
        }
        removed.push(path);
    }
    Ok(removed)
}
