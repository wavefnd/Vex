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
    assert!(!app
        .join(".vex/deps/pkg_a4888af4e46c129c695ee32775a8c233f113c82e7cd4e6fd3cbb1fda5659f36a")
        .exists());

    let first_fetch = vex(&app, &["fetch"]);
    assert_success(&first_fetch, "initial vex fetch");
    assert!(app
        .join(".vex/deps/pkg_a4888af4e46c129c695ee32775a8c233f113c82e7cd4e6fd3cbb1fda5659f36a")
        .is_dir());
    assert!(app
        .join(".vex/deps/pkg_9f91161f43433e49a6de6db680d79f60159f2e4ac9172621a12846428158440b")
        .is_dir());
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
        git_stdout(
            &app.join(
                ".vex/deps/pkg_9f91161f43433e49a6de6db680d79f60159f2e4ac9172621a12846428158440b"
            ),
            &["rev-parse", "HEAD"]
        ),
        leaf_initial
    );

    fs::remove_dir_all(
        app.join(".vex/deps/pkg_9f91161f43433e49a6de6db680d79f60159f2e4ac9172621a12846428158440b"),
    )
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
        git_stdout(
            &app.join(
                ".vex/deps/pkg_9f91161f43433e49a6de6db680d79f60159f2e4ac9172621a12846428158440b"
            ),
            &["rev-parse", "HEAD"]
        ),
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
        git_stdout(
            &app.join(
                ".vex/deps/pkg_9f91161f43433e49a6de6db680d79f60159f2e4ac9172621a12846428158440b"
            ),
            &["rev-parse", "HEAD"]
        ),
        leaf_updated
    );

    create_package(&app, "app", &[("middle", git_url(&middle), Some("other"))]);
    let mismatched = vex(&app, &["fetch", "--locked"]);
    assert_failure(&mismatched, "locked fetch with a changed manifest");
    let mismatched_stderr = String::from_utf8_lossy(&mismatched.stderr);
    assert!(mismatched_stderr.contains("does not match Git dependency `middle`"));
    assert_eq!(read_lock(&app), updated_lock);
}

#[test]
fn dirty_managed_checkouts_are_rejected_without_discarding_changes() {
    let fixture = TestDir::new();
    let compiler = fixture_compiler(fixture.path());
    let dependency = fixture.path().join("dep");
    let app = fixture.path().join("app");
    create_package(&dependency, "dep", &[]);
    init_git(&dependency);
    let commit = commit_all(&dependency, "initial dependency");
    create_package(
        &app,
        "app",
        &[("dep", git_url(&dependency), Some("master"))],
    );

    assert_success(&vex(&app, &["fetch"]), "initial dependency fetch");
    let locked = read_lock(&app);
    let checkout =
        app.join(".vex/deps/pkg_8ce3e71ef8635d2bf27913bb680d7f88ad0238ea42c779c4b30f4e587d07da8e");
    let source = checkout.join("src/lib.wave");
    let untracked = checkout.join("UNTRACKED.wave");
    let original = fs::read_to_string(&source).unwrap();

    for (changed_path, content) in [
        (source.as_path(), "pub fun tampered() {}\n"),
        (untracked.as_path(), "untracked source\n"),
    ] {
        fs::write(changed_path, content).unwrap();
        for args in [
            &["fetch"][..],
            &["fetch", "--locked", "--offline"][..],
            &["check", "--dry-run", "--locked", "--offline"][..],
            &["check", "--locked", "--offline"][..],
            &["tree", "--locked", "--offline"][..],
        ] {
            // Select a deterministic compiler path without relying on PATH or
            // a managed install. Dirty-source preflight must reject the graph
            // before compiler planning or compilation.
            let output = Command::new(env!("CARGO_BIN_EXE_vex"))
                .args(args)
                .current_dir(&app)
                .env("VEX_WAVEC", &compiler)
                .output()
                .expect("Vex dirty-checkout preflight must start");
            assert_failure(&output, &format!("reject dirty checkout for {args:?}"));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("managed Git dependency `dep`"), "{stderr}");
            assert_reported_dirty_checkout_path(&stderr, "dep", &checkout);
            assert!(stderr.contains("preserve those changes"), "{stderr}");
            assert_eq!(fs::read_to_string(changed_path).unwrap(), content);
            assert_eq!(read_lock(&app), locked);
            assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), commit);
        }
        if changed_path == source {
            fs::write(&source, &original).unwrap();
        } else {
            fs::remove_file(changed_path).unwrap();
        }
    }

    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "locked dependency after changes are restored",
    );
}

#[test]
fn locked_offline_resolution_rejects_a_transitive_root_name_conflict() {
    let fixture = TestDir::new();
    let conflicting = fixture.path().join("conflicting");
    let middle = fixture.path().join("middle");
    let app = fixture.path().join("app");

    create_package(&conflicting, "app", &[]);
    init_git(&conflicting);
    let conflicting_commit = commit_all(&conflicting, "initial conflicting package");

    create_package(
        &middle,
        "middle",
        &[("app", git_url(&conflicting), Some("master"))],
    );
    init_git(&middle);
    let middle_commit = commit_all(&middle, "initial middle package");

    create_package(
        &app,
        "root",
        &[("middle", git_url(&middle), Some("master"))],
    );
    assert_success(&vex(&app, &["fetch"]), "initial dependency fetch");
    let locked = read_lock(&app);
    assert!(locked.contains(&format!("commit = \"{conflicting_commit}\"")));
    assert!(locked.contains(&format!("commit = \"{middle_commit}\"")));

    let conflicting_checkout =
        app.join(".vex/deps/pkg_a172cedcae47474b615c54d510a5d84a8dea3032e958587430b413538be3f333");
    fs::remove_dir_all(&conflicting_checkout).expect("conflicting checkout must be removed");
    create_package(&app, "app", &[("middle", git_url(&middle), Some("master"))]);

    let output = vex(&app, &["fetch", "--locked", "--offline"]);
    assert_failure(&output, "locked offline root-name conflict");
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
    assert!(stderr.contains(&git_url(&conflicting)), "{stderr}");
    assert!(!stderr.contains("Fetching"), "{stderr}");
    assert_eq!(read_lock(&app), locked);
    assert!(!conflicting_checkout.exists());
    assert_eq!(
        git_stdout(
            &app.join(
                ".vex/deps/pkg_a4888af4e46c129c695ee32775a8c233f113c82e7cd4e6fd3cbb1fda5659f36a"
            ),
            &["rev-parse", "HEAD"]
        ),
        middle_commit
    );
}

#[test]
fn git_lock_accepts_uppercase_and_mixed_case_commit_ids_without_recheckout() {
    let fixture = TestDir::new();
    let leaf = fixture.path().join("leaf");
    let app = fixture.path().join("app");

    create_package(&leaf, "leaf", &[]);
    init_git(&leaf);
    let leaf_commit = commit_all(&leaf, "initial leaf");

    create_package(&app, "app", &[("leaf", git_url(&leaf), Some("master"))]);

    let first_fetch = vex(&app, &["fetch"]);
    assert_success(&first_fetch, "initial vex fetch");

    let initial_lock = read_lock(&app);
    assert!(initial_lock.contains(&format!("commit = \"{leaf_commit}\"")));

    // Uppercase commit ID in vex.lock
    let uppercase_commit = leaf_commit.to_ascii_uppercase();
    assert_ne!(leaf_commit, uppercase_commit);
    let uppercase_lock = initial_lock.replace(&leaf_commit, &uppercase_commit);
    fs::write(app.join("vex.lock"), &uppercase_lock).expect("uppercase lock must be written");

    // Locked offline fetch must succeed without re-fetching or rewriting the lockfile
    let locked_fetch = vex(&app, &["fetch", "--locked", "--offline"]);
    assert_success(
        &locked_fetch,
        "locked offline fetch with uppercase commit ID",
    );
    let locked_stderr = String::from_utf8_lossy(&locked_fetch.stderr);
    assert!(
        !locked_stderr.contains("Fetching"),
        "locked fetch unexpectedly contacted Git: {locked_stderr}"
    );
    assert_eq!(read_lock(&app), uppercase_lock);
    assert_eq!(
        git_stdout(
            &app.join(
                ".vex/deps/pkg_9f91161f43433e49a6de6db680d79f60159f2e4ac9172621a12846428158440b"
            ),
            &["rev-parse", "HEAD"]
        ),
        leaf_commit
    );

    // Locked offline tree command must also succeed without modifying the lockfile
    let tree_output = vex(&app, &["tree", "--locked", "--offline"]);
    assert_success(&tree_output, "locked offline tree with uppercase commit ID");
    assert_eq!(read_lock(&app), uppercase_lock);

    // Mixed-case commit ID in vex.lock
    let mixed_case_commit: String = leaf_commit
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let mixed_lock = initial_lock.replace(&leaf_commit, &mixed_case_commit);
    fs::write(app.join("vex.lock"), &mixed_lock).expect("mixed-case lock must be written");

    let mixed_locked_fetch = vex(&app, &["fetch", "--locked", "--offline"]);
    assert_success(
        &mixed_locked_fetch,
        "locked offline fetch with mixed-case commit ID",
    );
    assert_eq!(read_lock(&app), mixed_lock);
    assert_eq!(
        git_stdout(
            &app.join(
                ".vex/deps/pkg_9f91161f43433e49a6de6db680d79f60159f2e4ac9172621a12846428158440b"
            ),
            &["rev-parse", "HEAD"]
        ),
        leaf_commit
    );

    // Genuinely invalid commit ID fails
    let invalid_lock =
        initial_lock.replace(&leaf_commit, "0123456789invalidcommithexbad012345678901");
    fs::write(app.join("vex.lock"), &invalid_lock).expect("invalid lock must be written");
    let invalid_fetch = vex(&app, &["fetch", "--locked", "--offline"]);
    assert_failure(&invalid_fetch, "locked fetch with invalid commit hash");
}

#[test]
fn managed_checkouts_detach_even_when_head_already_matches() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    let app = fixture.path().join("app");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    let commit = commit_all(&dep, "initial");
    create_package(&app, "app", &[("dep", git_url(&dep), None)]);
    assert_success(&vex(&app, &["fetch"]), "initial fetch");
    let checkout =
        app.join(".vex/deps/pkg_8ce3e71ef8635d2bf27913bb680d7f88ad0238ea42c779c4b30f4e587d07da8e");
    assert_eq!(
        git_stdout(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD"
    );
    let locked = read_lock(&app);
    for args in [
        &["fetch"][..],
        &["fetch", "--locked", "--offline"],
        &["update", "dep"],
    ] {
        git_stdout(&checkout, &["checkout", "master"]);
        assert_success(&vex(&app, args), "detach checkout");
        assert_eq!(
            git_stdout(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "HEAD"
        );
        assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), commit);
        assert_eq!(read_lock(&app), locked);
    }
}

#[test]
fn selector_free_updates_follow_changed_remote_default_branches() {
    for update in [&["update"][..], &["update", "dep"]] {
        let fixture = TestDir::new();
        let dep = fixture.path().join("dep");
        let app = fixture.path().join("app");
        create_package(&dep, "dep", &[]);
        init_git(&dep);
        let old = commit_all(&dep, "initial");
        create_package(&app, "app", &[("dep", git_url(&dep), None)]);
        assert_success(&vex(&app, &["fetch"]), "initial default branch fetch");
        let locked = read_lock(&app);
        git_stdout(&dep, &["checkout", "-b", "next"]);
        fs::write(dep.join("revision"), "next").unwrap();
        let new = commit_all(&dep, "new default branch");
        for args in [&["fetch"][..], &["fetch", "--locked", "--offline"]] {
            assert_success(&vex(&app, args), "reuse old default branch lock");
            assert_eq!(read_lock(&app), locked);
            assert_eq!(
                git_stdout(&app.join(".vex/deps/pkg_8ce3e71ef8635d2bf27913bb680d7f88ad0238ea42c779c4b30f4e587d07da8e"), &["rev-parse", "HEAD"]),
                old
            );
        }
        assert_success(&vex(&app, update), "refresh default branch");
        assert!(read_lock(&app).contains(&new));
        assert_eq!(
            git_stdout(
                &app.join(".vex/deps/pkg_8ce3e71ef8635d2bf27913bb680d7f88ad0238ea42c779c4b30f4e587d07da8e"),
                &["symbolic-ref", "refs/remotes/origin/HEAD"]
            ),
            "refs/remotes/origin/next"
        );
    }
}

#[test]
fn tag_and_exact_revision_selectors_remain_pinned_on_update() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    let pinned = commit_all(&dep, "tagged revision");
    git_stdout(&dep, &["tag", "v1"]);
    git_stdout(
        &dep,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "tag",
            "-a",
            "v1-annotated",
            "-m",
            "annotated",
        ],
    );
    for (i, selector) in [
        "tag = \"v1\"".to_string(),
        "tag = \"v1-annotated\"".to_string(),
        format!("rev = \"{pinned}\""),
    ]
    .iter()
    .enumerate()
    {
        let app = fixture.path().join(format!("app_{i}"));
        create_package(&app, "app", &[("dep", git_url(&dep), Some("master"))]);
        let manifest = fs::read_to_string(app.join("vex.ws"))
            .unwrap()
            .replace("branch = \"master\"", selector);
        fs::write(app.join("vex.ws"), manifest).unwrap();
        assert_success(&vex(&app, &["fetch"]), "fetch explicit selector");
        let locked = read_lock(&app);
        assert!(locked.contains(&pinned));
        fs::write(dep.join("revision"), format!("revision {i}")).unwrap();
        let moved = commit_all(&dep, "advance branch");
        for args in [
            &["fetch", "--locked", "--offline"][..],
            &["update", "dep"],
            &["update"],
        ] {
            assert_success(&vex(&app, args), "reuse explicit selector");
            assert_eq!(read_lock(&app), locked);
            assert_eq!(
                git_stdout(&app.join(".vex/deps/pkg_8ce3e71ef8635d2bf27913bb680d7f88ad0238ea42c779c4b30f4e587d07da8e"), &["rev-parse", "HEAD"]),
                pinned
            );
            assert!(!read_lock(&app).contains(&moved));
        }
    }
}

#[test]
fn dependency_git_ignores_inherited_repository_context() {
    let fixture = TestDir::new();
    let compiler = fixture_compiler(fixture.path());
    let dep = fixture.path().join("dep");
    let other = fixture.path().join("unrelated");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    let dep_commit = commit_all(&dep, "dependency");
    create_package(&other, "other", &[]);
    init_git(&other);
    let other_commit = commit_all(&other, "unrelated");
    fs::write(other.join("src/lib.wave"), "user changes\n").unwrap();
    let index = fs::read(other.join(".git/index")).unwrap();
    let overrides = [
        ("GIT_DIR", other.join(".git")),
        ("GIT_WORK_TREE", other.clone()),
        ("GIT_COMMON_DIR", other.join(".git")),
        ("GIT_INDEX_FILE", other.join(".git/index")),
        ("GIT_OBJECT_DIRECTORY", other.join(".git/objects")),
        (
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            other.join(".git/objects"),
        ),
        ("GIT_NAMESPACE", PathBuf::from("unrelated")),
    ];
    // Test each variable independently and then all together. Overrides belong
    // only to child processes; no global environment races with other tests.
    for case in 0..=overrides.len() {
        let app = fixture.path().join(format!("app_{case}"));
        create_package(&app, "app", &[("dep", git_url(&dep), Some("master"))]);
        for args in [
            &["fetch"][..],
            &["update", "dep"],
            &["fetch", "--locked", "--offline"],
            &["check", "--dry-run", "--locked", "--offline"],
        ] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
            command
                .args(args)
                .current_dir(&app)
                .env("VEX_WAVEC", &compiler);
            for (i, (name, value)) in overrides.iter().enumerate() {
                if case == i || case == overrides.len() {
                    command.env(name, value);
                }
            }
            let output = command.output().unwrap();
            assert_success(&output, &format!("environment case {case}: {args:?}"));
            assert_eq!(
                git_stdout(&app.join(".vex/deps/pkg_8ce3e71ef8635d2bf27913bb680d7f88ad0238ea42c779c4b30f4e587d07da8e"), &["rev-parse", "HEAD"]),
                dep_commit
            );
            assert_eq!(git_stdout(&other, &["rev-parse", "HEAD"]), other_commit);
            assert_eq!(fs::read(other.join(".git/index")).unwrap(), index);
            assert_eq!(
                fs::read_to_string(other.join("src/lib.wave")).unwrap(),
                "user changes\n"
            );
        }
    }
}

#[test]
fn url_rewrites_preserve_declared_identity_and_reject_invalid_origins() {
    let fixture = TestDir::new();
    let compiler = fixture_compiler(fixture.path());
    let dep = fixture.path().join("dep");
    let app = fixture.path().join("app");
    let home = fixture.path().join("isolated-home");
    fs::create_dir_all(&home).unwrap();
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    commit_all(&dep, "initial");
    let declared = "https://vex-fixture.invalid/dep.git";
    create_package(&app, "app", &[("dep", declared.to_string(), None)]);
    let config = home.join("gitconfig");
    let rewrite = format!("url.{}.insteadOf", git_url(&dep));
    assert_success(
        &Command::new("git")
            .args(["config", "--file"])
            .arg(&config)
            .args([&rewrite, declared])
            .output()
            .unwrap(),
        "isolated URL rewrite",
    );
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vex"))
            .args(args)
            .current_dir(&app)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("VEX_WAVEC", &compiler)
            .output()
            .unwrap()
    };
    for args in [
        &["fetch"][..],
        &["fetch"],
        &["fetch", "--locked", "--offline"],
        &["update", "dep"],
    ] {
        assert_success(&run(args), "URL-rewritten dependency");
    }
    let output = run(&["check", "--dry-run", "--locked", "--offline"]);
    assert_success(&output, "compiler planning with rewritten origin");
    let locked = read_lock(&app);
    assert!(locked.contains(declared));
    assert!(!locked.contains(&git_url(&dep)));
    let checkout =
        app.join(".vex/deps/pkg_8ce3e71ef8635d2bf27913bb680d7f88ad0238ea42c779c4b30f4e587d07da8e");
    for invalid in ["changed", "multiple", "missing"] {
        git_stdout(
            &checkout,
            &["config", "--replace-all", "remote.origin.url", declared],
        );
        match invalid {
            "changed" => {
                git_stdout(&checkout, &["config", "remote.origin.url", "file:///wrong"]);
            }
            "multiple" => {
                git_stdout(
                    &checkout,
                    &["config", "--add", "remote.origin.url", declared],
                );
            }
            _ => {
                git_stdout(&checkout, &["config", "--unset-all", "remote.origin.url"]);
            }
        }
        let output = run(&["fetch", "--locked", "--offline"]);
        assert_failure(&output, invalid);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("expected exactly one origin") && stderr.contains("help:"),
            "{stderr}"
        );
        assert_eq!(read_lock(&app), locked);
    }
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

fn assert_reported_dirty_checkout_path(stderr: &str, package: &str, expected: &Path) {
    let prefix = format!("managed Git dependency `{package}` at `");
    let reported = stderr
        .split_once(&prefix)
        .and_then(|(_, rest)| rest.split_once("` has local changes"))
        .map(|(path, _)| path)
        .unwrap_or_else(|| panic!("dirty-checkout path was not reported:\n{stderr}"));
    let reported = fs::canonicalize(reported).unwrap_or_else(|error| {
        panic!("failed to canonicalize reported path `{reported}`: {error}")
    });
    let expected = fs::canonicalize(expected).unwrap_or_else(|error| {
        panic!(
            "failed to canonicalize expected path `{}`: {error}",
            expected.display()
        )
    });

    assert_eq!(reported, expected, "{stderr}");
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

#[test]
fn unchanged_fetch_has_no_transaction_or_backup_growth_and_legacy_migrates_lazily() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    let app = fixture.path().join("app");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    commit_all(&dep, "initial");
    create_package(&app, "app", &[("dep", git_url(&dep), None)]);
    assert_success(&vex(&app, &["fetch"]), "initial fetch");
    let lock = read_lock(&app);
    let decoded = lockfile::decode(&lock).unwrap();
    let lockfile::LockedSource::Git { resolved, .. } = &decoded.package("dep").unwrap().source
    else {
        panic!()
    };
    let encoded = app.join(resolved);
    let initial_transactions = fs::read_dir(app.join(".vex/transactions")).unwrap().count();
    let initial_usage = tree_usage(&app.join(".vex"));
    let index = fs::metadata(encoded.join(".git/index"))
        .unwrap()
        .modified()
        .unwrap();
    for flags in [
        &["fetch"][..],
        &["fetch", "--locked", "--offline"],
        &["fetch", "--offline"],
    ] {
        assert_success(&vex(&app, flags), "reuse exact checkout");
        assert_eq!(
            fs::read_dir(app.join(".vex/transactions")).unwrap().count(),
            initial_transactions
        );
        assert_eq!(
            fs::metadata(encoded.join(".git/index"))
                .unwrap()
                .modified()
                .unwrap(),
            index
        );
        assert_eq!(read_lock(&app), lock);
        assert_eq!(
            tree_usage(&app.join(".vex")),
            initial_usage,
            "unchanged fetch grew managed state"
        );
    }
    let legacy = app.join(".vex/deps/dep");
    fs::rename(&encoded, &legacy).unwrap();
    let old = lock.replace(
        &resolved.to_string_lossy().replace('\\', "/"),
        ".vex/deps/dep",
    );
    fs::write(app.join("vex.lock"), &old).unwrap();
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "legacy locked reuse",
    );
    assert_eq!(read_lock(&app), old);
    assert!(!encoded.exists());
    assert_success(
        &vex(&app, &["fetch", "--offline"]),
        "lazy offline migration",
    );
    assert_eq!(read_lock(&app), lock);
    assert!(encoded.is_dir());
    assert!(legacy.is_dir(), "legacy source is retained; no implicit GC");
}

#[test]
fn sha256_repositories_are_locked_and_reused_offline() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    let app = fixture.path().join("app");
    create_package(&dep, "dep", &[]);
    let output = Command::new("git")
        .args(["init", "--object-format=sha256", "--initial-branch=master"])
        .current_dir(&dep)
        .output()
        .unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unknown option") || stderr.contains("unknown hash algorithm"),
            "{stderr}"
        );
        eprintln!("SHA-256 fixture skipped: installed Git lacks SHA-256 support");
        return;
    }
    git_stdout(&dep, &["config", "user.name", "Vex Test"]);
    git_stdout(&dep, &["config", "user.email", "vex@example.invalid"]);
    let commit = commit_all(&dep, "initial SHA-256");
    assert_eq!(commit.len(), 64);
    create_package(&app, "app", &[("dep", git_url(&dep), None)]);
    assert_success(&vex(&app, &["fetch"]), "SHA-256 fetch");
    let initial = read_lock(&app);
    assert!(initial.contains(&commit));
    // Removing the source proves offline reuse cannot be accidentally supplied
    // by a local-file fetch. Also exercise the historical v2 encoding of a real
    // SHA-256 pin before migrating it, without changing the pinned identity.
    let hidden = fixture.path().join("unavailable-sha256-source");
    fs::rename(&dep, &hidden).unwrap();
    let v2 = initial.replacen("version = 3,", "version = 2,", 1);
    assert_ne!(v2, initial);
    for historical in [&v2, &initial] {
        fs::write(app.join("vex.lock"), historical).unwrap();
        assert_success(
            &vex(&app, &["fetch", "--locked", "--offline"]),
            "historical SHA-256 offline reuse",
        );
        assert_eq!(read_lock(&app), *historical);
        assert_success(
            &vex(&app, &["fetch", "--offline"]),
            "historical SHA-256 migration",
        );
        assert_eq!(read_lock(&app), initial);
    }
    fs::rename(&hidden, &dep).unwrap();
    assert_success(
        &vex(&app, &["fetch", "--offline", "--locked"]),
        "SHA-256 offline",
    );
    assert_eq!(read_lock(&app), initial);
    fs::write(dep.join("changed"), "revision").unwrap();
    let updated = commit_all(&dep, "update SHA-256");
    assert_success(&vex(&app, &["update", "dep"]), "SHA-256 targeted update");
    let updated_lock = read_lock(&app);
    assert_eq!(updated.len(), 64);
    assert!(updated_lock.contains(&updated));
    assert!(!updated_lock.contains(&commit));
    fs::rename(&dep, &hidden).unwrap();
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "updated SHA-256 pin without remote",
    );
    assert_eq!(read_lock(&app), updated_lock);
}

#[test]
fn authenticated_declarations_use_user_rewrites_without_storing_or_printing_credentials() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    let app = fixture.path().join("app");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    commit_all(&dep, "initial");
    let authenticated = "https://vexuser:SYNTHETIC_PASSWORD@example.invalid/repo.git";
    create_package(&app, "app", &[("dep", authenticated.into(), None)]);
    let config = fixture.path().join("gitconfig");
    fs::write(
        &config,
        format!(
            "[url {:?}]\n\tinsteadOf = {}\n",
            git_url(&dep),
            authenticated
        ),
    )
    .unwrap();
    for args in [
        &["fetch"][..],
        &["update", "dep"],
        &["fetch", "--locked", "--offline"],
        &["info"],
        &["tree"],
        &["metadata", "--locked", "--offline"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .args(args)
            .current_dir(&app)
            .env("GIT_CONFIG_GLOBAL", &config)
            .output()
            .unwrap();
        assert_success(&output, "credential source operation");
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("SYNTHETIC_PASSWORD"));
            assert!(!String::from_utf8_lossy(bytes).contains("vexuser"));
        }
        let lock = read_lock(&app);
        assert!(!lock.contains("SYNTHETIC_PASSWORD") && !lock.contains("vexuser"));
        assert!(lock.contains("https://example.invalid/repo.git"));
    }
}

#[test]
fn checkout_encoding_handles_case_distinct_and_windows_device_names() {
    let fixture = TestDir::new();
    let app = fixture.path().join("app");
    let mut sources = Vec::new();
    for (index, name) in ["Foo", "foo", "CON"].into_iter().enumerate() {
        let path = fixture.path().join(format!("remote{index}"));
        create_package(&path, name, &[]);
        init_git(&path);
        commit_all(&path, "initial");
        sources.push((name, git_url(&path), None));
    }
    create_package(&app, "app", &sources);
    assert_success(&vex(&app, &["fetch"]), "case-distinct checkout publication");
    let lock = lockfile::decode(&read_lock(&app)).unwrap();
    let mut names = std::collections::BTreeSet::new();
    for package in lock.packages {
        let lockfile::LockedSource::Git { resolved, .. } = package.source else {
            panic!()
        };
        assert!(names.insert(resolved.to_string_lossy().to_ascii_lowercase()));
        assert!(app.join(resolved).join("src/lib.wave").is_file());
    }
    assert_success(
        &vex(&app, &["fetch", "--locked", "--offline"]),
        "case-distinct offline reuse",
    );
}

fn tree_usage(path: &Path) -> (u64, u64) {
    let mut usage = (0, 0);
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.metadata().unwrap();
        if metadata.is_dir() {
            let child = tree_usage(&entry.path());
            usage.0 += child.0;
            usage.1 += child.1;
        } else {
            usage.0 += 1;
            usage.1 += metadata.len();
        }
    }
    usage
}

#[test]
fn missing_revision_is_resolution_but_unavailable_transport_is_environment() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    commit_all(&dep, "initial");
    for (name, url, category, code) in [
        ("missing_ref", git_url(&dep), "resolution", 3),
        (
            "bad_transport",
            git_url(&fixture.path().join("absent")),
            "environment",
            5,
        ),
    ] {
        let app = fixture.path().join(name);
        create_package(&app, "app", &[("dep", url, Some("does-not-exist"))]);
        let output = vex(&app, &["--message-file", "report.jsonl", "fetch"]);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = fs::read_to_string(app.join("report.jsonl")).unwrap();
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["category"], category);
    }
}

#[test]
fn long_managed_paths_support_clone_update_and_locked_offline_reuse() {
    for object_format in ["sha1", "sha256"] {
        let fixture = TestDir::new();
        let dep = fixture.path().join("dep");
        create_package(&dep, "dep", &[]);
        fs::write(dep.join(".gitattributes"), "*.wave text eol=lf\n").unwrap();
        let nested_file = Path::new(
            "src/nested_directory_one_with_a_long_name/nested_directory_two_with_a_long_name/nested_directory_three_with_a_long_name/value.wave",
        );
        fs::create_dir_all(dep.join(nested_file).parent().unwrap()).unwrap();
        fs::write(dep.join(nested_file), "pub fun value() {}\n").unwrap();
        git_stdout(
            &dep,
            &[
                "init",
                "-b",
                "master",
                &format!("--object-format={object_format}"),
            ],
        );
        let initial = commit_all(&dep, "initial long-path fixture");

        let mut app = fixture.path().to_path_buf();
        while app.as_os_str().len() < 100 {
            app.push("path");
        }
        app.push("app");
        create_package(&app, "app", &[("dep", git_url(&dep), None)]);
        let config = fixture.path().join("preserved.gitconfig");
        fs::write(&config, "[vexTest]\n\tmarker = preserve\n").unwrap();
        let original_config = fs::read(&config).unwrap();
        let fetch = |args: &[&str]| {
            let output = Command::new(env!("CARGO_BIN_EXE_vex"))
                .current_dir(&app)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", &config)
                .output()
                .unwrap();
            assert_success(&output, "managed Git operation beyond MAX_PATH");
        };
        fetch(&["fetch"]);
        let lock = lockfile::decode(&read_lock(&app)).unwrap();
        let lockfile::LockedSource::Git { resolved, .. } = &lock.packages[0].source else {
            panic!()
        };
        let checkout = app.join(resolved);
        assert!(checkout.join(nested_file).as_os_str().len() > 260);
        assert_eq!(
            fs::read(checkout.join(nested_file)).unwrap(),
            b"pub fun value() {}\n"
        );
        assert!(read_lock(&app).contains(&initial));
        fetch(&["fetch", "--locked", "--offline"]);

        fs::write(dep.join(nested_file), "pub fun updated() {}\n").unwrap();
        let updated = commit_all(&dep, "update long-path fixture");
        fetch(&["update", "dep"]);
        assert!(read_lock(&app).contains(&updated));
        assert_eq!(
            fs::read(checkout.join(nested_file)).unwrap(),
            b"pub fun updated() {}\n"
        );
        fetch(&["fetch", "--locked", "--offline"]);
        let locked_bytes = fs::read(app.join("vex.lock")).unwrap();
        let restored = app.join("restored");
        create_package(&restored, "app", &[("dep", git_url(&dep), None)]);
        fs::write(restored.join("vex.lock"), &locked_bytes).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&restored)
            .args(["fetch", "--locked"])
            .env("GIT_CONFIG_GLOBAL", &config)
            .output()
            .unwrap();
        assert_success(&result, "restore a pinned checkout with long source paths");
        assert_eq!(fs::read(restored.join("vex.lock")).unwrap(), locked_bytes);
        assert_eq!(fs::read(&config).unwrap(), original_config);
        #[cfg(windows)]
        {
            // Some Git for Windows versions cache an explicit false setting
            // before applying -c. Preserve state and explain this upstream
            // limitation; also permit versions that correctly honor -c.
            let disabled = "[core]\n\tlongpaths = false\n";
            fs::write(&config, disabled).unwrap();
            let result = Command::new(env!("CARGO_BIN_EXE_vex"))
                .current_dir(&app)
                .args(["update", "dep"])
                .env("GIT_CONFIG_GLOBAL", &config)
                .output()
                .unwrap();
            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                match result.status.code() {
                    Some(3) => assert!(stderr.contains("has local changes"), "{stderr}"),
                    Some(5) => {}
                    other => panic!("unexpected outcome {other:?}: {stderr}"),
                }
                assert!(
                    stderr.contains("git config --show-origin --get-all core.longpaths"),
                    "{stderr}"
                );
            }
            assert_eq!(fs::read(app.join("vex.lock")).unwrap(), locked_bytes);
            assert_eq!(
                fs::read(checkout.join(nested_file)).unwrap(),
                b"pub fun updated() {}\n"
            );
            assert_eq!(fs::read_to_string(config).unwrap(), disabled);
        }
    }
}

#[test]
#[cfg(windows)]
fn unsupported_windows_checkout_directory_preserves_existing_state() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    commit_all(&dep, "initial");
    let app = fixture.path().join("app");
    create_package(&app, "app", &[("dep", git_url(&dep), None)]);
    assert_success(
        &vex(&app, &["fetch"]),
        "initial fetch before moving project",
    );
    let lock = fs::read(app.join("vex.lock")).unwrap();
    let parsed = lockfile::decode(std::str::from_utf8(&lock).unwrap()).unwrap();
    let lockfile::LockedSource::Git { resolved, .. } = &parsed.packages[0].source else {
        panic!()
    };
    let source = fs::read(app.join(resolved).join("src/lib.wave")).unwrap();
    let mut deep = fixture.path().to_path_buf();
    while deep.as_os_str().len() < 175 {
        deep.push("nested project directory");
    }
    fs::create_dir_all(&deep).unwrap();
    deep.push("app");
    fs::rename(&app, &deep).unwrap();
    let output = vex(&deep, &["update", "dep"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(5), "{stderr}");
    assert!(
        stderr.contains("move the project to a shorter path"),
        "{stderr}"
    );
    assert!(!stderr.contains("Cloning"), "{stderr}");
    assert_eq!(fs::read(deep.join("vex.lock")).unwrap(), lock);
    assert_eq!(
        fs::read(deep.join(resolved).join("src/lib.wave")).unwrap(),
        source
    );
}

#[test]
fn submodule_dependency_is_rejected_before_publication() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    let app = fixture.path().join("app");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    let oid = commit_all(&dep, "initial");
    let status = Command::new("git")
        .current_dir(&dep)
        .args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},nested"),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .current_dir(&dep)
        .args([
            "-c",
            "user.name=Vex Test",
            "-c",
            "user.email=vex@example.invalid",
            "commit",
            "-qm",
            "gitlink",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    create_package(&app, "app", &[("dep", git_url(&dep), Some("master"))]);
    let output = vex(&app, &["fetch"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("submodules"));
    assert!(!app.join("vex.lock").exists());
}

#[test]
fn failed_git_operations_keep_previous_graph_usable_offline() {
    for operation in [
        "fetch Git dependency",
        "checkout locked Git dependency commit",
    ] {
        let fixture = TestDir::new();
        let dep = fixture.path().join("dep");
        let app = fixture.path().join("app");
        create_package(&dep, "dep", &[]);
        init_git(&dep);
        commit_all(&dep, "initial");
        create_package(&app, "app", &[("dep", git_url(&dep), Some("master"))]);
        assert_success(&vex(&app, &["fetch"]), "initial");
        let old = fs::read(app.join("vex.lock")).unwrap();
        fs::write(dep.join("new.txt"), "new").unwrap();
        commit_all(&dep, "next");
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&app)
            .env("VEX_TEST_GIT_FAIL_ACTION", operation)
            .arg("update")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(operation));
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), old);
        assert_success(
            &vex(&app, &["fetch", "--locked", "--offline"]),
            "old graph after failure",
        );
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), old);
    }
}

#[test]
fn unadvertised_locked_object_is_fetched_exactly_and_pruned_object_never_substituted() {
    let fixture = TestDir::new();
    let dep = fixture.path().join("dep");
    let app = fixture.path().join("app");
    create_package(&dep, "dep", &[]);
    init_git(&dep);
    let old = commit_all(&dep, "old root");
    create_package(&app, "app", &[("dep", git_url(&dep), Some("master"))]);
    assert_success(&vex(&app, &["fetch"]), "initial fetch");
    let lock = fs::read(app.join("vex.lock")).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&dep)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["checkout", "--orphan", "replacement"]);
    commit_all(&dep, "new root");
    git(&["branch", "-D", "master"]);
    git(&["branch", "-m", "master"]);
    git(&["config", "uploadpack.allowAnySHA1InWant", "true"]);
    fs::remove_dir_all(app.join(".vex/deps")).unwrap();
    assert_success(
        &vex(&app, &["fetch", "--locked"]),
        "fetch exact unadvertised commit",
    );
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
    fs::remove_dir_all(app.join(".vex/deps")).unwrap();
    git(&["reflog", "expire", "--expire=now", "--all"]);
    git(&["gc", "--prune=now"]);
    let output = vex(&app, &["fetch", "--locked"]);
    assert_eq!(output.status.code(), Some(3));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(&old) && error.contains("never substitute"),
        "{error}"
    );
    assert_eq!(fs::read(app.join("vex.lock")).unwrap(), lock);
}

#[test]
fn late_invalid_manifest_or_cycle_cannot_publish_earlier_updates() {
    for cycle in [false, true] {
        let fixture = TestDir::new();
        let first = fixture.path().join("first");
        let last = fixture.path().join("last");
        let app = fixture.path().join("app");
        for (path, name) in [(&first, "first"), (&last, "last")] {
            create_package(path, name, &[]);
            init_git(path);
            commit_all(path, "initial");
        }
        create_package(
            &app,
            "app",
            &[
                ("first", git_url(&first), Some("master")),
                ("last", git_url(&last), Some("master")),
            ],
        );
        assert_success(&vex(&app, &["fetch"]), "initial graph");
        let locked = fs::read(app.join("vex.lock")).unwrap();
        fs::write(first.join("new.txt"), "staged but never published").unwrap();
        commit_all(&first, "first update");
        if cycle {
            create_package(&last, "last", &[("first", git_url(&first), Some("master"))]);
            create_package(&first, "first", &[("last", git_url(&last), Some("master"))]);
            commit_all(&first, "first cycle edge");
        } else {
            fs::write(last.join("vex.ws"), "{ broken manifest").unwrap();
        }
        commit_all(&last, "invalid later package");
        assert_failure(&vex(&app, &["update"]), "reject invalid candidate graph");
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), locked);
        assert_success(
            &vex(&app, &["fetch", "--locked", "--offline"]),
            "reuse complete previous graph",
        );
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), locked);
    }
}

fn fixture_compiler(root: &Path) -> PathBuf {
    let source = root.join("fake.rs");
    let binary = root.join(if cfg!(windows) { "wavec.exe" } else { "wavec" });
    fs::write(&source, include_str!("fixtures/fake_wavec.rs")).unwrap();
    assert_success(
        &Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap(),
        "compile fixture",
    );
    binary
}

#[test]
fn failed_clone_keeps_old_graph_and_never_publishes_partial_checkout() {
    for long_path in [false, true] {
        let fixture = TestDir::new();
        let dep = fixture.path().join("dep");
        let next = fixture.path().join("next");
        let app = if long_path {
            // Exercise the >200 init/fetch route without exceeding Windows'
            // deliberate 240 UTF-16-unit checkout directory limit.
            let padding = 100usize
                .saturating_sub(fixture.path().as_os_str().len() + 5)
                .max(1);
            fixture.path().join("n".repeat(padding)).join("app")
        } else {
            fixture.path().join("app")
        };
        for (path, name) in [(&dep, "dep"), (&next, "next")] {
            create_package(path, name, &[]);
            init_git(path);
            commit_all(path, "initial");
        }
        create_package(&app, "app", &[("dep", git_url(&dep), None)]);
        assert_success(&vex(&app, &["fetch"]), "initial");
        let original_manifest = fs::read(app.join("vex.ws")).unwrap();
        let original_lock = fs::read(app.join("vex.lock")).unwrap();
        let old_checkouts = fs::read_dir(app.join(".vex/deps")).unwrap().count();
        create_package(
            &app,
            "app",
            &[("dep", git_url(&dep), None), ("next", git_url(&next), None)],
        );
        let output = Command::new(env!("CARGO_BIN_EXE_vex"))
            .current_dir(&app)
            .env("VEX_TEST_GIT_FAIL_ACTION", "clone Git dependency")
            .arg("fetch")
            .output()
            .unwrap();
        assert_failure(&output, "injected clone failure");
        assert!(String::from_utf8_lossy(&output.stderr).contains("clone Git dependency"));
        assert_eq!(fs::read(app.join("vex.lock")).unwrap(), original_lock);
        assert_eq!(
            fs::read_dir(app.join(".vex/deps")).unwrap().count(),
            old_checkouts
        );
        fs::write(app.join("vex.ws"), original_manifest).unwrap();
        assert_success(
            &vex(&app, &["fetch", "--locked", "--offline"]),
            "preserved old graph",
        );
    }
}
