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

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn insufficient_local_graph_requires_fetch_without_implicit_discovery() {
    let fixture = TestDir::new();
    let alpha = fixture.path().join("alpha");
    let beta = fixture.path().join("beta");
    let app = fixture.path().join("app");
    for (path, name) in [(&alpha, "alpha"), (&beta, "beta")] {
        create_package(path, name, &[]);
        init_git(path);
        commit_all(path, "initial");
    }
    create_package(&app, "app", &[("alpha", git_url(&alpha), None)]);
    let output = vex(&app, &["update", "typo"]);
    assert_failure(&output, "selection without local graph");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("run `vex fetch` first"), "{stderr}");
    assert!(
        !stderr.contains("Cloning") && !stderr.contains("Fetching"),
        "{stderr}"
    );
    assert!(!app.join(".vex/deps").exists());
    assert!(!app.join("vex.lock").exists());

    assert_success(&vex(&app, &["fetch"]), "prepare graph");
    let lock = fs::read(app.join("vex.lock")).unwrap();
    let head = git_stdout(
        &app.join(".vex/deps/pkg_8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8"),
        &["rev-parse", "HEAD"],
    );
    create_package(
        &app,
        "app",
        &[
            ("alpha", git_url(&alpha), None),
            ("beta", git_url(&beta), None),
        ],
    );
    for name in ["typo", "alpha", "beta"] {
        let output = vex(&app, &["update", name]);
        assert_failure(&output, "changed graph needs preparation");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("run `vex fetch` first"), "{stderr}");
        assert!(
            !stderr.contains("Cloning") && !stderr.contains("Fetching"),
            "{stderr}"
        );
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
        assert_eq!(
            git_stdout(&app.join(".vex/deps/pkg_8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8"), &["rev-parse", "HEAD"]),
            head
        );
        assert!(!app
            .join(".vex/deps/pkg_f44e64e75f3948e9f73f8dfa94721c4ce8cbb4f265c4790c702b2d41cfbf2753")
            .exists());
    }
}

#[test]
fn later_invalid_dependency_preserves_every_live_checkout_and_lock() {
    let fixture = TestDir::new();
    let alpha = fixture.path().join("alpha");
    let beta = fixture.path().join("beta");
    let app = fixture.path().join("app");
    for (path, name) in [(&alpha, "alpha"), (&beta, "beta")] {
        create_package(path, name, &[]);
        init_git(path);
        commit_all(path, "initial");
    }
    create_package(
        &app,
        "app",
        &[
            ("alpha", git_url(&alpha), None),
            ("beta", git_url(&beta), None),
        ],
    );
    assert_success(&vex(&app, &["fetch"]), "prepare graph");
    let lock = fs::read(app.join("vex.lock")).unwrap();
    let heads: Vec<_> = ["alpha", "beta"]
        .iter()
        .map(|n| git_stdout(&checkout(&app, n), &["rev-parse", "HEAD"]))
        .collect();
    fs::write(alpha.join("new-file"), "valid change").unwrap();
    commit_all(&alpha, "new alpha");
    fs::write(beta.join("vex.ws"), "{ name = false }").unwrap();
    commit_all(&beta, "invalid beta manifest");
    assert_failure(&vex(&app, &["update"]), "reject incomplete candidate graph");
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
    for (name, head) in ["alpha", "beta"].iter().zip(heads) {
        assert_eq!(
            git_stdout(&checkout(&app, name), &["rev-parse", "HEAD"]),
            head
        );
        assert_eq!(
            git_stdout(
                &checkout(&app, name),
                &["rev-parse", "refs/remotes/origin/master"]
            ),
            head
        );
    }
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "last locked graph remains usable",
    );
}

#[test]
fn interrupted_publication_is_recovered_by_the_next_command_but_not_dry_run() {
    let fixture = TestDir::new();
    let alpha = fixture.path().join("alpha");
    let app = fixture.path().join("app");
    create_package(&alpha, "alpha", &[]);
    init_git(&alpha);
    commit_all(&alpha, "initial");
    create_package(&app, "app", &[("alpha", git_url(&alpha), None)]);
    assert_success(&vex(&app, &["fetch"]), "prepare graph");
    let lock = read_lock(&app);
    let live =
        app.join(".vex/deps/pkg_8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8");
    let head = git_stdout(&live, &["rev-parse", "HEAD"]);
    let transaction = app.join(".vex/transactions/123");
    fs::create_dir_all(transaction.join("backup")).unwrap();
    fs::create_dir_all(transaction.join("deps")).unwrap();
    let record = format!(
        r#"{{"version":1,"directory":"123","entries":[{{"name":"pkg_8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8","had_old":true}}],"old_lock":{lock:?},"new_lock":null,"committed":false}}"#
    );
    fs::write(app.join(".vex/transaction.json"), &record).unwrap();
    fs::rename(
        &live,
        transaction
            .join("backup/pkg_8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8"),
    )
    .unwrap();
    // Recovery preflight must fail before invoking any compiler, independently
    // of whether the host has wavec on PATH or in a managed installation.
    let dry = Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(["check", "--dry-run", "--locked"])
        .current_dir(&app)
        .env(
            "VEX_WAVEC",
            fixture.path().join("deliberately-missing-wavec"),
        )
        .output()
        .expect("Vex recovery preflight must start");
    assert_failure(&dry, "dry-run cannot recover");
    assert!(String::from_utf8_lossy(&dry.stderr).contains("recovery is pending"));
    assert!(!live.exists());
    assert_eq!(
        fs::read_to_string(app.join(".vex/transaction.json")).unwrap(),
        record
    );
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "restart recovery",
    );
    assert_eq!(git_stdout(&live, &["rev-parse", "HEAD"]), head);
    assert_eq!(read_lock(&app), lock);
    assert!(!app.join(".vex/transaction.json").exists());
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vex targeted update test-{}-{id}#fixture",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("test directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn targeted_update_preserves_unrelated_commits_and_fetches() {
    let fixture = TestDir::new();
    let alpha = fixture.path().join("alpha");
    let beta = fixture.path().join("beta");
    let app = fixture.path().join("app");

    create_package(&alpha, "alpha", &[]);
    init_git(&alpha);
    let alpha_initial = commit_all(&alpha, "initial alpha");

    create_package(&beta, "beta", &[]);
    init_git(&beta);
    let beta_initial = commit_all(&beta, "initial beta");

    create_package(
        &app,
        "app",
        &[
            ("alpha", git_url(&alpha), Some("master")),
            ("beta", git_url(&beta), Some("master")),
        ],
    );

    assert_success(&vex(&app, &["fetch"]), "initial vex fetch");
    assert_eq!(locked_commit(&app, "alpha"), alpha_initial);
    assert_eq!(locked_commit(&app, "beta"), beta_initial);

    let locked_update = vex(&app, &["update", "alpha", "--locked"]);
    assert_failure(&locked_update, "locked targeted update");
    assert!(String::from_utf8_lossy(&locked_update.stderr)
        .contains("`--locked` cannot be used with `vex update`"));
    let offline_update = vex(&app, &["update", "alpha", "--offline"]);
    assert_failure(&offline_update, "offline targeted update");
    assert!(String::from_utf8_lossy(&offline_update.stderr)
        .contains("`--offline` cannot be used with `vex update`"));

    fs::write(alpha.join("REVISION.txt"), "new alpha revision\n")
        .expect("alpha update must be written");
    let alpha_updated = commit_all(&alpha, "update alpha");
    fs::write(beta.join("REVISION.txt"), "new beta revision\n")
        .expect("beta update must be written");
    let beta_updated = commit_all(&beta, "update beta");

    let update_alpha = vex(&app, &["update", "alpha"]);
    assert_success(&update_alpha, "targeted alpha update");
    let update_stderr = String::from_utf8_lossy(&update_alpha.stderr);
    assert!(update_stderr.contains("Updating"), "{update_stderr}");
    assert!(update_stderr.contains("`alpha`"), "{update_stderr}");
    assert_eq!(locked_commit(&app, "alpha"), alpha_updated);
    assert_eq!(locked_commit(&app, "beta"), beta_initial);
    assert_eq!(
        git_stdout(
            &app.join(
                ".vex/deps/pkg_f44e64e75f3948e9f73f8dfa94721c4ce8cbb4f265c4790c702b2d41cfbf2753"
            ),
            &["rev-parse", "refs/remotes/origin/master"]
        ),
        beta_initial,
        "the unrelated beta checkout must not fetch its updated branch"
    );

    let lock_before_unknown = read_lock(&app);
    let unknown = vex(&app, &["update", "missing"]);
    assert_failure(&unknown, "unknown targeted package update");
    let unknown_stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(
        unknown_stderr.contains("package `missing`"),
        "{unknown_stderr}"
    );
    assert!(
        unknown_stderr.contains("available Git packages: alpha, beta"),
        "{unknown_stderr}"
    );
    assert!(
        unknown_stderr.contains("vex update <package>..."),
        "{unknown_stderr}"
    );
    assert_eq!(read_lock(&app), lock_before_unknown);

    let update_all = vex(&app, &["update"]);
    assert_success(&update_all, "complete dependency update");
    assert_eq!(locked_commit(&app, "alpha"), alpha_updated);
    assert_eq!(locked_commit(&app, "beta"), beta_updated);
}

#[test]
fn targeted_update_accepts_transitive_packages_and_recalculates_their_graph() {
    let fixture = TestDir::new();
    let leaf = fixture.path().join("leaf");
    let added = fixture.path().join("added");
    let middle = fixture.path().join("middle");
    let app = fixture.path().join("app");

    create_package(&leaf, "leaf", &[]);
    init_git(&leaf);
    let leaf_initial = commit_all(&leaf, "initial leaf");

    create_package(&added, "added", &[]);
    init_git(&added);
    let added_commit = commit_all(&added, "initial added");

    create_package(
        &middle,
        "middle",
        &[("leaf", git_url(&leaf), Some("master"))],
    );
    init_git(&middle);
    let middle_initial = commit_all(&middle, "initial middle");

    create_package(&app, "app", &[("middle", git_url(&middle), Some("master"))]);
    assert_success(&vex(&app, &["fetch"]), "initial transitive fetch");

    fs::write(leaf.join("REVISION.txt"), "new leaf revision\n")
        .expect("leaf update must be written");
    let leaf_updated = commit_all(&leaf, "update leaf");

    let update_leaf = vex(&app, &["update", "leaf"]);
    assert_success(&update_leaf, "targeted transitive update");
    assert_eq!(locked_commit(&app, "leaf"), leaf_updated);
    assert_eq!(locked_commit(&app, "middle"), middle_initial);
    assert_ne!(leaf_initial, leaf_updated);

    create_package(
        &middle,
        "middle",
        &[
            ("leaf", git_url(&leaf), Some("master")),
            ("added", git_url(&added), Some("master")),
        ],
    );
    let middle_updated = commit_all(&middle, "add transitive dependency");

    let update_middle = vex(&app, &["update", "middle"]);
    assert_success(&update_middle, "targeted graph-changing update");
    assert_eq!(locked_commit(&app, "middle"), middle_updated);
    assert_eq!(locked_commit(&app, "leaf"), leaf_updated);
    assert_eq!(locked_commit(&app, "added"), added_commit);
    assert!(read_lock(&app).contains("dependencies = [\"added\", \"leaf\"]"));
}

fn create_package(path: &Path, name: &str, dependencies: &[(&str, String, Option<&str>)]) {
    fs::create_dir_all(path.join("src")).expect("package source directory must be created");
    fs::write(path.join("src/lib.wave"), "pub fun package_marker() {}\n")
        .expect("library entry must be written");
    let dependency_entries = dependencies
        .iter()
        .map(|(dependency, url, branch)| match branch {
            Some(branch) => format!(
                "        {{ name = \"{dependency}\", git = \"{url}\", branch = \"{branch}\" }}"
            ),
            None => format!("        {{ name = \"{dependency}\", git = \"{url}\" }}"),
        })
        .collect::<Vec<_>>();
    let dependencies = if dependency_entries.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{}\n    ]", dependency_entries.join(",\n"))
    };
    let manifest = format!(
        "{{\n    name = \"{name}\",\n    version = 0.1.0,\n    lib = true,\n    dependencies = {dependencies}\n}}\n"
    );
    fs::write(path.join("vex.ws"), manifest).expect("manifest must be written");
}

fn init_git(path: &Path) {
    let output = Command::new("git")
        .args(["init", "-q", "-b", "master"])
        .current_dir(path)
        .output()
        .expect("git init must start");
    assert_success(&output, "git init");
}

fn commit_all(path: &Path, message: &str) -> String {
    let add = Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("git add must start");
    assert_success(&add, "git add");

    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=Vex Test",
            "-c",
            "user.email=vex@example.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ])
        .current_dir(path)
        .output()
        .expect("git commit must start");
    assert_success(&commit, "git commit");
    git_stdout(path, &["rev-parse", "HEAD"])
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("git command must start");
    assert_success(&output, "git command");
    String::from_utf8(output.stdout)
        .expect("git output must be UTF-8")
        .trim()
        .to_string()
}

fn vex(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(args)
        .current_dir(path)
        .output()
        .expect("vex command must start")
}

fn read_lock(app: &Path) -> String {
    fs::read_to_string(app.join("vex.lock")).expect("vex.lock must exist")
}

fn locked_commit(app: &Path, package: &str) -> String {
    let lock = read_lock(app);
    let marker = format!("name = \"{package}\"");
    let package_entry = lock
        .split_once(&marker)
        .unwrap_or_else(|| panic!("package `{package}` must be present in lockfile"))
        .1;
    package_entry
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("commit = \"")
                .and_then(|value| value.strip_suffix("\","))
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("package `{package}` must have a locked commit"))
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &Output, action: &str) {
    assert!(
        !output.status.success(),
        "{action} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn checkout(root: &Path, name: &str) -> std::path::PathBuf {
    let lock = lockfile::decode(&std::fs::read_to_string(root.join("vex.lock")).unwrap()).unwrap();
    match &lock.package(name).unwrap().source {
        lockfile::LockedSource::Git { resolved, .. } => root.join(resolved),
        _ => panic!("expected Git source"),
    }
}

#[test]
fn updating_parent_replaces_and_then_removes_its_transitive_source() {
    let fixture = TestDir::new();
    let original = fixture.path().join("original");
    let replacement = fixture.path().join("replacement");
    let middle = fixture.path().join("middle");
    let app = fixture.path().join("app");
    let unrelated = fixture.path().join("unrelated");
    create_package(&unrelated, "unrelated", &[]);
    init_git(&unrelated);
    commit_all(&unrelated, "unrelated initial");
    for (path, message) in [
        (&original, "original source"),
        (&replacement, "replacement source"),
    ] {
        create_package(path, "leaf", &[]);
        init_git(path);
        commit_all(path, message);
    }
    create_package(&middle, "middle", &[("leaf", git_url(&original), None)]);
    init_git(&middle);
    commit_all(&middle, "original graph");
    create_package(
        &app,
        "app",
        &[
            ("middle", git_url(&middle), None),
            ("unrelated", git_url(&unrelated), None),
        ],
    );
    assert_success(&vex(&app, &["fetch"]), "initial transitive graph");
    let unrelated_checkout = checkout(&app, "unrelated");
    let unrelated_head = git_stdout(&unrelated_checkout, &["rev-parse", "HEAD"]);
    let unrelated_refs = git_stdout(
        &unrelated_checkout,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/remotes",
        ],
    );
    fs::write(unrelated.join("new"), "not selected").unwrap();
    commit_all(&unrelated, "unrelated remote advances");
    let old = git_stdout(&checkout(&app, "leaf"), &["rev-parse", "HEAD"]);
    create_package(&middle, "middle", &[("leaf", git_url(&replacement), None)]);
    commit_all(&middle, "replace transitive source");
    assert_success(
        &vex(&app, &["update", "middle"]),
        "replace transitive source",
    );
    let leaf = checkout(&app, "leaf");
    assert_ne!(git_stdout(&leaf, &["rev-parse", "HEAD"]), old);
    assert_eq!(
        git_stdout(&leaf, &["config", "--get", "remote.origin.url"]),
        git_url(&replacement)
    );
    let replacement_graph = lockfile::decode(&read_lock(&app)).unwrap();
    assert_eq!(
        replacement_graph.package("middle").unwrap().dependencies,
        ["leaf"]
    );
    assert!(
        matches!(&replacement_graph.package("leaf").unwrap().source, lockfile::LockedSource::Git {commit, ..} if commit == &git_stdout(&replacement, &["rev-parse", "HEAD"]))
    );
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "reuse replacement offline",
    );
    create_package(&middle, "middle", &[]);
    commit_all(&middle, "remove transitive source");
    assert_success(&vex(&app, &["update", "middle"]), "remove transitive edge");
    let graph = lockfile::decode(&read_lock(&app)).unwrap();
    assert!(graph.package("leaf").is_none());
    assert!(graph.package("middle").unwrap().dependencies.is_empty());
    assert!(
        leaf.is_dir(),
        "unreferenced checkouts are retained, without implicit GC"
    );
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "reuse reduced graph offline",
    );
    assert_eq!(
        git_stdout(&unrelated_checkout, &["rev-parse", "HEAD"]),
        unrelated_head
    );
    assert_eq!(
        git_stdout(
            &unrelated_checkout,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/remotes"
            ]
        ),
        unrelated_refs
    );
    assert!(
        matches!(&graph.package("unrelated").unwrap().source, lockfile::LockedSource::Git {commit, ..} if commit == &unrelated_head)
    );
}
