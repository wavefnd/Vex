// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("vex-cli-contract-{}-{id}", std::process::id()));
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
fn help_is_read_only_and_succeeds_without_a_manifest() {
    let fixture = TestDir::new();
    for arguments in [
        &["init", "--help"][..],
        &["build", "--help"],
        &["run", "--help"],
        &["check", "--help"],
        &["fetch", "--help"],
        &["update", "--help"],
        &["info", "--help"],
        &["tree", "--help"],
        &["metadata", "--help"],
        &["setup", "--help"],
        &["setup", "wavec", "--help"],
    ] {
        let output = vex(&fixture.0, arguments);
        assert_success(&output, &format!("vex {}", arguments.join(" ")));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("usage:"),
            "help output did not contain usage: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert!(!fixture.0.join("vex.ws").exists());
    assert!(!fixture.0.join("src").exists());
}

#[test]
fn invalid_init_and_info_fail_without_mutating_the_directory() {
    let fixture = TestDir::new();
    let invalid_init = vex(&fixture.0, &["init", "--unknown"]);
    assert_eq!(invalid_init.status.code(), Some(2));
    assert!(!fixture.0.join("vex.ws").exists());
    assert!(!fixture.0.join("src").exists());

    let missing_info = vex(&fixture.0, &["info"]);
    assert_eq!(missing_info.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&missing_info.stderr).contains("could not find `vex.ws`"));

    let invalid_info = vex(&fixture.0, &["info", "extra"]);
    assert_eq!(invalid_info.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_info.stderr).contains("unexpected argument"));

    let invalid_setup = vex(&fixture.0, &["setup", "wavec", "--version", "--unknown"]);
    assert_eq!(invalid_setup.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_setup.stderr).contains("version value"));
}

#[test]
fn init_scaffolds_public_library_and_ignores_generated_state() {
    let fixture = TestDir::new();
    let library = fixture.0.join("library");
    let app = fixture.0.join("app");
    fs::create_dir_all(&library).unwrap();
    fs::create_dir_all(&app).unwrap();

    assert_success(&vex(&library, &["init", "--lib"]), "initialize library");
    assert_eq!(
        fs::read_to_string(library.join("src/lib.wave")).unwrap(),
        "pub fun greet() {\n    println(\"Hello from library\");\n}\n"
    );
    assert_eq!(
        fs::read_to_string(library.join(".gitignore")).unwrap(),
        "/target/\n/.vex/\n"
    );
    assert!(!library.join(".vex/deps").exists());

    assert_success(&vex(&app, &["init"]), "initialize binary");
    assert_eq!(
        fs::read_to_string(app.join("src/main.wave")).unwrap(),
        "fun main() {\n    println(\"Hello World\");\n}\n"
    );
    assert_eq!(
        fs::read_to_string(app.join(".gitignore")).unwrap(),
        "/target/\n/.vex/\n"
    );
    assert!(!app.join(".vex/deps").exists());

    fs::write(
        app.join("vex.ws"),
        "{ name = \"app\", version = 0.1.0, dependencies = [{ name = \"library\", path = \"../library\" }] }\n",
    )
    .unwrap();
    assert_success(&vex(&app, &["fetch"]), "resolve generated path library");
    assert!(!app.join(".vex/deps").exists());
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "reuse path dependency without managed Git state",
    );
    assert!(!app.join(".vex/deps").exists());
}

#[test]
fn init_preserves_an_existing_gitignore() {
    let fixture = TestDir::new();
    for (name, args) in [("binary", &[][..]), ("library", &["--lib"][..])] {
        let project = fixture.0.join(name);
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(".gitignore"), "custom-rule\n").unwrap();
        assert_success(
            &vex(&project, &[&["init"][..], args].concat()),
            "initialize project",
        );
        assert_eq!(
            fs::read_to_string(project.join(".gitignore")).unwrap(),
            "custom-rule\n"
        );
    }
}

#[test]
fn manifest_optional_metadata_errors_name_the_field_and_file() {
    let fixture = TestDir::new();
    for field in ["description", "author", "license"] {
        fs::write(
            fixture.0.join("vex.ws"),
            format!("{{ name = \"app\", version = 0.1.0, {field} = 123, dependencies = [] }}\n"),
        )
        .unwrap();
        let output = vex(&fixture.0, &["info"]);
        assert_eq!(output.status.code(), Some(3));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("manifest `vex.ws`"), "{stderr}");
        assert!(
            stderr.contains(&format!("manifest field `{field}` must be a string")),
            "{stderr}"
        );
    }
}

#[test]
fn strict_manifest_errors_precede_project_mutation() {
    let fixture = TestDir::new();
    let cases = [
        (
            "{ name = \"app\", dependecies = [] }\n",
            "unknown field `dependecies`",
        ),
        (
            "{ name = \"app\", dependencies = [{ name = \"dep\", path = \"../dep\", pth = \"../dep\" }] }\n",
            "dependency `dep` contains unknown field `pth`",
        ),
        (
            "{ name = \"app\", dependencies = [{ name = \"dep\", path = \"../dep\", branch = \"main\" }] }\n",
            "field `branch` applies only to Git dependencies",
        ),
        (
            "{ name = \"app\", dependencies = [{ name = \"dep\", path = \" \" }] }\n",
            "field `path` must not be empty or whitespace-only",
        ),
        (
            "{ name = \"app\", dependencies = [{ name = \"dep\", git = \"\" }] }\n",
            "field `git` must not be empty or whitespace-only",
        ),
        (
            "{ name = \"app\", dependencies = [{ name = \"dep\", git = \"https://example.invalid/dep.git\", branch = \" \" }] }\n",
            "field `branch` must not be empty or whitespace-only",
        ),
        (
            "{ name = \"app\", dependencies = [{ name = \"dep\", git = \"https://example.invalid/dep.git\", tag = \" \" }] }\n",
            "field `tag` must not be empty or whitespace-only",
        ),
        (
            "{ name = \"app\", dependencies = [{ name = \"dep\", git = \"https://example.invalid/dep.git\", rev = \" \" }] }\n",
            "field `rev` must not be empty or whitespace-only",
        ),
    ];

    for (manifest, expected) in cases {
        fs::write(fixture.0.join("vex.ws"), manifest).unwrap();
        let output = vex(&fixture.0, &["fetch"]);
        assert_eq!(output.status.code(), Some(3));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("manifest `vex.ws`"), "{stderr}");
        assert!(stderr.contains(expected), "{stderr}");
        assert!(!fixture.0.join(".vex").exists());
        assert!(!fixture.0.join("vex.lock").exists());
        assert!(!fixture.0.join("target").exists());
    }
}

#[test]
fn duplicate_dependencies_are_invalid_for_every_project_command() {
    let fixture = TestDir::new();
    fs::write(
        fixture.0.join("vex.ws"),
        "{ name = \"app\", dependencies = [{ name = \"alpha\", path = \"../alpha\" }, { name = \"beta\", path = \"../beta\" }, { name = \"alpha\", path = \"../other-alpha\" }] }\n",
    )
    .unwrap();

    for command in ["info", "tree", "fetch", "update", "build", "run", "check"] {
        let output = vex(&fixture.0, &[command]);
        assert_eq!(output.status.code(), Some(3), "{command}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("manifest `vex.ws`"), "{stderr}");
        assert!(
            stderr.contains("dependency `alpha` is declared more than once"),
            "{stderr}"
        );
        assert!(!fixture.0.join(".vex").exists());
        assert!(!fixture.0.join("vex.lock").exists());
        assert!(!fixture.0.join("target").exists());
    }
}

#[test]
fn transitive_path_dependency_cannot_reuse_the_root_name() {
    let fixture = TestDir::new();
    let app = fixture.0.join("app");
    let middle = fixture.0.join("middle");
    let shadow = fixture.0.join("shadow-app");
    for package in [&app, &middle, &shadow] {
        fs::create_dir_all(package.join("src")).unwrap();
    }
    fs::write(
        app.join("vex.ws"),
        "{ name = \"app\", dependencies = [{ name = \"middle\", path = \"../middle\" }] }\n",
    )
    .unwrap();
    fs::write(
        middle.join("vex.ws"),
        "{ name = \"middle\", lib = true, dependencies = [{ name = \"app\", path = \"../shadow-app\" }] }\n",
    )
    .unwrap();
    fs::write(
        shadow.join("vex.ws"),
        "{ name = \"app\", lib = true, dependencies = [] }\n",
    )
    .unwrap();
    fs::write(middle.join("src/lib.wave"), "pub fun middle() {}\n").unwrap();
    fs::write(shadow.join("src/lib.wave"), "pub fun shadow() {}\n").unwrap();

    let output = vex(&app, &["fetch"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reuses root package name `app`"),
        "{stderr}"
    );
    let root_manifest = fs::canonicalize(app.join("vex.ws")).unwrap();
    assert!(
        stderr.contains(&root_manifest.display().to_string()),
        "{stderr}"
    );
    let shadow = fs::canonicalize(shadow).unwrap();
    assert!(stderr.contains(&shadow.display().to_string()), "{stderr}");
    assert!(!app.join("vex.lock").exists());
    assert!(app.join(".vex/state.lock").is_file());
    assert!(!app.join(".vex/deps").exists());
}

#[test]
fn missing_or_malformed_target_fails_before_project_work() {
    let fixture = TestDir::new();
    for mode in ["build", "run", "check"] {
        for args in [
            &[mode, "--target", "--release", "--dry-run"][..],
            &[mode, "--target", "--"][..],
        ] {
            let output = vex(&fixture.0, args);
            assert_eq!(output.status.code(), Some(2));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("missing value for `--target`"), "{stderr}");
            assert!(!stderr.contains("could not find `vex.ws`"), "{stderr}");
        }
        let malformed = vex(&fixture.0, &[mode, "--target=bad/target"]);
        assert_eq!(malformed.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&malformed.stderr).contains("invalid value"));
    }
    assert!(!fixture.0.join("target").exists());
    assert!(!fixture.0.join(".vex").exists());
}

#[test]
fn invalid_projects_never_report_dependency_or_compiler_work_started() {
    for manifest in [None, Some("{ name = false }")] {
        let fixture = TestDir::new();
        if let Some(manifest) = manifest {
            fs::write(fixture.0.join("vex.ws"), manifest).unwrap();
        }
        for command in ["update", "fetch", "build", "run", "check", "tree"] {
            let output = vex(&fixture.0, &[command]);
            assert_eq!(
                output.status.code(),
                Some(if manifest.is_some() { 3 } else { 5 })
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("vex.ws"), "{stderr}");
            for status in [
                "Updating",
                "Resolving",
                "Cloning",
                "Fetching",
                "Locking",
                "Compiling",
                "Checking",
                "Running",
            ] {
                assert!(!stderr.contains(status), "{command}: {stderr}");
            }
            assert!(!fixture.0.join("target").exists());
            assert!(!fixture.0.join(".vex").exists());
            assert!(!fixture.0.join("vex.lock").exists());
        }
    }
}

#[test]
fn dependencies_must_be_library_packages_with_src_lib_wave() {
    let fixture = TestDir::new();
    let app = fixture.0.join("app");
    let dependency = fixture.0.join("add");
    fs::create_dir_all(&app).unwrap();
    fs::create_dir_all(&dependency).unwrap();
    fs::write(
        app.join("vex.ws"),
        "{ name = \"app\", version = 0.1.0, dependencies = [{ name = \"add\", path = \"../add\" }] }\n",
    )
    .unwrap();
    fs::write(
        dependency.join("vex.ws"),
        "{ name = \"add\", version = 0.1.0, lib = false, dependencies = [] }\n",
    )
    .unwrap();

    let non_library = vex(&app, &["fetch"]);
    assert_eq!(non_library.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&non_library.stderr)
        .contains("dependency `add` is not a library package"));

    fs::write(
        dependency.join("vex.ws"),
        "{ name = \"add\", version = 0.1.0, lib = true, dependencies = [] }\n",
    )
    .unwrap();
    let missing_entry = vex(&app, &["fetch"]);
    assert_eq!(missing_entry.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&missing_entry.stderr).contains("has no canonical library entry")
    );

    fs::create_dir_all(dependency.join("src")).unwrap();
    fs::write(
        dependency.join("src/lib.wave"),
        "pub fun sum(a: i32, b: i32) -> i32 { return a + b; }\n",
    )
    .unwrap();
    assert_success(&vex(&app, &["fetch"]), "fetch canonical library");
}

#[test]
fn tree_prints_locked_direct_and_transitive_dependencies() {
    let fixture = TestDir::new();
    let app = fixture.0.join("app");
    let alpha = fixture.0.join("alpha");
    let shared = fixture.0.join("shared");
    let leaf = fixture.0.join("leaf");
    for package in [&app, &alpha, &shared, &leaf] {
        fs::create_dir_all(package.join("src")).unwrap();
    }
    fs::write(
        app.join("vex.ws"),
        "{ name = \"app\", version = 0.1.0, dependencies = [{ name = \"shared\", path = \"../shared\" }, { name = \"alpha\", path = \"../alpha\" }] }\n",
    )
    .unwrap();
    fs::write(
        alpha.join("vex.ws"),
        "{ name = \"alpha\", version = 1.0.0, lib = true, dependencies = [{ name = \"shared\", path = \"../shared\" }] }\n",
    )
    .unwrap();
    fs::write(
        shared.join("vex.ws"),
        "{ name = \"shared\", version = 2.0.0, lib = true, dependencies = [{ name = \"leaf\", path = \"../leaf\" }] }\n",
    )
    .unwrap();
    fs::write(
        leaf.join("vex.ws"),
        "{ name = \"leaf\", version = 3.0.0, lib = true, dependencies = [] }\n",
    )
    .unwrap();
    for package in [&alpha, &shared, &leaf] {
        fs::write(package.join("src/lib.wave"), "pub fun marker() {}\n").unwrap();
    }

    let first = vex(&app, &["tree"]);
    assert_success(&first, "resolve dependency tree");
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(stdout.contains("app v0.1.0"), "{stdout}");
    assert!(
        stdout.contains("├── alpha v1.0.0 (path ../alpha)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("│   └── shared v2.0.0 (path ../shared)"),
        "{stdout}"
    );
    assert!(stdout.contains("leaf v3.0.0 (path ../leaf)"), "{stdout}");
    assert!(
        stdout.contains("└── shared v2.0.0 (path ../shared) (*)"),
        "{stdout}"
    );
    assert!(app.join("vex.lock").is_file());

    let locked = vex(&app, &["tree", "--locked", "--offline"]);
    assert_success(&locked, "print locked offline dependency tree");
    assert_eq!(first.stdout, locked.stdout);
}

fn vex(path: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(args)
        .current_dir(path)
        .output()
        .expect("vex command must start")
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn init_rejects_invalid_names_before_creating_state() {
    let fixture = TestDir::new();
    for name in ["invalid-name", "0invalid", "space name", "한글"] {
        let project = fixture.0.join(name);
        fs::create_dir(&project).unwrap();
        let output = vex(&project, &["init"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid package name"));
        assert_eq!(fs::read_dir(project).unwrap().count(), 0);
    }
}

#[test]
fn init_preserves_existing_lock_and_rolls_back_failed_publication() {
    let fixture = TestDir::new();
    let project = fixture.0.join("app");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("vex.lock"), "preserve me").unwrap();
    assert!(!vex(&project, &["init"]).status.success());
    assert_eq!(fs::read(project.join("vex.lock")).unwrap(), b"preserve me");
    assert!(!project.join("src").exists());
    assert!(!project.join("vex.ws").exists());
    fs::remove_file(project.join("vex.lock")).unwrap();
    for point in ["src/main.wave", "vex.lock", ".gitignore"] {
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .args(["init"])
            .current_dir(&project)
            .env("VEX_TEST_INIT_FAIL_AFTER", point)
            .output()
            .unwrap();
        assert!(!output.status.success());
        for path in [
            "src",
            "vex.ws",
            "vex.lock",
            ".gitignore",
            ".vex/init.json",
            ".vex/init-stage",
        ] {
            assert!(!project.join(path).exists(), "{point}: {path}");
        }
    }
    assert_success(&vex(&project, &["init"]), "retry initialization");
}

#[test]
fn dependency_free_fetch_creates_a_missing_lockfile_offline() {
    let fixture = TestDir::new();
    fs::write(fixture.0.join("vex.ws"), "{ name = \"app\" }").unwrap();
    assert!(!vex(&fixture.0, &["fetch", "--locked", "--offline"])
        .status
        .success());
    assert!(!fixture.0.join("vex.lock").exists());
    assert_success(
        &vex(&fixture.0, &["fetch", "--offline"]),
        "empty offline fetch",
    );
    let bytes = fs::read(fixture.0.join("vex.lock")).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("version = 3"));
    assert_success(
        &vex(&fixture.0, &["fetch", "--locked", "--offline"]),
        "empty locked reuse",
    );
    assert_eq!(fs::read(fixture.0.join("vex.lock")).unwrap(), bytes);
}

#[test]
fn init_restart_recovers_crash_but_preserves_subsequent_edits() {
    let fixture = TestDir::new();
    for (name, edit) in [("clean", false), ("edited", true)] {
        let project = fixture.0.join(name);
        fs::create_dir(&project).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .arg("init")
            .current_dir(&project)
            .env("VEX_TEST_INIT_CRASH_AFTER", "vex.lock")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(99));
        assert!(project.join(".vex/init.json").is_file());
        if edit {
            fs::write(project.join("src/main.wave"), "keep my edit").unwrap();
        }
        let retry = vex(&project, &["init"]);
        if edit {
            assert!(!retry.status.success());
            assert_eq!(
                fs::read(project.join("src/main.wave")).unwrap(),
                b"keep my edit"
            );
            assert!(project.join(".vex/init.json").exists());
        } else {
            assert_success(&retry, "crash recovery and retry");
            assert!(project.join("vex.ws").is_file());
            assert!(!project.join(".vex/init.json").exists());
        }
    }
}

#[test]
fn message_contract_covers_help_usage_resolution_and_environment_for_all_commands() {
    use serde_json::Value;
    let fixture = TestDir::new();
    let mut index = 0;
    let mut check = |args: &[&str], category: &str, code: i32| {
        index += 1;
        let report = format!("report-{index}.jsonl");
        let mut full = vec!["--message-file", report.as_str()];
        full.extend_from_slice(args);
        let output = vex(&fixture.0, &full);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events: Vec<Value> = fs::read_to_string(fixture.0.join(&report))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(events.last().unwrap()["event"], "finished");
        assert_eq!(events.last().unwrap()["category"], category);
        assert_eq!(events.last().unwrap()["origin"], "vex");
        assert_eq!(events.last().unwrap()["exit_code"], code);
        assert_eq!(events.last().unwrap()["success"], code == 0);
    };
    for command in [
        "init", "build", "check", "run", "fetch", "update", "info", "tree", "setup",
    ] {
        check(&[command, "--help"], "success", 0);
        check(&[command, "--unknown"], "usage", 2);
    }
    for option in ["--version", "--help"] {
        check(&[option], "success", 0);
    }
    check(&["bad-command"], "usage", 2);
    check(
        &["setup", "wavec", "--version", "1.2.3+metadata"],
        "usage",
        2,
    );
    for command in ["info", "build", "check", "run", "tree", "fetch", "update"] {
        check(&[command], "environment", 5);
    }
    fs::write(fixture.0.join("vex.ws"), "{name=false}").unwrap();
    for command in ["info", "build", "check", "run", "tree", "fetch", "update"] {
        check(&[command], "resolution", 3);
    }
    fs::write(fixture.0.join("vex.ws"), "{name=\"app\"}").unwrap();
    fs::write(fixture.0.join("vex.lock"), "{version=999}").unwrap();
    check(&["fetch", "--locked"], "resolution", 3);
}

#[test]
fn message_file_never_overwrites_existing_paths_or_creates_parents() {
    let fixture = TestDir::new();
    fs::write(fixture.0.join("existing"), b"keep").unwrap();
    fs::create_dir(fixture.0.join("directory")).unwrap();
    for path in ["existing", "directory", "missing/report.jsonl"] {
        let output = vex(&fixture.0, &["--message-file", path, "--help"]);
        assert_eq!(output.status.code(), Some(5));
    }
    assert_eq!(fs::read(fixture.0.join("existing")).unwrap(), b"keep");
    assert!(!fixture.0.join("missing").exists());
    for args in [
        vec!["--message-file"],
        vec!["--message-file", "a", "--message-file", "b"],
    ] {
        assert_eq!(vex(&fixture.0, &args).status.code(), Some(2));
    }
    assert!(!fixture.0.join("a").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        symlink(fixture.0.join("absent"), fixture.0.join("link")).unwrap();
        assert_eq!(
            vex(&fixture.0, &["--message-file", "link", "--help"])
                .status
                .code(),
            Some(5)
        );
        assert!(!fixture.0.join("absent").exists());
        assert!(vex(&fixture.0, &["--message-file", "private", "--help"])
            .status
            .success());
        assert_eq!(
            fs::metadata(fixture.0.join("private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn internal_outcomes_are_reported_as_vex_failures() {
    let fixture = TestDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_vex"))
        .current_dir(&fixture.0)
        .args(["--message-file", "internal.jsonl", "info"])
        .env("VEX_TEST_INTERNAL_FAILURE", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = fs::read_to_string(fixture.0.join("internal.jsonl")).unwrap();
    let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(last["category"], "internal");
    assert_eq!(last["origin"], "vex");
}

#[test]
fn message_diagnostics_redact_credential_bearing_arguments() {
    let fixture = TestDir::new();
    let output = vex(
        &fixture.0,
        &[
            "--message-file",
            "redacted.jsonl",
            "info",
            "https://login:secret_contract_value@example.invalid/repo?token=secret_query_value",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let text = fs::read_to_string(fixture.0.join("redacted.jsonl")).unwrap();
    for secret in ["login", "secret_contract_value", "secret_query_value"] {
        assert!(!text.contains(secret), "credential exposed in report");
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
    assert!(text.contains("example.invalid"));
}

#[test]
fn help_is_order_independent_and_never_initializes_a_project() {
    let fixture = TestDir::new();
    for command in [
        "init", "build", "check", "run", "fetch", "update", "info", "tree", "metadata", "setup",
    ] {
        for args in [
            [command, "--unknown", "--help"],
            [command, "--help", "--unknown"],
        ] {
            assert_success(&vex(&fixture.0, &args), "mixed help");
        }
    }
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[test]
fn version_has_no_ansi_when_redirected_or_no_color_is_set() {
    let fixture = TestDir::new();
    for no_color in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command.current_dir(&fixture.0).arg("--version");
        if no_color {
            command.env("NO_COLOR", "1");
        }
        let output = command.output().unwrap();
        assert_success(&output, "plain version");
        assert!(!output.stdout.contains(&0x1b));
    }
}

#[test]
fn missing_canonical_entry_never_selects_arbitrary_sources() {
    let fixture = TestDir::new();
    fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(
        fixture.0.join("src/other.wave"),
        "// fun main() in a comment\n",
    )
    .unwrap();
    for (lib, entry) in [(false, "src/main.wave"), (true, "src/lib.wave")] {
        fs::write(
            fixture.0.join("vex.ws"),
            format!("{{name=\"app\",lib={lib}}}"),
        )
        .unwrap();
        for mode in ["build", "check", "run"] {
            let output = Command::new(env!("CARGO_BIN_EXE_vex"))
                .current_dir(&fixture.0)
                .env("VEX_WAVEC", fixture.0.join("must_not_run"))
                .arg(mode)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(3),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            if !(lib && mode == "run") {
                assert!(String::from_utf8_lossy(&output.stderr).contains(entry));
            }
            assert!(!fixture.0.join("target").exists());
            assert!(!fixture.0.join(".vex").exists());
        }
    }
}

#[test]
fn init_reports_ignore_creation_only_when_created() {
    let fixture = TestDir::new();
    for lib in [false, true] {
        for existing in [false, true] {
            let project = fixture.0.join(format!("app_{}_{}", lib, existing));
            fs::create_dir(&project).unwrap();
            if existing {
                fs::write(project.join(".gitignore"), "mine\n").unwrap();
            }
            let output = vex(&project, if lib { &["init", "--lib"] } else { &["init"] });
            assert_success(&output, "init");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).contains("created .gitignore"),
                !existing
            );
            if existing {
                assert_eq!(
                    fs::read_to_string(project.join(".gitignore")).unwrap(),
                    "mine\n"
                );
            }
        }
    }
}
