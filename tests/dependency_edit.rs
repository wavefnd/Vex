// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct F(PathBuf);
impl F {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-edit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let f = Self(root);
        f.cmd()
            .args(["new", "app"])
            .output()
            .map(|o| check(o, 0))
            .unwrap();
        f.cmd()
            .args(["new", "dep", "--lib"])
            .output()
            .map(|o| check(o, 0))
            .unwrap();
        f
    }
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vex"));
        c.current_dir(&self.0)
            .env("VEX_CACHE_HOME", self.0.join("cache"));
        c
    }
    fn app(&self) -> Command {
        let mut c = self.cmd();
        c.current_dir(self.0.join("app"));
        c
    }
    fn pair(&self) -> (String, String) {
        (
            fs::read_to_string(self.0.join("app/vex.ws")).unwrap(),
            fs::read_to_string(self.0.join("app/vex.lock")).unwrap(),
        )
    }
}
impl Drop for F {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn check(out: Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn add_remove_preserve_comments_and_restore_failed_resolution() {
    let f = F::new();
    let (raw, _) = f.pair();
    let raw = raw.replace("dependencies = []", "dependencies = [ /*owned comment*/ ]");
    fs::write(f.0.join("app/vex.ws"), &raw).unwrap();
    check(
        f.app()
            .args(["add", "dep", "--path", "../dep", "--offline"])
            .output()
            .unwrap(),
        0,
    );
    assert!(f.pair().0.contains("/*owned comment*/"));
    check(
        f.app()
            .args(["fetch", "--locked", "--offline"])
            .output()
            .unwrap(),
        0,
    );
    let pair = f.pair();
    check(
        f.app()
            .args(["add", "missing", "--path", "../missing", "--offline"])
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(pair, f.pair());
    check(
        f.app()
            .args(["remove", "dep", "--offline"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(f.pair().0.replace("\n        \n    ", ""), raw);
    check(f.app().args(["remove", "dep"]).output().unwrap(), 3);
}
#[test]
fn crash_recovery_is_readonly_until_normal_command() {
    for phase in ["VEX_TEST_EDIT_CRASH_MANIFEST", "VEX_TEST_EDIT_CRASH_LOCK"] {
        let f = F::new();
        let pair = f.pair();
        check(
            f.app()
                .env(phase, "1")
                .args(["add", "dep", "--path", "../dep", "--offline"])
                .output()
                .unwrap(),
            99,
        );
        let modified = f.pair();
        check(
            f.app()
                .args(["metadata", "--format-version", "2"])
                .output()
                .unwrap(),
            5,
        );
        assert_eq!(modified, f.pair());
        check(f.app().args(["fetch", "--offline"]).output().unwrap(), 0);
        assert_eq!(pair, f.pair());
        assert!(!f.0.join("app/.vex/edit.json").exists());
    }
}
#[test]
fn new_rejects_invalid_existing_and_preserves_unicode_destination() {
    let f = F::new();
    check(f.cmd().args(["new", "invalid-name"]).output().unwrap(), 2);
    assert!(!f.0.join("invalid-name").exists());
    check(f.cmd().args(["new", "app"]).output().unwrap(), 5);
    check(
        f.cmd()
            .args(["new", "유니코드", "--name", "portable"])
            .output()
            .unwrap(),
        0,
    );
    assert!(f.0.join("유니코드/vex.ws").is_file());
    assert!(!f.0.join("유니코드/.git").exists());
    check(
        f.cmd().args(["new", "git_app", "--git"]).output().unwrap(),
        0,
    );
    assert!(f.0.join("git_app/.git").is_dir());
}

#[test]
fn cache_dry_run_preserves_message_file_contract() {
    let f = F::new();
    check(
        f.app()
            .args([
                "--message-file",
                "preview.jsonl",
                "cache",
                "prune",
                "--dry-run",
            ])
            .output()
            .unwrap(),
        0,
    );
    assert!(!f.0.join("app/preview.jsonl").exists());
    fs::write(f.0.join("app/preview.jsonl"), "existing").unwrap();
    check(
        f.app()
            .args([
                "--message-file",
                "preview.jsonl",
                "cache",
                "prune",
                "--dry-run",
            ])
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(
        fs::read_to_string(f.0.join("app/preview.jsonl")).unwrap(),
        "existing"
    );
}
