// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

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
        .args(["run", "--locked", "--offline"])
        .current_dir(&project)
        .env("VEX_WAVEC", &fake)
        .env("FAKE_WAVEC_LOG", &override_log)
        .output()
        .expect("Vex with VEX_WAVEC must start");
    assert_success(&override_run, "VEX_WAVEC override run");
    assert!(String::from_utf8_lossy(&override_run.stdout).contains("FAKE_WAVEC_EXECUTED"));
    assert_contract_invocations(&override_log);

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
    fs::write(
        &source,
        r#"
use std::env;
use std::fs::OpenOptions;
use std::io::Write;

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if let Ok(path) = env::var("FAKE_WAVEC_LOG") {
        let mut log = OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(log, "{}", args.join(" ")).unwrap();
    }
    if args.iter().any(|arg| arg == "--dry-run") {
        let schema = env::var("FAKE_SCHEMA").unwrap_or_else(|_| "1".to_string());
        println!(
            "{{\"schema_version\":{schema},\"mode\":\"build\",\"target\":\"test-target\",\"emit\":\"bin\",\"emit_kinds\":[],\"control_mode\":null,\"forced_input_type\":null,\"inputs\":[],\"emit_jobs\":[],\"compile\":[],\"link\":null,\"execute\":null}}"
        );
    } else {
        println!("FAKE_WAVEC_EXECUTED");
    }
}
"#,
    )
    .expect("fake wavec source must be written");
    let binary = root.join(if cfg!(windows) {
        "fake-wavec.exe"
    } else {
        "fake-wavec"
    });
    let compile = Command::new("rustc")
        .args(["--edition=2021"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("rustc for fake wavec must start");
    assert_success(&compile, "compile fake wavec");
    binary
}

fn assert_contract_invocations(log: &Path) {
    let lines = fs::read_to_string(log).expect("fake wavec log must exist");
    let lines = lines.lines().collect::<Vec<_>>();
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
