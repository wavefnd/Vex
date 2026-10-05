use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    compiler: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vex-state-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("vex.ws"), "{name=\"app\",version=0.1.0}").unwrap();
        fs::write(root.join("src/main.wave"), "fun main() {}\n").unwrap();
        fs::write(root.join("vex.lock"), "{version=2,package=[]}\n").unwrap();
        fs::write(root.join("fake.rs"), include_str!("fixtures/fake_wavec.rs")).unwrap();
        let compiler = root.join(if cfg!(windows) { "wavec.exe" } else { "wavec" });
        assert!(Command::new("rustc")
            .arg(root.join("fake.rs"))
            .arg("-o")
            .arg(&compiler)
            .status()
            .unwrap()
            .success());
        Self { root, compiler }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vex"));
        cmd.args(args)
            .current_dir(&self.root)
            .env("VEX_WAVEC", &self.compiler)
            .env_remove("NO_COLOR")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        cmd
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Barrier(TcpListener);
impl Barrier {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        Self(listener)
    }
    fn address(&self) -> String {
        self.0.local_addr().unwrap().to_string()
    }
    fn reached(&self) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.0.accept() {
                Ok((mut socket, _)) => {
                    // BSD/macOS can inherit the listener's nonblocking mode.
                    // The handshake uses a bounded blocking read on every OS.
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(15)))
                        .unwrap();
                    socket.read_exact(&mut [0]).unwrap();
                    return socket;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "child did not reach barrier");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }
}
fn finish(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("child did not finish");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn wait_for_lock(child: &mut Child) {
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if line.unwrap().contains("Waiting") {
                let _ = tx.send(());
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(15))
        .expect("waiter must report lock contention");
    assert!(child.try_wait().unwrap().is_none());
}

#[test]
fn compiler_source_protection_survives_parent_death() {
    let f = Fixture::new();
    let barrier = Barrier::new();
    let mut holder = f
        .command(&["build", "--locked", "--offline"])
        .env("VEX_TEST_COMPILE", barrier.address())
        .spawn()
        .unwrap();
    let mut compiler = barrier.reached();
    holder.kill().unwrap();
    holder.wait().unwrap();
    let mut writer = f
        .command(&["fetch", "--locked", "--offline"])
        .spawn()
        .unwrap();
    #[cfg(unix)]
    {
        wait_for_lock(&mut writer);
        compiler.write_all(&[1]).unwrap();
    }
    #[cfg(windows)]
    {
        // Parent death closes the Job Object and terminates the compiler tree.
        // Its inherited project lease closes with it, so a writer can proceed.
        match compiler.read(&mut [0]) {
            Ok(0) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("terminated compiler retained its socket: {result:?}"),
        }
    }
    finish(&mut writer);
    assert_eq!(
        fs::read_to_string(f.root.join("vex.lock")).unwrap(),
        "{version=2,package=[]}\n"
    );
}

#[test]
fn running_program_releases_state_and_retains_its_generation() {
    let f = Fixture::new();
    let barrier = Barrier::new();
    let mut running = f
        .command(&[
            "run",
            "--locked",
            "--offline",
            "--",
            "--flag",
            "with spaces",
        ])
        .env("VEX_TEST_RUN", barrier.address())
        .env("VEX_TEST_NESTED", env!("CARGO_BIN_EXE_vex"))
        .spawn()
        .unwrap();
    // The application performs a nested fetch before this barrier. Reaching it
    // proves the lock was released before spawn, not merely after first output.
    let mut application = barrier.reached();
    let generation = fs::read_dir(f.root.join("target/.vex-run"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let artifact = generation.join(if cfg!(windows) {
        "program.exe"
    } else {
        "program"
    });
    let original = fs::read(&artifact).unwrap();
    let mut update = f.command(&["update"]).spawn().unwrap();
    finish(&mut update);
    let mut build = f.command(&["build"]).spawn().unwrap();
    finish(&mut build);
    assert_eq!(fs::read(&artifact).unwrap(), original);
    application.write_all(&[1]).unwrap();
    finish(&mut running);
    assert!(
        artifact.is_file(),
        "initial policy has no automatic generation GC"
    );
}

#[test]
fn readers_share_a_lock_and_block_a_writer() {
    let f = Fixture::new();
    let barrier = Barrier::new();
    let mut first = f
        .command(&["check", "--dry-run", "--locked"])
        .env("VEX_TEST_PLAN", barrier.address())
        .spawn()
        .unwrap();
    let mut one = barrier.reached();
    let mut second = f
        .command(&["check", "--dry-run", "--locked"])
        .env("VEX_TEST_PLAN", barrier.address())
        .spawn()
        .unwrap();
    let mut two = barrier.reached();
    let mut writer = f.command(&["fetch", "--locked"]).spawn().unwrap();
    wait_for_lock(&mut writer);
    one.write_all(&[1]).unwrap();
    finish(&mut first);
    assert!(writer.try_wait().unwrap().is_none());
    two.write_all(&[1]).unwrap();
    finish(&mut second);
    finish(&mut writer);
    assert!(!f.root.join("target").exists());
}

#[test]
fn rejected_dependency_preflight_does_not_create_target() {
    let f = Fixture::new();
    fs::remove_file(f.root.join("vex.lock")).unwrap();
    for mode in ["build", "check", "run"] {
        assert!(!f
            .command(&[mode, "--locked"])
            .output()
            .unwrap()
            .status
            .success());
        assert!(!f.root.join("target").exists());
    }
}

#[test]
fn runtime_exit_codes_are_preserved() {
    let f = Fixture::new();
    for code in [0, 7, 42, 125] {
        let output = f
            .command(&["run", "--locked", "--offline"])
            .env("VEX_TEST_RUN_EXIT", code.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
    }
}

#[cfg(unix)]
#[test]
fn cancellation_stops_compiler_and_releases_the_project_lease() {
    let f = Fixture::new();
    let barrier = Barrier::new();
    let mut build = f
        .command(&[
            "--message-file",
            "cancel.jsonl",
            "build",
            "--locked",
            "--offline",
        ])
        .env("VEX_TEST_COMPILE", barrier.address())
        .spawn()
        .unwrap();
    let mut compiler = barrier.reached();
    assert!(Command::new("kill")
        .args(["-TERM", &build.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(status) = build.try_wait().unwrap() {
            break status.code();
        }
        assert!(Instant::now() < deadline, "cancelled compiler did not exit");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(code, Some(130));
    let events = message_events(&f.root, "cancel.jsonl");
    assert_eq!(events.last().unwrap()["category"], "cancelled");
    assert_eq!(events.last().unwrap()["origin"], "vex");
    assert_eq!(compiler.read(&mut [0]).unwrap(), 0);
    let mut writer = f
        .command(&["fetch", "--locked", "--offline"])
        .spawn()
        .unwrap();
    finish(&mut writer);
}

fn message_events(root: &std::path::Path, name: &str) -> Vec<serde_json::Value> {
    let events: Vec<serde_json::Value> = fs::read_to_string(root.join(name))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["schema_version"], 1);
        assert_eq!(event["sequence"], index + 1);
    }
    assert_eq!(events.first().unwrap()["event"], "started");
    events
}

#[test]
fn structured_outcomes_distinguish_vex_from_program_and_preserve_stdio() {
    let f = Fixture::new();
    for code in [0, 1, 2, 3, 4, 5, 42, 124, 130] {
        let report = format!("run-{code}.jsonl");
        let mut child = f
            .command(&[
                "--message-file",
                &report,
                "run",
                "--locked",
                "--offline",
                "--",
                "--message-file",
                "runtime.jsonl",
                "--dry-run",
            ])
            .env("VEX_TEST_RUN_EXIT", code.to_string())
            .env("VEX_TEST_ECHO_STDIN", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"hello program\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(code));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("PROGRAM_STDIN:hello program"), "{stdout}");
        assert!(stdout.contains("runtime.jsonl"), "{stdout}");
        assert!(!stdout.contains("schema_version"), "{stdout}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("PROGRAM_STDERR"));
        let events = message_events(&f.root, &report);
        let last = events.last().unwrap();
        assert_eq!(last["event"], "finished");
        assert_eq!(last["origin"], "program");
        assert_eq!(last["category"], "program");
        assert_eq!(last["exit_code"], code);
        assert_eq!(last["success"], code == 0);
        assert!(!f.root.join("runtime.jsonl").exists());
    }
}

#[test]
fn compiler_failures_are_typed_and_missing_compiler_is_environmental() {
    let f = Fixture::new();
    for (env_name, value, category, code) in [
        ("FAKE_SCHEMA", "999", "compiler", 4),
        ("VEX_TEST_COMPILE_EXIT", "1", "compiler", 4),
        ("VEX_TEST_COMPILE_EXIT", "3", "environment", 5),
        (
            "VEX_WAVEC",
            "nonexistent-compiler-for-contract-test",
            "environment",
            5,
        ),
    ] {
        let report = format!("{env_name}-{value}.jsonl");
        let output = f
            .command(&["--message-file", &report, "build", "--locked", "--offline"])
            .env(env_name, value)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = message_events(&f.root, &report);
        assert_eq!(events.last().unwrap()["category"], category);
        assert_eq!(events.last().unwrap()["origin"], "vex");
        assert!(events
            .iter()
            .any(|e| e["event"] == "diagnostic" && e["category"] == category));
    }
}

#[test]
fn dry_run_validates_message_destination_without_creating_it() {
    let f = Fixture::new();
    let lock = fs::read(f.root.join("vex.lock")).unwrap();
    for mode in ["build", "check", "run"] {
        let output = f
            .command(&[
                "--message-file",
                "dry.jsonl",
                mode,
                "--dry-run",
                "--locked",
                "--offline",
            ])
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        assert!(!f.root.join("dry.jsonl").exists());
        assert!(!f.root.join("target").exists());
        assert_eq!(fs::read(f.root.join("vex.lock")).unwrap(), lock);
    }
    fs::write(f.root.join("dry.jsonl"), b"keep").unwrap();
    for args in [vec!["build", "--dry-run"], vec!["build"]] {
        let mut full = vec!["--message-file", "dry.jsonl"];
        full.extend(args);
        assert_eq!(f.command(&full).output().unwrap().status.code(), Some(5));
        assert_eq!(fs::read(f.root.join("dry.jsonl")).unwrap(), b"keep");
    }
    assert!(!f.root.join("target").exists());
}

#[cfg(debug_assertions)]
#[test]
fn message_failure_before_spawn_stops_run_but_after_run_preserves_exit() {
    let f = Fixture::new();
    for (event, expected, ran) in [
        ("started", 5, false),
        ("compiler", 5, false),
        ("artifact", 5, false),
        ("running", 5, false),
        ("finished", 42, true),
    ] {
        let report = format!("fail-{event}.jsonl");
        let output = f
            .command(&["--message-file", &report, "run", "--locked", "--offline"])
            .env("VEX_TEST_MESSAGE_FAIL_EVENT", event)
            .env("VEX_TEST_RUN_EXIT", "42")
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).contains("FAKE_WAVEC_EXECUTED"),
            ran
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("message file"));
        let raw = fs::read_to_string(f.root.join(report)).unwrap();
        assert!(!raw.lines().any(
            |line| serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "finished"
        ));
    }
    // Successful programs also retain 0 when final reporting fails.
    let output = f
        .command(&[
            "--message-file",
            "fail-success.jsonl",
            "run",
            "--locked",
            "--offline",
        ])
        .env("VEX_TEST_MESSAGE_FAIL_EVENT", "finished")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn git_timeout_has_a_structured_timeout_outcome() {
    let f = Fixture::new();
    let git = f.root.join(if cfg!(windows) { "git.exe" } else { "git" });
    fs::copy(&f.compiler, git).unwrap();
    fs::write(
        f.root.join("vex.ws"),
        "{name=\"app\",dependencies=[{name=\"dep\",git=\"https://example.invalid/dep\"}]}",
    )
    .unwrap();
    let output = f
        .command(&["--message-file", "timeout.jsonl", "fetch"])
        .env("PATH", &f.root)
        .env("VEX_GIT_TIMEOUT", "1")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = message_events(&f.root, "timeout.jsonl");
    assert_eq!(events.last().unwrap()["category"], "timeout");
    assert_eq!(events.last().unwrap()["origin"], "vex");
    assert!(!events.last().unwrap()["success"].as_bool().unwrap());
}

#[cfg(unix)]
#[test]
fn program_signal_is_distinct_from_vex_cancellation() {
    let f = Fixture::new();
    let barrier = Barrier::new();
    let mut child = f
        .command(&[
            "--message-file",
            "signal.jsonl",
            "run",
            "--locked",
            "--offline",
        ])
        .env("VEX_TEST_RUN", barrier.address())
        .spawn()
        .unwrap();
    let mut program = barrier.reached();
    // The fixture writes its PID while paused, so only the program receives TERM.
    let pid = fs::read_to_string(f.root.join("program.pid")).unwrap();
    assert!(Command::new("kill")
        .args(["-TERM", pid.trim()])
        .status()
        .unwrap()
        .success());
    assert_eq!(child.wait().unwrap().code(), Some(143));
    assert_eq!(program.read(&mut [0]).unwrap(), 0);
    let events = message_events(&f.root, "signal.jsonl");
    assert_eq!(events.last().unwrap()["origin"], "program");
    assert_eq!(events.last().unwrap()["signal"], 15);
    assert_eq!(events.last().unwrap()["exit_code"], 143);
}

#[test]
fn metadata_acquires_shared_lease_before_reading_lockfile() {
    let f = Fixture::new();
    let barrier = Barrier::new();
    let mut holder = f
        .command(&["build", "--locked", "--offline"])
        .env("VEX_TEST_COMPILE", barrier.address())
        .spawn()
        .unwrap();
    let mut compiler = barrier.reached();
    // If metadata reads before acquiring its lease, this transient state fails.
    fs::write(f.root.join("vex.lock"), "not a lockfile").unwrap();
    let mut reader = f
        .command(&["metadata", "--locked"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_lock(&mut reader);
    fs::write(f.root.join("vex.lock"), "{version=2,package=[]}\n").unwrap();
    compiler.write_all(&[1]).unwrap();
    finish(&mut holder);
    let result = reader.wait_with_output().unwrap();
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
}

#[test]
fn compiler_version_requirement_fails_before_dependency_state_changes() {
    let fixture = Fixture::new();
    let lock = fs::read(fixture.root.join("vex.lock")).unwrap();
    fs::write(
        fixture.root.join("vex.ws"),
        "{format=2,name=\"app\",compiler=\"0.2.0-pre-beta\"}",
    )
    .unwrap();
    for command in ["build", "check", "run"] {
        let output = fixture.command(&[command]).output().unwrap();
        assert_eq!(output.status.code(), Some(3));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("0.2.0-pre-beta") && error.contains("0.2.1-pre-beta"),
            "{error}"
        );
        assert!(!fixture.root.join(".vex").exists());
        assert!(!fixture.root.join("target").exists());
        assert_eq!(fs::read(fixture.root.join("vex.lock")).unwrap(), lock);
    }
    fs::write(
        fixture.root.join("vex.ws"),
        "{format=2,name=\"app\",compiler=\"0.2.1-pre-beta\"}",
    )
    .unwrap();
    assert!(fixture
        .command(&["check", "--locked", "--offline"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn transitive_compiler_requirement_rejects_before_lock_publication() {
    let fixture = Fixture::new();
    let lock = fs::read(fixture.root.join("vex.lock")).unwrap();
    for path in ["middle", "middle/leaf"] {
        fs::create_dir_all(fixture.root.join(path).join("src")).unwrap();
        fs::write(
            fixture.root.join(path).join("src/lib.wave"),
            "pub fun value() {}\n",
        )
        .unwrap();
    }
    fs::write(
        fixture.root.join("vex.ws"),
        "{name=\"app\",dependencies=[{name=\"middle\",path=\"middle\"}]}",
    )
    .unwrap();
    fs::write(
        fixture.root.join("middle/vex.ws"),
        "{name=\"middle\",lib=true,dependencies=[{name=\"leaf\",path=\"leaf\"}]}",
    )
    .unwrap();
    fs::write(
        fixture.root.join("middle/leaf/vex.ws"),
        "{name=\"leaf\",lib=true,compiler=\"0.2.0-pre-beta\"}",
    )
    .unwrap();
    let output = fixture.command(&["check"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("leaf")
            && error.contains("0.2.0-pre-beta")
            && error.contains("0.2.1-pre-beta"),
        "{error}"
    );
    assert_eq!(fs::read(fixture.root.join("vex.lock")).unwrap(), lock);
    assert!(!fixture.root.join("target").exists());
    fs::write(
        fixture.root.join("middle/leaf/vex.ws"),
        "{name=\"leaf\",lib=true,compiler=\"0.2.1-pre-beta\"}",
    )
    .unwrap();
    assert!(fixture
        .command(&["check"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn run_status_only_reports_completed_successful_boundaries() {
    let fixture = Fixture::new();
    for (failure, code, running, finished) in [
        ("VEX_TEST_COMPILE_EXIT", "1", false, false),
        ("VEX_TEST_RUN_EXIT", "42", true, false),
        ("VEX_TEST_RUN_EXIT", "0", true, true),
    ] {
        let output = fixture
            .command(&["run"])
            .env(failure, code)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.contains("Running"), running, "{stderr}");
        assert_eq!(stderr.contains("Finished"), finished, "{stderr}");
    }
}

#[test]
fn conflict_reports_both_dependency_paths_as_structured_context() {
    let fixture = Fixture::new();
    for (directory, name, dependency) in [
        ("alpha", "alpha", Some("../first")),
        ("beta", "beta", Some("../second")),
        ("first", "shared", None),
        ("second", "shared", None),
    ] {
        let root = fixture.root.join(directory);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.wave"), "pub fun value() {}\n").unwrap();
        let dependency = dependency
            .map(|path| format!(",dependencies=[{{name=\"shared\",path=\"{path}\"}}]"))
            .unwrap_or_default();
        fs::write(
            root.join("vex.ws"),
            format!("{{name=\"{name}\",lib=true{dependency}}}"),
        )
        .unwrap();
    }
    fs::write(fixture.root.join("vex.ws"), "{name=\"app\",dependencies=[{name=\"alpha\",path=\"alpha\"},{name=\"beta\",path=\"beta\"}]}").unwrap();
    let lock = fs::read(fixture.root.join("vex.lock")).unwrap();
    let output = fixture
        .command(&["--message-file", "conflict.jsonl", "check"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let events: Vec<serde_json::Value> = fs::read_to_string(fixture.root.join("conflict.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let diagnostic = events
        .iter()
        .find(|event| event["event"] == "diagnostic")
        .unwrap();
    let context = diagnostic["context"].as_array().unwrap();
    for (key, value) in [
        ("first_dependency_path", "app -> alpha -> shared"),
        ("second_dependency_path", "app -> beta -> shared"),
    ] {
        assert!(
            context
                .iter()
                .any(|item| item["key"] == key && item["value"] == value),
            "{diagnostic}"
        );
    }
    assert!(!diagnostic["causes"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(fixture.root.join("vex.lock")).unwrap(), lock);
}
