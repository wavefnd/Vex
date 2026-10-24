// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

mod support;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    compiler: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-output-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("vex.ws"), "{name=\"app\",version=0.1.0}").unwrap();
        fs::write(root.join("src/main.wave"), "fun main() {}\n").unwrap();
        fs::write(root.join("vex.lock"), "{version=3,package=[]}\n").unwrap();
        let compiler = root.join(if cfg!(windows) { "wavec.exe" } else { "wavec" });
        Self { root, compiler }
    }

    fn compile_fixture(&self) {
        fs::write(
            self.root.join("fake.rs"),
            include_str!("fixtures/fake_wavec.rs"),
        )
        .unwrap();
        support::compile_fixture("fake_wavec", &self.compiler);
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(&self.root)
            .args(args)
            .env("VEX_WAVEC", &self.compiler);
        command
    }

    fn events(&self, name: &str) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root.join(name))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn quiet_hides_only_progress_and_preserves_results_and_jsonl() {
    let f = Fixture::new();
    let normal = f
        .command(&[
            "fetch",
            "--locked",
            "--offline",
            "--message-file",
            "normal.jsonl",
        ])
        .output()
        .unwrap();
    success(&normal);
    assert!(String::from_utf8_lossy(&normal.stderr).contains("Finished"));
    let quiet = f
        .command(&[
            "--quiet",
            "fetch",
            "--locked",
            "-q",
            "--offline",
            "--message-file",
            "quiet.jsonl",
        ])
        .output()
        .unwrap();
    success(&quiet);
    assert!(
        quiet.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert_eq!(normal.stdout, quiet.stdout);
    assert_eq!(f.events("normal.jsonl"), f.events("quiet.jsonl"));
    let info = f.command(&["info"]).output().unwrap();
    let quiet_info = f.command(&["info", "--quiet"]).output().unwrap();
    success(&info);
    success(&quiet_info);
    assert_eq!(info.stdout, quiet_info.stdout);
    assert!(!quiet_info.stdout.is_empty());
    let error = f.command(&["--quiet", "unknown-command"]).output().unwrap();
    assert_eq!(error.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&error.stderr).contains("error: unknown command"));
}

#[test]
fn verbosity_levels_are_global_and_do_not_pollute_json() {
    let f = Fixture::new();
    success(
        &f.command(&["fetch", "--locked", "--offline"])
            .output()
            .unwrap(),
    );
    for (flags, details, trace) in [
        (vec![], false, false),
        (vec!["--quiet"], false, false),
        (vec!["--verbose"], true, false),
        (vec!["-v", "--verbose"], true, true),
        (vec!["-vv"], true, true),
        (vec!["-vv", "-v"], true, true),
    ] {
        let mut args = vec!["metadata", "--locked", "--offline"];
        args.extend(flags);
        let output = f.command(&args).output().unwrap();
        success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.contains("Detail"), details, "{args:?}: {stderr}");
        assert_eq!(stderr.contains("Trace"), trace, "{args:?}: {stderr}");
    }
}

#[test]
fn contradictory_output_flags_are_usage_errors_without_side_effects() {
    let f = Fixture::new();
    for args in [
        vec!["--quiet", "-v", "fetch"],
        vec!["fetch", "--verbose", "-q"],
        vec!["-vv", "fetch", "--quiet"],
    ] {
        let output = f.command(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used together"));
        assert!(!f.root.join(".vex").exists());
    }
}

#[test]
fn verbose_diagnostics_and_jsonl_redact_source_credentials() {
    let f = Fixture::new();
    fs::write(f.root.join("vex.ws"), "{name=\"app\",version=0.1.0,dependencies=[{name=\"dep\",git=\"https://SYNTHETIC_USER:SYNTHETIC_SECRET@example.invalid/dep?token=SYNTHETIC_TOKEN\"}]}").unwrap();
    let output = f
        .command(&["-vv", "fetch", "--offline", "--message-file", "error.jsonl"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Trace"), "{stderr}");
    assert!(stderr.contains("error category: resolution"), "{stderr}");
    let report = fs::read_to_string(f.root.join("error.jsonl")).unwrap();
    for text in [&*stderr, &report] {
        for secret in ["SYNTHETIC_USER", "SYNTHETIC_SECRET", "SYNTHETIC_TOKEN"] {
            assert!(!text.contains(secret), "{text}");
        }
    }
    assert!(f
        .events("error.jsonl")
        .iter()
        .any(|event| event["event"] == "diagnostic"));
}

#[test]
fn quiet_keeps_program_stdio_arguments_exit_code_and_reporting_warnings() {
    let f = Fixture::new();
    f.compile_fixture();
    let mut child = f
        .command(&[
            "--quiet",
            "--message-file",
            "run.jsonl",
            "run",
            "--locked",
            "--offline",
            "--",
            "-q",
            "-vv",
            "--verbose",
        ])
        .env("VEX_TEST_RUN_EXIT", "42")
        .env("VEX_TEST_ECHO_STDIN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello program\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(42));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PROGRAM_STDIN:hello program"), "{stdout}");
    assert!(
        stdout.contains("\"-q\", \"-vv\", \"--verbose\""),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("PROGRAM_STDERR"), "{stderr}");
    for status in [
        "Resolving",
        "Compiling",
        "Running",
        "Finished",
        "Detail",
        "Trace",
    ] {
        assert!(!stderr.contains(status), "{stderr}");
    }
    let events = f.events("run.jsonl");
    assert_eq!(events.last().unwrap()["origin"], "program");
    assert_eq!(events.last().unwrap()["exit_code"], 42);

    #[cfg(debug_assertions)]
    {
        let warning = f
            .command(&[
                "-q",
                "--message-file",
                "warning.jsonl",
                "run",
                "--locked",
                "--offline",
            ])
            .env("VEX_TEST_MESSAGE_FAIL_EVENT", "finished")
            .env("VEX_TEST_RUN_EXIT", "42")
            .output()
            .unwrap();
        assert_eq!(warning.status.code(), Some(42));
        assert!(
            String::from_utf8_lossy(&warning.stderr).contains("warning: cannot write message file")
        );
    }
}

#[test]
fn compiler_dry_run_json_is_valid_at_every_output_level() {
    let f = Fixture::new();
    f.compile_fixture();
    for flag in ["-q", "-v", "-vv"] {
        let output = f
            .command(&[
                flag,
                "--message-file",
                "dry.jsonl",
                "build",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--dry-run",
                "--locked",
                "--offline",
            ])
            .output()
            .unwrap();
        success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if flag == "-q" {
            assert!(stderr.is_empty(), "{stderr}");
        } else {
            for decision in [
                "compiler:",
                "freshness: planning only",
                "network: disabled by offline mode",
                "lockfile: preserved",
            ] {
                assert!(stderr.contains(decision), "missing {decision}: {stderr}");
            }
        }
        if flag == "-vv" {
            for detail in [
                "project lease: acquiring shared",
                "dry_run=true, locked=true, offline=true",
                "x86_64-unknown-linux-gnu",
            ] {
                assert!(stderr.contains(detail), "missing {detail}: {stderr}");
            }
        }
        assert!(!f.root.join("dry.jsonl").exists());
        assert!(!f.root.join("target").exists());
    }
}

#[test]
fn maintenance_dry_runs_never_create_message_files_and_protect_existing_files() {
    let f = Fixture::new();
    success(
        &f.command(&["fetch", "--locked", "--offline"])
            .output()
            .unwrap(),
    );
    let before = snapshot(&f.root);
    for command in ["clean", "prune"] {
        let output = f
            .command(&["--message-file", "dry.jsonl", command, "--dry-run"])
            .output()
            .unwrap();
        success(&output);
        assert!(!f.root.join("dry.jsonl").exists());
    }
    assert_eq!(snapshot(&f.root), before);
    fs::write(f.root.join("dry.jsonl"), "preserve").unwrap();
    let before = snapshot(&f.root);
    for command in ["clean", "prune"] {
        let output = f
            .command(&["--message-file", "dry.jsonl", command, "--dry-run"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(5));
        assert_eq!(
            fs::read_to_string(f.root.join("dry.jsonl")).unwrap(),
            "preserve"
        );
    }
    assert!(!f.root.join("target").exists());
    assert_eq!(snapshot(&f.root), before);
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let key = path.strip_prefix(root).unwrap().to_owned();
            if path.is_dir() {
                entries.insert(key, None);
                visit(root, &path, entries);
            } else {
                entries.insert(key, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
fn verbose_resolution_explains_network_leases_and_lockfile_decisions() {
    let f = Fixture::new();
    fs::remove_file(f.root.join("vex.lock")).unwrap();
    let created = f.command(&["-vv", "fetch", "--offline"]).output().unwrap();
    success(&created);
    let stderr = String::from_utf8_lossy(&created.stderr);
    for decision in [
        "dependency policy: reuse locked source selections",
        "network: disabled by offline mode",
        "project lease: acquiring exclusive",
        "project lease: acquired before reading vex.lock",
        "lockfile: write required because vex.lock is missing",
        "update vex.lock=true",
    ] {
        assert!(stderr.contains(decision), "missing {decision}: {stderr}");
    }
    let reused = f.command(&["fetch", "-vv", "--locked"]).output().unwrap();
    success(&reused);
    let stderr = String::from_utf8_lossy(&reused.stderr);
    for decision in [
        "network: allowed for dependency retrieval",
        "lockfile: preserved",
        "update vex.lock=false",
    ] {
        assert!(stderr.contains(decision), "missing {decision}: {stderr}");
    }
    fs::create_dir_all(f.root.join("dep/src")).unwrap();
    fs::write(
        f.root.join("dep/vex.ws"),
        "{name=\"dep\",version=0.1.0,lib=true}",
    )
    .unwrap();
    fs::write(f.root.join("dep/src/lib.wave"), "pub fun marker() {}\n").unwrap();
    fs::write(
        f.root.join("vex.ws"),
        "{name=\"app\",version=0.1.0,dependencies=[{name=\"dep\",path=\"dep\"}]}",
    )
    .unwrap();
    let lock = fs::read(f.root.join("vex.lock")).unwrap();
    let rejected = f
        .command(&["-v", "fetch", "--locked", "--offline"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains(
        "lockfile: write required because dependency graph or exact source selections changed"
    ));
    assert_eq!(fs::read(f.root.join("vex.lock")).unwrap(), lock);
    let refreshed = f.command(&["-v", "fetch", "--offline"]).output().unwrap();
    success(&refreshed);
    assert!(String::from_utf8_lossy(&refreshed.stderr).contains(
        "lockfile: write required because dependency graph or exact source selections changed"
    ));
}
