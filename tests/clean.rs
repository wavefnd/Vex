// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
mod support;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vex-clean-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("vex.ws"), "{name=\"app\",version=0.1.0}").unwrap();
        fs::write(root.join("src/main.wave"), "fun main() {}\n").unwrap();
        Self { root }
    }
    fn command(&self, args: &[&str]) -> Command {
        static COMPILER: OnceLock<PathBuf> = OnceLock::new();
        let compiler = COMPILER.get_or_init(|| {
            let root =
                std::env::temp_dir().join(format!("vex-clean-compiler-{}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("fake.rs"), include_str!("fixtures/fake_wavec.rs")).unwrap();
            let executable = root.join(if cfg!(windows) { "wavec.exe" } else { "wavec" });
            support::compile_fixture("fake_wavec", &executable);
            executable
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .args(args)
            .current_dir(&self.root)
            .env("VEX_WAVEC", compiler)
            .env("NO_COLOR", "1");
        command
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn select_binary(&self, name: &str) {
        fs::write(self.root.join("vex.ws"), format!(
            "{{format=3,name=\"app\",version=0.1.0,targets={{bins=[{{name=\"{name}\",path=\"src/main.wave\"}}]}}}}"
        )).unwrap();
    }
    fn build(&self, name: &str, fail: bool) {
        self.build_profile(name, "dev", fail);
    }
    fn build_profile(&self, name: &str, profile: &str, fail: bool) {
        self.select_binary(name);
        let result = self
            .command(&["build", "--profile", profile])
            .env("FAKE_BUILD_OUTPUT", artifact_profile(name, profile))
            .env("VEX_TEST_COMPILE_EXIT", if fail { "1" } else { "0" })
            .output()
            .unwrap();
        assert_eq!(
            result.status.success(),
            !fail,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fn generation(&self) -> PathBuf {
        fs::read_dir(self.root.join("target/.vex-run"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn target() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "x86_64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}
fn artifact_profile(name: &str, profile: &str) -> String {
    format!(
        "target/{}/{profile}/bin/{name}{}",
        target(),
        std::env::consts::EXE_SUFFIX
    )
}
fn artifact(name: &str) -> String {
    artifact_profile(name, "dev")
}
fn program() -> String {
    artifact("app").strip_prefix("target/").unwrap().to_owned()
}

#[test]
fn pristine_and_legacy_outputs_are_preserved_without_creating_state() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("target/.vex-run/legacy")).unwrap();
    fs::write(f.root.join("target/.vex-run/legacy/important"), "user data").unwrap();
    f.ok(&["clean", "--dry-run"]);
    f.ok(&["clean"]);
    assert!(!f.root.join(".vex").exists());
    assert!(!f.root.join("vex.lock").exists());
    assert_eq!(
        fs::read(f.root.join("target/.vex-run/legacy/important")).unwrap(),
        b"user data"
    );
}

#[test]
fn removes_only_new_unchanged_outputs_and_dry_run_preserves_everything() {
    let f = Fixture::new();
    f.build("owned", false);
    let registry = fs::read(f.root.join(".vex/artifacts.json")).unwrap();
    let lock = fs::read(f.root.join("vex.lock")).unwrap();
    fs::write(f.root.join("target/unknown"), "keep").unwrap();
    let preview = f.ok(&["--message-file", "preview.jsonl", "clean", "--dry-run"]);
    assert!(String::from_utf8_lossy(&preview.stdout).contains(&artifact("owned")));
    assert!(!f.root.join("preview.jsonl").exists());
    assert!(f.root.join(artifact("owned")).exists());
    assert_eq!(
        fs::read(f.root.join(".vex/artifacts.json")).unwrap(),
        registry
    );
    f.ok(&["clean"]);
    assert!(!f.root.join(artifact("owned")).exists());
    assert_eq!(fs::read(f.root.join("target/unknown")).unwrap(), b"keep");
    assert_eq!(fs::read(f.root.join("vex.lock")).unwrap(), lock);
    assert!(f.root.join("src/main.wave").exists());
    f.ok(&["clean"]);
}

#[test]
fn modified_and_preexisting_outputs_are_preserved_before_compilation() {
    let f = Fixture::new();
    f.select_binary("legacy");
    fs::create_dir_all(f.root.join(artifact("legacy")).parent().unwrap()).unwrap();
    fs::write(f.root.join(artifact("legacy")), "old compiler output").unwrap();
    let output = f
        .command(&["build"])
        .env("FAKE_BUILD_OUTPUT", artifact("legacy"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(
        fs::read(f.root.join(artifact("legacy"))).unwrap(),
        b"old compiler output"
    );
    f.build("edited", false);
    fs::write(f.root.join(artifact("edited")), "user change").unwrap();
    f.ok(&["clean"]);
    let output = f
        .command(&["build"])
        .env("FAKE_BUILD_OUTPUT", artifact("edited"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    f.ok(&["clean"]);
    assert_eq!(
        fs::read(f.root.join(artifact("edited"))).unwrap(),
        b"user change"
    );
}

#[test]
fn partial_compile_output_is_owned_but_unknown_generation_sidecars_survive() {
    let f = Fixture::new();
    f.build("partial", true);
    f.select_binary("app");
    f.ok(&["run"]);
    let generation = f.generation();
    fs::write(generation.join("sidecar"), "program-owned").unwrap();
    f.ok(&["clean"]);
    assert!(!f.root.join(artifact("partial")).exists());
    assert!(!generation.join(program()).exists());
    assert_eq!(
        fs::read(generation.join("sidecar")).unwrap(),
        b"program-owned"
    );
    f.ok(&["clean"]);
}

#[test]
fn malformed_registry_and_pending_recovery_fail_before_deletion() {
    let f = Fixture::new();
    f.build("owned", false);
    let registry = fs::read(f.root.join(".vex/artifacts.json")).unwrap();
    for journal in ["transaction.json", "init.json"] {
        fs::write(f.root.join(".vex").join(journal), "pending").unwrap();
        let output = f.command(&["clean"]).output().unwrap();
        assert!(!output.status.success());
        assert!(f.root.join(artifact("owned")).exists());
        assert_eq!(
            fs::read(f.root.join(".vex").join(journal)).unwrap(),
            b"pending"
        );
        fs::remove_file(f.root.join(".vex").join(journal)).unwrap();
    }
    for content in [
        "{",
        r#"{"version":1,"files":{"target/../src/main.wave":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"directories":[],"generations":[]}"#,
        r#"{"version":99,"files":{},"directories":[],"generations":[]}"#,
    ] {
        fs::write(f.root.join(".vex/artifacts.json"), content).unwrap();
        assert!(!f.command(&["clean"]).output().unwrap().status.success());
        assert!(f.root.join(artifact("owned")).exists());
    }
    fs::write(f.root.join(".vex/artifacts.json"), registry).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinks_and_permission_edits_are_preserved() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new();
    f.build_profile("owned", "release", false);
    let output = f.root.join(artifact_profile("owned", "release"));
    let directory = output.parent().unwrap();
    fs::rename(directory, f.root.join("saved")).unwrap();
    symlink(f.root.join("saved"), directory).unwrap();
    f.build("mode", false);
    fs::set_permissions(
        f.root.join(artifact("mode")),
        fs::Permissions::from_mode(
            fs::metadata(f.root.join(artifact("mode")))
                .unwrap()
                .permissions()
                .mode()
                ^ 0o100,
        ),
    )
    .unwrap();
    f.ok(&["clean"]);
    assert!(f
        .root
        .join(format!("saved/owned{}", std::env::consts::EXE_SUFFIX))
        .exists());
    assert!(f.root.join(artifact("mode")).exists());
}

fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                stream.read_exact(&mut [0]).unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "program did not reach barrier");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn clean_skips_active_generation_and_reclaims_it_after_exit() {
    let f = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut child = f
        .command(&["run"])
        .env("VEX_TEST_RUN", listener.local_addr().unwrap().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut socket = accept(&listener);
    let generation = f.generation();
    let output = f.ok(&["clean"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("active"));
    assert!(generation.join(program()).is_file());
    socket.write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    let preview = f.ok(&["clean", "--dry-run"]);
    assert!(String::from_utf8_lossy(&preview.stdout)
        .contains(generation.file_name().unwrap().to_str().unwrap()));
    assert!(generation.exists());
    f.ok(&["clean"]);
    assert!(!generation.exists());
}

#[cfg(unix)]
#[test]
fn run_lease_protects_orphaned_program_after_vex_is_killed() {
    let f = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut child = f
        .command(&["run"])
        .env("VEX_TEST_RUN", listener.local_addr().unwrap().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut socket = accept(&listener);
    let generation = f.generation();
    child.kill().unwrap();
    child.wait().unwrap();
    f.ok(&["clean"]);
    assert!(generation.join(program()).is_file());
    socket.write_all(&[1]).unwrap();
    let mut buffer = [0; 1];
    assert_eq!(socket.read(&mut buffer).unwrap(), 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while generation.exists() {
        f.ok(&["clean"]);
        assert!(Instant::now() < deadline, "orphan lease did not close");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn nonregular_owned_paths_do_not_block_cleanup() {
    let f = Fixture::new();
    f.build("owned", false);
    fs::remove_file(f.root.join(artifact("owned"))).unwrap();
    assert!(Command::new("mkfifo")
        .arg(f.root.join(artifact("owned")))
        .status()
        .unwrap()
        .success());
    let mut child = f
        .command(&["clean"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("clean blocked on FIFO");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fs::symlink_metadata(f.root.join(artifact("owned"))).is_ok());
}
