// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vex-path-lock-portability-{}-{id}",
            std::process::id()
        ));
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
fn path_lock_survives_moving_the_same_relative_package_tree() {
    let fixture = TestDir::new();
    let first = fixture.0.join("first");
    let second = fixture.0.join("second");
    create_tree(&first);

    let initial = vex(&first.join("app"), &["fetch"]);
    assert_success(&initial, "initial path dependency fetch");
    let lock = fs::read_to_string(first.join("app/vex.lock")).expect("lockfile must exist");
    let first_dependency = first
        .join("dep")
        .canonicalize()
        .expect("dependency path must canonicalize");
    assert!(
        !lock.contains(&first_dependency.to_string_lossy().to_string()),
        "lockfile leaked an absolute path:\n{lock}"
    );

    create_tree(&second);
    fs::write(second.join("app/vex.lock"), &lock).expect("lockfile must be copied");
    let moved = vex(&second.join("app"), &["fetch", "--locked", "--offline"]);
    assert_success(&moved, "locked offline fetch after moving package tree");
    assert_eq!(
        fs::read_to_string(second.join("app/vex.lock")).expect("moved lockfile must exist"),
        lock
    );
}

fn create_tree(root: &Path) {
    fs::create_dir_all(root.join("app")).expect("app directory must be created");
    fs::create_dir_all(root.join("dep")).expect("dependency directory must be created");
    fs::write(
        root.join("dep/vex.ws"),
        "{\n    name = \"dep\",\n    version = 0.1.0,\n    lib = true,\n    dependencies = []\n}\n",
    )
    .expect("dependency manifest must be written");
    fs::write(
        root.join("app/vex.ws"),
        "{\n    name = \"app\",\n    version = 0.1.0,\n    dependencies = [{ name = \"dep\", path = \"../dep\" }]\n}\n",
    )
    .expect("app manifest must be written");
}

fn vex(path: &Path, args: &[&str]) -> Output {
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
