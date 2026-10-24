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

// An independent process boundary: this fixture accepts compiler arguments and
// reports their actual outputs; it does not depend on Vex's native model.
const COMPILER_SOURCE: &str = include_str!("fixtures/native_wavec.rs");

struct Fixture {
    root: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new(lib: bool, native: &str) -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vex native build {} {}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        let project = root.join("app");
        let fixture = Self { root, project };
        fixture.package("app", lib, native, "");
        fs::write(fixture.project.join("vex.lock"), "{version=3,package=[]}\n").unwrap();
        fixture
    }
    fn package(&self, name: &str, lib: bool, native: &str, dependencies: &str) -> PathBuf {
        let root = self.root.join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("vex.ws"),
            format!("{{format=2,name=\"{name}\",lib={lib},dependencies=[{dependencies}]{native}}}"),
        )
        .unwrap();
        fs::write(
            root.join(if lib { "src/lib.wave" } else { "src/main.wave" }),
            "fun main() {}\n",
        )
        .unwrap();
        root
    }
    fn command(&self) -> Command {
        static COMPILER: OnceLock<PathBuf> = OnceLock::new();
        let compiler = COMPILER.get_or_init(|| {
            let root =
                std::env::temp_dir().join(format!("vex-native-compiler-{}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            let source = root.join("fixture.rs");
            fs::write(&source, COMPILER_SOURCE).unwrap();
            let compiler = root.join(if cfg!(windows) {
                "fake-native-wavec.exe"
            } else {
                "fake-native-wavec"
            });
            support::compile_fixture("native_wavec", &compiler);
            compiler
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(&self.project)
            .env("VEX_WAVEC", compiler)
            .env("NATIVE_COMPILER_LOG", self.root.join("compiler.jsonl"))
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
    fn build_arguments(&self) -> Vec<String> {
        self.invocations()
            .into_iter()
            .rev()
            .find(|args| args.iter().any(|arg| arg == "build"))
            .expect("compiler build invocation")
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
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    if value.get("plans").is_some() {
        assert_eq!(value["plans"].as_array().unwrap().len(), 1);
        value["plans"][0]["compiler_plan"].clone()
    } else {
        value
    }
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if metadata.is_dir() {
                result.insert(relative, Vec::new());
                visit(root, &path, result);
            } else if metadata.is_file() {
                result.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}
fn assert_path(actual: &str, expected: &Path) {
    assert_eq!(
        Path::new(actual).canonicalize().unwrap(),
        expected.canonicalize().unwrap()
    );
}
fn flag_value<'a>(args: &'a [String], flag: &str) -> &'a str {
    let index = args.iter().position(|arg| arg == flag).unwrap();
    &args[index + 1]
}

fn link_arguments(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix("-Clink-arg="))
        .collect()
}

#[test]
fn dry_run_selects_package_target_and_profile_outputs_without_mutation() {
    for (lib, native, emit, filename) in [
        (false, ",native={}", "bin", "app"),
        (true, "", "obj", "app.o"),
        (true, ",native={output=\"object\"}", "obj", "app.o"),
        (true, ",native={output=\"shared\"}", "bin", "libapp.so"),
    ] {
        let f = Fixture::new(lib, native);
        code(&f.run(&["fetch"]), 0);
        let before = snapshot(&f.project);
        let value = plan(&f.run(&["build", "--release", "--dry-run"]));
        assert_eq!(value["target"], "x86_64-unknown-linux-gnu");
        assert_eq!(value["emit"], emit);
        let output = if emit == "obj" {
            &value["compile"][0]["output"]
        } else {
            &value["link"]["output"]
        };
        assert_eq!(
            output,
            &json!(format!(
                "target/x86_64-unknown-linux-gnu/release/{}/{filename}",
                if lib { "lib" } else { "bin" }
            ))
        );
        assert_eq!(snapshot(&f.project), before);
        assert!(!f.project.join("compiler-started").exists());
        assert_eq!(
            f.invocations()
                .iter()
                .filter(|args| args.iter().any(|arg| arg == "target-spec"))
                .count(),
            1
        );
    }
}

#[test]
fn explicit_targets_choose_windows_and_macos_library_names() {
    for (target, native, filename) in [
        ("aarch64-pc-windows-msvc", "object", "app.obj"),
        ("x86_64-pc-windows-msvc", "shared", "app.dll"),
        ("aarch64-apple-darwin", "shared", "libapp.dylib"),
    ] {
        let exports = if native == "shared" {
            ",exports=[\"wave_add\"]"
        } else {
            ""
        };
        let f = Fixture::new(true, &format!(",native={{output=\"{native}\"{exports}}}"));
        let value = plan(&f.run(&["build", "--target", target, "--dry-run"]));
        let output = if native == "object" {
            &value["compile"][0]["output"]
        } else {
            &value["link"]["output"]
        };
        assert_eq!(
            output,
            &json!(format!("target/{target}/dev/lib/{filename}"))
        );
        assert_eq!(value["target"], target);
        assert!(!f.project.join("target").exists());
    }
}

#[test]
fn windows_shared_requires_explicit_exports_and_keeps_them_in_metadata() {
    for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
        let f = Fixture::new(true, ",native={output=\"shared\"}");
        code(&f.run(&["fetch", "--offline"]), 0);
        let before = snapshot(&f.project);
        let result = f.run(&["build", "--target", target, "--dry-run"]);
        code(&result, 3);
        assert!(String::from_utf8_lossy(&result.stderr).contains("native.exports"));
        assert_eq!(snapshot(&f.project), before);
        assert!(!f.project.join("compiler-started").exists());

        f.package(
            "app",
            true,
            ",native={output=\"shared\",exports=[\"wave_add\",\"_other2\"]}",
            "",
        );
        let value = plan(&f.run(&["build", "--target", target, "--dry-run"]));
        let args = value["link"]["args"].as_array().unwrap();
        assert!(args.contains(&json!("/EXPORT:wave_add")));
        assert!(args.contains(&json!("/EXPORT:_other2")));
        let value = plan(&f.run(&["metadata", "--offline"]));
        assert_eq!(
            value["root"]["native"]["exports"],
            json!(["wave_add", "_other2"])
        );
    }
}

#[test]
fn root_and_transitive_inputs_use_the_declaring_package_and_preserve_link_order() {
    let f = Fixture::new(false, "");
    let leaf = f.package(
        "leaf",
        true,
        ",native={libraries=[{path=\"native/leaf.a\"}],search_paths=[{path=\"native\"}]}",
        "",
    );
    let middle = f.package(
        "middle",
        true,
        ",native={libraries=[{path=\"native/middle.a\"}]}",
        "{name=\"leaf\",path=\"../leaf\"}",
    );
    f.package("app", false, ",native={libraries=[{path=\"native/root.o\"},{name=\"system_math\"}],search_paths=[{path=\"native\"}]}", "{name=\"middle\",path=\"../middle\"}");
    for (root, filename) in [
        (&f.project, "root.o"),
        (&middle, "middle.a"),
        (&leaf, "leaf.a"),
    ] {
        fs::create_dir(root.join("native")).unwrap();
        fs::write(root.join("native").join(filename), "prebuilt input").unwrap();
    }
    code(&f.run(&["fetch"]), 0);
    let lock = fs::read(f.project.join("vex.lock")).unwrap();
    code(&f.run(&["build", "--locked", "--offline"]), 0);
    let args = f.build_arguments();
    let inputs = link_arguments(&args);
    assert_eq!(inputs.len(), 4);
    assert_eq!(inputs[1], "-lsystem_math");
    for (actual, expected) in [inputs[0], inputs[2], inputs[3]].iter().zip([
        f.project.join("native/root.o"),
        middle.join("native/middle.a"),
        leaf.join("native/leaf.a"),
    ]) {
        assert_path(actual, &expected);
    }
    assert!(!args.iter().any(|arg| arg == "-L" || arg == "--link"));
    assert_eq!(fs::read(f.project.join("vex.lock")).unwrap(), lock);
    assert!(f
        .project
        .join("target/x86_64-unknown-linux-gnu/dev/bin/app")
        .is_file());
}

#[test]
fn mixed_named_and_path_archives_preserve_manifest_order() {
    let f = Fixture::new(false, ",native={libraries=[{name=\"first\"},{path=\"native/second.a\"},{name=\"last\"}],search_paths=[{path=\"native\"}]}");
    fs::create_dir(f.project.join("native")).unwrap();
    for name in ["libfirst.a", "second.a", "liblast.a"] {
        fs::write(f.project.join("native").join(name), name).unwrap();
    }
    plan(&f.run(&["build", "--dry-run"]));
    let args = f.build_arguments();
    let inputs = link_arguments(&args);
    assert_eq!(inputs.len(), 3, "{args:?}");
    for (input, name) in inputs.iter().zip(["libfirst.a", "second.a", "liblast.a"]) {
        assert_path(input, &f.project.join("native").join(name));
    }
    assert!(!args.iter().any(|arg| arg == "-L" || arg == "--link"));
}

#[test]
fn same_library_name_is_resolved_independently_for_each_declaring_package() {
    let f = Fixture::new(false, "");
    let native = ",native={libraries=[{name=\"private_impl\"}],search_paths=[{path=\"native\"}]}";
    let leaf = f.package("leaf", true, native, "");
    f.package("app", false, native, "{name=\"leaf\",path=\"../leaf\"}");
    for (root, contents) in [
        (&f.project, "root implementation"),
        (&leaf, "leaf implementation"),
    ] {
        fs::create_dir(root.join("native")).unwrap();
        fs::write(root.join("native/libprivate_impl.a"), contents).unwrap();
    }
    code(&f.run(&["fetch"]), 0);
    plan(&f.run(&["build", "--locked", "--offline", "--dry-run"]));
    let args = f.build_arguments();
    let inputs = link_arguments(&args);
    assert_eq!(inputs.len(), 2, "{args:?}");
    assert_path(inputs[0], &f.project.join("native/libprivate_impl.a"));
    assert_path(inputs[1], &leaf.join("native/libprivate_impl.a"));
    assert!(!args.iter().any(|arg| arg == "-L" || arg == "--link"));
}

#[test]
fn native_named_library_uses_target_specific_shared_then_static_candidates() {
    for (target, names, selected) in [
        (
            "x86_64-unknown-linux-gnu",
            ["libchoice.so", "libchoice.a"],
            "libchoice.so",
        ),
        (
            "aarch64-apple-darwin",
            ["libchoice.dylib", "libchoice.a"],
            "libchoice.dylib",
        ),
        (
            "x86_64-pc-windows-msvc",
            ["choice.lib", "libchoice.a"],
            "choice.lib",
        ),
    ] {
        let f = Fixture::new(
            false,
            ",native={libraries=[{name=\"choice\"}],search_paths=[{path=\"native\"}]}",
        );
        fs::create_dir(f.project.join("native")).unwrap();
        for name in names {
            fs::write(f.project.join("native").join(name), name).unwrap();
        }
        plan(&f.run(&["build", "--target", target, "--dry-run"]));
        let args = f.build_arguments();
        let inputs = link_arguments(&args);
        assert_eq!(inputs.len(), 1, "{args:?}");
        assert_path(inputs[0], &f.project.join("native").join(selected));
    }
}

#[test]
fn prebuilt_native_input_that_aliases_a_compile_output_is_preserved() {
    let relative = "target/x86_64-unknown-linux-gnu/dev/.vex-obj/bin/app/binary/native.o";
    let f = Fixture::new(
        false,
        &format!(",native={{libraries=[{{path=\"{relative}\"}}]}}"),
    );
    let input = f.project.join(relative);
    fs::create_dir_all(input.parent().unwrap()).unwrap();
    fs::write(&input, "prebuilt native input").unwrap();
    let output = f.run(&["build"]);
    code(&output, 4);
    assert_eq!(fs::read(&input).unwrap(), b"prebuilt native input");
    assert!(!f.project.join("compiler-started").exists());
    assert!(!f
        .project
        .join("target/x86_64-unknown-linux-gnu/dev/bin/app")
        .exists());
}

#[test]
fn valid_package_names_that_are_windows_devices_cannot_be_native_outputs() {
    for (name, lib, native) in [
        ("CON", false, "{}"),
        ("NUL", true, "{output=\"object\"}"),
        ("LPT9", true, "{output=\"shared\"}"),
    ] {
        let f = Fixture::new(lib, &format!(",native={native}"));
        fs::write(
            f.project.join("vex.ws"),
            format!("{{name=\"{name}\",lib={lib},native={native}}}"),
        )
        .unwrap();
        code(&f.run(&["info"]), 0);
        let output = f.run(&["build", "--target=x86_64-pc-windows-msvc", "--dry-run"]);
        code(&output, 3);
        assert!(String::from_utf8_lossy(&output.stderr).contains("Windows"));
        assert!(!f
            .invocations()
            .iter()
            .any(|args| args.iter().any(|arg| arg == "build")));
        assert!(!f.project.join("compiler-started").exists());
        assert!(!f.project.join("target").exists());
    }
}

#[test]
fn metadata_reports_native_declarations_without_loading_compiler_or_inputs() {
    let f = Fixture::new(false, ",native={libraries=[{name=\"rootlib\"}]}");
    f.package("dep", true, ",native={output=\"shared\",libraries=[{path=\"missing.a\",target=\"aarch64-apple-darwin\"}],search_paths=[{path=\"missing\",target=\"aarch64-apple-darwin\"}]}", "");
    f.package(
        "app",
        false,
        ",native={libraries=[{name=\"rootlib\"}]}",
        "{name=\"dep\",path=\"../dep\"}",
    );
    code(&f.run(&["fetch"]), 0);
    let before = snapshot(&f.project);
    let output = f
        .command()
        .env("VEX_WAVEC", f.root.join("not-a-compiler"))
        .args(["metadata", "--locked", "--offline"])
        .output()
        .unwrap();
    let value = plan(&output);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["root"]["native"]["libraries"][0]["name"], "rootlib");
    assert_eq!(
        value["packages"][0]["native"],
        json!({"output":"shared","exports":[],"libraries":[{"path":"missing.a","name":null,"target":"aarch64-apple-darwin"}],"search_paths":[{"path":"missing","target":"aarch64-apple-darwin"}]})
    );
    assert_eq!(snapshot(&f.project), before);
    assert!(f.invocations().is_empty());
}

#[test]
fn unselected_target_files_are_ignored_but_selected_missing_inputs_fail() {
    let f = Fixture::new(false, ",native={libraries=[{path=\"missing.a\",target=\"aarch64-apple-darwin\"}],search_paths=[{path=\"missing\",target=\"aarch64-apple-darwin\"}]}");
    plan(&f.run(&["build", "--dry-run", "--target=x86_64-unknown-linux-gnu"]));
    let output = f.run(&["build", "--dry-run", "--target=aarch64-apple-darwin"]);
    code(&output, 3);
    assert!(String::from_utf8_lossy(&output.stderr).contains("native input"));
    assert!(!f.project.join("compiler-started").exists());
    assert!(!f.project.join("target").exists());
}

#[test]
fn native_inputs_must_have_the_selected_file_or_directory_kind() {
    for native in [
        ",native={libraries=[{path=\"directory.a\"}]}",
        ",native={search_paths=[{path=\"file\"}]}",
    ] {
        let f = Fixture::new(false, native);
        fs::create_dir(f.project.join("directory.a")).unwrap();
        fs::write(f.project.join("file"), "not a directory").unwrap();
        let output = f.run(&["build", "--dry-run"]);
        code(&output, 3);
        assert!(String::from_utf8_lossy(&output.stderr).contains("inside package"));
        assert!(!f.project.join("compiler-started").exists());
    }
}

#[cfg(unix)]
#[test]
fn native_input_symlinks_cannot_escape_the_declaring_package() {
    for search in [false, true] {
        let native = if search {
            ",native={search_paths=[{path=\"escape\"}]}"
        } else {
            ",native={libraries=[{path=\"escape.a\"}]}"
        };
        let f = Fixture::new(false, native);
        let outside = f.root.join("outside");
        if search {
            fs::create_dir(&outside).unwrap();
        } else {
            fs::write(&outside, "outside input").unwrap();
        }
        std::os::unix::fs::symlink(
            &outside,
            f.project.join(if search { "escape" } else { "escape.a" }),
        )
        .unwrap();
        let output = f.run(&["build", "--dry-run"]);
        code(&output, 3);
        assert!(String::from_utf8_lossy(&output.stderr).contains("inside package"));
        assert!(!f.project.join("compiler-started").exists());
    }
}

#[test]
fn check_validates_manifest_inputs_without_linking_or_emitting_native_artifacts() {
    let f = Fixture::new(true, ",native={output=\"shared\",libraries=[{path=\"native.a\"},{name=\"math\"}],search_paths=[{path=\"libs\"}]}");
    fs::write(f.project.join("native.a"), "archive").unwrap();
    fs::create_dir(f.project.join("libs")).unwrap();
    code(&f.run(&["fetch"]), 0);
    let before = snapshot(&f.project);
    let value = plan(&f.run(&["check", "--dry-run"]));
    assert_eq!(value["emit"], "check");
    assert_eq!(value["compile"], json!([]));
    assert!(value["link"].is_null());
    let args = f.build_arguments();
    for flag in ["-o", "-L", "--link", "--shared", "--emit=obj"] {
        assert!(!args.iter().any(|arg| arg == flag), "{args:?}");
    }
    assert!(!args
        .iter()
        .any(|arg| arg.ends_with("native.a") || arg.starts_with("--target-dir=")));
    assert!(link_arguments(&args).is_empty());
    assert_eq!(snapshot(&f.project), before);
    code(&f.run(&["check"]), 0);
    assert!(f.project.join("compiler-started").exists());
    assert!(!f
        .project
        .join("target/x86_64-unknown-linux-gnu/dev/lib/libapp.so")
        .exists());
}

#[test]
fn invalid_target_spec_stops_before_a_compile_plan_or_execution() {
    for response in [
        "not json",
        "{}",
        "[]",
        r#"{"triple":"../escape","supported":true,"os":"linux","object_format":"elf"}"#,
        r#"{"triple":"aarch64-unknown-linux-gnu","supported":true,"os":"linux","object_format":"elf"}"#,
        r#"{"triple":"x86_64-unknown-linux-gnu","supported":false,"os":"linux","object_format":"elf"}"#,
    ] {
        let f = Fixture::new(false, ",native={}");
        code(&f.run(&["fetch"]), 0);
        let before = snapshot(&f.project);
        let output = f
            .command()
            .env("NATIVE_BAD_SPEC", response)
            .args(["build", "--dry-run", "--target=x86_64-unknown-linux-gnu"])
            .output()
            .unwrap();
        code(&output, 4);
        assert!(String::from_utf8_lossy(&output.stderr).contains("target-spec"));
        assert!(!f
            .invocations()
            .iter()
            .any(|args| args.iter().any(|arg| arg == "build")));
        assert_eq!(snapshot(&f.project), before);
    }
}

#[test]
fn native_configuration_does_not_enable_raw_compiler_cli_options() {
    let f = Fixture::new(false, ",native={}");
    for option in [
        "--link=foreign",
        "-Lvendor",
        "--shared",
        "--emit=obj",
        "-o",
        "foreign.a",
    ] {
        code(&f.run(&["build", option]), 2);
    }
    assert!(f.invocations().is_empty());
    assert!(!f.project.join(".vex").exists());
}

#[test]
fn native_run_uses_an_independent_generation_and_preserves_program_exit_and_arguments() {
    let f = Fixture::new(false, ",native={}");
    let output = f
        .command()
        .env(
            "NATIVE_DEFAULT_TARGET",
            if cfg!(windows) {
                "x86_64-pc-windows-msvc"
            } else {
                "x86_64-unknown-linux-gnu"
            },
        )
        .args(["run", "--", "with spaces", "--program-flag"])
        .output()
        .unwrap();
    code(&output, 17);
    assert!(String::from_utf8_lossy(&output.stdout).contains("native fixture program"));
    let arguments: Value =
        serde_json::from_slice(&fs::read(f.project.join("program-started")).unwrap()).unwrap();
    assert_eq!(arguments, json!(["with spaces", "--program-flag"]));
    let args = f.build_arguments();
    assert!(!args.iter().any(|arg| arg == "--run"));
    let output = Path::new(flag_value(&args, "-o"));
    assert!(output.starts_with("target/.vex-run"));
    assert!(f.project.join(output).is_file());
    assert!(!f
        .project
        .join("target/x86_64-unknown-linux-gnu/dev/bin/app")
        .exists());
}

#[test]
fn format_three_shared_library_and_same_named_binary_have_disjoint_windows_outputs() {
    let f = Fixture::new(false, "");
    fs::write(
        f.project.join("src/lib.wave"),
        "export(c) fun wave_add() -> i32 { return 1; }\n",
    )
    .unwrap();
    fs::write(f.project.join("vex.ws"), r#"{format=3,name="app",targets={lib={output="shared",exports=["wave_add"]},bins=[{name="app",path="src/main.wave"}]},default_run="app"}"#).unwrap();
    let result = f.run(&["build", "--target=x86_64-pc-windows-msvc", "--dry-run"]);
    code(&result, 0);
    let envelope: Value = serde_json::from_slice(&result.stdout).unwrap();
    let plans = envelope["plans"].as_array().unwrap();
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[0]["package_target"]["kind"], "library");
    assert_eq!(plans[1]["package_target"]["kind"], "binary");
    assert_eq!(
        plans[0]["compiler_plan"]["link"]["output"],
        "target/x86_64-pc-windows-msvc/dev/lib/app.dll"
    );
    assert_eq!(
        plans[1]["compiler_plan"]["link"]["output"],
        "target/x86_64-pc-windows-msvc/dev/bin/app.exe"
    );
    assert_eq!(
        plans[0]["compiler_plan"]["link"]["args"],
        json!(["/EXPORT:wave_add"])
    );
    assert_eq!(plans[1]["compiler_plan"]["link"]["args"], json!([]));
    assert_ne!(
        plans[0]["compiler_plan"]["compile"][0]["output"],
        plans[1]["compiler_plan"]["compile"][0]["output"]
    );
    let invocations = f.invocations();
    let binary = invocations
        .iter()
        .rev()
        .find(|args| args.iter().any(|arg| arg == "src/main.wave"))
        .unwrap();
    assert!(binary.iter().any(|arg| arg.starts_with("--dep=app=")));
    assert!(!f.project.join("target").exists());
}

#[test]
fn format_three_mixed_dependencies_link_only_declared_native_inputs_in_order() {
    let f = Fixture::new(false, "");
    let leaf = f.package("leaf", true, ",native={libraries=[{path=\"leaf.a\"}]}", "");
    let middle = f.package("middle", true, "", "{name=\"leaf\",path=\"../leaf\"}");
    fs::write(middle.join("vex.ws"), r#"{format=3,name="middle",targets={lib={},bins=[{name="tool",path="src/tool.wave"}]},native={libraries=[{path="middle.a"}]},dependencies=[{name="leaf",path="../leaf"}]}"#).unwrap();
    fs::write(
        f.project.join("src/lib.wave"),
        "pub fun answer() -> i32 { return 42; }\n",
    )
    .unwrap();
    fs::write(f.project.join("vex.ws"), r#"{format=3,name="app",targets={lib={},bins=[{name="app",path="src/main.wave"}]},native={libraries=[{path="root.a"}]},dependencies=[{name="middle",path="../middle"}]}"#).unwrap();
    for (root, name) in [
        (&f.project, "root.a"),
        (&middle, "middle.a"),
        (&leaf, "leaf.a"),
    ] {
        fs::write(root.join(name), "prebuilt archive").unwrap();
    }
    code(&f.run(&["fetch", "--offline"]), 0);
    let result = f.run(&[
        "build",
        "--bin",
        "app",
        "--locked",
        "--offline",
        "--dry-run",
    ]);
    code(&result, 0);
    let args = f.build_arguments();
    let inputs = link_arguments(&args);
    assert_eq!(inputs.len(), 3);
    for (actual, expected) in inputs.iter().zip([
        f.project.join("root.a"),
        middle.join("middle.a"),
        leaf.join("leaf.a"),
    ]) {
        assert_path(actual, &expected);
    }
    assert!(args.iter().any(|arg| arg.starts_with("--dep=app=")));
    assert!(!args.iter().any(|arg| arg.contains("src/tool.wave")));
    assert!(!args
        .iter()
        .any(|arg| arg.ends_with("lib/app.o") || arg.ends_with("lib/middle.o")));
    assert!(!f.project.join("compiler-started").exists());
}
