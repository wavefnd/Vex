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
            "vex targeted update test-{}-{id}#fixture",
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
fn targeted_update_preserves_unrelated_commits_and_fetches() {
    let fixture = TestDir::new();
    let alpha = fixture.path().join("alpha");
    let beta = fixture.path().join("beta");
    let app = fixture.path().join("app");

    create_package(&alpha, "alpha", &[]);
    init_git(&alpha);
    let alpha_initial = commit_all(&alpha, "initial alpha");

    create_package(&beta, "beta", &[]);
    init_git(&beta);
    let beta_initial = commit_all(&beta, "initial beta");

    create_package(
        &app,
        "app",
        &[
            ("alpha", git_url(&alpha), Some("master")),
            ("beta", git_url(&beta), Some("master")),
        ],
    );

    assert_success(&vex(&app, &["fetch"]), "initial vex fetch");
    assert_eq!(locked_commit(&app, "alpha"), alpha_initial);
    assert_eq!(locked_commit(&app, "beta"), beta_initial);

    let locked_update = vex(&app, &["update", "alpha", "--locked"]);
    assert_failure(&locked_update, "locked targeted update");
    assert!(String::from_utf8_lossy(&locked_update.stderr)
        .contains("`--locked` cannot be used with `vex update`"));
    let offline_update = vex(&app, &["update", "alpha", "--offline"]);
    assert_failure(&offline_update, "offline targeted update");
    assert!(String::from_utf8_lossy(&offline_update.stderr)
        .contains("`--offline` cannot be used with `vex update`"));

    fs::write(alpha.join("REVISION.txt"), "new alpha revision\n")
        .expect("alpha update must be written");
    let alpha_updated = commit_all(&alpha, "update alpha");
    fs::write(beta.join("REVISION.txt"), "new beta revision\n")
        .expect("beta update must be written");
    let beta_updated = commit_all(&beta, "update beta");

    let update_alpha = vex(&app, &["update", "alpha"]);
    assert_success(&update_alpha, "targeted alpha update");
    let update_stderr = String::from_utf8_lossy(&update_alpha.stderr);
    assert!(update_stderr.contains("Updating"), "{update_stderr}");
    assert!(update_stderr.contains("`alpha`"), "{update_stderr}");
    assert_eq!(locked_commit(&app, "alpha"), alpha_updated);
    assert_eq!(locked_commit(&app, "beta"), beta_initial);
    assert_eq!(
        git_stdout(
            &app.join(".vex/deps/beta"),
            &["rev-parse", "refs/remotes/origin/master"]
        ),
        beta_initial,
        "the unrelated beta checkout must not fetch its updated branch"
    );

    let lock_before_unknown = read_lock(&app);
    let unknown = vex(&app, &["update", "missing"]);
    assert_failure(&unknown, "unknown targeted package update");
    let unknown_stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(
        unknown_stderr.contains("package `missing`"),
        "{unknown_stderr}"
    );
    assert!(
        unknown_stderr.contains("available Git packages: alpha, beta"),
        "{unknown_stderr}"
    );
    assert!(
        unknown_stderr.contains("vex update <package>..."),
        "{unknown_stderr}"
    );
    assert_eq!(read_lock(&app), lock_before_unknown);

    let update_all = vex(&app, &["update"]);
    assert_success(&update_all, "complete dependency update");
    assert_eq!(locked_commit(&app, "alpha"), alpha_updated);
    assert_eq!(locked_commit(&app, "beta"), beta_updated);
}

#[test]
fn targeted_update_accepts_transitive_packages_and_recalculates_their_graph() {
    let fixture = TestDir::new();
    let leaf = fixture.path().join("leaf");
    let added = fixture.path().join("added");
    let middle = fixture.path().join("middle");
    let app = fixture.path().join("app");

    create_package(&leaf, "leaf", &[]);
    init_git(&leaf);
    let leaf_initial = commit_all(&leaf, "initial leaf");

    create_package(&added, "added", &[]);
    init_git(&added);
    let added_commit = commit_all(&added, "initial added");

    create_package(
        &middle,
        "middle",
        &[("leaf", git_url(&leaf), Some("master"))],
    );
    init_git(&middle);
    let middle_initial = commit_all(&middle, "initial middle");

    create_package(&app, "app", &[("middle", git_url(&middle), Some("master"))]);
    assert_success(&vex(&app, &["fetch"]), "initial transitive fetch");

    fs::write(leaf.join("REVISION.txt"), "new leaf revision\n")
        .expect("leaf update must be written");
    let leaf_updated = commit_all(&leaf, "update leaf");

    let update_leaf = vex(&app, &["update", "leaf"]);
    assert_success(&update_leaf, "targeted transitive update");
    assert_eq!(locked_commit(&app, "leaf"), leaf_updated);
    assert_eq!(locked_commit(&app, "middle"), middle_initial);
    assert_ne!(leaf_initial, leaf_updated);

    create_package(
        &middle,
        "middle",
        &[
            ("leaf", git_url(&leaf), Some("master")),
            ("added", git_url(&added), Some("master")),
        ],
    );
    let middle_updated = commit_all(&middle, "add transitive dependency");

    let update_middle = vex(&app, &["update", "middle"]);
    assert_success(&update_middle, "targeted graph-changing update");
    assert_eq!(locked_commit(&app, "middle"), middle_updated);
    assert_eq!(locked_commit(&app, "leaf"), leaf_updated);
    assert_eq!(locked_commit(&app, "added"), added_commit);
    assert!(read_lock(&app).contains("dependencies = [\"added\", \"leaf\"]"));
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

fn locked_commit(app: &Path, package: &str) -> String {
    let lock = read_lock(app);
    let marker = format!("name = \"{package}\"");
    let package_entry = lock
        .split_once(&marker)
        .unwrap_or_else(|| panic!("package `{package}` must be present in lockfile"))
        .1;
    package_entry
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("commit = \"")
                .and_then(|value| value.strip_suffix("\","))
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("package `{package}` must have a locked commit"))
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
