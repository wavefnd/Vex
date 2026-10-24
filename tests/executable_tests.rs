// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use serde_json::Value;
mod support;
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
#[test]
fn executes_all_tests_and_keeps_first_program_failure_and_results() {
    let f = Fixture::new();
    f.package("alpha","alpha",r#"targets={lib={},tests=[{name="zeta",path="src/main.wave"},{name="alpha",path="src/main.wave"}]}"#);
    let out = f
        .command(&f.0)
        .env("TARGET_PROGRAM_EXIT_MAP", "alpha=7,zeta=9")
        .env("TARGET_PROGRAM_LOG", f.0.join("runs"))
        .args(["-p", "alpha", "--message-file", "results.jsonl", "test"])
        .output()
        .unwrap();
    code(&out, 7);
    assert!(String::from_utf8_lossy(&out.stderr).contains("test alpha::alpha ... FAILED (exit 7)"));
    assert_eq!(
        fs::read_to_string(f.0.join("runs")).unwrap(),
        "alpha\nzeta\n"
    );
    let records = fs::read_to_string(f.0.join("results.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    let results = records
        .iter()
        .filter(|e| e["event"] == "test_result")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["exit_code"], 7);
    assert_eq!(results[1]["exit_code"], 9);
    assert_eq!(records.last().unwrap()["origin"], "program");
    fs::remove_file(f.0.join("runs")).unwrap();
    code(
        &f.command(&f.0)
            .env("TARGET_PROGRAM_EXIT", "0")
            .env("TARGET_PROGRAM_LOG", f.0.join("runs"))
            .args([
                "--quiet",
                "-p",
                "alpha",
                "test",
                "zet",
                "--locked",
                "--offline",
            ])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(fs::read_to_string(f.0.join("runs")).unwrap(), "zeta\n");
}
#[test]
fn test_dependencies_are_locked_but_not_built_or_propagated_normally() {
    let f = Fixture::new();
    f.package("helper", "helper", r#"targets={lib={}}"#);
    f.package("alpha","alpha",r#"targets={lib={},tests=[{name="smoke",path="src/main.wave"}]},test_dependencies=[{name="helper",path="../helper"}]"#);
    code(&f.run(&["-p", "alpha", "fetch"]), 0);
    let lock = fs::read(f.0.join("vex.lock")).unwrap();
    let normal = f.metadata(&f.0, &["-p", "alpha"]);
    assert_eq!(normal["packages"].as_array().unwrap().len(), 0);
    let tests = f.metadata(&f.0, &["-p", "alpha", "--include-tests"]);
    assert_eq!(tests["packages"].as_array().unwrap().len(), 1);
    let out = f
        .command(&f.0)
        .env("TARGET_PROGRAM_EXIT", "0")
        .args(["-p", "alpha", "test", "--locked", "--offline"])
        .output()
        .unwrap();
    code(&out, 0);
    assert_eq!(fs::read(f.0.join("vex.lock")).unwrap(), lock);
    assert!(f
        .builds()
        .iter()
        .any(|args| args.iter().any(|s| s.starts_with("--dep=helper="))));
}
#[test]
fn dry_run_and_late_logging_failure_do_not_run_early_or_replace_program_code() {
    let f = Fixture::new();
    f.package("alpha","alpha",r#"targets={lib={},tests=[{name="one",path="src/main.wave"},{name="two",path="src/main.wave"}]}"#);
    code(&f.run(&["-p", "alpha", "fetch"]), 0);
    code(
        &f.command(&f.0)
            .args([
                "-p",
                "alpha",
                "--message-file",
                "absent.jsonl",
                "test",
                "--dry-run",
            ])
            .output()
            .unwrap(),
        0,
    );
    assert!(!f.0.join("absent.jsonl").exists());
    assert!(f
        .builds()
        .iter()
        .all(|args| args.iter().any(|arg| arg == "--dry-run")));
    let out = f
        .command(&f.0)
        .env("TARGET_PROGRAM_EXIT", "8")
        .env("TARGET_PROGRAM_LOG", f.0.join("runs"))
        .env("VEX_TEST_MESSAGE_FAIL_EVENT", "test_result")
        .args(["-p", "alpha", "--message-file", "broken.jsonl", "test"])
        .output()
        .unwrap();
    code(&out, 8);
    assert_eq!(fs::read_to_string(f.0.join("runs")).unwrap(), "one\ntwo\n");
}
