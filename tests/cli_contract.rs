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
