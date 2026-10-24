// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
#![allow(dead_code)] // Each integration test uses a different subset of helpers.

use std::fmt::Write;
use std::path::Path;

/// Cross CI copies verified target executables; native tests compile the same
/// source locally. A missing/stale prebuilt fixture must never fall back to an
/// emulated compiler and silently change the acceptance lane.
#[allow(dead_code)]
pub fn compile_fixture(name: &str, output: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.rs"));
    compile_fixture_from(name, &source, output);
}

pub fn compile_fixture_from(name: &str, source: &Path, output: &Path) {
    if let Some(directory) = std::env::var_os("VEX_TEST_FIXTURE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        assert_eq!(
            std::fs::read(source).unwrap(),
            std::fs::read(directory.join(format!("{name}.rs"))).unwrap(),
            "prebuilt fixture source differs from this checkout"
        );
        std::fs::copy(directory.join(name), output).unwrap();
    } else {
        let result = std::process::Command::new("rustc")
            .arg("--edition=2021")
            .arg(source)
            .arg("-o")
            .arg(output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

/// Git's trace destinations and command-line paths do not accept Rust's
/// verbatim Windows prefix. Keep filesystem spelling (not URL escaping).
pub fn git_path(path: &Path) -> String {
    normalize_git_path(&path.to_string_lossy(), cfg!(windows))
}

pub fn normalize_git_path(path: &str, windows: bool) -> String {
    if windows {
        let path = path
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_owned());
        path.replace('\\', "/")
    } else {
        path.to_owned()
    }
}

pub fn git_url(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path = percent_encode_path(&git_path(&path));
    if path.starts_with("//") {
        format!("file:{path}")
    } else if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(byte as char);
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}
