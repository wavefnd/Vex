// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

mod support;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("vex-wavec-contract-{}-{id}", std::process::id()));
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
fn vex_uses_path_and_override_wavec_and_rejects_unknown_schema() {
    let fixture = TestDir::new();
    let project = fixture.0.join("project");
    fs::create_dir_all(project.join("src")).expect("project source directory must be created");
    fs::write(
        project.join("vex.ws"),
        "{ name = \"app\", version = 0.1.0, dependencies = [] }\n",
    )
    .expect("manifest must be written");
    fs::write(project.join("vex.lock"), "{ version = 2, package = [] }\n")
        .expect("lockfile must be written");
    fs::write(
        project.join("src/main.wave"),
        "fun main() { println(\"Hello World\"); }\n",
    )
    .expect("source must be written");

    let fake = compile_fake_wavec(&fixture.0);
    let override_log = fixture.0.join("override.log");
    let override_run = Command::new(env!("CARGO_BIN_EXE_vex"))
        .args([
            "run",
            "--locked",
            "--offline",
            "--",
            "--flag",
            "with spaces",
        ])
        .current_dir(&project)
        .env("VEX_WAVEC", &fake)
        .env("FAKE_WAVEC_LOG", &override_log)
        .output()
        .expect("Vex with VEX_WAVEC must start");
    assert_success(&override_run, "VEX_WAVEC override run");
    assert!(String::from_utf8_lossy(&override_run.stdout).contains("FAKE_WAVEC_EXECUTED"));
    assert_contract_invocations(&override_log);
    let output = String::from_utf8_lossy(&override_run.stdout);
    assert!(
        output.contains("--flag") && output.contains("with spaces"),
        "{output}"
    );

    let bin_dir = fixture.0.join("bin");
    fs::create_dir_all(&bin_dir).expect("fake PATH directory must be created");
    let path_wavec = bin_dir.join(if cfg!(windows) { "wavec.exe" } else { "wavec" });
    fs::copy(&fake, &path_wavec).expect("fake PATH wavec must be copied");
    let path_log = fixture.0.join("path.log");
    let path = env::join_paths(
        std::iter::once(bin_dir).chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )
    .expect("PATH must be assembled");
    let path_build = Command::new(env!("CARGO_BIN_EXE_vex"))
        .arg("build")
        .current_dir(&project)
        .env_remove("VEX_WAVEC")
        .env("PATH", path)
        .env("FAKE_WAVEC_LOG", &path_log)
        .output()
        .expect("Vex with PATH wavec must start");
    assert_success(&path_build, "PATH wavec build");
    assert_contract_invocations(&path_log);

    let schema_log = fixture.0.join("schema.log");
    let incompatible = Command::new(env!("CARGO_BIN_EXE_vex"))
        .arg("build")
        .current_dir(&project)
        .env("VEX_WAVEC", &fake)
        .env("FAKE_WAVEC_LOG", &schema_log)
        .env("FAKE_SCHEMA", "2")
        .output()
        .expect("Vex incompatible-schema check must start");
    assert!(!incompatible.status.success());
    let stderr = String::from_utf8_lossy(&incompatible.stderr);
    assert!(
        stderr.contains("schema_version `2`; expected `1`"),
        "{stderr}"
    );
    assert!(stderr.contains("VEX_WAVEC=/path/to/wavec"), "{stderr}");
    assert_eq!(
        fs::read_to_string(schema_log)
            .expect("schema log must exist")
            .lines()
            .filter(|line| line.contains(" build "))
            .count(),
        1,
        "Vex must not execute a real build after incompatible dry-run output"
    );
}

#[test]
fn vex_passes_direct_and_transitive_library_mappings_to_wavec() {
    let fixture = TestDir::new();
    let project = fixture.0.join("project");
    let add = fixture.0.join("add");
    let math = fixture.0.join("math");
    for package in [&project, &add, &math] {
        fs::create_dir_all(package.join("src")).unwrap();
    }

    fs::write(
        math.join("vex.ws"),
        "{ name = \"math\", version = 0.1.0, lib = true, dependencies = [] }\n",
    )
    .unwrap();
    fs::write(
        math.join("src/lib.wave"),
        "pub fun double(value: i32) -> i32 { return value * 2; }\n",
    )
    .unwrap();
    fs::write(
        add.join("vex.ws"),
        "{ name = \"add\", version = 0.1.0, lib = true, dependencies = [{ name = \"math\", path = \"../math\" }] }\n",
    )
    .unwrap();
    fs::write(
        add.join("src/lib.wave"),
        "import(\"math\");\npub fun sum(a: i32, b: i32) -> i32 { return a + b; }\n",
    )
    .unwrap();
    fs::write(
        project.join("vex.ws"),
        "{ name = \"app\", version = 0.1.0, dependencies = [{ name = \"add\", path = \"../add\" }] }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/main.wave"),
        "import(\"add\")::{sum};\nfun main() { var value: i32 = sum(1, 2); }\n",
    )
    .unwrap();

    let fake = compile_fake_wavec(&fixture.0);
    let log = fixture.0.join("dependency.log");
    let output = Command::new(env!("CARGO_BIN_EXE_vex"))
        .arg("check")
        .current_dir(&project)
        .env("VEX_WAVEC", &fake)
        .env("FAKE_WAVEC_LOG", &log)
        .output()
        .expect("Vex dependency check must start");
    assert_success(&output, "Vex dependency check");

    let invocations = fs::read_to_string(log).unwrap();
    for package in ["add", "math"] {
        let path = Path::new("..").join(package);
        let expected = format!("--dep={package}={}", path.display());
        assert!(invocations.contains(&expected), "{invocations}");
    }
    assert!(invocations.contains("src/main.wave"), "{invocations}");
}

fn compile_fake_wavec(root: &Path) -> PathBuf {
    let source = root.join("fake_wavec.rs");
    fs::write(&source, include_str!("fixtures/fake_wavec.rs"))
        .expect("fake wavec source must be written");
    let binary = root.join(if cfg!(windows) {
        "fake-wavec.exe"
    } else {
        "fake-wavec"
    });
    support::compile_fixture("fake_wavec", &binary);
    binary
}

fn assert_contract_invocations(log: &Path) {
    let lines = fs::read_to_string(log).expect("fake wavec log must exist");
    let lines = lines
        .lines()
        .filter(|line| line.contains(" build "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        2,
        "expected dry-run and real invocation: {lines:?}"
    );
    assert!(lines[0].contains("build"), "{lines:?}");
    assert!(lines[0].contains("--dry-run"), "{lines:?}");
    assert!(lines[0].contains("--error-format=json"), "{lines:?}");
    assert!(lines[1].contains("build"), "{lines:?}");
    assert!(!lines[1].contains("--dry-run"), "{lines:?}");
    assert!(!lines[1].contains("--error-format=json"), "{lines:?}");
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn simple_project(fixture: &TestDir) -> PathBuf {
    let project = fixture.0.join("app");
    fs::create_dir_all(project.join("src/nested")).unwrap();
    fs::write(project.join("vex.ws"), "{ name = \"app\" }").unwrap();
    fs::write(project.join("src/main.wave"), "fun main() {}\n").unwrap();
    project
}

#[test]
fn capability_preflight_rejects_targets_before_state_and_queries_once_per_invocation() {
    let f = TestDir::new();
    let project = simple_project(&f);
    let fake = compile_fake_wavec(&f.0);
    let log = f.0.join("capabilities.log");
    for (targets, expected) in [
        ("[\"aarch64-unknown-linux-gnu\"]", 3),
        ("{}", 4),
        ("[]", 4),
        ("[1]", 4),
        ("[\"\"]", 4),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&project)
            .args(["build", "--target", "x86_64-unknown-linux-gnu"])
            .env("VEX_WAVEC", &fake)
            .env("FAKE_TARGETS", targets)
            .env("FAKE_WAVEC_LOG", &log)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!project.join(".vex").exists());
        assert!(!project.join("vex.lock").exists());
        assert!(!project.join("target").exists());
    }
    fs::remove_file(&log).unwrap();
    for command in ["build", "check"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&project)
            .args([command, "--target=x86_64-unknown-linux-gnu"])
            .env("VEX_WAVEC", &fake)
            .env("FAKE_WAVEC_LOG", &log)
            .output()
            .unwrap();
        assert_success(&out, command);
    }
    let logged = fs::read_to_string(log).unwrap();
    assert_eq!(logged.lines().count(), 9);
    assert_eq!(
        logged
            .lines()
            .filter(|line| line.starts_with("print std-path"))
            .count(),
        1
    );
    assert_eq!(
        logged
            .lines()
            .filter(|s| s.starts_with("print supported-targets"))
            .count(),
        2
    );
}

#[test]
fn explicitly_empty_compiler_override_never_falls_back() {
    let f = TestDir::new();
    let project = simple_project(&f);
    for value in ["", " \t\r\n"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&project)
            .arg("build")
            .env("VEX_WAVEC", value)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(5));
        assert!(String::from_utf8_lossy(&out.stderr).contains("VEX_WAVEC is explicitly empty"));
        assert!(!project.join(".vex").exists());
    }
}

#[test]
fn runtime_arguments_and_cwd_survive_project_selection_and_dry_run_is_single_json() {
    let f = TestDir::new();
    let project = simple_project(&f);
    let fake = compile_fake_wavec(&f.0);
    // A relative override is anchored at invocation cwd, before ancestor discovery.
    let nested = project.join("src/nested");
    let local_compiler = nested.join(fake.file_name().unwrap());
    fs::copy(fake, &local_compiler).unwrap();
    let runtime = [
        "",
        "two words",
        "한글",
        "a\"b",
        "slash\\value",
        "--dry-run",
        "--manifest-path",
        "missing",
        "--message-file",
        "never.jsonl",
        "--",
    ];
    let log = f.0.join("run.log");
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&nested)
        .args(["run", "--"])
        .args(runtime)
        .env("VEX_WAVEC", local_compiler.file_name().unwrap())
        .env("FAKE_WAVEC_LOG", &log)
        .output()
        .unwrap();
    assert_success(&out, "argument forwarding");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let expected: Vec<std::ffi::OsString> = runtime.iter().map(Into::into).collect();
    assert!(
        stdout.contains(&format!("FAKE_WAVEC_EXECUTED {expected:?}")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "PROGRAM_CWD:{:?}",
            project.canonicalize().unwrap()
        )),
        "{stdout}"
    );
    assert!(!project.join("never.jsonl").exists());
    assert!(!nested.join(".vex").exists());
    let logged = fs::read_to_string(&log).unwrap();
    assert!(!logged.contains("two words"));
    fs::remove_file(&log).unwrap();
    let before = fs::read(project.join("vex.lock")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&nested)
        .args(["run", "--dry-run", "--locked", "--offline", "--"])
        .args(runtime)
        .env("VEX_WAVEC", &local_compiler)
        .env("FAKE_WAVEC_LOG", &log)
        .output()
        .unwrap();
    assert_success(&out, "dry run");
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        plan["plans"][0]["compiler_plan"]["execute"]["args"],
        serde_json::json!(runtime)
    );
    assert_eq!(
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|line| line.contains(" build "))
            .count(),
        1
    );
    assert_eq!(before, fs::read(project.join("vex.lock")).unwrap());
    assert!(!project.join("target/.vex-run/planned").exists());
}

#[cfg(unix)]
#[test]
fn non_utf8_runtime_bytes_are_preserved() {
    use std::os::unix::ffi::OsStringExt;
    let f = TestDir::new();
    let project = simple_project(&f);
    let fake = compile_fake_wavec(&f.0);
    let arg = std::ffi::OsString::from_vec(b"argument-\xfe".to_vec());
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&project)
        .args(["run", "--"])
        .arg(&arg)
        .env("VEX_WAVEC", &fake)
        .env("VEX_TEST_ARGUMENT_BYTES", "1")
        .output()
        .unwrap();
    assert_success(&out, "OS arguments");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!("ARG_BYTES:{:?}", b"argument-\xfe")),
        "{:?}",
        out
    );
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&project)
        .args(["run", "--dry-run", "--"])
        .arg(arg)
        .env("VEX_WAVEC", &fake)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn compiler_selected_runner_keeps_runtime_arguments_and_exit_code() {
    let f = TestDir::new();
    let project = simple_project(&f);
    let fake = compile_fake_wavec(&f.0);
    let runner = f.0.join(if cfg!(windows) {
        "runner.exe"
    } else {
        "runner"
    });
    fs::copy(&fake, &runner).unwrap();
    let runtime = ["", "two words", "--dry-run", "quote\"slash\\"];
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&project)
        .args(["run", "--"])
        .args(runtime)
        .env("VEX_WAVEC", &fake)
        .env("FAKE_RUNNER", &runner)
        .env("VEX_TEST_RUN_EXIT", "37")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(37), "{out:?}");
    let expected: Vec<std::ffi::OsString> = runtime.iter().map(Into::into).collect();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!("FAKE_WAVEC_EXECUTED {expected:?}")),
        "{out:?}"
    );
}

// Linux permits these raw filename bytes; macOS CI filesystems reject them.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_compiler_path_is_preserved() {
    use std::os::unix::ffi::OsStringExt;
    let f = TestDir::new();
    let project = simple_project(&f);
    let fake = compile_fake_wavec(&f.0);
    let renamed =
        f.0.join(std::ffi::OsString::from_vec(b"wavec-\xff".to_vec()));
    fs::rename(fake, &renamed).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&project)
        .arg("check")
        .env("VEX_WAVEC", &renamed)
        .output()
        .unwrap();
    assert_success(&out, "non-UTF-8 compiler path");
}
