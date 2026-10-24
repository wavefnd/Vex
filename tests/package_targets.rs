// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use serde_json::{json, Value};
mod support;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

const MIXED: &str = r#"targets={lib={},bins=[{name="zeta",path="src/bin/zeta.wave"},{name="app",path="src/main.wave"},{name="admin",path="src/bin/admin.wave"}]}"#;
fn triple() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "x86_64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}
fn compiler() -> &'static Path {
    static COMPILER: OnceLock<PathBuf> = OnceLock::new();
    COMPILER.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("vex-target-compiler-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let compiler = root.join(if cfg!(windows) {
            "target-wavec.exe"
        } else {
            "target-wavec"
        });
        support::compile_fixture("target_wavec", &compiler);
        compiler
    })
}
struct Fixture {
    root: PathBuf,
    app: PathBuf,
}
impl Fixture {
    fn new(fields: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vex package targets {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let app = root.join("app");
        let fixture = Self { root, app };
        fixture.package("app", fields);
        fs::write(fixture.app.join("vex.lock"), "{version=3,package=[]}\n").unwrap();
        fixture
    }
    fn package(&self, name: &str, fields: &str) -> PathBuf {
        let root = self.root.join(name);
        fs::create_dir_all(root.join("src/bin")).unwrap();
        fs::write(
            root.join("vex.ws"),
            format!("{{format=3,name=\"{name}\",{fields}}}"),
        )
        .unwrap();
        for path in [
            "src/lib.wave",
            "src/main.wave",
            "src/bin/admin.wave",
            "src/bin/zeta.wave",
        ] {
            fs::write(root.join(path), "fun main() {}\n").unwrap();
        }
        root
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(&self.app)
            .env("VEX_WAVEC", compiler())
            .env("TARGET_COMPILER_LOG", self.root.join("compiler.jsonl"))
            .env("NO_COLOR", "1");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn invocations(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.root.join("compiler.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn build_calls(&self) -> Vec<Vec<String>> {
        self.invocations()
            .into_iter()
            .filter(|args| args.iter().any(|a| a == "build"))
            .collect()
    }
    fn artifact(&self, kind: &str, name: &str, profile: &str) -> PathBuf {
        let filename = if kind == "lib" {
            format!("{name}.{}", if cfg!(windows) { "obj" } else { "o" })
        } else {
            format!("{name}{}", if cfg!(windows) { ".exe" } else { "" })
        };
        self.app
            .join("target")
            .join(triple())
            .join(profile)
            .join(kind)
            .join(filename)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn code(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn plan(output: &Output) -> Value {
    code(output, 0);
    serde_json::from_slice(&output.stdout).unwrap()
}
fn identities(plan: &Value) -> Vec<Value> {
    plan["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plan| plan["package_target"].clone())
        .collect()
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, entries);
            } else {
                entries.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
fn default_build_plans_library_then_sorted_binaries_once_without_mutation() {
    let f = Fixture::new(MIXED);
    code(&f.run(&["fetch"]), 0);
    let before = snapshot(&f.app);
    let output = plan(&f.run(&["build", "--dry-run", "--locked", "--offline"]));
    assert_eq!(output["schema"], "vex.build-plan");
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["target"], triple());
    assert_eq!(output["profile"], json!({"name":"dev","opt_level":0}));
    assert_eq!(
        identities(&output),
        vec![
            json!({"kind":"library","name":"app"}),
            json!({"kind":"binary","name":"admin"}),
            json!({"kind":"binary","name":"app"}),
            json!({"kind":"binary","name":"zeta"})
        ]
    );
    assert_eq!(f.build_calls().len(), 4);
    assert!(f
        .build_calls()
        .iter()
        .all(|args| args.iter().any(|arg| arg == "--dry-run")));
    assert_eq!(snapshot(&f.app), before);
    assert!(!f.app.join("target").exists());
    let plans = output["plans"].as_array().unwrap();
    assert_eq!(
        Path::new(
            plans[0]["compiler_plan"]["compile"][0]["output"]
                .as_str()
                .unwrap()
        ),
        f.artifact("lib", "app", "dev")
            .strip_prefix(&f.app)
            .unwrap()
    );
    assert_eq!(
        Path::new(
            plans[2]["compiler_plan"]["link"]["output"]
                .as_str()
                .unwrap()
        ),
        f.artifact("bin", "app", "dev")
            .strip_prefix(&f.app)
            .unwrap()
    );
}

#[test]
fn selections_support_library_and_multiple_named_binaries_without_other_sources() {
    for (args, expected) in [
        (
            vec!["build", "--lib", "--dry-run"],
            vec![json!({"kind":"library","name":"app"})],
        ),
        (
            vec!["build", "--bin=zeta", "--bin", "admin", "--dry-run"],
            vec![
                json!({"kind":"binary","name":"admin"}),
                json!({"kind":"binary","name":"zeta"}),
            ],
        ),
        (
            vec!["build", "--lib", "--bin", "admin", "--dry-run"],
            vec![
                json!({"kind":"library","name":"app"}),
                json!({"kind":"binary","name":"admin"}),
            ],
        ),
    ] {
        let f = Fixture::new(MIXED);
        fs::remove_file(f.app.join("src/main.wave")).unwrap();
        assert_eq!(identities(&plan(&f.run(&args))), expected);
    }
}

#[test]
fn check_defaults_to_all_targets_without_artifacts() {
    let f = Fixture::new(MIXED);
    code(&f.run(&["check", "--locked", "--offline"]), 0);
    let calls = f.build_calls();
    assert_eq!(calls.len(), 8);
    assert!(calls
        .iter()
        .all(|args| args.iter().any(|arg| arg == "--emit=check")));
    assert_eq!(
        calls
            .iter()
            .filter(|args| args.iter().any(|arg| arg == "--dry-run"))
            .count(),
        4
    );
    assert!(!f.artifact("lib", "app", "dev").exists());
    assert!(!f.artifact("bin", "app", "dev").exists());
}

#[test]
fn run_uses_default_or_explicit_binary_and_preserves_program_exit_and_arguments() {
    let f = Fixture::new(&format!("{MIXED},default_run=\"admin\""));
    let output = f
        .command()
        .args(["run", "--", "a b", "--profile", "program-only"])
        .env(
            "TARGET_PROGRAM_CHECK_LOCK_RELEASE",
            env!("CARGO_BIN_EXE_vex"),
        )
        .output()
        .unwrap();
    code(&output, 17);
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains(r#"TARGET_PROGRAM:["a b","--profile","program-only"]"#));
    let calls = f.build_calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].iter().any(|arg| arg == "src/bin/admin.wave"));
    assert!(!calls[1].iter().any(|arg| arg == "--run"));
    let output = plan(&f.run(&["run", "--bin", "zeta", "--dry-run", "--", "hello"]));
    assert_eq!(
        identities(&output),
        vec![json!({"kind":"binary","name":"zeta"})]
    );
    assert_eq!(
        output["plans"][0]["compiler_plan"]["execute"]["args"],
        json!(["hello"])
    );
    assert!(!f.artifact("bin", "admin", "dev").exists());
}

#[test]
fn run_requires_an_unambiguous_binary_and_rejects_library_selection_without_state() {
    for (fields, args, expected) in [
        (MIXED, vec!["run"], 3),
        (MIXED, vec!["run", "--lib"], 2),
        (MIXED, vec!["run", "--bin", "app", "--bin", "admin"], 2),
        ("targets={lib={}}", vec!["run"], 3),
    ] {
        let f = Fixture::new(fields);
        let before = snapshot(&f.app);
        code(&f.run(&args), expected);
        assert!(f.invocations().is_empty());
        assert_eq!(snapshot(&f.app), before);
        assert!(!f.app.join(".vex").exists());
    }
    let f = Fixture::new(r#"targets={bins=[{name="only",path="src/main.wave"}]}"#);
    assert_eq!(
        identities(&plan(&f.run(&["run", "--dry-run"]))),
        vec![json!({"kind":"binary","name":"only"})]
    );
}

#[test]
fn profile_selection_changes_optimization_and_artifact_identity() {
    for (selection, profile, level) in [
        (vec!["--profile", "fast"], "fast", "-O3"),
        (vec!["--release"], "release", "-O2"),
        (vec![], "dev", "-O0"),
    ] {
        let f = Fixture::new(
            r#"targets={bins=[{name="app",path="src/main.wave"}]},profiles={fast={inherits="release",opt_level=3}}"#,
        );
        let mut args = vec!["build", "--dry-run"];
        args.extend(selection);
        let output = plan(&f.run(&args));
        assert_eq!(output["profile"]["name"], profile);
        assert!(f.build_calls()[0].iter().any(|arg| arg == level));
        assert_eq!(
            Path::new(
                output["plans"][0]["compiler_plan"]["link"]["output"]
                    .as_str()
                    .unwrap()
            ),
            f.artifact("bin", "app", profile)
                .strip_prefix(&f.app)
                .unwrap()
        );
    }
}

#[test]
fn invalid_or_repeated_selection_fails_before_compiler_or_project_mutation() {
    for (args, expected) in [
        (vec!["build", "--bin", "missing"], 3),
        (vec!["build", "--profile", "missing"], 3),
        (vec!["build", "--lib", "--lib"], 2),
        (vec!["build", "--bin", "app", "--bin=app"], 2),
        (vec!["build", "--profile=dev", "--profile", "release"], 2),
        (vec!["build", "--release", "--profile", "release"], 2),
        (vec!["build", "--bin"], 2),
        (vec!["build", "--profile="], 2),
    ] {
        let f = Fixture::new(MIXED);
        let before = snapshot(&f.app);
        code(&f.run(&args), expected);
        assert!(f.invocations().is_empty());
        assert_eq!(snapshot(&f.app), before);
        assert!(!f.app.join("target").exists());
        assert!(!f.app.join(".vex").exists());
    }
}

#[test]
fn missing_last_selected_source_prevents_all_compiler_calls_and_output() {
    let f = Fixture::new(MIXED);
    fs::remove_file(f.app.join("src/bin/zeta.wave")).unwrap();
    let before = snapshot(&f.app);
    code(&f.run(&["build"]), 3);
    assert!(f.invocations().is_empty());
    assert_eq!(snapshot(&f.app), before);
    assert!(!f.app.join("target").exists());
}

#[test]
fn all_plans_and_ownership_preflight_complete_before_any_compilation() {
    let f = Fixture::new(MIXED);
    let output = f
        .command()
        .args(["build"])
        .env("TARGET_PLAN_FAIL_INPUT", "zeta.wave")
        .output()
        .unwrap();
    code(&output, 4);
    let calls = f.build_calls();
    assert_eq!(calls.len(), 4);
    assert!(calls
        .iter()
        .all(|args| args.iter().any(|arg| arg == "--dry-run")));
    assert!(!f.artifact("lib", "app", "dev").exists());
    assert!(!f.artifact("bin", "admin", "dev").exists());
    let f = Fixture::new(MIXED);
    let existing = f.artifact("bin", "zeta", "dev");
    fs::create_dir_all(existing.parent().unwrap()).unwrap();
    fs::write(&existing, b"user file").unwrap();
    code(&f.run(&["build"]), 5);
    assert!(f
        .build_calls()
        .iter()
        .all(|args| args.iter().any(|arg| arg == "--dry-run")));
    assert!(!f.artifact("lib", "app", "dev").exists());
    assert!(!f.artifact("bin", "admin", "dev").exists());
    assert_eq!(fs::read(existing).unwrap(), b"user file");
}

#[test]
fn first_compile_failure_stops_batch_and_keeps_earlier_successful_artifacts() {
    let f = Fixture::new(MIXED);
    let output = f
        .command()
        .args(["build"])
        .env("TARGET_COMPILE_FAIL_INPUT", "admin.wave")
        .output()
        .unwrap();
    code(&output, 4);
    assert!(f.artifact("lib", "app", "dev").is_file());
    assert!(!f.artifact("bin", "admin", "dev").exists());
    assert!(!f.artifact("bin", "app", "dev").exists());
    let executed = f
        .build_calls()
        .into_iter()
        .filter(|args| !args.iter().any(|arg| arg == "--dry-run"))
        .collect::<Vec<_>>();
    assert_eq!(executed.len(), 2);
    assert!(executed[0].iter().any(|arg| arg == "src/lib.wave"));
    assert!(executed[1].iter().any(|arg| arg == "src/bin/admin.wave"));
}

#[test]
fn mixed_dependency_consumes_only_library_and_self_library_mapping_is_not_locked() {
    let f = Fixture::new(&format!(
        r#"{MIXED},dependencies=[{{name="tools",path="../tools"}}]"#
    ));
    let dependency = f.package(
        "tools",
        r#"targets={lib={},bins=[{name="tool",path="src/missing.wave"}]}"#,
    );
    fs::remove_file(f.app.join("vex.lock")).unwrap();
    code(&f.run(&["fetch"]), 0);
    let lock = fs::read(f.app.join("vex.lock")).unwrap();
    let before = snapshot(&f.app);
    let output = plan(&f.run(&["build", "--locked", "--offline", "--dry-run"]));
    assert_eq!(output["plans"].as_array().unwrap().len(), 4);
    let calls = f.build_calls();
    assert_eq!(calls.len(), 4);
    for (index, args) in calls.iter().enumerate() {
        assert!(
            args.iter().any(|arg| arg.starts_with("--dep=tools=")),
            "{args:?}"
        );
        assert_eq!(
            args.iter().any(|arg| arg.starts_with("--dep=app=")),
            index > 0,
            "{args:?}"
        );
        assert!(!args.iter().any(|arg| arg == "src/missing.wave"));
    }
    assert!(dependency.join("src/lib.wave").is_file());
    assert_eq!(fs::read(f.app.join("vex.lock")).unwrap(), lock);
    assert_eq!(snapshot(&f.app), before);
    let parsed = lockfile::decode(std::str::from_utf8(&lock).unwrap()).unwrap();
    assert_eq!(parsed.packages.len(), 1);
    assert_eq!(parsed.packages[0].name, "tools");
    assert!(parsed.packages[0].dependencies.is_empty());
}

#[test]
fn bare_wasm_rejects_program_arguments_before_planning_or_allocating_a_generation() {
    let f = Fixture::new(
        r#"targets={bins=[{name="app",path="src/main.wave",mode="bare_wasm"}]},build={target="wasm32-unknown-unknown"}"#,
    );
    let result = f.run(&["run", "--", "arg"]);
    code(&result, 3);
    assert!(String::from_utf8_lossy(&result.stderr).contains("process argument ABI"));
    assert!(f.build_calls().is_empty());
    assert!(!f.app.join("target").exists());
}

#[test]
fn project_selection_errors_precede_missing_compiler() {
    let f = Fixture::new(MIXED);
    for arguments in [
        vec!["build", "--bin", "missing"],
        vec!["build", "--profile", "missing"],
    ] {
        let result = f
            .command()
            .args(arguments)
            .env("VEX_WAVEC", "")
            .output()
            .unwrap();
        code(&result, 3);
        assert!(f.build_calls().is_empty());
        assert!(!f.app.join(".vex").exists());
    }
}

#[test]
fn only_active_dependency_compiler_requirements_gate_a_build() {
    let f = Fixture::new("targets={bins=[{name=\"app\",path=\"src/main.wave\"}]}");
    f.package("extra", "targets={lib={}},compiler=\"9.0.0\"");
    fs::write(f.app.join("vex.ws"), "{format=4,name=\"app\",targets={bins=[{name=\"app\",path=\"src/main.wave\"}]},dependencies=[{name=\"extra\",path=\"../extra\",optional=true}]}").unwrap();
    code(&f.run(&["fetch"]), 0);
    let lock = fs::read(f.app.join("vex.lock")).unwrap();
    code(&f.run(&["build", "--locked", "--offline"]), 0);
    assert!(f
        .build_calls()
        .iter()
        .all(|args| !args.iter().any(|arg| arg.starts_with("--dep=extra="))));
    let calls = f.build_calls().len();
    code(
        &f.run(&["build", "--features=extra", "--locked", "--offline"]),
        3,
    );
    assert_eq!(f.build_calls().len(), calls);
    assert_eq!(fs::read(f.app.join("vex.lock")).unwrap(), lock);
}

#[test]
fn every_selected_mode_is_validated_before_any_compiler_execution() {
    let f = Fixture::new("targets={bins=[{name=\"app\",path=\"src/main.wave\"}]}");
    f.package("extra", "targets={lib={}},compiler=\"9.0.0\"");
    fs::write(f.app.join("vex.ws"), "{format=4,name=\"app\",targets={bins=[{name=\"host\",path=\"src/main.wave\",mode=\"hosted\"},{name=\"bare\",path=\"src/bin/admin.wave\",mode=\"freestanding\",startup=\"none\",entry=\"_start\"}]},dependencies=[{name=\"extra\",path=\"../extra\",target={mode=\"freestanding\"}}]}").unwrap();
    code(
        &f.run(&["fetch", "--target", "x86_64-unknown-linux-gnu"]),
        0,
    );
    code(
        &f.run(&[
            "build",
            "--bin",
            "host",
            "--target",
            "x86_64-unknown-linux-gnu",
        ]),
        0,
    );
    let calls = f.build_calls().len();
    let lock = fs::read(f.app.join("vex.lock")).unwrap();
    code(
        &f.run(&["build", "--target", "x86_64-unknown-linux-gnu"]),
        3,
    );
    assert_eq!(f.build_calls().len(), calls);
    assert_eq!(fs::read(f.app.join("vex.lock")).unwrap(), lock);
}
