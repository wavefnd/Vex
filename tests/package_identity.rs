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
            "vex-identity-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn package(&self, directory: &str, name: &str, version: &str, dependencies: &str) -> PathBuf {
        let path = self.0.join(directory);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("src/lib.wave"), "pub fun marker() {}\n").unwrap();
        fs::write(path.join("vex.ws"), format!("{{format=4,name=\"{name}\",version=\"{version}\",targets={{lib={{}}}},dependencies=[{dependencies}]}}")).unwrap();
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
#[test]
fn aliases_and_two_versions_have_concrete_identity_and_locked_metadata() {
    let f = Fixture::new();
    f.package("one", "math", "1.2.3", "");
    f.package("two", "math", "2.0.0", "");
    let app = f.package("app", "app", "1.0.0", "{name=\"old_math\",package=\"math\",path=\"../one\",version_req=\"^1.2\"},{name=\"new_math\",package=\"math\",path=\"../two\",version_req=\"^2\"}");
    success(&f.vex(&app, &["fetch"]));
    let bytes = fs::read(app.join("vex.lock")).unwrap();
    let lock = lockfile::decode(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(lock.packages.len(), 2);
    assert!(lock.packages.iter().all(|p| p.package_name() == "math"));
    assert_ne!(lock.packages[0].id, lock.packages[1].id);
    success(&f.vex(&app, &["fetch", "--locked", "--offline"]));
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), bytes);
    let out = f.vex(&app, &["tree", "--locked", "--offline"]);
    success(&out);
    let tree = String::from_utf8(out.stdout).unwrap();
    assert!(tree.contains("new_math => math v2.0.0"));
    assert!(tree.contains("old_math => math v1.2.3"));
    let out = f.vex(&app, &["metadata", "--format-version", "2", "--locked"]);
    success(&out);
    let data: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(data["packages"][0]["name"], "math");
    assert!(data["packages"][0]["alias"].is_string());
    assert!(data["packages"][0]["id"].is_string());
}
#[test]
fn compatible_transitive_requirements_share_an_alias_and_mismatch_preserves_lock() {
    let f = Fixture::new();
    f.package("shared", "shared", "1.5.0", "");
    f.package(
        "left",
        "left",
        "1.0.0",
        "{name=\"shared\",path=\"../shared\",version_req=\"^1.2\"}",
    );
    f.package(
        "right",
        "right",
        "1.0.0",
        "{name=\"shared\",path=\"../shared\",version_req=\">=1.3, <2\"}",
    );
    let app = f.package(
        "app",
        "app",
        "1.0.0",
        "{name=\"left\",path=\"../left\"},{name=\"right\",path=\"../right\"}",
    );
    success(&f.vex(&app, &["fetch"]));
    let old = fs::read(app.join("vex.lock")).unwrap();
    f.package(
        "right",
        "right",
        "1.0.0",
        "{name=\"shared\",path=\"../shared\",version_req=\"^2\"}",
    );
    let out = f.vex(&app, &["fetch"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("^2"));
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), old);
}
#[test]
fn scoped_alias_conflicts_explain_compiler_limit_before_publication() {
    let f = Fixture::new();
    f.package("one", "math", "1.0.0", "");
    f.package("two", "math", "2.0.0", "");
    f.package("left", "left", "1.0.0", "{name=\"math\",path=\"../one\"}");
    f.package("right", "right", "1.0.0", "{name=\"math\",path=\"../two\"}");
    let app = f.package(
        "app",
        "app",
        "1.0.0",
        "{name=\"left\",path=\"../left\"},{name=\"right\",path=\"../right\"}",
    );
    let out = f.vex(&app, &["fetch"]);
    assert_eq!(out.status.code(), Some(3));
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("scoped compiler dependency mappings"),
        "{error}"
    );
    assert!(!app.join("vex.lock").exists());
}
#[test]
fn locked_legacy_file_is_never_silently_migrated() {
    let f = Fixture::new();
    let app = f.package("app", "app", "1.0.0", "");
    let old = b"{version=3,package=[]}\n";
    fs::write(app.join("vex.lock"), old).unwrap();
    let out = f.vex(&app, &["fetch", "--locked"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), old);
    success(&f.vex(&app, &["fetch"]));
    assert_eq!(
        lockfile::decode(&fs::read_to_string(app.join("vex.lock")).unwrap())
            .unwrap()
            .version,
        4
    );
}
