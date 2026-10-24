// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
mod support;
use support::git_url;
static ID: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-cache-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn vex(&self, project: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(project)
            .env("VEX_CACHE_HOME", self.0.join("cache"))
            .args(args)
            .output()
            .unwrap()
    }
    fn source(&self, format: &str) -> PathBuf {
        let source = self.0.join("source");
        package(&source.join("packages/dep"), "dep", "");
        git(
            &source,
            &[
                "init",
                "--initial-branch=master",
                &format!("--object-format={format}"),
            ],
        );
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "fixture"]);
        source
    }
    fn app(&self, name: &str, source: &Path) -> PathBuf {
        let app = self.0.join(name);
        package(
            &app,
            "app",
            &format!(
                "{{name=\"dep\",git=\"{}\",subdir=\"packages/dep\"}}",
                git_url(source)
            ),
        );
        app
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn package(path: &Path, name: &str, deps: &str) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.wave"), "pub fun marker() {}\n").unwrap();
    fs::write(path.join("vex.ws"), format!("{{format=4,name=\"{name}\",version=\"1.0.0\",targets={{lib={{}}}},dependencies=[{deps}]}}\n")).unwrap();
}
fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(path)
        .args([
            "-c",
            "user.name=Vex Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    success(&out);
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
fn success(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exact_cached_objects_restore_offline_and_survive_cache_prune() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::new();
        let source = f.source(format);
        let first = f.app("first", &source);
        success(&f.vex(&first, &["fetch"]));
        let bytes = fs::read(first.join("vex.lock")).unwrap();
        let lock = lockfile::decode(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(lock.version, 4);
        assert_eq!(lock.packages[0].subdir.as_deref(), Some("packages/dep"));
        let second = f.app("second", &source);
        fs::write(second.join("vex.lock"), &bytes).unwrap();
        fs::rename(&source, source.with_extension("hidden")).unwrap();
        success(&f.vex(&second, &["fetch", "--offline", "--locked"]));
        assert_eq!(fs::read(second.join("vex.lock")).unwrap(), bytes);
        let out = f.vex(&second, &["metadata", "--format-version", "2", "--locked"]);
        success(&out);
        let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(Path::new(metadata["packages"][0]["root"].as_str().unwrap())
            .ends_with(Path::new("packages").join("dep")));
        success(&f.vex(&second, &["cache", "prune", "--apply"]));
        success(&f.vex(&second, &["fetch", "--offline", "--locked"]));
        let checkout = second.join(match &lock.packages[0].source {
            lockfile::LockedSource::Git { resolved, .. } => resolved,
            _ => panic!(),
        });
        assert!(!checkout.join(".git/objects/info/alternates").exists());
        assert_eq!(git(&checkout, &["fsck", "--full", "--no-dangling"]), "");
    }
}

#[test]
fn dry_run_never_restores_a_checkout_from_cache() {
    let f = Fixture::new();
    let source = f.source("sha1");
    let app = f.app("app", &source);
    success(&f.vex(&app, &["fetch"]));
    fs::remove_dir_all(app.join(".vex/deps")).unwrap();
    let out = f.vex(&app, &["fetch", "--dry-run", "--offline", "--locked"]);
    assert!(!out.status.success());
    assert!(!app.join(".vex/deps").exists());
}

#[test]
fn subdir_rejects_unsafe_syntax_and_escape_before_publication() {
    let f = Fixture::new();
    let source = f.source("sha1");
    let app = f.app("app", &source);
    for value in [
        "", "/etc", "a//b", "../dep", "a/../dep", "C:/dep", "a\\b", ".",
    ] {
        let raw = format!("{{format=4,name=\"app\",targets={{lib={{}}}},dependencies=[{{name=\"dep\",git=\"{}\",subdir={}}}]}}", git_url(&source), serde_json::to_string(value).unwrap());
        assert!(
            manifest::decode(&raw, app.join("vex.ws")).is_err(),
            "{value}"
        );
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../../../../", source.join("escape")).unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "escape"]);
        let raw = fs::read_to_string(app.join("vex.ws"))
            .unwrap()
            .replace("packages/dep", "escape");
        fs::write(app.join("vex.ws"), raw).unwrap();
        let out = f.vex(&app, &["fetch"]);
        assert!(!out.status.success());
        assert!(!app.join("vex.lock").exists());
        assert!(!app.join(".vex/deps").exists());
    }
}

#[test]
fn corrupted_cache_is_quarantined_and_live_project_is_preserved() {
    let f = Fixture::new();
    let source = f.source("sha1");
    let first = f.app("first", &source);
    success(&f.vex(&first, &["fetch"]));
    let cache = fs::read_dir(f.0.join("cache/git"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|v| v == "git"))
        .unwrap();
    fs::write(cache.join("config"), "not a git config\n").unwrap();
    let second = f.app("second", &source);
    fs::copy(first.join("vex.lock"), second.join("vex.lock")).unwrap();
    let old = fs::read(first.join("vex.lock")).unwrap();
    let out = f.vex(&second, &["fetch", "--offline", "--locked"]);
    assert!(!out.status.success());
    assert!(!second.join(".vex/deps").exists());
    assert_eq!(fs::read(first.join("vex.lock")).unwrap(), old);
    assert!(fs::read_dir(f.0.join("cache/git")).unwrap().any(|e| e
        .unwrap()
        .path()
        .to_string_lossy()
        .contains("quarantine")));
    success(&f.vex(&second, &["fetch", "--locked"]));
    success(&f.vex(&first, &["fetch", "--locked", "--offline"]));
}

#[test]
fn concurrent_projects_publish_one_cache_and_keep_isolated_worktrees() {
    let f = Fixture::new();
    let source = f.source("sha1");
    let first = f.app("first", &source);
    let second = f.app("second", &source);
    let children: Vec<_> = [&first, &second]
        .into_iter()
        .map(|project| {
            Command::new(env!("CARGO_BIN_EXE_vex"))
                .current_dir(project)
                .env("VEX_CACHE_HOME", f.0.join("cache"))
                .arg("fetch")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        success(&child.wait_with_output().unwrap());
    }
    assert_eq!(
        fs::read(first.join("vex.lock")).unwrap(),
        fs::read(second.join("vex.lock")).unwrap()
    );
    assert_eq!(
        fs::read_dir(f.0.join("cache/git"))
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|v| v == "git"))
            .count(),
        1
    );
}

#[test]
fn extending_cache_bundles_staged_objects_and_keeps_old_pins_independent() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::new();
        let source = f.source(format);
        let old = git(&source, &["rev-parse", "HEAD"]);
        let first = f.app("first", &source);
        success(&f.vex(&first, &["fetch"]));
        let old_lock = fs::read(first.join("vex.lock")).unwrap();
        // Replace the remote history. The new checkout cannot provide the old
        // objects, so extending a cache must retain its independently pinned set.
        git(&source, &["checkout", "--orphan", "replacement"]);
        fs::write(
            source.join("packages/dep/src/lib.wave"),
            "pub fun changed() {}\n",
        )
        .unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "replacement history"]);
        git(&source, &["branch", "-D", "master"]);
        git(&source, &["reflog", "expire", "--expire=now", "--all"]);
        git(&source, &["gc", "--prune=now"]);
        let new = git(&source, &["rev-parse", "HEAD"]);
        assert_ne!(old, new);
        let trace = f.0.join("cache-transfer.trace");
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&first)
            .env("VEX_CACHE_HOME", f.0.join("cache"))
            .env("GIT_TRACE", support::git_path(&trace))
            .args(["update", "dep"])
            .output()
            .unwrap();
        success(&output);
        let trace = fs::read_to_string(trace).unwrap().replace('\\', "/");
        assert!(trace.contains("bundle create"), "{trace}");
        assert!(trace.contains("bundle unbundle"), "{trace}");
        assert!(
            !trace
                .lines()
                .any(|line| line.contains("fetch --no-tags --")
                    && line.contains("/.vex/transactions/")),
            "cache must not serve a deep staged checkout: {trace}"
        );
        assert!(fs::read_to_string(first.join("vex.lock"))
            .unwrap()
            .contains(&new));
        let second = f.app("second", &source);
        fs::write(second.join("vex.lock"), &old_lock).unwrap();
        fs::rename(&source, source.with_extension("hidden")).unwrap();
        success(&f.vex(&second, &["fetch", "--locked", "--offline"]));
        assert_eq!(fs::read(second.join("vex.lock")).unwrap(), old_lock);
        success(&f.vex(&first, &["fetch", "--locked", "--offline"]));
        assert!(fs::read_dir(f.0.join("cache/git"))
            .unwrap()
            .all(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|ext| ext != "bundle")));
    }
}
