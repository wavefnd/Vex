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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    work: PathBuf,
    bin: PathBuf,
    wavec: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-doctor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let work = root.join("work");
        let bin = root.join("bin");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let wavec = bin.join(exe("wavec"));
        fs::copy(fake(), &wavec).unwrap();
        fs::copy(fake(), bin.join(exe("git"))).unwrap();
        Self {
            root,
            work,
            bin,
            wavec,
        }
    }
    fn project(&self, coordinated: bool) {
        fs::create_dir_all(self.work.join("src/nested")).unwrap();
        fs::write(
            self.work.join("vex.ws"),
            "{ name = \"doctor_test\", version = 0.1.0, dependencies = [] }\n",
        )
        .unwrap();
        fs::write(self.work.join("src/main.wave"), "fun main() {}\n").unwrap();
        if coordinated {
            fs::create_dir_all(self.work.join(".vex")).unwrap();
            fs::write(self.work.join(".vex/state.lock"), "").unwrap();
            fs::write(
                self.work.join("vex.lock"),
                "{ version = 3, package = [] }\n",
            )
            .unwrap();
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(&self.work)
            .env("PATH", &self.bin)
            .env("VEX_WAVEC", &self.wavec)
            .env("DOCTOR_LOG", self.root.join("calls.log"))
            .env("DOCTOR_HOST_TARGET", host_target())
            .env(
                "DOCTOR_TARGETS",
                json!([host_target(), "doctor-cross-target"]).to_string(),
            );
        command
    }
    fn json(&self) -> Output {
        self.command()
            .args(["doctor", "--format=json"])
            .output()
            .unwrap()
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls.log")).unwrap_or_default()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}
fn host_target() -> String {
    let platforms: Value = serde_json::from_str(include_str!("../platforms.json")).unwrap();
    platforms["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["os"] == std::env::consts::OS && p["arch"] == std::env::consts::ARCH)
        .map(|p| {
            if p["os"] == "linux" {
                format!("{}-unknown-linux-gnu", p["arch"].as_str().unwrap())
            } else {
                p["rust_target"].as_str().unwrap().to_owned()
            }
        })
        .unwrap_or_else(|| "test-host".into())
}
fn fake() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("vex-doctor-tools-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("fake.rs");
        fs::write(&source, include_str!("fixtures/doctor_tools.rs")).unwrap();
        let path = root.join(exe("fake"));
        support::compile_fixture("doctor_tools", &path);
        path
    })
}
fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid doctor report: {error}; stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn check<'a>(value: &'a Value, id: &str) -> &'a Value {
    value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("missing check {id}: {value}"))
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Option<Vec<u8>>, std::time::SystemTime)> {
    fn visit(
        root: &Path,
        path: &Path,
        map: &mut BTreeMap<PathBuf, (Option<Vec<u8>>, std::time::SystemTime)>,
    ) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            map.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (
                    if meta.is_file() {
                        Some(fs::read(&path).unwrap())
                    } else {
                        None
                    },
                    meta.modified().unwrap(),
                ),
            );
            if meta.is_dir() {
                visit(root, &path, map);
            }
        }
    }
    let mut map = BTreeMap::new();
    visit(root, root, &mut map);
    map
}

#[test]
fn outside_project_reports_tools_and_explicitly_unchecked_dry_run_without_writes() {
    let f = Fixture::new();
    let before = snapshot(&f.work);
    let out = f.json();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = document(&out);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["success"], true);
    assert_eq!(check(&report, "git")["status"], "ok");
    assert_eq!(check(&report, "wavec")["details"]["selection"], "VEX_WAVEC");
    assert_eq!(check(&report, "project")["status"], "not_checked");
    assert_eq!(check(&report, "dry_run")["status"], "not_checked");
    assert!(!f.calls().contains("build"));
    assert_eq!(before, snapshot(&f.work));
}
#[test]
fn unprepared_graph_is_not_fetched_and_does_not_create_coordination_state() {
    let f = Fixture::new();
    f.project(false);
    let before = snapshot(&f.work);
    let out = f.json();
    assert!(out.status.success());
    let report = document(&out);
    assert_eq!(check(&report, "local_graph")["status"], "warning");
    assert_eq!(check(&report, "dry_run")["status"], "not_checked");
    assert!(!f.calls().contains("build"));
    assert_eq!(before, snapshot(&f.work));
}
#[test]
fn prepared_project_plan_is_checked_under_explicit_selection_without_build_or_mutation() {
    let f = Fixture::new();
    f.project(true);
    let before = snapshot(&f.work);
    let out = f
        .command()
        .current_dir(&f.root)
        .args([
            "doctor",
            "--format",
            "json",
            "--manifest-path",
            "work/vex.ws",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = document(&out);
    assert_eq!(check(&report, "dry_run")["status"], "ok");
    let calls = f.calls();
    assert_eq!(
        calls
            .lines()
            .filter(|s| s.contains("print supported-targets"))
            .count(),
        1
    );
    assert_eq!(calls.lines().filter(|s| s.contains("build")).count(), 1);
    assert!(calls.contains("src/main.wave"));
    assert!(calls.contains("--dry-run") && calls.contains("--error-format=json"));
    assert!(calls.contains("-O0"));
    assert!(calls.contains(&format!("target/{}/dev/bin/doctor_test", host_target())));
    assert_eq!(before, snapshot(&f.work));
}
#[test]
fn incompatible_schema_is_compiler_failure_with_complete_report() {
    let f = Fixture::new();
    f.project(true);
    let before = snapshot(&f.work);
    let out = f
        .command()
        .env("DOCTOR_SCHEMA", "2")
        .args(["doctor", "--format=json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let report = document(&out);
    assert_eq!(report["success"], false);
    assert_eq!(check(&report, "git")["status"], "ok");
    assert_eq!(check(&report, "dry_run")["status"], "error");
    assert_eq!(before, snapshot(&f.work));
}
#[test]
fn explicit_invalid_compiler_never_falls_back_to_working_path_compiler() {
    let f = Fixture::new();
    let out = f
        .command()
        .env("VEX_WAVEC", f.root.join("absent-wavec"))
        .args(["doctor", "--format=json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    let report = document(&out);
    assert_eq!(check(&report, "wavec")["status"], "error");
    assert_eq!(check(&report, "git")["status"], "ok");
    assert_eq!(check(&report, "project")["status"], "not_checked");
    assert!(!f.calls().contains("wavec:"));
}
#[test]
fn missing_git_still_collects_compiler_and_project_diagnostics() {
    let f = Fixture::new();
    fs::remove_file(f.bin.join(exe("git"))).unwrap();
    let out = f.json();
    assert_eq!(out.status.code(), Some(5));
    let report = document(&out);
    assert_eq!(check(&report, "git")["status"], "error");
    assert_eq!(check(&report, "wavec")["status"], "ok");
    assert_eq!(check(&report, "capabilities")["status"], "ok");
}
#[test]
fn unsupported_target_is_resolution_failure_and_cross_linker_absence_is_a_warning() {
    let f = Fixture::new();
    f.project(true);
    let out = f
        .command()
        .args(["doctor", "--format=json", "--target=not-supported"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(check(&document(&out), "target")["status"], "error");
    assert!(!f.calls().contains("build"));
    let out = f
        .command()
        .env("DOCTOR_LINK", "missing-doctor-linker")
        .args(["doctor", "--format=json", "--target", "doctor-cross-target"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(check(&document(&out), "linker")["status"], "warning");
    let out = f
        .command()
        .env("DOCTOR_LINK", "missing-doctor-linker")
        .args(["doctor", "--format=json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(check(&document(&out), "linker")["status"], "error");

    let out = f
        .command()
        .env("DOCTOR_ERROR", "backend tool unavailable")
        .env("DOCTOR_EXIT", "3")
        .args(["doctor", "--format=json", "--target=doctor-cross-target"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(check(&document(&out), "dry_run")["status"], "warning");
    let out = f
        .command()
        .env("DOCTOR_ERROR", "backend tool unavailable")
        .env("DOCTOR_EXIT", "3")
        .args(["doctor", "--format=json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(check(&document(&out), "dry_run")["status"], "error");
}
#[test]
fn required_manifest_version_and_missing_source_are_project_failures() {
    let f = Fixture::new();
    f.project(true);
    fs::write(
        f.work.join("vex.ws"),
        "{ name = \"doctor_test\", version = 0.1.0, compiler = \"0.2.9\", dependencies = [] }\n",
    )
    .unwrap();
    let out = f.json();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        check(&document(&out), "project_compiler")["status"],
        "error"
    );
    fs::remove_file(f.work.join("src/main.wave")).unwrap();
    let out = f.json();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(check(&document(&out), "entry")["status"], "error");
    assert_eq!(check(&document(&out), "dry_run")["status"], "not_checked");
}
#[test]
fn pending_recovery_is_reported_but_never_performed() {
    let f = Fixture::new();
    f.project(true);
    fs::write(
        f.work.join(".vex/transaction.json"),
        "interrupted transaction",
    )
    .unwrap();
    let before = snapshot(&f.work);
    let out = f.json();
    assert_eq!(check(&document(&out), "local_graph")["status"], "warning");
    assert_eq!(check(&document(&out), "dry_run")["status"], "not_checked");
    assert_eq!(before, snapshot(&f.work));
    assert!(!f.calls().contains("build"));
}
#[test]
fn compiler_credentials_are_redacted_in_json_and_human_reports() {
    let f = Fixture::new();
    f.project(true);
    let error = "failed https://doctor-user:doctor-secret@example.invalid/repo?token=doctor-token ; helper echoed doctor-secret";
    for args in [&["doctor", "--format=json"][..], &["doctor"][..]] {
        let out = f
            .command()
            .env("DOCTOR_ERROR", error)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
        let rendered = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        for secret in ["doctor-user", "doctor-secret", "doctor-token"] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
        assert!(rendered.contains("example.invalid"));
    }
}
#[test]
fn unknown_options_and_help_do_not_spawn_tools() {
    let f = Fixture::new();
    for args in [
        &["doctor", "--format=xml"][..],
        &["doctor", "--target", "--format=json"][..],
        &["doctor", "--target=x", "--target=y"][..],
    ] {
        assert_eq!(
            f.command().args(args).output().unwrap().status.code(),
            Some(2)
        );
    }
    let out = f
        .command()
        .args(["doctor", "--bad", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("usage: vex doctor"));
    assert!(f.calls().is_empty());
}

#[test]
fn unsafe_compiler_outputs_fail_doctor_before_linker_checks_without_writes() {
    for output in ["src/main.wave", "target/../src/main.wave"] {
        let f = Fixture::new();
        f.project(true);
        let before = snapshot(&f.work);
        let out = f
            .command()
            .env("DOCTOR_LINK", "missing-doctor-linker")
            .env("DOCTOR_LINK_OUTPUT", output)
            .args(["doctor", "--format=json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
        let report = document(&out);
        assert_eq!(report["success"], false);
        assert_eq!(check(&report, "dry_run")["status"], "error");
        assert!(check(&report, "dry_run")["message"]
            .as_str()
            .unwrap()
            .contains("strictly inside target"));
        assert!(!report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "linker"));
        assert_eq!(before, snapshot(&f.work));
    }
}

#[test]
fn mixed_targets_use_dev_profile_self_library_mapping_and_all_plans_without_writes() {
    let f = Fixture::new();
    f.project(true);
    fs::create_dir_all(f.work.join("src/bin")).unwrap();
    fs::write(f.work.join("src/lib.wave"), "pub fun greet() {}\n").unwrap();
    fs::write(f.work.join("src/bin/admin.wave"), "fun main() {}\n").unwrap();
    fs::write(
        f.work.join("vex.ws"),
        r#"{format=3, name="doctor_test", targets={lib={}, bins=[
        {name="doctor_test",path="src/main.wave"}, {name="admin",path="src/bin/admin.wave"}
    ]}, profiles={dev={opt_level=1}}}"#,
    )
    .unwrap();
    let before = snapshot(&f.work);
    let out = f.json();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = document(&out);
    let plans = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["id"] == "dry_run")
        .collect::<Vec<_>>();
    assert_eq!(plans.len(), 3);
    assert_eq!(plans[0]["details"]["package_target"]["kind"], "library");
    assert_eq!(plans[1]["details"]["package_target"]["name"], "admin");
    assert!(plans.iter().all(|item| item["status"] == "ok"
        && item["details"]["profile"] == "dev"
        && item["details"]["opt_level"] == 1));
    let calls = f.calls();
    let builds = calls
        .lines()
        .filter(|call| call.contains(" build "))
        .collect::<Vec<_>>();
    assert_eq!(builds.len(), 3);
    assert!(builds.iter().all(|call| call.contains("-O1")));
    assert!(!builds[0].contains("--dep=doctor_test="));
    assert!(builds[1..]
        .iter()
        .all(|call| call.contains("--dep=doctor_test=")));
    assert_eq!(
        calls
            .lines()
            .filter(|call| call.contains("print target-spec"))
            .count(),
        1
    );
    assert_eq!(before, snapshot(&f.work));
    assert!(!f.work.join("target").exists());
}

#[test]
fn project_default_compiler_target_is_checked_and_explicit_target_takes_precedence() {
    let f = Fixture::new();
    f.project(true);
    fs::write(f.work.join("vex.ws"), r#"{format=3,name="doctor_test",targets={bins=[{name="app",path="src/main.wave"}]}, build={target="doctor-cross-target"}}"#).unwrap();
    let out = f
        .command()
        .env("DOCTOR_TARGETS", json!(["doctor-cross-target"]).to_string())
        .args(["doctor", "--format=json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = document(&out);
    assert_eq!(
        check(&report, "target")["details"]["requested"],
        "doctor-cross-target"
    );
    assert_eq!(
        check(&report, "dry_run")["details"]["target"],
        "doctor-cross-target"
    );
    assert!(f.calls().contains("--target=doctor-cross-target"));
    let out = f
        .command()
        .args(["doctor", "--format=json", "--target", &host_target()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        check(&document(&out), "target")["details"]["requested"],
        host_target()
    );
}

#[test]
fn every_declared_entry_is_validated_before_doctor_plans_any_target() {
    let f = Fixture::new();
    f.project(true);
    fs::write(f.work.join("src/lib.wave"), "pub fun greet() {}\n").unwrap();
    fs::write(f.work.join("vex.ws"), r#"{format=3,name="doctor_test",targets={lib={},bins=[{name="missing",path="src/bin/missing.wave"}]}}"#).unwrap();
    let before = snapshot(&f.work);
    let out = f.json();
    assert_eq!(out.status.code(), Some(3));
    let report = document(&out);
    assert_eq!(check(&report, "entry")["status"], "error");
    assert!(check(&report, "entry")["message"]
        .as_str()
        .unwrap()
        .contains("missing"));
    assert_eq!(check(&report, "dry_run")["status"], "not_checked");
    assert!(!f.calls().lines().any(|call| call.contains(" build ")));
    assert_eq!(before, snapshot(&f.work));
}
