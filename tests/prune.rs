// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

mod support;
use support::{git_path, git_url};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    app: PathBuf,
    checkout: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        #[cfg(unix)]
        let root = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let root = std::env::temp_dir();
        let root = root.join(format!(
            "vp-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let source = root.join("src");
        let app = root.join("app");
        package(&source, "dep", "");
        fs::write(source.join(".gitignore"), "ignored.bin\n").unwrap();
        package(&source.join("nested"), "nested", "");
        git(&source, &["init", "-b", "master"]);
        commit(&source);
        package(
            &app,
            "app",
            &format!("{{ name = \"dep\", git = \"{}\" }}", git_url(&source)),
        );
        success(&vex(&app, &["fetch"]));
        let lock = lockfile::decode(&fs::read_to_string(app.join("vex.lock")).unwrap()).unwrap();
        let lockfile::LockedSource::Git { resolved, .. } = &lock.packages[0].source else {
            panic!("expected Git dependency")
        };
        let checkout = app.join(resolved);
        assert!(checkout.join(".git/vex-managed.json").is_file());
        Self {
            root,
            source,
            app,
            checkout,
        }
    }

    fn unused(&self) {
        package(&self.app, "app", "");
        success(&vex(&self.app, &["fetch"]));
        assert!(self.checkout.is_dir());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn preview_and_dry_run_are_unchanged_and_apply_removes_only_unused_owned_checkouts() {
    let fixture = Fixture::new();
    let output = vex(&fixture.app, &["prune", "--apply"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("current local graph"));
    assert!(fixture.checkout.is_dir());

    fixture.unused();
    let before = snapshot(&fixture.app);
    for args in [
        &["prune"][..],
        &["prune", "--dry-run"],
        &["prune", "--apply", "--dry-run"],
    ] {
        let output = vex(&fixture.app, args);
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("Would remove 1"));
        assert_eq!(snapshot(&fixture.app), before, "{args:?}");
    }
    let lock = fs::read(fixture.app.join("vex.lock")).unwrap();
    let output = vex(&fixture.app, &["prune", "--apply"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Removed 1"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("requires `vex fetch`"));
    assert!(!fixture.checkout.exists());
    assert_eq!(fs::read(fixture.app.join("vex.lock")).unwrap(), lock);
    assert!(fixture.app.join(".vex/state.lock").is_file());
    success(&vex(&fixture.app, &["fetch", "--locked", "--offline"]));
}

#[test]
fn any_user_change_in_the_checkout_or_git_metadata_is_preserved() {
    for change in [
        "dirty",
        "untracked",
        "ignored",
        "commit",
        "branch",
        "config",
        "git-file",
        "empty-directory",
        "missing-record",
        "invalid-record",
    ] {
        let fixture = Fixture::new();
        fixture.unused();
        match change {
            "dirty" => fs::write(fixture.checkout.join("src/lib.wave"), "local source").unwrap(),
            "untracked" => fs::write(fixture.checkout.join("user.txt"), "local file").unwrap(),
            "ignored" => fs::write(fixture.checkout.join("ignored.bin"), "ignored file").unwrap(),
            "commit" => {
                fs::write(fixture.checkout.join("user.txt"), "local commit").unwrap();
                commit(&fixture.checkout);
            }
            "branch" => {
                git(&fixture.checkout, &["branch", "user-work"]);
            }
            "config" => {
                git(&fixture.checkout, &["config", "user.note", "keep me"]);
            }
            "git-file" => fs::write(fixture.checkout.join(".git/user.txt"), "keep me").unwrap(),
            "empty-directory" => fs::create_dir(fixture.checkout.join("empty-user-dir")).unwrap(),
            "missing-record" => {
                fs::remove_file(fixture.checkout.join(".git/vex-managed.json")).unwrap()
            }
            "invalid-record" => {
                fs::write(fixture.checkout.join(".git/vex-managed.json"), "{}").unwrap()
            }
            _ => unreachable!(),
        }
        let before = snapshot(&fixture.app);
        let output = vex(&fixture.app, &["prune", "--apply"]);
        success(&output);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Removed 0"),
            "{change}"
        );
        assert_eq!(snapshot(&fixture.app), before, "{change}");
    }
}

#[test]
fn only_previously_owned_unchanged_checkouts_receive_new_records_on_update() {
    for changed in [false, true] {
        let fixture = Fixture::new();
        if changed {
            fs::write(fixture.checkout.join("ignored.bin"), "user output").unwrap();
        }
        fs::write(fixture.source.join("upstream.txt"), "upstream update").unwrap();
        commit(&fixture.source);
        success(&vex(&fixture.app, &["update", "dep"]));
        fixture.unused();
        let output = vex(&fixture.app, &["prune", "--apply"]);
        success(&output);
        assert_eq!(fixture.checkout.exists(), changed);
        if changed {
            assert_eq!(
                fs::read(fixture.checkout.join("ignored.bin")).unwrap(),
                b"user output"
            );
        }
    }
}

#[test]
fn incomplete_graph_and_pending_recovery_refuse_prune_without_mutation_or_network() {
    for problem in [
        "missing-lock",
        "changed-manifest",
        "missing-source",
        "pending-recovery",
    ] {
        let fixture = Fixture::new();
        match problem {
            "missing-lock" => fs::remove_file(fixture.app.join("vex.lock")).unwrap(),
            "changed-manifest" => package(&fixture.app, "app", ""),
            "missing-source" => fs::remove_dir_all(&fixture.checkout).unwrap(),
            "pending-recovery" => {
                fs::write(fixture.app.join(".vex/transaction.json"), "{}").unwrap()
            }
            _ => unreachable!(),
        }
        let trace = fixture.root.join("trace.jsonl");
        let before = snapshot(&fixture.app);
        let output = foreground(Command::new(env!("CARGO_BIN_EXE_vex")))
            .current_dir(&fixture.app)
            .args(["prune", "--apply"])
            .env("GIT_TRACE2_EVENT", git_path(&trace))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{problem}");
        assert_eq!(snapshot(&fixture.app), before, "{problem}");
        let trace = fs::read_to_string(trace).unwrap_or_default();
        for event in trace
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        {
            if event["event"] == "cmd_name" {
                assert!(
                    !matches!(
                        event["name"].as_str(),
                        Some("clone" | "fetch" | "ls-remote")
                    ),
                    "{event}"
                );
            }
        }
    }
}

#[test]
fn path_dependency_inside_an_old_git_checkout_keeps_the_entire_checkout() {
    let fixture = Fixture::new();
    let relative = fixture
        .checkout
        .strip_prefix(&fixture.app)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    package(
        &fixture.app,
        "app",
        &format!("{{ name = \"nested\", path = \"{relative}/nested\" }}"),
    );
    success(&vex(&fixture.app, &["fetch"]));
    let before = snapshot(&fixture.app);
    success(&vex(&fixture.app, &["prune", "--apply"]));
    assert_eq!(snapshot(&fixture.app), before);
}

#[test]
fn path_dependency_containing_old_checkouts_keeps_every_descendant() {
    let fixture = Fixture::new();
    package(&fixture.app.join(".vex/deps"), "collection", "");
    package(
        &fixture.app,
        "app",
        "{ name = \"collection\", path = \".vex/deps\" }",
    );
    success(&vex(&fixture.app, &["fetch"]));
    let before = snapshot(&fixture.app);
    success(&vex(&fixture.app, &["prune", "--apply"]));
    assert_eq!(snapshot(&fixture.app), before);
    assert!(fixture.checkout.is_dir());
}

#[cfg(unix)]
#[test]
fn copying_a_modified_checkout_never_restores_its_old_deletion_ownership() {
    use std::os::unix::fs::PermissionsExt;
    for change in ["directory-mode", "root-mode", "hard-link"] {
        let fixture = Fixture::new();
        // Populate FETCH_HEAD before recording the baseline so a repeated
        // same-SHA fetch need not change the Git metadata snapshot.
        success(&vex(&fixture.app, &["update", "dep"]));
        let marker = fixture.checkout.join(".git/vex-managed.json");
        let record = fs::read(&marker).unwrap();
        let permissions = if change == "hard-link" {
            fs::hard_link(
                fixture.checkout.join("src/lib.wave"),
                fixture.root.join("user-linked-source"),
            )
            .unwrap();
            None
        } else {
            let path = if change == "root-mode" {
                fixture.checkout.clone()
            } else {
                fixture.checkout.join(".git")
            };
            let mode = fs::metadata(&path).unwrap().permissions().mode() ^ 0o020;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            Some((path, mode))
        };
        for _ in 0..2 {
            success(&vex(&fixture.app, &["update", "dep"]));
            assert_eq!(fs::read(&marker).unwrap(), record, "{change}");
            assert!(
                fixture.checkout.join(".git/vex-prune-protected").is_file(),
                "{change}"
            );
            if let Some((path, expected)) = &permissions {
                assert_eq!(fs::metadata(path).unwrap().permissions().mode(), *expected);
            }
        }
        fixture.unused();
        let before = snapshot(&fixture.app);
        success(&vex(&fixture.app, &["prune", "--apply"]));
        assert_eq!(snapshot(&fixture.app), before, "{change}");
        if change == "hard-link" {
            assert_eq!(
                fs::read(fixture.root.join("user-linked-source")).unwrap(),
                b"pub fun marker() {}\n"
            );
        }
    }
}

#[test]
fn prune_output_redacts_known_credentials_from_preserved_paths() {
    let fixture = Fixture::new();
    let authenticated = "https://prune-user:PRUNE_SECRET@example.invalid/dep";
    package(
        &fixture.app,
        "app",
        &format!("{{ name = \"dep\", git = \"{authenticated}\" }}"),
    );
    let config = fixture.root.join("gitconfig");
    fs::write(
        &config,
        format!(
            "[url {:?}]\n\tinsteadOf = {authenticated}\n",
            git_url(&fixture.source)
        ),
    )
    .unwrap();
    success(
        &foreground(Command::new(env!("CARGO_BIN_EXE_vex")))
            .current_dir(&fixture.app)
            .arg("fetch")
            .env("GIT_CONFIG_GLOBAL", git_path(&config))
            .output()
            .unwrap(),
    );
    fs::write(fixture.app.join(".vex/deps/PRUNE_SECRET"), "preserve").unwrap();
    let output = vex(&fixture.app, &["prune", "--apply"]);
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("PRUNE_SECRET"), "{stdout}");
    assert!(stdout.contains("[redacted]"), "{stdout}");
    assert!(fixture.app.join(".vex/deps/PRUNE_SECRET").is_file());
}

#[test]
fn unknown_and_legacy_paths_are_preserved() {
    let fixture = Fixture::new();
    fixture.unused();
    let legacy = fixture.app.join(".vex/deps/dep");
    fs::rename(&fixture.checkout, &legacy).unwrap();
    fs::write(fixture.app.join(".vex/deps/user.txt"), "keep").unwrap();
    let before = snapshot(&fixture.app);
    success(&vex(&fixture.app, &["prune", "--apply"]));
    assert_eq!(snapshot(&fixture.app), before);
}

#[cfg(unix)]
#[test]
fn symlinks_inside_and_in_place_of_checkouts_are_preserved_without_following_them() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture.unused();
    let external = fixture.root.join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("user.txt"), "outside").unwrap();
    symlink(&external, fixture.checkout.join("user-link")).unwrap();
    symlink(&external, fixture.app.join(".vex/deps/linked-checkout")).unwrap();
    success(&vex(&fixture.app, &["prune", "--apply"]));
    assert!(fixture.checkout.is_dir());
    assert!(fixture
        .checkout
        .join("user-link")
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(external.join("user.txt")).unwrap(), b"outside");
}

fn package(path: &Path, name: &str, dependencies: &str) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.wave"), "pub fun marker() {}\n").unwrap();
    fs::write(path.join("vex.ws"), format!("{{ name = \"{name}\", version = \"0.1.0\", lib = true, dependencies = [{dependencies}] }}\n")).unwrap();
}

fn git(path: &Path, args: &[&str]) {
    success(
        &foreground(Command::new("git"))
            .args([
                "-c",
                "user.name=Vex Test",
                "-c",
                "user.email=vex@example.invalid",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .unwrap(),
    );
}

fn commit(path: &Path) {
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "fixture"]);
}

fn vex(path: &Path, args: &[&str]) -> Output {
    foreground(Command::new(env!("CARGO_BIN_EXE_vex")))
        .current_dir(path)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// Git 2.55+ may detach post-commit/fetch maintenance. Its temporary files
// otherwise race the strict whole-project snapshots even after the initiating
// command exits. Keep maintenance enabled but wait for it in these fixtures.
fn foreground(mut command: Command) -> Command {
    command
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "maintenance.autoDetach")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "gc.autoDetach")
        .env("GIT_CONFIG_VALUE_1", "false");
    command
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if path.is_dir() {
                result.insert(relative, Vec::new());
                visit(root, &path, result);
            } else {
                result.insert(
                    relative,
                    fs::read(&path)
                        .unwrap_or_else(|error| panic!("snapshot {}: {error}", path.display())),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}
