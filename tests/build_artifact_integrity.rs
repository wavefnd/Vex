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
        Self { root }
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
    assert!(f.root.join(intermediate).is_file());
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
    assert!(f.root.join(path).is_dir());
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
