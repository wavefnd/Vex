use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

mod support;
use support::git_url;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vex git lock test-{}-{id}#fixture",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("test directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn git_lock_keeps_transitive_graph_until_explicit_update() {
    let fixture = TestDir::new();
    let leaf = fixture.path().join("leaf");
    let middle = fixture.path().join("middle");
    let app = fixture.path().join("app");

    create_package(&leaf, "leaf", &[]);
    init_git(&leaf);
    let leaf_initial = commit_all(&leaf, "initial leaf");

    create_package(
        &middle,
        "middle",
        &[("leaf", git_url(&leaf), Some("master"))],
    );
    init_git(&middle);
    let middle_commit = commit_all(&middle, "initial middle");

    create_package(&app, "app", &[("middle", git_url(&middle), Some("master"))]);

    let missing_lock = vex(&app, &["fetch", "--locked"]);
    assert_failure(&missing_lock, "locked fetch without a lockfile");
    let missing_lock_stderr = String::from_utf8_lossy(&missing_lock.stderr);
    assert!(missing_lock_stderr.contains("required by `--locked`"));
    assert!(!app.join(".vex/deps/middle").exists());

    let first_fetch = vex(&app, &["fetch"]);
    assert_success(&first_fetch, "initial vex fetch");
    let first_stderr = String::from_utf8_lossy(&first_fetch.stderr);
    assert!(first_stderr.contains("Resolving"), "{first_stderr}");
    assert!(first_stderr.contains("Fetching"), "{first_stderr}");
    assert!(first_stderr.contains("Locking"), "{first_stderr}");

    let first_lock = read_lock(&app);
    assert!(first_lock.contains(&format!("commit = \"{leaf_initial}\"")));
    assert!(first_lock.contains(&format!("commit = \"{middle_commit}\"")));
    assert!(first_lock.contains("dependencies = [\"leaf\"]"));

    fs::write(leaf.join("REVISION.txt"), "new leaf revision\n")
        .expect("leaf update must be written");
    let leaf_updated = commit_all(&leaf, "update leaf");
    assert_ne!(leaf_initial, leaf_updated);

    let locked_fetch = vex(&app, &["fetch", "--locked", "--offline"]);
    assert_success(&locked_fetch, "locked offline vex fetch");
    let locked_stderr = String::from_utf8_lossy(&locked_fetch.stderr);
    assert!(locked_stderr.contains("Resolving"), "{locked_stderr}");
    assert!(
        !locked_stderr.contains("Fetching"),
        "locked fetch unexpectedly contacted Git: {locked_stderr}"
    );
    assert_eq!(read_lock(&app), first_lock);
    assert_eq!(
        git_stdout(&app.join(".vex/deps/leaf"), &["rev-parse", "HEAD"]),
        leaf_initial
    );

    fs::remove_dir_all(app.join(".vex/deps/leaf"))
        .expect("managed leaf checkout must be removed for the offline test");
    let missing_offline = vex(&app, &["fetch", "--locked", "--offline"]);
    assert_failure(&missing_offline, "offline fetch with a missing checkout");
    let missing_offline_stderr = String::from_utf8_lossy(&missing_offline.stderr);
    assert!(missing_offline_stderr.contains("not available locally in offline mode"));
    assert!(missing_offline_stderr.contains("run `vex fetch` while online"));
    assert_eq!(read_lock(&app), first_lock);

    let restored = vex(&app, &["fetch", "--locked"]);
    assert_success(&restored, "online locked fetch restoring a checkout");
    assert_eq!(read_lock(&app), first_lock);
    assert_eq!(
        git_stdout(&app.join(".vex/deps/leaf"), &["rev-parse", "HEAD"]),
        leaf_initial
    );

    let update = vex(&app, &["update"]);
    assert_success(&update, "vex update");
    let update_stderr = String::from_utf8_lossy(&update.stderr);
    assert!(update_stderr.contains("Fetching"), "{update_stderr}");
    assert!(update_stderr.contains("Locking"), "{update_stderr}");

    let updated_lock = read_lock(&app);
    assert!(updated_lock.contains(&format!("commit = \"{leaf_updated}\"")));
    assert!(!updated_lock.contains(&format!("commit = \"{leaf_initial}\"")));
    assert!(updated_lock.contains("dependencies = [\"leaf\"]"));
    assert_eq!(
        git_stdout(&app.join(".vex/deps/leaf"), &["rev-parse", "HEAD"]),
        leaf_updated
    );

    create_package(&app, "app", &[("middle", git_url(&middle), Some("other"))]);
    let mismatched = vex(&app, &["fetch", "--locked"]);
    assert_failure(&mismatched, "locked fetch with a changed manifest");
    let mismatched_stderr = String::from_utf8_lossy(&mismatched.stderr);
    assert!(mismatched_stderr.contains("does not match Git dependency `middle`"));
    assert_eq!(read_lock(&app), updated_lock);
}

fn create_package(path: &Path, name: &str, dependencies: &[(&str, String, Option<&str>)]) {
    fs::create_dir_all(path.join("src")).expect("package source directory must be created");
    fs::write(path.join("src/lib.wave"), "pub fun package_marker() {}\n")
        .expect("library entry must be written");
    let dependency_entries = dependencies
        .iter()
        .map(|(dependency, url, branch)| match branch {
            Some(branch) => format!(
                "        {{ name = \"{dependency}\", git = \"{url}\", branch = \"{branch}\" }}"
            ),
            None => format!("        {{ name = \"{dependency}\", git = \"{url}\" }}"),
        })
        .collect::<Vec<_>>();
    let dependencies = if dependency_entries.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{}\n    ]", dependency_entries.join(",\n"))
    };
    let manifest = format!(
        "{{\n    name = \"{name}\",\n    version = 0.1.0,\n    lib = true,\n    dependencies = {dependencies}\n}}\n"
    );
    fs::write(path.join("vex.ws"), manifest).expect("manifest must be written");
}

fn init_git(path: &Path) {
    let output = Command::new("git")
        .args(["init", "-q", "-b", "master"])
        .current_dir(path)
        .output()
        .expect("git init must start");
    assert_success(&output, "git init");
}

fn commit_all(path: &Path, message: &str) -> String {
    let add = Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("git add must start");
    assert_success(&add, "git add");

    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=Vex Test",
            "-c",
            "user.email=vex@example.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ])
        .current_dir(path)
        .output()
        .expect("git commit must start");
    assert_success(&commit, "git commit");
    git_stdout(path, &["rev-parse", "HEAD"])
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("git command must start");
    assert_success(&output, "git command");
    String::from_utf8(output.stdout)
        .expect("git output must be UTF-8")
        .trim()
        .to_string()
}

fn vex(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(args)
        .current_dir(path)
        .output()
        .expect("vex command must start")
}

fn read_lock(app: &Path) -> String {
    fs::read_to_string(app.join("vex.lock")).expect("vex.lock must exist")
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &Output, action: &str) {
    assert!(
        !output.status.success(),
        "{action} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
