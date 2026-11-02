// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use serde_json::{json, Value};
mod support;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

const COMPILER_SOURCE: &str = include_str!("fixtures/artifact_wavec.rs");

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vex artifact integrity {} {}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("vex.ws"), "{name=\"app\",version=0.1.0,lib=true}").unwrap();
        fs::write(root.join("src/lib.wave"), "fun main() {}\n").unwrap();
        fs::write(root.join("src/main.wave"), "fun main() {}\n").unwrap();
        Self {
            root: root.canonicalize().unwrap(),
        }
    }
    fn shared() -> Self {
        let fixture = Self::new();
        fs::write(fixture.root.join("vex.ws"), "{format=2,name=\"app\",version=0.1.0,lib=true,native={output=\"shared\",exports=[\"wave_add\"]}}").unwrap();
        fixture
    }
    fn command(&self, plan: &Value, outputs: &[&str]) -> Command {
        static COMPILER: OnceLock<PathBuf> = OnceLock::new();
        let compiler = COMPILER.get_or_init(|| {
            let root =
                std::env::temp_dir().join(format!("vex-artifact-compiler-{}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            let source = root.join("fake.rs");
            fs::write(&source, COMPILER_SOURCE).unwrap();
            let compiler = root.join(if cfg!(windows) { "wavec.exe" } else { "wavec" });
            support::compile_fixture("artifact_wavec", &compiler);
            compiler
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(&self.root)
            .env("VEX_WAVEC", compiler)
            .env("PLAN", plan.to_string())
            .env("PLAN_TARGET", plan["target"].as_str().unwrap())
            .env("OUTPUTS", outputs.join("\n"))
            .env("NO_COLOR", "1");
        command
    }
    fn build(&self, plan: &Value, outputs: &[&str]) -> Output {
        self.command(plan, outputs).arg("build").output().unwrap()
    }
    fn clean(&self) {
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .arg("clean")
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("messages.jsonl"))
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

fn plan(outputs: &[&str]) -> Value {
    json!({"schema_version":1,"mode":"build","target":"x86_64-unknown-linux-gnu","emit":"obj","emit_kinds":["obj"],"control_mode":null,"forced_input_type":null,"inputs":[{"path":"src/lib.wave","kind":"wave"}],"emit_jobs":[],"compile":outputs.iter().map(|output| json!({"input":"src/lib.wave","kind":"wave","output":output,"command":"compiler"})).collect::<Vec<_>>(),"link":null,"execute":null})
}
fn plan_object(output: &str) -> Value {
    plan(&[output])
}
fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_and_modified_outputs_are_rejected_before_compiler_execution() {
    for modified in [false, true] {
        let f = Fixture::new();
        let plan = plan(&["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]);
        if modified {
            assert_code(
                &f.build(&plan, &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
                0,
            );
            fs::remove_file(f.root.join("compiled")).unwrap();
        } else {
            fs::create_dir_all(f.root.join("target/x86_64-unknown-linux-gnu/dev/lib")).unwrap();
        }
        fs::write(
            f.root.join("target/x86_64-unknown-linux-gnu/dev/lib/app.o"),
            b"user content",
        )
        .unwrap();
        let registry = fs::read(f.root.join(".vex/artifacts.json")).ok();
        assert_code(
            &f.build(&plan, &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
            5,
        );
        assert!(!f.root.join("compiled").exists());
        assert_eq!(
            fs::read(f.root.join("target/x86_64-unknown-linux-gnu/dev/lib/app.o")).unwrap(),
            b"user content"
        );
        assert_eq!(fs::read(f.root.join(".vex/artifacts.json")).ok(), registry);
        f.clean();
        assert_eq!(
            fs::read(f.root.join("target/x86_64-unknown-linux-gnu/dev/lib/app.o")).unwrap(),
            b"user content"
        );
    }
}

#[test]
fn unchanged_owned_outputs_can_be_rebuilt_and_cleaned() {
    let f = Fixture::new();
    let plan = plan(&["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]);
    for _ in 0..2 {
        assert_code(
            &f.build(&plan, &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
            0,
        );
    }
    f.clean();
    assert!(!f
        .root
        .join("target/x86_64-unknown-linux-gnu/dev/lib/app.o")
        .exists());
}

#[test]
fn missing_or_nonregular_outputs_do_not_publish_artifact_success() {
    // A real linked plan has both its intermediate and final image as required
    // outputs. Producing only the intermediate must not report success.
    let f = Fixture::shared();
    let plan = shared_plan(false);
    let intermediate = plan["compile"][0]["output"].as_str().unwrap();
    let output = f
        .command(&plan, &[intermediate])
        .args(["--message-file", "messages.jsonl", "build"])
        .output()
        .unwrap();
    assert_code(&output, 4);
    assert!(
        f.root.join("compiled").exists(),
        "fixture must reach compilation"
    );
    assert!(!f.events().iter().any(|event| event["event"] == "artifact"));
    assert_eq!(f.events().last().unwrap()["category"], "compiler");
    assert!(
        !f.root.join(intermediate).exists(),
        "failed compilation must not publish partial intermediate outputs"
    );
    f.clean();
    assert!(!f.root.join(intermediate).exists());

    let f = Fixture::new();
    let path = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
    let output = f
        .command(&plan_object(path), &[path])
        .env("DIRECTORY_OUTPUT", "1")
        .args(["--message-file", "messages.jsonl", "build"])
        .output()
        .unwrap();
    assert_code(&output, 4);
    assert!(
        f.root.join("compiled").exists(),
        "fixture must reach compilation"
    );
    assert!(
        !f.root.join(path).exists(),
        "nonregular staged output must never be published"
    );
    assert!(!f.events().iter().any(|event| event["event"] == "artifact"));
    assert_eq!(f.events().last().unwrap()["category"], "compiler");
}

#[test]
fn ownership_failure_preserves_primary_compiler_error_and_structured_context() {
    for (compiler_code, expected) in [(1, 4), (3, 5), (0, 5)] {
        let f = Fixture::new();
        let output = f
            .command(
                &plan(&["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
                &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"],
            )
            .args(["--message-file", "messages.jsonl", "build"])
            .env("COMPILE_EXIT", compiler_code.to_string())
            .env("FAIL_OWNERSHIP", "1")
            .output()
            .unwrap();
        assert_code(&output, expected);
        let events = f.events();
        assert!(!events.iter().any(|event| event["event"] == "artifact"));
        if compiler_code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("wavec build failed"), "{stderr}");
            let error = events
                .iter()
                .find(|event| event["event"] == "diagnostic")
                .unwrap();
            assert!(error["context"]
                .to_string()
                .contains("artifact_ownership_error"));
        }
    }
}

#[test]
fn escaping_duplicate_and_overlapping_outputs_fail_before_execution() {
    for paths in [
        vec!["src/main.wave"],
        vec!["target/../src/main.wave"],
        vec!["target/a.o", "target/a.o"],
        vec!["target/a.o", "target/./a.o"],
        vec!["target/output", "target/output/a.o"],
    ] {
        let f = Fixture::new();
        let output = f.build(&plan(&paths), &paths);
        assert_code(&output, 4);
        assert!(!f.root.join("compiled").exists());
        assert_eq!(
            fs::read(f.root.join("src/main.wave")).unwrap(),
            b"fun main() {}\n"
        );
    }
}

#[test]
fn direct_input_and_final_output_aliases_are_rejected() {
    let f = Fixture::new();
    let mut plan = plan(&["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]);
    assert_code(
        &f.build(&plan, &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
        0,
    );
    fs::remove_file(f.root.join("compiled")).unwrap();
    plan["inputs"][0]["path"] = json!("target/x86_64-unknown-linux-gnu/dev/lib/app.o");
    assert_code(
        &f.build(&plan, &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
        4,
    );
    assert!(!f.root.join("compiled").exists());
}

#[test]
fn hard_links_to_owned_outputs_are_not_overwritten() {
    let f = Fixture::new();
    let plan = plan(&["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]);
    assert_code(
        &f.build(&plan, &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
        0,
    );
    fs::remove_file(f.root.join("compiled")).unwrap();
    fs::hard_link(
        f.root.join("target/x86_64-unknown-linux-gnu/dev/lib/app.o"),
        f.root.join("preserve.o"),
    )
    .unwrap();
    assert_code(
        &f.build(&plan, &["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
        5,
    );
    assert!(!f.root.join("compiled").exists());
    assert_eq!(
        fs::read(f.root.join("preserve.o")).unwrap(),
        b"compiled artifact"
    );
}

fn shared_plan(windows: bool) -> Value {
    let triple = if windows {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let intermediate = format!("target/{triple}/dev/.vex-obj/lib/app/shared/input.o");
    let output = format!(
        "target/{triple}/dev/lib/{}",
        if windows { "app.dll" } else { "libapp.so" }
    );
    let mut plan = plan(&[&intermediate]);
    plan["target"] = json!(triple);
    plan["emit"] = json!("bin");
    plan["emit_kinds"] = json!(["bin"]);
    plan["link"] = json!({"output":output,"inputs":[intermediate],"program":"fixture linker","args":if windows { vec![format!("/OUT:{output}"), "/DLL".into()] } else { vec![] }});
    plan
}
fn msvc_plan() -> Value {
    shared_plan(true)
}

#[test]
fn optional_msvc_companions_are_reported_and_cleaned_only_when_produced() {
    let f = Fixture::shared();
    let outputs = [
        "target/x86_64-pc-windows-msvc/dev/.vex-obj/lib/app/shared/input.o",
        "target/x86_64-pc-windows-msvc/dev/lib/app.dll",
        "target/x86_64-pc-windows-msvc/dev/lib/app.lib",
        "target/x86_64-pc-windows-msvc/dev/lib/app.exp",
    ];
    let output = f
        .command(&msvc_plan(), &outputs)
        .args(["--message-file", "messages.jsonl", "build"])
        .output()
        .unwrap();
    assert_code(&output, 0);
    let events = f.events();
    let artifact = events
        .iter()
        .find(|event| event["event"] == "artifact")
        .unwrap();
    let paths = artifact["paths"].as_array().unwrap();
    for expected in &outputs[1..] {
        assert!(paths.contains(&json!(expected)));
    }
    assert_eq!(paths.len(), 3);
    f.clean();
    for output in outputs {
        assert!(!f.root.join(output).exists());
    }
}

#[test]
fn optional_msvc_output_collisions_and_unowned_files_are_protected() {
    for sidecar in [
        "app.lib",
        "app.exp",
        "app.pdb",
        "app.ilk",
        "app.map",
        "app.dll.manifest",
    ] {
        let f = Fixture::shared();
        fs::create_dir_all(f.root.join("target/x86_64-pc-windows-msvc/dev/lib")).unwrap();
        let path = f
            .root
            .join("target/x86_64-pc-windows-msvc/dev/lib")
            .join(sidecar);
        fs::write(&path, "user content").unwrap();
        assert_code(&f.build(&msvc_plan(), &[]), 5);
        assert!(!f.root.join("compiled").exists());
        assert_eq!(fs::read(path).unwrap(), b"user content");
    }
    let f = Fixture::shared();
    let mut plan = msvc_plan();
    plan["link"]["args"].as_array_mut().unwrap().push(json!(
        "/PDB:target/x86_64-pc-windows-msvc/dev/.vex-obj/lib/app/shared/input.o"
    ));
    assert_code(&f.build(&plan, &[]), 4);
    assert!(!f.root.join("compiled").exists());
}

#[test]
fn dry_run_validates_destinations_without_creating_outputs() {
    let f = Fixture::new();
    let fetch = Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(["fetch", "--offline"])
        .current_dir(&f.root)
        .output()
        .unwrap();
    assert_code(&fetch, 0);
    let output = f
        .command(&plan(&["src/main.wave"]), &[])
        .args(["build", "--dry-run", "--locked", "--offline"])
        .output()
        .unwrap();
    assert_code(&output, 4);
    assert!(!f.root.join("target").exists());
    assert!(!f.root.join("compiled").exists());
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn case_only_output_aliases_are_rejected_on_case_insensitive_hosts() {
    let f = Fixture::new();
    assert_code(
        &f.build(&plan(&["target/answer.o", "target/ANSWER.o"]), &[]),
        4,
    );
    assert!(!f.root.join("compiled").exists());
}

#[cfg(windows)]
#[test]
fn windows_trailing_dot_and_alternate_stream_destinations_are_rejected() {
    for path in [
        "target/answer.o.",
        "target/answer.o ",
        "target/answer.o:stream",
    ] {
        let f = Fixture::new();
        assert_code(&f.build(&plan(&[path]), &[]), 4);
        assert!(!f.root.join("compiled").exists());
    }
}

#[cfg(unix)]
#[test]
fn symlink_destinations_are_rejected_before_compilation() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("target/x86_64-unknown-linux-gnu/dev/lib")).unwrap();
    symlink(
        f.root.join("src/main.wave"),
        f.root.join("target/x86_64-unknown-linux-gnu/dev/lib/app.o"),
    )
    .unwrap();
    assert_code(
        &f.build(
            &plan(&["target/x86_64-unknown-linux-gnu/dev/lib/app.o"]),
            &[],
        ),
        4,
    );
    assert!(!f.root.join("compiled").exists());
    assert_eq!(
        fs::read(f.root.join("src/main.wave")).unwrap(),
        b"fun main() {}\n"
    );
}

#[test]
fn compiler_cannot_change_selected_artifact_kind_or_namespace() {
    let f = Fixture::new();
    assert_code(&f.build(&plan(&["target/ignored-selection.o"]), &[]), 4);
    assert!(!f.root.join("compiled").exists());

    let f = Fixture::shared();
    let object_instead_of_shared = plan(&["target/x86_64-unknown-linux-gnu/dev/lib/libapp.so"]);
    assert_code(&f.build(&object_instead_of_shared, &[]), 4);
    assert!(!f.root.join("compiled").exists());

    let f = Fixture::new();
    let mut linked_instead_of_object = shared_plan(false);
    linked_instead_of_object["link"]["output"] =
        json!("target/x86_64-unknown-linux-gnu/dev/lib/app.o");
    assert_code(&f.build(&linked_instead_of_object, &[]), 4);
    assert!(!f.root.join("compiled").exists());

    let f = Fixture::shared();
    let mut escaped_intermediate = shared_plan(false);
    escaped_intermediate["compile"][0]["output"] = json!("target/another-package.o");
    escaped_intermediate["link"]["inputs"] = json!(["target/another-package.o"]);
    assert_code(&f.build(&escaped_intermediate, &[]), 4);
    assert!(!f.root.join("compiled").exists());
}

#[test]
fn staged_failure_and_mid_publication_rollback_keep_the_last_complete_generation() {
    let f = Fixture::new();
    let name = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
    let plan = plan_object(name);
    assert_code(&f.build(&plan, &[name]), 0);
    let pointers = f.root.join("target/.vex-results");
    let pointer = fs::read_dir(&pointers)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let before = fs::read(&pointer).unwrap();
    let result: Value = serde_json::from_slice(&before).unwrap();
    let generation = PathBuf::from(result["artifacts"][0]["path"].as_str().unwrap());
    let good = fs::read(&generation).unwrap();
    for (variable, value) in [
        ("COMPILE_EXIT", "1"),
        ("VEX_TEST_PUBLICATION_FAIL_AFTER", "0"),
    ] {
        let output = f
            .command(&plan, &[name])
            .env("ARTIFACT_BYTES", "changed artifact")
            .env(variable, value)
            .arg("build")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(fs::read(f.root.join(name)).unwrap(), good);
        assert_eq!(fs::read(&generation).unwrap(), good);
        assert_eq!(fs::read(&pointer).unwrap(), before);
        assert!(!f.root.join(".vex/build-publication.json").exists());
    }
    assert_code(
        &f.command(&plan, &[name])
            .env("ARTIFACT_BYTES", "changed artifact")
            .arg("build")
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(fs::read(f.root.join(name)).unwrap(), b"changed artifact");
    assert_eq!(fs::read(generation).unwrap(), good);
}

#[test]
fn external_roots_reports_and_toolchain_pins_preserve_source_and_owned_files() {
    let f = Fixture::new();
    let name = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
    let mut plan = plan_object(name);
    plan["source_inputs"] = json!(["src/lib.wave"]);
    let first = f.root.join("build one é");
    let second = f.root.join("build two");
    let report = f.root.join("result #$.json");
    let depfile = f.root.join("result #$.d");
    let standard = f.root.join("prepared std");
    fs::create_dir(&standard).unwrap();
    fs::write(standard.join("lib.wave"), "std input").unwrap();
    let command = |build: &std::path::Path| {
        let mut command = f.command(&plan, &[name]);
        command
            .env("MODERN_CONTRACT", "1")
            .env("VEX_WAVE_STD", &standard)
            .arg("--build-dir")
            .arg(build);
        command
    };
    assert_code(
        &command(&first)
            .args(["fetch", "--offline"])
            .output()
            .unwrap(),
        0,
    );
    let lock = fs::read(f.root.join("vex.lock")).unwrap();
    let build = || {
        let mut c = command(&first);
        c.args(["build", "--locked", "--offline", "--artifact-manifest"])
            .arg(&report)
            .arg("--depfile")
            .arg(&depfile);
        c
    };
    assert_code(&build().output().unwrap(), 0);
    assert!(!f.root.join("target").exists());
    assert!(!f.root.join(".vex").exists());
    assert!(first.join(name).is_file());
    let document: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(document["schema"], "vex.artifacts");
    assert_eq!(
        document["results"][0]["metadata"]["compiler"]["std_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let deps = fs::read_to_string(&depfile).unwrap();
    assert!(deps.contains("src/lib.wave"));
    assert!(deps.contains("vex.lock"));
    assert!(deps.contains("prepared\\ std"));
    let previous = fs::read(&report).unwrap();
    let previous_depfile = fs::read(&depfile).unwrap();
    let mtime = fs::metadata(&report).unwrap().modified().unwrap();
    assert_code(&build().output().unwrap(), 0);
    assert_eq!(fs::metadata(&report).unwrap().modified().unwrap(), mtime);
    fs::write(
        f.root.join("src/lib.wave"),
        "fun main() {} // invalidate the fixture cache\n",
    )
    .unwrap();
    let failed = build()
        .env("ARTIFACT_BYTES", "replacement")
        .env("VEX_TEST_REPORT_FAIL_AFTER", "0")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(fs::read(&report).unwrap(), previous);
    assert_eq!(fs::read(&depfile).unwrap(), previous_depfile);
    assert_code(
        &command(&first)
            .args(["toolchain", "capture"])
            .output()
            .unwrap(),
        0,
    );
    let pinned = fs::read(f.root.join("vex.toolchain.lock")).unwrap();
    fs::write(
        standard.join("lib.wave"),
        "changed std, same compiler version",
    )
    .unwrap();
    let mismatch = build().output().unwrap();
    assert_code(&mismatch, 3);
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("std_sha256"));
    assert_eq!(fs::read(f.root.join("vex.toolchain.lock")).unwrap(), pinned);
    assert_eq!(fs::read(f.root.join("vex.lock")).unwrap(), lock);
    fs::write(standard.join("lib.wave"), "std input").unwrap();
    assert_code(
        &command(&second)
            .args(["fetch", "--locked", "--offline"])
            .output()
            .unwrap(),
        0,
    );
    assert_code(
        &command(&second)
            .args(["build", "--locked", "--offline"])
            .output()
            .unwrap(),
        0,
    );
    assert!(second.join(name).is_file());
    assert_code(&command(&first).arg("clean").output().unwrap(), 0);
    assert!(second.join(name).is_file());
    fs::write(&report, "user file").unwrap();
    let refused = build().output().unwrap();
    assert_code(&refused, 5);
    assert_eq!(fs::read(&report).unwrap(), b"user file");
}

#[test]
fn interrupted_publication_is_readonly_in_dry_run_and_recovers_on_a_normal_build() {
    for existing in [false, true] {
        let f = Fixture::new();
        let name = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
        let plan = plan_object(name);
        if existing {
            assert_code(&f.build(&plan, &[name]), 0);
        }
        let crashed = f
            .command(&plan, &[name])
            .env("ARTIFACT_BYTES", "new complete generation")
            .env("VEX_TEST_PUBLICATION_CRASH_AFTER", "0")
            .arg("build")
            .output()
            .unwrap();
        assert_code(&crashed, 97);
        let journal = fs::read(f.root.join(".vex/build-publication.json")).unwrap();
        let staged = fs::read(f.root.join(name)).unwrap();
        let dry = f
            .command(&plan, &[name])
            .args(["build", "--dry-run"])
            .output()
            .unwrap();
        assert!(!dry.status.success());
        assert_eq!(
            fs::read(f.root.join(".vex/build-publication.json")).unwrap(),
            journal
        );
        assert_eq!(fs::read(f.root.join(name)).unwrap(), staged);
        assert_code(&f.build(&plan, &[name]), 0);
        assert!(!f.root.join(".vex/build-publication.json").exists());
        assert_eq!(fs::read(f.root.join(name)).unwrap(), b"compiled artifact");
    }
}

#[test]
fn interrupted_report_publication_requires_writable_recovery() {
    for existing in [false, true] {
        let f = Fixture::new();
        let name = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
        let mut plan = plan_object(name);
        plan["source_inputs"] = json!(["src/lib.wave"]);
        let report = f.root.join("artifacts.json");
        let depfile = f.root.join("artifacts.d");
        let build = || {
            let mut c = f.command(&plan, &[name]);
            c.env("MODERN_CONTRACT", "1")
                .args(["build", "--artifact-manifest"])
                .arg(&report)
                .arg("--depfile")
                .arg(&depfile);
            c
        };
        if existing {
            assert_code(&build().output().unwrap(), 0);
        }
        fs::write(f.root.join("src/lib.wave"), "fun main() {} // changed\n").unwrap();
        let crashed = build()
            .env("ARTIFACT_BYTES", "replacement")
            .env("VEX_TEST_REPORT_CRASH_AFTER", "0")
            .output()
            .unwrap();
        assert_code(&crashed, 97);
        let journal_path = f.root.join(".vex/report-publication.json");
        let journal = fs::read(&journal_path).unwrap();
        let staged = fs::read(&report).ok();
        let old_depfile = fs::read(&depfile).ok();
        let dry = build().arg("--dry-run").output().unwrap();
        assert!(!dry.status.success());
        assert_eq!(fs::read(&journal_path).unwrap(), journal);
        assert_eq!(fs::read(&report).ok(), staged);
        assert_eq!(fs::read(&depfile).ok(), old_depfile);
        assert_code(&build().output().unwrap(), 0);
        assert!(!journal_path.exists());
        let result: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        assert_eq!(result["schema"], "vex.artifacts");
        assert!(fs::read_to_string(&depfile).unwrap().contains("vex.ws"));
    }
}

#[test]
fn same_version_different_compiler_bytes_fail_before_dependency_mutation() {
    use std::io::Write;
    let f = Fixture::new();
    let name = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
    let plan = plan_object(name);
    let template = f.command(&plan, &[name]);
    let original = template
        .get_envs()
        .find(|(name, _)| *name == "VEX_WAVEC")
        .unwrap()
        .1
        .unwrap();
    let prepared = f.root.join(if cfg!(windows) {
        "prepared.exe"
    } else {
        "prepared"
    });
    fs::copy(original, &prepared).unwrap();
    let command = || {
        let mut c = f.command(&plan, &[name]);
        c.env("VEX_WAVEC", &prepared);
        c
    };
    assert_code(
        &command().args(["toolchain", "capture"]).output().unwrap(),
        0,
    );
    let pin = fs::read(f.root.join("vex.toolchain.lock")).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&prepared)
        .unwrap()
        .write_all(b"same version, different build")
        .unwrap();
    let output = command().args(["build", "--offline"]).output().unwrap();
    assert_code(&output, 3);
    assert!(String::from_utf8_lossy(&output.stderr).contains("compiler_sha256"));
    assert!(!f.root.join("target").exists());
    assert!(!f.root.join("vex.lock").exists());
    assert!(!f.root.join("compiled").exists());
    assert_eq!(fs::read(f.root.join("vex.toolchain.lock")).unwrap(), pin);
}

#[test]
fn absolute_external_outputs_and_private_report_aliases_use_the_same_storage_boundary() {
    let f = Fixture::new();
    let build = f.root.join("external build");
    let name = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
    // On Windows the compiler receives a normal drive path while the selected
    // build root is canonical/verbatim. Both denote this exact owned location.
    let output = state::layout::path_form(&build.join(name));
    let mut plan = plan_object(output.to_str().unwrap());
    plan["source_inputs"] = json!(["src/lib.wave"]);
    let first = f
        .command(&plan, &[name])
        .arg("--build-dir")
        .arg(&build)
        .arg("build")
        .output()
        .unwrap();
    assert_code(&first, 0);
    assert!(output.is_file());
    assert!(!f.root.join(name).exists());
    let before = fs::read(build.join(".vex/build-owner.json")).unwrap();
    let alias = build.join("absent/../.vex/report.json");
    let refused = f
        .command(&plan, &[name])
        .arg("--build-dir")
        .arg(&build)
        .args(["build", "--artifact-manifest"])
        .arg(alias)
        .output()
        .unwrap();
    assert_code(&refused, 2);
    assert_eq!(
        fs::read(build.join(".vex/build-owner.json")).unwrap(),
        before
    );
    assert!(!build.join(".vex/report.json").exists());
    let other = Fixture::new();
    let refused = other
        .command(&plan, &[name])
        .arg("--build-dir")
        .arg(&build)
        .arg("build")
        .output()
        .unwrap();
    assert_code(&refused, 2);
    assert_eq!(
        fs::read(build.join(".vex/build-owner.json")).unwrap(),
        before
    );
}

#[cfg(unix)]
#[test]
fn cancelled_rebuild_preserves_completed_artifact_and_consumer_reports() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let name = "target/x86_64-unknown-linux-gnu/dev/lib/app.o";
    let mut plan = plan_object(name);
    plan["source_inputs"] = json!(["src/lib.wave"]);
    let report = f.root.join("artifacts.json");
    let depfile = f.root.join("artifacts.d");
    let build = || {
        let mut c = f.command(&plan, &[name]);
        c.env("MODERN_CONTRACT", "1")
            .args(["build", "--artifact-manifest"])
            .arg(&report)
            .arg("--depfile")
            .arg(&depfile);
        c
    };
    assert_code(&build().output().unwrap(), 0);
    let before = [
        fs::read(f.root.join(name)).unwrap(),
        fs::read(&report).unwrap(),
        fs::read(&depfile).unwrap(),
    ];
    fs::write(f.root.join("src/lib.wave"), "fun main() {} // changed\n").unwrap();
    let marker = f.root.join("staged-ready");
    let mut child = build()
        .env("PAUSE_AFTER_OUTPUT", &marker)
        .env("ARTIFACT_BYTES", "unpublished replacement")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !marker.exists() {
        if child.try_wait().unwrap().is_some() || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("compiler did not stage replacement outputs");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert_eq!(
        [
            fs::read(f.root.join(name)).unwrap(),
            fs::read(&report).unwrap(),
            fs::read(&depfile).unwrap()
        ],
        before
    );
    assert_code(&build().output().unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn nonregular_build_owner_is_rejected_without_waiting_for_a_fifo_writer() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let build = f.root.join("external-fifo-owner");
    fs::create_dir_all(build.join(".vex")).unwrap();
    assert!(Command::new("mkfifo")
        .arg(build.join(".vex/build-owner.json"))
        .status()
        .unwrap()
        .success());
    let plan = plan_object("target/x86_64-unknown-linux-gnu/dev/lib/app.o");
    let mut child = f
        .command(&plan, &[])
        .arg("--build-dir")
        .arg(build)
        .arg("build")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("opening an invalid storage owner blocked");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!f.root.join("compiled").exists());
}
