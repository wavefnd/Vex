// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vex-wson-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn fetch(&self, args: &[&str]) {
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .arg("fetch")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn generated_manifest_round_trips_metadata_without_reinterpreting_legacy_values() {
    let f = Fixture::new();
    let value = "quote\" slash\\ newline\n tab\t CR\r Unicode한글 /*literal*/ #, __VEX_WSON_URL_SEPARATOR__";
    fs::write(
        f.0.join("vex.ws"),
        manifest::render_new_manifest("app", value, false),
    )
    .unwrap();
    let manifest = manifest::Manifest::load_from(f.0.join("vex.ws")).unwrap();
    assert_eq!(manifest.author.as_deref(), Some(value));
    fs::write(
        f.0.join("vex.ws"),
        r#"{name="app",author="C:\tmp\new",description="a,b#//{}[]__VEX_LOCK_URL_SEPARATOR__"}"#,
    )
    .unwrap();
    let manifest = manifest::Manifest::load_from(f.0.join("vex.ws")).unwrap();
    assert_eq!(manifest.author.as_deref(), Some(r"C:\tmp\new"));
    assert_eq!(
        manifest.description.as_deref(),
        Some("a,b#//{}[]__VEX_LOCK_URL_SEPARATOR__")
    );
}

#[test]
fn unusual_path_round_trips_through_v3_and_locked_preserves_bytes() {
    let f = Fixture::new();
    let name = "lib,#한글";
    let dependency = f.0.join(name);
    fs::create_dir_all(dependency.join("src")).unwrap();
    fs::write(dependency.join("src/lib.wave"), "pub fun value() {}\n").unwrap();
    fs::write(
        dependency.join("vex.ws"),
        "{format=2,name=\"dep\",version=0.1.0,lib=true}",
    )
    .unwrap();
    fs::write(
        f.0.join("vex.ws"),
        format!("{{format=2,name=\"app\",dependencies=[{{name=\"dep\",path=\"{name}\"}}]}}"),
    )
    .unwrap();
    f.fetch(&[]);
    let lock = fs::read_to_string(f.0.join("vex.lock")).unwrap();
    assert!(lock.contains("version = 3"));
    assert_eq!(lockfile::decode(&lock).unwrap().packages[0].name, "dep");
    f.fetch(&["--locked", "--offline"]);
    assert_eq!(fs::read_to_string(f.0.join("vex.lock")).unwrap(), lock);
}

#[test]
fn v2_migration_preserves_backslashes_and_locked_preserves_the_original_file() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("dep/src")).unwrap();
    fs::write(f.0.join("dep/src/lib.wave"), "pub fun value() {}\n").unwrap();
    fs::write(
        f.0.join("dep/vex.ws"),
        r#"{name="dep",version="version\literal",lib=true}"#,
    )
    .unwrap();
    fs::write(
        f.0.join("vex.ws"),
        r#"{name="app",dependencies=[{name="dep",path="dep"}]}"#,
    )
    .unwrap();
    let legacy = r#"{version=2,package=[{name="dep",version="version\literal",source="path",path="dep",resolved="dep",dependencies=[]}]}"#;
    fs::write(f.0.join("vex.lock"), legacy).unwrap();
    f.fetch(&["--locked", "--offline"]);
    assert_eq!(fs::read_to_string(f.0.join("vex.lock")).unwrap(), legacy);
    f.fetch(&["--offline"]);
    let migrated = fs::read_to_string(f.0.join("vex.lock")).unwrap();
    assert!(migrated.contains("version = 3"));
    assert_eq!(
        lockfile::decode(&migrated).unwrap().packages,
        lockfile::decode(legacy).unwrap().packages
    );
    f.fetch(&["--locked", "--offline"]);
    assert_eq!(fs::read_to_string(f.0.join("vex.lock")).unwrap(), migrated);
}
