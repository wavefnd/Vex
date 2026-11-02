// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Prepared compiler/std identities; observation never installs a toolchain.
use diagnostic::Error;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub const FILE: &str = "vex.toolchain.lock";
fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
pub fn file_digest(path: &Path) -> Result<String, Error> {
    state::reject_link(path).map_err(Error::environment)?;
    let mut input = state::open_regular(path).map_err(Error::environment)?;
    if !input.metadata().map_err(Error::environment)?.is_file() {
        return Err(Error::environment("identity input must be a regular file"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        if process::cancelled() {
            return Err(Error::new(
                diagnostic::Category::Cancelled,
                "toolchain hashing cancelled",
            ));
        }
        let n = input.read(&mut buffer).map_err(Error::environment)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex(hash.finalize()))
}
pub fn tree_digest(root: &Path) -> Result<String, Error> {
    state::reject_link(root).map_err(Error::environment)?;
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(Error::environment)? {
            let path = entry.map_err(Error::environment)?.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            state::reject_link(&path).map_err(Error::environment)?;
            let metadata = fs::symlink_metadata(&path).map_err(Error::environment)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.push(path);
            } else {
                return Err(Error::environment("std identity contains a special file"));
            }
            if pending.len() + files.len() > 65536 {
                return Err(Error::environment(
                    "std identity exceeds 65536 files/directories",
                ));
            }
        }
    }
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"vex.std-tree.v1\0");
    for path in files {
        let name = path
            .strip_prefix(root)
            .map_err(Error::internal)?
            .to_str()
            .ok_or_else(|| Error::resolution("std identity requires UTF-8 paths"))?
            .replace('\\', "/");
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(file_digest(&path)?.as_bytes());
    }
    Ok(hex(hash.finalize()))
}
pub fn host() -> String {
    source::graph::Context::host().triple
}
fn valid_digest(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub fn read(project: &Path) -> Result<Option<Value>, Error> {
    let path = project.join(FILE);
    state::reject_link(&path).map_err(Error::environment)?;
    let file = match state::open_regular(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::environment(e)),
    };
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::environment)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(Error::resolution("toolchain lock exceeds 8 MiB"));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(Error::resolution)?;
    if value["schema_version"] != 1
        || value.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !matches!(k.as_str(), "schema_version" | "hosts"))
        })
    {
        return Err(Error::resolution("unsupported toolchain lock schema"));
    }
    let hosts = value["hosts"]
        .as_object()
        .ok_or_else(|| Error::resolution("toolchain lock requires host identities"))?;
    for (host, identity) in hosts {
        source::graph::Context::new(host).map_err(Error::resolution)?;
        if identity
            .get("source_revision")
            .is_some_and(|value| !value.is_null())
        {
            return Err(Error::resolution("source revision pins require a reliable compiler provenance contract; this schema currently records null"));
        }
        if ["compiler_sha256", "std_sha256", "compiler_version"]
            .iter()
            .any(|key| identity.get(*key).is_none())
        {
            return Err(Error::resolution(
                "toolchain identity is missing a required field",
            ));
        }
        if !valid_digest(&identity["compiler_sha256"])
            || (!identity["std_sha256"].is_null() && !valid_digest(&identity["std_sha256"]))
            || identity["compiler_version"]
                .as_str()
                .is_none_or(|s| semver::Version::parse(s).is_err())
            || identity.as_object().is_none_or(|o| {
                o.keys().any(|k| {
                    !matches!(
                        k.as_str(),
                        "compiler_sha256" | "std_sha256" | "compiler_version" | "source_revision"
                    )
                })
            })
        {
            return Err(Error::resolution("malformed pinned compiler/std identity"));
        }
    }
    Ok(Some(value))
}
impl crate::Compiler {
    pub fn select_explicit(path: Option<&Path>) -> Result<Self, Error> {
        let Some(path) = path else {
            return Self::select();
        };
        Self::from_executable(path)
    }
    pub fn selected_standard(&self) -> Option<&Path> {
        self.standard.as_deref()
    }
    pub fn set_standard(&mut self, path: &Path) -> Result<(), Error> {
        self.standard = Some(path.canonicalize().map_err(Error::environment)?);
        self.identity = None;
        Ok(())
    }
    pub fn observe_identity(&mut self) -> Result<Value, Error> {
        let output = process::output(
            self.command().args(["print", "std-path", "--format=json"]),
            Duration::from_secs(60),
        )
        .map_err(Error::environment)?;
        if !output.status.success() {
            return Err(Error::compiler(
                "compiler cannot report the selected standard library",
            ));
        }
        let standard: Option<PathBuf> =
            serde_json::from_slice(&output.stdout).map_err(Error::compiler)?;
        if let Some(path) = standard.as_ref() {
            if !path.is_absolute() || !path.is_dir() {
                return Err(Error::environment(
                    "compiler reported an unavailable std root",
                ));
            }
            if self
                .standard
                .as_ref()
                .is_some_and(|explicit| explicit.canonicalize().ok() != path.canonicalize().ok())
            {
                return Err(Error::compiler(
                    "compiler did not retain the explicitly selected std root",
                ));
            }
        } else if self.standard.is_some() {
            return Err(Error::compiler(
                "compiler ignored the selected standard library",
            ));
        }
        let std_digest = standard.as_ref().map(|p| tree_digest(p)).transpose()?;
        let identity = json!({"compiler_version":self.version()?,"compiler_sha256":file_digest(&self.path.canonicalize().map_err(Error::environment)?)?,"std_sha256":std_digest,"source_revision":null});
        self.standard = standard;
        self.identity = Some(identity.clone());
        Ok(identity)
    }
    pub fn validate_identity(&mut self, project: &Path) -> Result<(), Error> {
        let Some(lock) = read(project)? else {
            return Ok(());
        };
        let expected = &lock["hosts"][host()];
        if expected.is_null() {
            return Err(Error::resolution(format!(
                "toolchain lock has no identity for host {}",
                host()
            )));
        }
        let observed = self.observe_identity()?;
        for key in ["compiler_version", "compiler_sha256", "std_sha256"] {
            if expected[key] != observed[key] {
                return Err(Error::resolution(format!("selected toolchain does not match vex.toolchain.lock: {key} differs; select the pinned compiler/std pair explicitly")));
            }
        }
        Ok(())
    }
    pub fn identity(&self) -> Option<&Value> {
        self.identity.as_ref()
    }
    pub fn capture_identity(&mut self, project: &Path, replace: bool) -> Result<Value, Error> {
        let _guard = state::Guard::acquire(false, |_, _| {}).map_err(Error::environment)?;
        let mut lock = read(project)?.unwrap_or_else(|| json!({"schema_version":1,"hosts":{}}));
        let host = host();
        if !lock["hosts"][&host].is_null() && !replace {
            return Err(Error::usage(
                "this host is already pinned; use --replace to update its identity explicitly",
            ));
        }
        let identity = self.observe_identity()?;
        lock["hosts"][&host] = identity.clone();
        let mut bytes = serde_json::to_string_pretty(&lock).map_err(Error::internal)?;
        bytes.push('\n');
        state::edit::replace(&project.join(FILE), &bytes).map_err(Error::environment)?;
        Ok(identity)
    }
}
