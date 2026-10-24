#![cfg(target_os = "linux")]
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
mod support;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};
static ID: AtomicU64 = AtomicU64::new(0);
fn compiler() -> &'static Path {
    static COMPILER: OnceLock<PathBuf> = OnceLock::new();
    COMPILER.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("vex-fresh-compiler-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(if cfg!(windows) {
            "target-wavec.exe"
        } else {
            "target-wavec"
        });
        support::compile_fixture("target_wavec", &path);
        path
    })
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vex-ws-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let f = Self(root.canonicalize().unwrap());
        f.package(
            "alpha",
            "alpha",
            "targets={lib={},bins=[{name=\"tool\",path=\"src/main.wave\"}]}",
        );
        f.package("beta", "beta", "targets={bins=[{name=\"tool\",path=\"src/main.wave\"}]},dependencies=[{name=\"alpha\",path=\"../alpha\"}]");
        f.owner("workspace={members=[\"alpha\",\"beta\"],default_members=[\"beta\"]},profiles={fast={inherits=\"release\",opt_level=3}}");
        f
    }
    fn owner(&self, fields: &str) {
        fs::write(self.0.join("vex.ws"), format!("{{format=4,{fields}}}")).unwrap();
    }
    fn package(&self, directory: &str, name: &str, fields: &str) -> PathBuf {
        let path = self.0.join(directory);
        fs::create_dir_all(path.join("src/nested")).unwrap();
        for file in ["main.wave", "lib.wave"] {
            fs::write(path.join("src").join(file), "fun main() {}\n").unwrap();
        }
        fs::write(
            path.join("vex.ws"),
            format!("{{format=4,name=\"{name}\",version=\"1.0.0\",{fields}}}"),
        )
        .unwrap();
        path
    }
    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vex"));
        command
            .current_dir(cwd)
            .env("VEX_WAVEC", compiler())
            .env("TARGET_COMPILER_LOG", self.0.join("compiler.jsonl"))
            .env("VEX_CACHE_HOME", self.0.join("cache"))
            .env("NO_COLOR", "1");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(&self.0).args(args).output().unwrap()
    }
    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.0.join("compiler.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn builds(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|a| a.iter().any(|a| a == "build") && !a.iter().any(|a| a == "--dry-run"))
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn code(out: &Output, expected: i32) {
    assert_eq!(
        out.status.code(),
        Some(expected),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn cache_command(f: &Fixture) -> Command {
    let standard = f.0.join("standard");
    fs::create_dir_all(&standard).unwrap();
    if !standard.join("prelude.wave").exists() {
        fs::write(standard.join("prelude.wave"), "// std1").unwrap();
    }
    let mut command = f.command(&f.0);
    command.env("TARGET_STD_PATH", standard).arg("-v");
    command
}
#[test]
fn hashes_source_dependency_std_features_profile_and_compiler_not_mtime() {
    let f = Fixture::new();
    f.package("helper", "helper", r#"targets={lib={}}"#);
    f.package(
        "alpha",
        "alpha",
        r#"targets={lib={}},dependencies=[{name="helper",path="../helper"}],features={extra=[]}"#,
    );
    let build = |args: &[&str]| {
        let out = cache_command(&f)
            .args(["-p", "alpha", "build", "--lib"])
            .args(args)
            .output()
            .unwrap();
        code(&out, 0);
        out
    };
    build(&[]);
    assert_eq!(f.builds().len(), 1);
    let hit = build(&[]);
    assert!(String::from_utf8_lossy(&hit.stderr).contains("freshness: fresh"));
    assert_eq!(f.builds().len(), 1);
    let path = f.0.join("alpha/src/lib.wave");
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes).unwrap();
    build(&[]);
    assert_eq!(f.builds().len(), 1);
    fs::write(&path, "pub fun changed() {}\n").unwrap();
    build(&[]);
    assert_eq!(f.builds().len(), 2);
    fs::write(
        f.0.join("helper/src/lib.wave"),
        "pub fun dependency_changed() {}\n",
    )
    .unwrap();
    build(&[]);
    assert_eq!(f.builds().len(), 3);
    fs::write(f.0.join("standard/prelude.wave"), "// std2").unwrap();
    build(&[]);
    assert_eq!(f.builds().len(), 4);
    build(&["--features", "extra"]);
    assert_eq!(f.builds().len(), 5);
    build(&["--release"]);
    assert_eq!(f.builds().len(), 6);
    build(&["--target", "aarch64-unknown-linux-gnu"]);
    assert_eq!(f.builds().len(), 7);
    let changed = f.0.join("tools/target-wavec");
    fs::create_dir_all(changed.parent().unwrap()).unwrap();
    fs::copy(compiler(), &changed).unwrap();
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&changed)
        .unwrap()
        .write_all(b"different compiler identity")
        .unwrap();
    code(
        &cache_command(&f)
            .env("VEX_WAVEC", changed)
            .args(["-p", "alpha", "build", "--lib"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(f.builds().len(), 8);
}
#[test]
fn corrupt_or_partial_cache_is_a_miss_and_cleanup_owns_cache_files() {
    let f = Fixture::new();
    f.package("alpha", "alpha", r#"targets={lib={}}"#);
    let build = || {
        code(
            &cache_command(&f)
                .args(["-p", "alpha", "build"])
                .output()
                .unwrap(),
            0,
        );
    };
    build();
    assert_eq!(f.builds().len(), 1);
    let entry = fs::read_dir(f.0.join("target/.vex-cache"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(entry.join("0"), "corrupted").unwrap();
    build();
    assert_eq!(f.builds().len(), 2);
    assert_eq!(fs::read(entry.join("0")).unwrap(), b"corrupted");
    fs::write(entry.join("record.json"), "{").unwrap();
    build();
    assert_eq!(f.builds().len(), 3);
    // Modified cache files are preserved by clean, alongside ordinary ownership rules.
    code(&f.run(&["clean"]), 0);
    assert!(entry.join("0").exists());
}
#[test]
fn unknown_hosted_link_inputs_always_rebuild() {
    let f = Fixture::new();
    for _ in 0..2 {
        code(
            &cache_command(&f)
                .args(["-p", "beta", "build"])
                .output()
                .unwrap(),
            0,
        );
    }
    assert_eq!(f.builds().len(), 2);
}

#[test]
fn reused_run_artifacts_have_independent_inodes_and_program_always_executes() {
    let f = Fixture::new();
    f.package(
        "alpha",
        "alpha",
        r#"targets={lib={},bins=[{name="tool",path="src/main.wave"}]}"#,
    );
    for _ in 0..2 {
        let out = cache_command(&f)
            .env("TARGET_LINK_PROGRAM", compiler())
            .env("TARGET_PROGRAM_EXIT", "0")
            .env("TARGET_PROGRAM_LOG", f.0.join("runs"))
            .args(["-p", "alpha", "run", "--target", "wasm32-unknown-unknown"])
            .output()
            .unwrap();
        code(&out, 0);
    }
    assert_eq!(f.builds().len(), 1);
    assert_eq!(
        fs::read_to_string(f.0.join("runs")).unwrap(),
        "tool\ntool\n"
    );
    let paths = fs::read_dir(f.0.join("target/.vex-run"))
        .unwrap()
        .map(|e| {
            e.unwrap()
                .path()
                .join("wasm32-unknown-unknown/dev/packages/alpha/bin/tool.wasm")
        })
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 2);
    use std::os::unix::fs::MetadataExt;
    assert_ne!(
        fs::metadata(&paths[0]).unwrap().ino(),
        fs::metadata(&paths[1]).unwrap().ino()
    );
    let original = fs::read(&paths[1]).unwrap();
    fs::write(&paths[0], "changed old generation").unwrap();
    assert_eq!(fs::read(&paths[1]).unwrap(), original);
}
