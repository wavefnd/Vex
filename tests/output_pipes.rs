// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-pipes-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.wave"), "fun main() {}\n").unwrap();
        fs::write(root.join("vex.ws"), "{name=\"pipes\",version=0.1.0}").unwrap();
        let fixture = Self(root);
        assert!(fixture
            .command()
            .arg("fetch")
            .arg("--offline")
            .output()
            .unwrap()
            .status
            .success());
        fixture
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command.current_dir(&self.0).env("RUST_BACKTRACE", "1");
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn closed_pipe() -> Stdio {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    writer.into()
}

#[test]
fn closed_stdout_is_successful_for_human_and_json_output() {
    let fixture = Fixture::new();
    for (i, args) in [
        vec!["--help"],
        vec!["--version"],
        vec!["info"],
        vec!["metadata", "--locked", "--offline"],
        vec!["tree", "--locked", "--offline"],
        vec!["build", "--help"],
    ]
    .iter()
    .enumerate()
    {
        let report = fixture.0.join(format!("messages-{i}.jsonl"));
        let output = fixture
            .command()
            .arg("--message-file")
            .arg(&report)
            .args(args)
            .stdout(closed_pipe())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{args:?}: {stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
        let text = fs::read_to_string(report).unwrap();
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["exit_code"], 0);
        assert_eq!(last["origin"], "vex");
    }
}

#[test]
fn closed_stderr_keeps_the_real_command_result_and_report() {
    let fixture = Fixture::new();
    for (i, args, code) in [
        (0, vec!["fetch", "--offline"], 0),
        (1, vec!["unknown-command"], 2),
    ] {
        let report = fixture.0.join(format!("stderr-{i}.jsonl"));
        let status = fixture
            .command()
            .arg("--message-file")
            .arg(&report)
            .args(args)
            .stderr(closed_pipe())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(code));
        let text = fs::read_to_string(report).unwrap();
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["exit_code"], code);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn real_stdout_failure_is_an_environment_error_not_success() {
    let fixture = Fixture::new();
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let output = fixture
        .command()
        .arg("--help")
        .stdout(full)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot write stdout"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
