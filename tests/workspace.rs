// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};
static ID: AtomicU64 = AtomicU64::new(0);
fn compiler() -> &'static Path {
    static COMPILER: OnceLock<PathBuf> = OnceLock::new();
    COMPILER.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("vex-ws-compiler-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(if cfg!(windows) {
            "target-wavec.exe"
        } else {
            "target-wavec"
        });
        support::compile_fixture("target_wavec", &path);
        path
    })
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-ws-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let f = Self(root.canonicalize().unwrap());
        f.package(
            "alpha",
            "alpha",
            "targets={lib={},bins=[{name=\"tool\",path=\"src/main.wave\"}]}",
        );
        f.package("beta", "beta", "targets={bins=[{name=\"tool\",path=\"src/main.wave\"}]},dependencies=[{name=\"alpha\",path=\"../alpha\"}]");
        f.owner("workspace={members=[\"alpha\",\"beta\"],default_members=[\"beta\"]},profiles={fast={inherits=\"release\",opt_level=3}}");
        f
    }
    fn owner(&self, fields: &str) {
        fs::write(self.0.join("vex.ws"), format!("{{format=4,{fields}}}")).unwrap();
    }
    fn package(&self, directory: &str, name: &str, fields: &str) -> PathBuf {
        let path = self.0.join(directory);
        fs::create_dir_all(path.join("src/nested")).unwrap();
        for file in ["main.wave", "lib.wave"] {
            fs::write(path.join("src").join(file), "fun main() {}\n").unwrap();
        }
        fs::write(
            path.join("vex.ws"),
            format!("{{format=4,name=\"{name}\",version=\"1.0.0\",{fields}}}"),
        )
        .unwrap();
        path
    }
    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(cwd)
            .env("VEX_WAVEC", compiler())
            .env("TARGET_COMPILER_LOG", self.0.join("compiler.jsonl"))
            .env("VEX_CACHE_HOME", self.0.join("cache"))
            .env("NO_COLOR", "1");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(&self.0).args(args).output().unwrap()
    }
    fn metadata(&self, cwd: &Path, options: &[&str]) -> Value {
        let mut args = vec!["metadata", "--format-version", "2", "--locked"];
        args.extend_from_slice(options);
        let out = self.command(cwd).args(args).output().unwrap();
        code(&out, 0);
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.0.join("compiler.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn builds(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|a| a.iter().any(|a| a == "build"))
            .collect()
    }
    fn triple() -> &'static str {
        if cfg!(windows) {
            "x86_64-pc-windows-msvc"
        } else if cfg!(target_os = "macos") {
            "x86_64-apple-darwin"
        } else {
            "x86_64-unknown-linux-gnu"
        }
    }
    fn artifact(&self, package: &str) -> PathBuf {
        self.0
            .join("target")
            .join(Self::triple())
            .join("dev/packages")
            .join(package)
            .join("bin")
            .join(if cfg!(windows) { "tool.exe" } else { "tool" })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn code(out: &Output, expected: i32) {
    assert_eq!(
        out.status.code(),
        Some(expected),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn selected(value: &Value) -> Vec<&str> {
    value["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["selected"] == true)
        .map(|p| p["name"].as_str().unwrap())
        .collect()
}

#[test]
fn discovery_defaults_explicit_members_and_shared_lock_are_deterministic() {
    let f = Fixture::new();
    let legacy = b"{version=3,package=[]}\n";
    fs::write(f.0.join("alpha/vex.lock"), legacy).unwrap();
    code(&f.run(&["fetch"]), 0);
    let bytes = fs::read(f.0.join("vex.lock")).unwrap();
    let lock = lockfile::decode(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(lock.roots.len(), 2);
    assert_eq!(selected(&f.metadata(&f.0, &[])), ["beta"]);
    assert_eq!(
        selected(&f.metadata(&f.0.join("alpha/src/nested"), &[])),
        ["alpha"]
    );
    assert_eq!(
        selected(&f.metadata(&f.0, &["--workspace"])),
        ["alpha", "beta"]
    );
    assert_eq!(
        selected(&f.metadata(
            &f.0.join("alpha"),
            &["--manifest-path", f.0.join("beta/vex.ws").to_str().unwrap()]
        )),
        ["beta"]
    );
    assert_eq!(fs::read(f.0.join("vex.lock")).unwrap(), bytes);
    assert_eq!(fs::read(f.0.join("alpha/vex.lock")).unwrap(), legacy);
    assert!(!f.0.join("alpha/.vex").exists());
    assert!(!f.0.join("beta/.vex").exists());
    code(&f.run(&["metadata"]), 3);
    code(&f.run(&["build", "-p", "typo"]), 3);
}
#[test]
fn batch_plans_every_member_and_namespaces_equal_artifact_names() {
    let f = Fixture::new();
    code(&f.run(&["fetch"]), 0);
    let dry = f.run(&["build", "--workspace", "--dry-run", "--locked"]);
    code(&dry, 0);
    let value: Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(value["plans"].as_array().unwrap().len(), 3);
    assert!(!f.0.join("target").exists());
    code(&f.run(&["build", "--workspace", "--locked"]), 0);
    assert!(f.artifact("alpha").is_file());
    assert!(f.artifact("beta").is_file());
    assert!(!f.0.join("alpha/target").exists());
    code(&f.run(&["build", "-p", "beta", "--profile", "fast"]), 0);
    assert!(f.builds().iter().any(|args| args.contains(&"-O3".into())));
}
#[test]
fn last_member_preflight_failure_prevents_all_compiler_execution() {
    let f = Fixture::new();
    code(&f.run(&["fetch"]), 0);
    let out = f
        .command(&f.0)
        .args(["build", "--workspace"])
        .env("TARGET_PLAN_FAIL_INPUT", "beta/src/main.wave")
        .output()
        .unwrap();
    code(&out, 4);
    assert!(!f.0.join("target").exists());
    assert!(f
        .builds()
        .iter()
        .all(|args| args.contains(&"--dry-run".into())));
    fs::remove_file(f.0.join("beta/src/main.wave")).unwrap();
    let calls = f.calls().len();
    code(&f.run(&["build", "--workspace"]), 3);
    assert_eq!(f.calls().len(), calls);
}
#[test]
fn member_run_uses_member_directory_and_releases_workspace_lease() {
    let f = Fixture::new();
    code(&f.run(&["fetch"]), 0);
    let out = f
        .command(&f.0)
        .args([
            "run",
            "-p",
            "beta",
            "--locked",
            "--offline",
            "--",
            "argument with spaces",
        ])
        .env("TARGET_PROGRAM_EXIT", "29")
        .env("TARGET_PROGRAM_EXPECT_CWD", f.0.join("beta"))
        .env(
            "TARGET_PROGRAM_CHECK_LOCK_RELEASE",
            env!("CARGO_BIN_EXE_vex"),
        )
        .output()
        .unwrap();
    code(&out, 29);
    assert!(String::from_utf8_lossy(&out.stdout).contains("argument with spaces"));
    code(&f.run(&["run", "--workspace"]), 3);
}
#[test]
fn workspace_members_unify_features_but_keep_compiler_alias_maps_separate() {
    let f = Fixture::new();
    for name in ["x", "y"] {
        f.package(name, name, "targets={lib={}}");
    }
    f.package("shared", "shared", "targets={lib={}},dependencies=[{name=\"x\",path=\"../x\",optional=true},{name=\"y\",path=\"../y\",optional=true}],features={x=[\"dep:x\"],y=[\"dep:y\"]}");
    f.package("alpha","alpha","targets={bins=[{name=\"tool\",path=\"src/main.wave\"}]},dependencies=[{name=\"shared\",path=\"../shared\"}],features={a=[\"shared/x\"]}");
    f.package("beta","beta","targets={bins=[{name=\"tool\",path=\"src/main.wave\"}]},dependencies=[{name=\"shared\",path=\"../shared\"}],features={b=[\"shared/y\"]}");
    code(
        &f.run(&["fetch", "--workspace", "--features", "alpha/a,beta/b"]),
        0,
    );
    let value = f.metadata(&f.0, &["--workspace", "--features", "alpha/a,beta/b"]);
    for name in ["x", "y"] {
        assert!(value["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == name));
    }
    assert_eq!(value["selections"].as_array().unwrap().len(), 2);
    code(&f.run(&["fetch", "--workspace", "--features", "a"]), 3);
    f.package("one", "common", "targets={lib={}}");
    f.package("two", "common", "targets={lib={}}");
    f.package("alpha","alpha","targets={bins=[{name=\"tool\",path=\"src/main.wave\"}]},dependencies=[{name=\"common\",path=\"../one\"}]");
    f.package("beta","beta","targets={bins=[{name=\"tool\",path=\"src/main.wave\"}]},dependencies=[{name=\"common\",path=\"../two\"}]");
    code(&f.run(&["build", "--workspace"]), 0);
}

#[test]
fn exclusions_nested_ownership_duplicate_names_and_escapes_are_explicit() {
    let f = Fixture::new();
    f.package(
        "alpha",
        "alpha",
        "targets={lib={}},workspace={members=[\"sub\"]}",
    );
    f.package("alpha/sub", "sub", "targets={lib={}}");
    code(&f.run(&["fetch"]), 3);
    assert!(!f.0.join(".vex").exists());
    f.owner(
        "workspace={members=[\"alpha\",\"beta\"],exclude=[\"alpha\"],default_members=[\"beta\"]}",
    );
    code(&f.run(&["fetch"]), 0);
    let value = f.metadata(&f.0, &[]);
    assert_eq!(value["root"]["members"], serde_json::json!(["beta"]));
    let out = f.command(&f.0.join("alpha")).arg("fetch").output().unwrap();
    code(&out, 0);
    assert!(f.0.join("alpha/vex.lock").exists());
    let other = Fixture::new();
    other.package("beta", "ALPHA", "targets={lib={}}");
    code(&other.run(&["fetch"]), 3);
    assert!(!other.0.join(".vex").exists());
    for member in ["../outside", "/absolute", "alpha/*", "alpha//src"] {
        other.owner(&format!(
            "workspace={{members=[{}]}}",
            serde_json::to_string(member).unwrap()
        ));
        code(&other.run(&["fetch"]), 3);
        assert!(!other.0.join(".vex").exists());
    }
}

mod support;
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
    code(&out, 0);
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn commit(path: &Path) -> String {
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "fixture"]);
    git(path, &["rev-parse", "HEAD"])
}
fn commits(f: &Fixture) -> std::collections::BTreeMap<String, String> {
    lockfile::decode(&fs::read_to_string(f.0.join("vex.lock")).unwrap())
        .unwrap()
        .packages
        .iter()
        .filter_map(|p| match &p.source {
            lockfile::LockedSource::Git { commit, .. } => {
                Some((p.import_name().to_owned(), commit.clone()))
            }
            _ => None,
        })
        .collect()
}
#[test]
fn selected_member_update_preserves_unrelated_bindings_in_the_same_repository() {
    let f = Fixture::new();
    for directory in ["repo/one", "repo/two"] {
        f.package(directory, "common", "targets={lib={}}");
    }
    let repo = f.0.join("repo");
    git(&repo, &["init", "--initial-branch=master"]);
    let old = commit(&repo);
    let url = support::git_url(&repo);
    for (member, alias, subdir) in [("alpha", "one", "one"), ("beta", "two", "two")] {
        f.package(member,member,&format!("targets={{bins=[{{name=\"tool\",path=\"src/main.wave\"}}]}},dependencies=[{{name=\"{alias}\",package=\"common\",git=\"{url}\",branch=\"master\",subdir=\"{subdir}\"}}]"));
    }
    code(&f.run(&["fetch", "--workspace"]), 0);
    assert!(commits(&f).values().all(|value| value == &old));
    fs::write(repo.join("one/revision"), "new").unwrap();
    let new = commit(&repo);
    code(&f.run(&["update", "one", "-p", "alpha"]), 0);
    let pins = commits(&f);
    assert_eq!(pins["one"], new);
    assert_eq!(pins["two"], old);
    code(&f.run(&["update", "one", "-p", "beta"]), 3);
    assert_eq!(commits(&f), pins);
    code(&f.run(&["update", "-p", "beta"]), 0);
    assert!(commits(&f).values().all(|value| value == &new));
    let bytes = fs::read(f.0.join("vex.lock")).unwrap();
    fs::rename(&repo, repo.with_extension("hidden")).unwrap();
    code(
        &f.run(&["fetch", "--workspace", "--locked", "--offline"]),
        0,
    );
    assert_eq!(fs::read(f.0.join("vex.lock")).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn metadata_rejects_unrepresentable_member_paths_without_panicking_or_writing() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = Fixture::new();
    code(&f.run(&["fetch"]), 0);
    let bytes = fs::read(f.0.join("vex.lock")).unwrap();
    let invalid = f.0.join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
    if let Err(error) = fs::rename(f.0.join("alpha"), &invalid) {
        // APFS rejects ill-formed UTF-8 filenames with EILSEQ before Vex can
        // see them. Still exercise the Vex rejection on filesystems that permit
        // this fixture; other filesystem errors must remain test failures.
        if cfg!(target_os = "macos") && error.raw_os_error() == Some(92) {
            assert!(f.0.join("alpha/vex.ws").is_file());
            assert_eq!(fs::read(f.0.join("vex.lock")).unwrap(), bytes);
            eprintln!("filesystem rejected the non-UTF-8 filename (EILSEQ)");
            return;
        }
        panic!("cannot prepare non-UTF-8 fixture: {error}");
    }
    symlink(&invalid, f.0.join("alpha")).unwrap();
    let out = f.run(&["metadata", "--format-version", "2"]);
    code(&out, 3);
    assert!(String::from_utf8_lossy(&out.stderr).contains("non-UTF-8"));
    assert_eq!(fs::read(f.0.join("vex.lock")).unwrap(), bytes);
}
