use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

const COMPILER_SOURCE: &str = r#"
use std::{env, fs, path::Path, process};
fn main() {
    let args: Vec<_> = env::args().collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("wavec 0.2.1-pre-beta");
        return;
    }
    if args.iter().any(|arg| arg == "--dry-run") {
        println!("{}", env::var("PLAN").unwrap());
        return;
    }
    fs::write("compiled", "compiler started").unwrap();
    for path in env::var("OUTPUTS").unwrap_or_default().lines() {
        let path = Path::new(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if env::var_os("DIRECTORY_OUTPUT").is_some() {
            fs::create_dir(path).unwrap();
        } else {
            fs::write(path, b"compiled artifact").unwrap();
        }
    }
    if env::var_os("FAIL_OWNERSHIP").is_some() {
        fs::create_dir(".vex/artifacts.json").unwrap();
    }
    let code = env::var("COMPILE_EXIT").unwrap_or_else(|_| "0".into()).parse().unwrap();
    if code != 0 { eprintln!("fixture compiler diagnostic"); }
    process::exit(code);
}
"#;

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
        fs::write(root.join("vex.ws"), "{name=\"app\",version=0.1.0}").unwrap();
        fs::write(root.join("src/main.wave"), "fun main() {}\n").unwrap();
        Self { root }
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
            assert!(Command::new("rustc")
                .arg(source)
                .arg("-o")
                .arg(&compiler)
                .status()
                .unwrap()
                .success());
            compiler
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(&self.root)
            .env("VEX_WAVEC", compiler)
            .env("PLAN", plan.to_string())
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
    json!({"schema_version":1,"mode":"build","target":"x86_64-unknown-linux-gnu","emit":"obj","emit_kinds":["obj"],"control_mode":null,"forced_input_type":null,"inputs":[{"path":"src/main.wave","kind":"wave"}],"emit_jobs":[],"compile":outputs.iter().map(|output| json!({"input":"src/main.wave","kind":"wave","output":output,"command":"compiler"})).collect::<Vec<_>>(),"link":null,"execute":null})
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
        let plan = plan(&["target/result.o"]);
        if modified {
            assert_code(&f.build(&plan, &["target/result.o"]), 0);
            fs::remove_file(f.root.join("compiled")).unwrap();
        } else {
            fs::create_dir(f.root.join("target")).unwrap();
        }
        fs::write(f.root.join("target/result.o"), b"user content").unwrap();
        let registry = fs::read(f.root.join(".vex/artifacts.json")).ok();
        assert_code(&f.build(&plan, &["target/result.o"]), 5);
        assert!(!f.root.join("compiled").exists());
        assert_eq!(
            fs::read(f.root.join("target/result.o")).unwrap(),
            b"user content"
        );
        assert_eq!(fs::read(f.root.join(".vex/artifacts.json")).ok(), registry);
        f.clean();
        assert_eq!(
            fs::read(f.root.join("target/result.o")).unwrap(),
            b"user content"
        );
    }
}

#[test]
fn unchanged_owned_outputs_can_be_rebuilt_and_cleaned() {
    let f = Fixture::new();
    let plan = plan(&["target/result.o"]);
    for _ in 0..2 {
        assert_code(&f.build(&plan, &["target/result.o"]), 0);
    }
    f.clean();
    assert!(!f.root.join("target/result.o").exists());
}

#[test]
fn missing_or_nonregular_outputs_do_not_publish_artifact_success() {
    for directory in [false, true] {
        let f = Fixture::new();
        let plan = plan(&["target/result.o", "target/partial.o"]);
        let mut command = f.command(&plan, &["target/partial.o"]);
        if directory {
            command.env("DIRECTORY_OUTPUT", "1");
        }
        let output = command
            .args(["--message-file", "messages.jsonl", "build"])
            .output()
            .unwrap();
        assert_code(&output, 4);
        let events = f.events();
        assert!(!events.iter().any(|event| event["event"] == "artifact"));
        assert_eq!(events.last().unwrap()["category"], "compiler");
        if !directory {
            f.clean();
            assert!(!f.root.join("target/partial.o").exists());
        }
    }
}

#[test]
fn ownership_failure_preserves_primary_compiler_error_and_structured_context() {
    for (compiler_code, expected) in [(1, 4), (3, 5), (0, 5)] {
        let f = Fixture::new();
        let output = f
            .command(&plan(&["target/result.o"]), &["target/result.o"])
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
    let mut plan = plan(&["target/result.o"]);
    assert_code(&f.build(&plan, &["target/result.o"]), 0);
    fs::remove_file(f.root.join("compiled")).unwrap();
    plan["inputs"][0]["path"] = json!("target/result.o");
    assert_code(&f.build(&plan, &["target/result.o"]), 4);
    assert!(!f.root.join("compiled").exists());
}

#[test]
fn hard_links_to_owned_outputs_are_not_overwritten() {
    let f = Fixture::new();
    let plan = plan(&["target/result.o"]);
    assert_code(&f.build(&plan, &["target/result.o"]), 0);
    fs::remove_file(f.root.join("compiled")).unwrap();
    fs::hard_link(f.root.join("target/result.o"), f.root.join("preserve.o")).unwrap();
    assert_code(&f.build(&plan, &["target/result.o"]), 5);
    assert!(!f.root.join("compiled").exists());
    assert_eq!(
        fs::read(f.root.join("preserve.o")).unwrap(),
        b"compiled artifact"
    );
}

fn msvc_plan() -> Value {
    let mut plan = plan(&["target/native/input.o"]);
    plan["target"] = json!("x86_64-pc-windows-msvc");
    plan["emit"] = json!("bin");
    plan["link"] = json!({"output":"target/native/answer.dll","inputs":["target/native/input.o"],"program":"lld-link","args":["/OUT:target/native/answer.dll","/DLL"]});
    plan
}

#[test]
fn optional_msvc_companions_are_reported_and_cleaned_only_when_produced() {
    let f = Fixture::new();
    let outputs = [
        "target/native/input.o",
        "target/native/answer.dll",
        "target/native/answer.lib",
        "target/native/answer.exp",
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
        "answer.lib",
        "answer.exp",
        "answer.pdb",
        "answer.ilk",
        "answer.map",
        "answer.dll.manifest",
    ] {
        let f = Fixture::new();
        fs::create_dir_all(f.root.join("target/native")).unwrap();
        let path = f.root.join("target/native").join(sidecar);
        fs::write(&path, "user content").unwrap();
        assert_code(&f.build(&msvc_plan(), &[]), 5);
        assert!(!f.root.join("compiled").exists());
        assert_eq!(fs::read(path).unwrap(), b"user content");
    }
    let f = Fixture::new();
    let mut plan = msvc_plan();
    plan["link"]["args"]
        .as_array_mut()
        .unwrap()
        .push(json!("/PDB:target/native/input.o"));
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
    fs::create_dir(f.root.join("target")).unwrap();
    symlink(f.root.join("src/main.wave"), f.root.join("target/result.o")).unwrap();
    assert_code(&f.build(&plan(&["target/result.o"]), &[]), 4);
    assert!(!f.root.join("compiled").exists());
    assert_eq!(
        fs::read(f.root.join("src/main.wave")).unwrap(),
        b"fun main() {}\n"
    );
}
