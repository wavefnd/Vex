// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

mod support;
use support::{git_path, git_url};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        // macOS's per-user temp directory is long enough to select the separate
        // init/fetch implementation. These tests must exercise a genuine clone.
        #[cfg(unix)]
        let root = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let root = std::env::temp_dir();
        let root = root.join(format!(
            "v43-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn source(&self, object_format: &str) -> (PathBuf, String) {
        let source = self.0.join("src");
        package(&source, "dep", None);
        git(
            &source,
            &[
                "init",
                "--initial-branch=master",
                &format!("--object-format={object_format}"),
            ],
        );
        let commit = commit(&source);
        (source, commit)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn initial_clone_reuses_default_branch_branch_tag_and_exact_revision_objects() {
    for format in ["sha1", "sha256"] {
        let fixture = Fixture::new();
        let (source, expected) = fixture.source(format);
        git(&source, &["tag", "v1"]);
        git(&source, &["tag", "-a", "v1-annotated", "-m", "version one"]);
        for (index, selector) in [
            String::new(),
            "branch = \"master\"".into(),
            "tag = \"v1\"".into(),
            "tag = \"v1-annotated\"".into(),
            format!("rev = \"{expected}\""),
        ]
        .into_iter()
        .enumerate()
        {
            let app = fixture.0.join(format!("a{index}"));
            package(&app, "app", Some((&source, &selector)));
            let (output, commands) = traced_vex(&app, &["fetch"]);
            success(&output);
            assert_commands(&commands, "clone", 1);
            assert_commands(&commands, "fetch", 0);
            assert_commands(&commands, "ls-remote", 0);
            assert_eq!(locked_commit(&app), expected);
            let lock = fs::read(app.join("vex.lock")).unwrap();
            let (output, commands) = traced_vex(&app, &["fetch", "--locked", "--offline"]);
            success(&output);
            assert_commands(&commands, "clone", 0);
            assert_commands(&commands, "fetch", 0);
            assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
        }
    }
}

#[test]
fn explicit_update_still_fetches_and_refreshes_the_remote_default_branch() {
    let fixture = Fixture::new();
    let (source, initial) = fixture.source("sha1");
    let app = fixture.0.join("app");
    package(&app, "app", Some((&source, "")));
    success(&traced_vex(&app, &["fetch"]).0);
    git(&source, &["checkout", "-b", "next"]);
    fs::write(source.join("updated"), "new default branch").unwrap();
    let next = commit(&source);
    assert_ne!(initial, next);

    let (output, commands) = traced_vex(&app, &["update", "dep"]);
    success(&output);
    assert_commands(&commands, "clone", 0);
    assert_commands(&commands, "fetch", 1);
    assert_commands(&commands, "ls-remote", 1);
    assert_eq!(locked_commit(&app), next);
}

#[test]
fn init_only_long_path_candidates_still_fetch_before_resolution() {
    for format in ["sha1", "sha256"] {
        let fixture = Fixture::new();
        let (source, expected) = fixture.source(format);
        // Keep the staged checkout above the clone cutoff (200) but below the
        // Windows managed-directory bound (240 UTF-16 units).
        let padding = 100usize.saturating_sub(fixture.0.as_os_str().len() + 5);
        let app = fixture.0.join("p".repeat(padding.max(1))).join("app");
        package(&app, "app", Some((&source, "")));
        let (output, commands) = traced_vex(&app, &["fetch"]);
        success(&output);
        assert_commands(&commands, "clone", 0);
        assert_commands(&commands, "init", 1);
        assert_commands(&commands, "fetch", 1);
        assert_commands(&commands, "ls-remote", 2);
        assert_eq!(locked_commit(&app), expected);
    }
}

#[test]
fn a_reference_missing_from_clone_is_fetched_before_reporting_resolution_failure() {
    let fixture = Fixture::new();
    let (source, _) = fixture.source("sha1");
    let app = fixture.0.join("app");
    package(&app, "app", Some((&source, "tag = \"missing\"")));
    let (output, commands) = traced_vex(&app, &["fetch"]);
    assert_eq!(output.status.code(), Some(3));
    assert_commands(&commands, "clone", 1);
    assert_commands(&commands, "fetch", 1);
    assert!(!app.join("vex.lock").exists());
    assert!(!app.join(".vex/deps").exists());
}

#[test]
fn restoring_a_locked_unadvertised_object_reuses_the_cached_exact_object() {
    let fixture = Fixture::new();
    let (source, expected) = fixture.source("sha1");
    let app = fixture.0.join("app");
    package(&app, "app", Some((&source, "branch = \"master\"")));
    success(&traced_vex(&app, &["fetch"]).0);
    let lock = fs::read(app.join("vex.lock")).unwrap();
    git(&source, &["checkout", "--orphan", "replacement"]);
    fs::write(source.join("replacement"), "new root history").unwrap();
    assert_ne!(commit(&source), expected);
    git(&source, &["branch", "-D", "master"]);
    git(&source, &["branch", "-m", "master"]);
    git(
        &source,
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    fs::remove_dir_all(app.join(".vex/deps")).unwrap();

    let (output, commands) = traced_vex(&app, &["fetch", "--locked"]);
    success(&output);
    assert_commands(&commands, "clone", 0);
    assert_commands(&commands, "fetch", 0);
    assert_commands(&commands, "ls-remote", 0);
    assert_eq!(locked_commit(&app), expected);
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
}

#[test]
fn verbosity_explains_clone_reuse_and_locked_offline_decisions() {
    let fixture = Fixture::new();
    let (source, expected) = fixture.source("sha1");
    let app = fixture.0.join("app");
    package(&app, "app", Some((&source, "")));
    let (output, _) = traced_vex(&app, &["-v", "fetch"]);
    success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("skip redundant fetch"), "{stderr}");
    assert!(!stderr.contains("selected Git commit"), "{stderr}");

    let (output, _) = traced_vex(&app, &["-vv", "fetch", "--locked", "--offline"]);
    success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reuse locked local checkout (offline)"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("selected Git commit {expected}")),
        "{stderr}"
    );

    let (output, _) = traced_vex(&app, &["fetch", "--locked", "--offline"]);
    success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Detail") && !stderr.contains("Trace"),
        "{stderr}"
    );
}

fn package(path: &Path, name: &str, dependency: Option<(&Path, &str)>) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.wave"), "pub fun marker() {}\n").unwrap();
    let dependencies = dependency
        .map(|(source, selector)| {
            format!(
                "{{ name = \"dep\", git = \"{}\", {selector} }}",
                git_url(source)
            )
        })
        .unwrap_or_default();
    fs::write(
        path.join("vex.ws"),
        format!(
            "{{ name = \"{name}\", version = \"0.1.0\", lib = true, dependencies = [{dependencies}] }}\n"
        ),
    )
    .unwrap();
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Vex Test",
            "-c",
            "user.email=vex@example.invalid",
        ])
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    success(&output);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit(path: &Path) -> String {
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "fixture"]);
    git(path, &["rev-parse", "HEAD"])
}

fn traced_vex(app: &Path, args: &[&str]) -> (Output, Vec<String>) {
    let trace = app.join("git-trace.jsonl");
    if trace.exists() {
        fs::remove_file(&trace).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(app)
        .args(args)
        .env("GIT_TRACE2_EVENT", git_path(&trace))
        .env("VEX_CACHE_HOME", app.join(".test-cache"))
        .output()
        .unwrap();
    let events = fs::read_to_string(&trace)
        .unwrap_or_else(|error| {
            panic!(
                "missing Git trace {}: {error}; stderr: {}",
                trace.display(),
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    // Count source acquisition, not local cache maintenance subprocesses.
    let cache_sessions: Vec<_> = events
        .iter()
        .filter(|event| {
            event["event"] == "start"
                && event["argv"].as_array().is_some_and(|args| {
                    args.iter()
                        .any(|arg| arg.as_str().is_some_and(|arg| arg.contains(".test-cache")))
                })
        })
        .filter_map(|event| event["sid"].as_str().map(str::to_owned))
        .collect();
    let commands = events
        .into_iter()
        .filter(|event| {
            !cache_sessions.iter().any(|sid| {
                event["sid"]
                    .as_str()
                    .is_some_and(|value| value.starts_with(sid))
            })
        })
        .filter(|event| event["event"] == "cmd_name")
        .map(|event| event["name"].as_str().unwrap().to_owned())
        .collect();
    (output, commands)
}

fn assert_commands(commands: &[String], command: &str, expected: usize) {
    assert_eq!(
        commands.iter().filter(|name| *name == command).count(),
        expected,
        "unexpected {command} calls: {commands:?}"
    );
}

fn locked_commit(app: &Path) -> String {
    let text = fs::read_to_string(app.join("vex.lock")).unwrap();
    let lock = lockfile::decode(&text).unwrap();
    let lockfile::LockedSource::Git { commit, .. } = &lock.packages[0].source else {
        panic!("expected Git dependency")
    };
    commit.clone()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
