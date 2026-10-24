// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-target-metadata-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn package(&self, name: &str, fields: &str) -> PathBuf {
        let root = self.0.join(name);
        fs::create_dir_all(root.join("src/bin")).unwrap();
        fs::write(
            root.join("vex.ws"),
            format!("{{ format = 3, name = \"{name}\", {fields} }}"),
        )
        .unwrap();
        for path in ["src/main.wave", "src/lib.wave", "src/bin/admin.wave"] {
            fs::write(root.join(path), "fun main() {}\n").unwrap();
        }
        root
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn vex(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(root)
        .args(args)
        .env("VEX_WAVEC", root.join("compiler-must-not-be-invoked"))
        .output()
        .unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>,
    ) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(root, &path, entries);
            } else if metadata.is_file() {
                entries.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    (fs::read(&path).unwrap(), metadata.modified().unwrap()),
                );
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}
#[test]
fn schema_two_exposes_mixed_targets_profiles_and_declared_platform_settings_without_compiler() {
    let f = Fixture::new();
    let app = f.package("app", r#"
        targets = { lib = { output = "shared", exports = ["wave_add"] }, bins = [
            { name = "app", path = "src/main.wave" },
            { name = "admin", path = "src/bin/admin.wave", mode = "freestanding", entry = "start", startup = "none", linker_script = "link/app.ld" }
        ] },
        default_run = "app",
        profiles = { fast = { inherits = "release", opt_level = 3 } },
        build = { target = "x86_64-unknown-linux-gnu" },
        target_config = [{ triple = "x86_64-unknown-linux-gnu", sysroot = "/sdk", linker = "clang", bins = [{ name = "app", mode = "hosted" }] }]
    "#);
    success(&vex(&app, &["fetch"]));
    let before = snapshot(&f.0);
    let output = vex(
        &app,
        &["metadata", "--format-version", "2", "--locked", "--offline"],
    );
    success(&output);
    assert_eq!(before, snapshot(&f.0));
    assert!(!app.join("target").exists());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 2);
    let root = &value["root"];
    assert!(root.get("kind").is_none());
    assert!(root.get("entry_path").is_none());
    assert_eq!(root["manifest_format"], 3);
    assert_eq!(root["default_run"], "app");
    assert_eq!(root["build_target"], "x86_64-unknown-linux-gnu");
    assert_eq!(root["profiles"]["dev"]["opt_level"], 0);
    assert_eq!(root["profiles"]["release"]["opt_level"], 2);
    assert_eq!(root["profiles"]["fast"]["opt_level"], 3);
    assert_eq!(root["profiles"]["fast"]["inherits"], "release");
    let targets = root["targets"].as_array().unwrap();
    let library = targets
        .iter()
        .find(|target| target["kind"] == "library")
        .unwrap();
    assert_eq!(library["name"], "app");
    assert_eq!(library["output"], "shared");
    assert_eq!(library["exports"], serde_json::json!(["wave_add"]));
    let admin = targets
        .iter()
        .find(|target| target["name"] == "admin")
        .unwrap();
    assert_eq!(
        admin["entry_path"],
        app.canonicalize()
            .unwrap()
            .join("src/bin/admin.wave")
            .to_str()
            .unwrap()
    );
    assert_eq!(admin["platform"]["mode"], "freestanding");
    assert_eq!(admin["platform"]["entry"], "start");
    assert_eq!(admin["platform"]["startup"], "none");
    assert_eq!(admin["platform"]["linker_script"], "link/app.ld");
    assert_eq!(root["target_config"][0]["linker"], "clang");
    assert_eq!(
        root["target_config"][0]["bins"][0]["platform"]["mode"],
        "hosted"
    );
    assert_eq!(
        output.stdout,
        vex(&app, &["metadata", "--format-version=2"]).stdout
    );
}
#[test]
fn schema_one_rejects_ambiguous_root_without_writing_state() {
    let f = Fixture::new();
    let app = f.package(
        "app",
        r#"targets = { lib = {}, bins = [{name = "app", path = "src/main.wave"}] }"#,
    );
    let before = snapshot(&f.0);
    for args in [vec!["metadata"], vec!["metadata", "--format-version", "1"]] {
        let output = vex(&app, &args);
        assert_eq!(output.status.code(), Some(3));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--format-version 2"));
    }
    assert_eq!(before, snapshot(&f.0));
}
#[test]
fn schema_one_rejects_a_mixed_dependency_and_schema_two_exposes_it() {
    let f = Fixture::new();
    f.package(
        "tools",
        r#"targets = { lib = {}, bins = [{name = "tools", path = "src/main.wave"}] }"#,
    );
    let app = f.package("app", r#"targets = { bins = [{name = "app", path = "src/main.wave"}] }, dependencies = [{name = "tools", path = "../tools"}]"#);
    success(&vex(&app, &["fetch"]));
    let before = snapshot(&f.0);
    let output = vex(&app, &["metadata"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("tools"));
    let output = vex(&app, &["metadata", "--format-version", "2"]);
    success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["packages"][0]["targets"].as_array().unwrap().len(), 2);
    assert_eq!(before, snapshot(&f.0));
}
#[test]
fn schema_one_preserves_legacy_shape_and_supports_a_single_declared_binary() {
    let f = Fixture::new();
    let app = f.package(
        "app",
        r#"targets = { bins = [{name = "admin", path = "src/bin/admin.wave"}] }"#,
    );
    success(&vex(&app, &["fetch"]));
    let output = vex(&app, &["metadata"]);
    success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["root"]["kind"], "binary");
    assert_eq!(
        value["root"]["entry_path"],
        app.canonicalize()
            .unwrap()
            .join("src/bin/admin.wave")
            .to_str()
            .unwrap()
    );
    assert!(value["root"].get("targets").is_none());
    fs::write(app.join("vex.ws"), "{format = 2, name = \"app\"}").unwrap();
    let output = vex(&app, &["metadata"]);
    success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["root"]["kind"], "binary");
    assert_eq!(
        value["root"]["entry_path"],
        app.canonicalize()
            .unwrap()
            .join("src/main.wave")
            .to_str()
            .unwrap()
    );
}
#[test]
fn format_version_options_fail_before_project_discovery() {
    let f = Fixture::new();
    for args in [
        vec!["metadata", "--format-version"],
        vec!["metadata", "--format-version", "3"],
        vec!["metadata", "--format-version=0"],
        vec!["metadata", "--format-version", "2", "--format-version=2"],
    ] {
        let output = vex(&f.0, &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(fs::read_dir(&f.0).unwrap().next().is_none());
}
