// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
static ID: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "vex-features-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn package(&self, directory: &str, name: &str, version: &str, fields: &str) -> PathBuf {
        let path = self.0.join(directory);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("src/lib.wave"), "pub fun marker() {}\n").unwrap();
        fs::write(
            path.join("vex.ws"),
            format!(
                "{{format=4,name=\"{name}\",version=\"{version}\",targets={{lib={{}}}},{fields}}}"
            ),
        )
        .unwrap();
        path
    }
    fn vex(&self, project: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(project)
            .env("VEX_CACHE_HOME", self.0.join("cache"))
            .args(args)
            .output()
            .unwrap()
    }
    fn metadata(&self, project: &Path, options: &[&str]) -> serde_json::Value {
        let mut args = vec!["metadata", "--format-version", "2", "--locked", "--offline"];
        args.extend_from_slice(options);
        let out = self.vex(project, &args);
        success(&out);
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn success(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn git(path: &Path, args: &[&str]) {
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
}
mod support;
#[test]
fn target_union_locks_local_git_sources_and_switches_offline_without_writes() {
    let f = Fixture::new();
    let one = f.package("one", "platform", "1.0.0", "dependencies=[]");
    let two = f.package("two", "platform", "2.0.0", "dependencies=[]");
    for source in [&one, &two] {
        git(source, &["init", "--initial-branch=master"]);
        git(source, &["add", "."]);
        git(source, &["commit", "-m", "fixture"]);
    }
    let app = f.package("app", "app", "1.0.0", &format!("dependencies=[{{name=\"platform\",git=\"{}\",target={{os=\"linux\"}}}},{{name=\"platform\",git=\"{}\",target={{os=\"windows\"}}}}]", support::git_url(&one), support::git_url(&two)));
    success(&f.vex(&app, &["fetch", "--target", "x86_64-unknown-linux-gnu"]));
    let lock = fs::read(app.join("vex.lock")).unwrap();
    assert_eq!(
        lockfile::decode(std::str::from_utf8(&lock).unwrap())
            .unwrap()
            .packages
            .len(),
        2
    );
    fs::rename(&one, one.with_extension("hidden")).unwrap();
    fs::rename(&two, two.with_extension("hidden")).unwrap();
    for (triple, version) in [
        ("x86_64-unknown-linux-gnu", "1.0.0"),
        ("aarch64-pc-windows-msvc", "2.0.0"),
    ] {
        success(&f.vex(
            &app,
            &["fetch", "--target", triple, "--offline", "--locked"],
        ));
        let value = f.metadata(&app, &["--target", triple]);
        assert_eq!(value["packages"].as_array().unwrap().len(), 1);
        assert_eq!(value["packages"][0]["version"], version);
        assert_eq!(value["union_bindings"].as_array().unwrap().len(), 2);
        assert_eq!(value["selection"]["target"]["triple"], triple);
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
    }
}
#[test]
fn optional_defaults_transitive_forwarding_and_disabled_defaults_are_reproducible() {
    let f = Fixture::new();
    f.package("leaf", "leaf", "1.0.0", "dependencies=[]");
    f.package("opt", "opt", "1.0.0", "dependencies=[{name=\"leaf\",path=\"../leaf\",optional=true}],features={extra=[\"dep:leaf\"]}");
    let app = f.package("app", "app", "1.0.0", "dependencies=[{name=\"opt\",path=\"../opt\",optional=true,default_features=false}],features={default=[\"fast\"],fast=[\"dep:opt\",\"opt/extra\"]}");
    success(&f.vex(&app, &["fetch", "--no-default-features"]));
    let lock = fs::read(app.join("vex.lock")).unwrap();
    assert_eq!(
        lockfile::decode(std::str::from_utf8(&lock).unwrap())
            .unwrap()
            .packages
            .len(),
        2
    );
    assert!(f.metadata(&app, &["--no-default-features"])["packages"]
        .as_array()
        .unwrap()
        .is_empty());
    for flags in [vec![], vec!["--no-default-features", "--features", "fast"]] {
        let value = f.metadata(&app, &flags);
        assert_eq!(value["packages"].as_array().unwrap().len(), 2);
        assert!(value["selected_features"]
            .as_object()
            .unwrap()
            .values()
            .any(|v| v.as_array().unwrap().contains(&serde_json::json!("extra"))));
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
    }
}
#[test]
fn aliases_of_one_concrete_package_unify_additive_features() {
    let f = Fixture::new();
    for name in ["x", "y"] {
        f.package(name, name, "1.0.0", "dependencies=[]");
    }
    f.package("shared", "shared", "1.0.0", "dependencies=[{name=\"x\",path=\"../x\",optional=true},{name=\"y\",path=\"../y\",optional=true}],features={first=[\"dep:x\"],second=[\"dep:y\"]}");
    let app = f.package("app", "app", "1.0.0", "dependencies=[{name=\"one\",package=\"shared\",path=\"../shared\",features=[\"first\"],default_features=false},{name=\"two\",package=\"shared\",path=\"../shared\",features=[\"second\"],default_features=false}]");
    success(&f.vex(&app, &["fetch"]));
    let value = f.metadata(&app, &[]);
    let packages = value["packages"].as_array().unwrap();
    assert!(packages.iter().any(|v| v["name"] == "x"));
    assert!(packages.iter().any(|v| v["name"] == "y"));
    let shared: Vec<_> = packages.iter().filter(|v| v["name"] == "shared").collect();
    assert_eq!(shared.len(), 2);
    assert_eq!(shared[0]["id"], shared[1]["id"]);
    assert_eq!(shared[0]["dependencies"], shared[1]["dependencies"]);
}
#[test]
fn overlapping_predicates_and_unknown_features_never_publish_state() {
    let f = Fixture::new();
    f.package("one", "lib", "1.0.0", "dependencies=[]");
    f.package("two", "lib", "2.0.0", "dependencies=[]");
    let app = f.package("app", "app", "1.0.0", "dependencies=[{name=\"lib\",path=\"../one\",target={os=\"linux\"}},{name=\"lib\",path=\"../two\",target={arch=\"x86_64\"}}]");
    let out = f.vex(&app, &["fetch", "--target", "x86_64-unknown-linux-gnu"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("multiple active"));
    assert!(!app.join("vex.lock").exists());
    let out = f.vex(&app, &["fetch", "--features", "typo"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown package feature"));
    assert!(!app.join("vex.lock").exists());
    let untouched = f.package("untouched", "untouched", "1.0.0", "dependencies=[]");
    let out = f.vex(&untouched, &["fetch", "--features", "typo"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(!untouched.join(".vex").exists());
}
#[test]
fn malformed_union_edges_fail_before_checkout_publication() {
    let f = Fixture::new();
    let app = f.package("app", "app", "1.0.0", "dependencies=[]");
    success(&f.vex(&app, &["fetch"]));
    let raw = fs::read_to_string(app.join("vex.lock")).unwrap().replace("edges = []", "edges = [{key=\"missing\",alias=\"missing\",optional=false,default_features=true,features=[],target=null}]");
    assert!(lockfile::decode(&raw).is_err());
    fs::write(app.join("vex.lock"), &raw).unwrap();
    let out = f.vex(&app, &["fetch", "--locked"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(fs::read_to_string(app.join("vex.lock")).unwrap(), raw);
    assert!(!app.join(".vex/transactions").exists());
}
