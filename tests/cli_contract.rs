// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("vex-cli-contract-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("test directory must be created");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn help_is_read_only_and_succeeds_without_a_manifest() {
    let fixture = TestDir::new();
    for arguments in [
        &["init", "--help"][..],
        &["build", "--help"],
        &["run", "--help"],
        &["check", "--help"],
        &["fetch", "--help"],
        &["update", "--help"],
        &["info", "--help"],
        &["setup", "--help"],
        &["setup", "wavec", "--help"],
    ] {
        let output = vex(&fixture.0, arguments);
        assert_success(&output, &format!("vex {}", arguments.join(" ")));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("usage:"),
            "help output did not contain usage: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert!(!fixture.0.join("vex.ws").exists());
    assert!(!fixture.0.join("src").exists());
}

#[test]
fn invalid_init_and_info_fail_without_mutating_the_directory() {
    let fixture = TestDir::new();
    let invalid_init = vex(&fixture.0, &["init", "--unknown"]);
    assert_eq!(invalid_init.status.code(), Some(2));
    assert!(!fixture.0.join("vex.ws").exists());
    assert!(!fixture.0.join("src").exists());

    let missing_info = vex(&fixture.0, &["info"]);
    assert_eq!(missing_info.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing_info.stderr).contains("could not find `vex.ws`"));

    let invalid_info = vex(&fixture.0, &["info", "extra"]);
    assert_eq!(invalid_info.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_info.stderr).contains("unexpected argument"));

    let invalid_setup = vex(&fixture.0, &["setup", "wavec", "--version", "--unknown"]);
    assert_eq!(invalid_setup.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_setup.stderr).contains("version value"));
}

#[test]
fn dependencies_must_be_library_packages_with_src_lib_wave() {
    let fixture = TestDir::new();
    let app = fixture.0.join("app");
    let dependency = fixture.0.join("add");
    fs::create_dir_all(&app).unwrap();
    fs::create_dir_all(&dependency).unwrap();
    fs::write(
        app.join("vex.ws"),
        "{ name = \"app\", version = 0.1.0, dependencies = [{ name = \"add\", path = \"../add\" }] }\n",
    )
    .unwrap();
    fs::write(
        dependency.join("vex.ws"),
        "{ name = \"add\", version = 0.1.0, lib = false, dependencies = [] }\n",
    )
    .unwrap();

    let non_library = vex(&app, &["fetch"]);
    assert_eq!(non_library.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&non_library.stderr)
        .contains("dependency `add` is not a library package"));

    fs::write(
        dependency.join("vex.ws"),
        "{ name = \"add\", version = 0.1.0, lib = true, dependencies = [] }\n",
    )
    .unwrap();
    let missing_entry = vex(&app, &["fetch"]);
    assert_eq!(missing_entry.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&missing_entry.stderr).contains("has no canonical library entry")
    );

    fs::create_dir_all(dependency.join("src")).unwrap();
    fs::write(
        dependency.join("src/lib.wave"),
        "pub fun sum(a: i32, b: i32) -> i32 { return a + b; }\n",
    )
    .unwrap();
    assert_success(&vex(&app, &["fetch"]), "fetch canonical library");
}

fn vex(path: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(args)
        .current_dir(path)
        .output()
        .expect("vex command must start")
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
