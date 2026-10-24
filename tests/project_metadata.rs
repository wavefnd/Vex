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
        let path = std::env::temp_dir().join(format!(
            "vex-project-metadata-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn package(&self, name: &str, dependencies: &str, lib: bool) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.join("src/nested")).unwrap();
        fs::write(path.join("vex.ws"), format!("{{ name = \"{name}\", version = 0.1.0, lib = {lib}, dependencies = [{dependencies}] }}")).unwrap();
        fs::write(
            path.join(if lib { "src/lib.wave" } else { "src/main.wave" }),
            "fun main() {}\n",
        )
        .unwrap();
        path
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
        path: &Path,
        files: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>,
    ) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(root, &path, files);
            } else if metadata.is_file() {
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    (fs::read(&path).unwrap(), metadata.modified().unwrap()),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}
#[test]
fn discovery_selects_nearest_project_and_explicit_path_wins() {
    let f = Fixture::new();
    let app = f.package("app", "", false);
    let other = f.package("other", "", false);
    let nested = app.join("src/nested");
    let out = vex(&nested, &["info"]);
    success(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("name: app\n"));
    let out = vex(
        &nested,
        &["--manifest-path", "../../../other/vex.ws", "info"],
    );
    success(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("name: other\n"));
    let out = vex(
        &nested,
        &[
            "info",
            "--manifest-path",
            other.join("vex.ws").to_str().unwrap(),
        ],
    );
    success(&out);
    fs::write(nested.join("vex.ws"), "{ name = false }").unwrap();
    assert_eq!(vex(&nested, &["info"]).status.code(), Some(3));
    assert_eq!(
        vex(&nested, &["info", "--manifest-path", "missing/vex.ws"])
            .status
            .code(),
        Some(5)
    );
    assert!(!app.join(".vex").exists());
}
#[test]
fn init_keeps_invocation_directory_and_global_selector_requires_a_project_command() {
    let f = Fixture::new();
    let app = f.package("app", "", false);
    let child = app.join("src/nested");
    success(&vex(&child, &["init"]));
    assert!(child.join("vex.ws").is_file());
    assert_eq!(
        vex(&app, &["init", "--manifest-path", "vex.ws"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        vex(
            &app,
            &[
                "info",
                "--manifest-path",
                "vex.ws",
                "--manifest-path",
                "vex.ws"
            ]
        )
        .status
        .code(),
        Some(2)
    );
}
#[test]
fn metadata_is_deterministic_read_only_and_exposes_transitive_edges() {
    let f = Fixture::new();
    f.package("leaf", "", true);
    f.package("middle", "{ name = \"leaf\", path = \"../leaf\" }", true);
    let app = f.package("app", "{ name = \"middle\", path = \"../middle\" }", false);
    let before = snapshot(&f.0);
    let out = vex(&app, &["metadata"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("vex fetch"));
    assert_eq!(before, snapshot(&f.0));
    assert!(!app.join(".vex").exists());
    success(&vex(&app, &["fetch"]));
    let before = snapshot(&f.0);
    let first = vex(&app, &["metadata"]);
    success(&first);
    let second = vex(
        &app.join("src/nested"),
        &["metadata", "--format", "json", "--locked", "--offline"],
    );
    success(&second);
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(before, snapshot(&f.0));
    assert!(!app.join("target").exists());
    let json: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["root"]["dependencies"], serde_json::json!(["middle"]));
    assert_eq!(json["packages"][0]["name"], "leaf");
    assert_eq!(
        json["packages"][1]["dependencies"],
        serde_json::json!(["leaf"])
    );
    assert_eq!(
        json["root"]["manifest_path"],
        app.join("vex.ws").canonicalize().unwrap().to_str().unwrap()
    );
    fs::write(app.join("vex.ws"), "{ name = \"app\", dependencies = [] }").unwrap();
    let before = snapshot(&f.0);
    let out = vex(&app, &["metadata"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(before, snapshot(&f.0));
}
#[test]
fn metadata_preserves_v2_and_rejects_pending_recovery() {
    let f = Fixture::new();
    let app = f.package("app", "", false);
    success(&vex(&app, &["fetch"]));
    fs::write(app.join("vex.lock"), "{ version = 2, package = [] }\r\n").unwrap();
    let before = snapshot(&f.0);
    success(&vex(&app, &["metadata", "--locked"]));
    assert_eq!(before, snapshot(&f.0));
    // Even an invalid journal must not trigger repair during inspection.
    fs::write(app.join(".vex/transaction.json"), "invalid").unwrap();
    let before = snapshot(&f.0);
    let out = vex(&app, &["metadata"]);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(before, snapshot(&f.0));
}
#[cfg(unix)]
#[test]
fn discovery_uses_physical_ancestors_and_rejects_broken_nearest_manifest() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let app = f.package("app", "", false);
    let other = f.package("other", "", false);
    symlink(app.join("src/nested"), other.join("linked")).unwrap();
    let out = vex(&other.join("linked"), &["info"]);
    success(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("name: app\n"));
    symlink("missing", app.join("src/nested/vex.ws")).unwrap();
    assert!(!vex(&app.join("src/nested"), &["info"]).status.success());
}
#[test]
fn unicode_project_paths_are_preserved_in_selection_and_metadata() {
    let f = Fixture::new();
    let app = f.package("app", "", false);
    let renamed = f.0.join("project 한글");
    fs::rename(app, &renamed).unwrap();
    let manifest = renamed.join("vex.ws");
    for command in ["fetch", "metadata"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&f.0)
            .arg("--manifest-path")
            .arg(&manifest)
            .arg(command)
            .output()
            .unwrap();
        success(&out);
        if command == "metadata" {
            let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(
                value["root"]["manifest_path"],
                manifest.canonicalize().unwrap().to_str().unwrap()
            );
        }
    }
}

// These fixtures need raw-byte filenames; macOS CI rejects them with EILSEQ.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_manifest_path_is_preserved_but_metadata_json_rejects_it() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let app = f.package("app", "", false);
    let renamed = f.0.join(std::ffi::OsString::from_vec(b"app-\xff".to_vec()));
    fs::rename(app, &renamed).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&f.0)
        .arg("--manifest-path")
        .arg(renamed.join("vex.ws"))
        .arg("info")
        .output()
        .unwrap();
    success(&out);
    let before = snapshot(&f.0);
    let out = vex(&renamed, &["metadata"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("non-UTF-8"));
    assert!(out.stdout.is_empty());
    assert_eq!(before, snapshot(&f.0));
}

#[test]
fn message_file_is_relative_to_invocation_directory_when_project_moves() {
    let f = Fixture::new();
    let app = f.package("app", "", false);
    let nested = app.join("src/nested");
    success(&vex(&nested, &["--message-file", "events.jsonl", "info"]));
    assert!(nested.join("events.jsonl").is_file());
    assert!(!app.join("events.jsonl").exists());
    let events = fs::read_to_string(nested.join("events.jsonl")).unwrap();
    let end: serde_json::Value = serde_json::from_str(events.lines().last().unwrap()).unwrap();
    assert_eq!(end["exit_code"], 0);
}

// These fixtures need raw-byte filenames; macOS CI rejects them with EILSEQ.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_dependency_location_never_replaces_lockfile_with_lossy_paths() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = Fixture::new();
    let dep = f.package("dep", "", true);
    let app = f.package("app", "{ name = \"dep\", path = \"../alias\" }", false);
    let raw_path = f.0.join(std::ffi::OsString::from_vec(b"dep-\xff".to_vec()));
    fs::rename(dep, &raw_path).unwrap();
    symlink(raw_path, f.0.join("alias")).unwrap();
    let lock = b"{version=2,package=[]}\n";
    fs::write(app.join("vex.lock"), lock).unwrap();
    let out = vex(&app, &["fetch"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("non-UTF-8"));
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
    let out = vex(&app, &["metadata"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
}
