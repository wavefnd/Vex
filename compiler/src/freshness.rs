// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Conservative content fingerprints. Unknown compiler/runtime/SDK inputs miss.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Clone)]
pub struct Inputs {
    sources: Vec<PathBuf>,
    files: Vec<PathBuf>,
    standard: PathBuf,
    environment: String,
    context: String,
}
fn bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}
fn file(hash: &mut Sha256, path: &Path) -> Result<(), String> {
    state::reject_link(path)?;
    if !fs::metadata(path).map_err(|e| e.to_string())?.is_file() {
        return Err("non-regular fingerprint input".into());
    }
    bytes(
        hash,
        &fs::metadata(path)
            .map_err(|e| e.to_string())?
            .len()
            .to_le_bytes(),
    );
    bytes(
        hash,
        path.to_str()
            .ok_or("non-UTF-8 fingerprint path")?
            .as_bytes(),
    );
    let mut reader = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buffer = [0; 65536];
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(())
}
fn tree(hash: &mut Sha256, path: &Path, sources: bool) -> Result<(), String> {
    state::reject_link(path)?;
    let mut children = fs::read_dir(path)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    children.sort_by_key(|e| e.file_name());
    for child in children {
        let path = child.path();
        let name = child.file_name();
        if name == ".git" || (sources && [".vex", "target"].iter().any(|n| name == *n)) {
            continue;
        }
        state::reject_link(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            tree(hash, &path, sources)?;
        } else if !sources
            || path.extension().is_some_and(|e| e == "wave")
            || name == "vex.ws"
            || name == "vex.lock"
        {
            file(hash, &path)?;
        }
    }
    Ok(())
}
impl Inputs {
    pub fn capture(
        compiler: &crate::Compiler,
        mut sources: Vec<PathBuf>,
        context: &str,
    ) -> Result<Self, String> {
        sources.sort();
        sources.dedup();
        let output = process::output(
            Command::new(compiler.path()).args(["print", "std-path", "--format=json"]),
            Duration::from_secs(60),
        )?;
        if !output.status.success() {
            return Err("compiler cannot describe its standard library".into());
        }
        let standard: PathBuf = serde_json::from_slice::<Option<String>>(&output.stdout)
            .map_err(|_| "compiler std-path is not a known path")?
            .ok_or("standard library path is unknown")?
            .into();
        if !standard.is_absolute() || !standard.is_dir() {
            return Err("standard library path is unavailable".into());
        }
        let mut files = vec![compiler.path().canonicalize().map_err(|e| e.to_string())?];
        // The executable alone does not identify a dynamically linked compiler.
        // Other hosts remain functional and conservatively compile every time.
        if !cfg!(target_os = "linux") {
            return Err("compiler runtime closure is not known on this host".into());
        }
        let mut magic = [0; 4];
        fs::File::open(&files[0])
            .and_then(|mut f| f.read_exact(&mut magic))
            .map_err(|e| e.to_string())?;
        if magic != *b"\x7fELF" {
            return Err("compiler runtime closure requires an ELF executable".into());
        }
        let runtime = process::output(
            Command::new("/usr/bin/ldd")
                .arg(&files[0])
                .env("LC_ALL", "C"),
            Duration::from_secs(60),
        )?;
        if !runtime.status.success() {
            return Err("compiler runtime dependency query was unavailable".into());
        }
        let text = String::from_utf8(runtime.stdout).map_err(|e| e.to_string())?;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if line.starts_with("linux-vdso.so.") {
                continue;
            }
            let path = line
                .split_once("=>")
                .map_or(line, |(_, tail)| tail.trim())
                .split_whitespace()
                .next()
                .ok_or("unknown compiler runtime input")?;
            if !Path::new(path).is_absolute() || !Path::new(path).is_file() {
                return Err("unresolved compiler runtime input".into());
            }
            files.push(Path::new(path).canonicalize().map_err(|e| e.to_string())?);
        }
        files.sort();
        files.dedup();
        let mut environment = std::env::vars_os()
            .filter(|(name, _)| {
                let name = name.to_string_lossy();
                name.starts_with("WAVE_")
                    || name.starts_with("LLVM_")
                    || name.starts_with("CLANG_")
                    || name.starts_with("DYLD_")
                    || matches!(
                        name.as_ref(),
                        "PATH"
                            | "PATHEXT"
                            | "HOME"
                            | "USERPROFILE"
                            | "SDKROOT"
                            | "MACOSX_DEPLOYMENT_TARGET"
                            | "LIBRARY_PATH"
                            | "LD_LIBRARY_PATH"
                            | "LD_PRELOAD"
                            | "LIB"
                            | "INCLUDE"
                            | "CC"
                            | "CXX"
                    )
            })
            .collect::<Vec<_>>();
        environment.sort();
        let environment = format!("{environment:?}");
        Ok(Self {
            sources,
            files,
            standard,
            environment,
            context: context.into(),
        })
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        let mut hash = Sha256::new();
        bytes(&mut hash, b"vex.freshness.v1");
        bytes(&mut hash, env!("CARGO_PKG_VERSION").as_bytes());
        bytes(&mut hash, self.environment.as_bytes());
        bytes(&mut hash, self.context.as_bytes());
        for source in &self.sources {
            tree(&mut hash, source, true)?;
        }
        tree(&mut hash, &self.standard, false)?;
        for input in &self.files {
            file(&mut hash, input)?;
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

pub(crate) fn key(
    base: &str,
    plan: &Value,
    args: &[String],
    generation: Option<&Path>,
) -> Result<String, String> {
    if !plan["sysroot"].is_null() {
        return Err("SDK/sysroot contents are not fully declared".into());
    }
    let mut hash = Sha256::new();
    bytes(&mut hash, base.as_bytes());
    if !plan["link"].is_null() {
        // Hosted links include implicit system libraries and SDK selection.
        if !plan["target"]
            .as_str()
            .is_some_and(|t| t.starts_with("wasm"))
        {
            return Err("linker/SDK closure is not completely declared".into());
        }
        let program = plan["link"]["program"]
            .as_str()
            .ok_or("linker program missing")?;
        if !Path::new(program).is_absolute() {
            return Err("linker identity is not absolute".into());
        }
        file(&mut hash, Path::new(program))?;
        let compiled = plan["compile"]
            .as_array()
            .ok_or("compile jobs missing")?
            .iter()
            .filter_map(|job| job["output"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for input in plan["link"]["inputs"]
            .as_array()
            .ok_or("linker inputs unknown")?
        {
            let input = input.as_str().ok_or("linker input missing")?;
            if !compiled.contains(input) {
                file(&mut hash, Path::new(input))?;
            }
        }
        let runtime = process::output(
            Command::new("/usr/bin/ldd").arg(program).env("LC_ALL", "C"),
            Duration::from_secs(60),
        )?;
        if !runtime.status.success() {
            return Err("linker runtime closure unknown".into());
        }
        for line in String::from_utf8(runtime.stdout)
            .map_err(|e| e.to_string())?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            if line.starts_with("linux-vdso.so.") {
                continue;
            }
            let path = line
                .split_once("=>")
                .map_or(line, |(_, tail)| tail.trim())
                .split_whitespace()
                .next()
                .ok_or("unknown linker runtime input")?;
            if !Path::new(path).is_absolute() {
                return Err("unresolved linker runtime input".into());
            }
            file(
                &mut hash,
                &Path::new(path).canonicalize().map_err(|e| e.to_string())?,
            )?;
        }
        for arg in plan["link"]["args"]
            .as_array()
            .ok_or("linker arguments unknown")?
        {
            let value = arg.as_str().ok_or("linker argument unknown")?;
            if ["-l", "-L", "@"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
                || value.contains("sysroot")
                || value.contains("script")
            {
                return Err(
                    "linker contains implicit libraries, SDK, script or response inputs".into(),
                );
            }
        }
    }
    if args.iter().any(|a| {
        a.starts_with("--sysroot=")
            || a.starts_with("-Clink-sysroot=")
            || a.starts_with("-Csysroot=")
            || a.starts_with("-Clinker=")
            || a.starts_with("--linker-script")
    }) {
        return Err("external SDK or linker input closure is unknown".into());
    }
    for input in plan["inputs"].as_array().ok_or("plan inputs missing")? {
        if input["kind"] == "wave" {
            file(
                &mut hash,
                Path::new(input["path"].as_str().ok_or("input path missing")?),
            )?;
        }
        if input["kind"] != "wave" {
            if ![
                "obj",
                "object",
                "archive",
                "static_library",
                "shared_library",
            ]
            .iter()
            .any(|kind| input["kind"] == *kind)
            {
                return Err("native source dependency closure is unknown".into());
            }
            file(
                &mut hash,
                Path::new(input["path"].as_str().ok_or("input path missing")?),
            )?;
        }
    }
    let normalize = |text: &str| {
        generation.and_then(|p| p.to_str()).map_or_else(
            || text.to_owned(),
            |g| text.replace(g, "target/.vex-run/GENERATION"),
        )
    };
    let mut normalized = plan.clone();
    normalized["execute"] = Value::Null;
    bytes(&mut hash, normalize(&normalized.to_string()).as_bytes());
    for arg in args {
        if arg != "--run" {
            bytes(&mut hash, normalize(arg).as_bytes());
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn content_digest(path: &Path) -> Result<String, String> {
    state::reject_link(path)?;
    if !path.is_file() {
        return Err("cached output is not a regular file".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn directory(root: &Path, key: &str) -> PathBuf {
    root.join("target/.vex-cache").join(key)
}
fn check_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(crate) fn restore(
    root: &Path,
    key: &str,
    outputs: &[PathBuf],
    optional: &[PathBuf],
) -> Result<bool, String> {
    if !check_key(key) {
        return Err("invalid freshness key".into());
    }
    state::reject_link(&root.join("target"))?;
    state::reject_link(&root.join("target/.vex-cache"))?;
    let directory = directory(root, key);
    state::reject_link(&directory)?;
    state::reject_link(&directory.join("record.json"))?;
    if fs::metadata(directory.join("record.json")).is_ok_and(|m| !m.is_file() || m.len() > 262144) {
        return Err("invalid cache metadata file".into());
    }
    let Ok(raw) = fs::read(directory.join("record.json")) else {
        return Ok(false);
    };
    let record: Value = serde_json::from_slice(&raw).map_err(|_| "corrupt freshness record")?;
    if record["version"] != 1 || record["key"] != key {
        return Err("unsupported freshness record".into());
    }
    let files = record["files"].as_array().ok_or("missing cache outputs")?;
    let destinations = outputs.iter().chain(optional).collect::<Vec<_>>();
    if files.len() != destinations.len() {
        return Err("cache output shape changed".into());
    }
    for (index, entry) in files.iter().enumerate() {
        if entry.is_null() {
            if index < outputs.len() {
                return Err("required cached output missing".into());
            }
            continue;
        }
        let path = directory.join(index.to_string());
        state::reject_link(&path)?;
        if content_digest(&path)? != entry.as_str().ok_or("invalid cache digest")? {
            return Err("cached output content was modified".into());
        }
    }
    for (index, (entry, destination)) in files.iter().zip(destinations).enumerate() {
        if entry.is_null() {
            if destination.exists() {
                fs::remove_file(destination).map_err(|e| e.to_string())?;
            }
            continue;
        }
        create_parents(root, destination)?;
        // A fresh inode is essential for independent running generations.
        fs::copy(directory.join(index.to_string()), destination).map_err(|e| e.to_string())?;
    }
    Ok(true)
}
fn create_parents(root: &Path, path: &Path) -> Result<(), String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let relative = path
        .parent()
        .unwrap()
        .strip_prefix(root)
        .map_err(|_| "output outside project")?;
    let mut parent = root.to_path_buf();
    for component in relative.components() {
        parent.push(component);
        state::ensure_dir(&parent)?;
    }
    Ok(())
}
pub(crate) fn store(
    root: &Path,
    key: &str,
    outputs: &[PathBuf],
    optional: &[PathBuf],
) -> Result<(), String> {
    if !check_key(key) {
        return Err("invalid freshness key".into());
    }
    let directory = directory(root, key);
    // Never replace an unknown or corrupted cache entry implicitly.
    if fs::symlink_metadata(&directory).is_ok() {
        return Ok(());
    }
    let mut entries = Vec::new();
    let mut sources = Vec::new();
    for (index, path) in outputs.iter().chain(optional).enumerate() {
        state::reject_link(path)?;
        if !path.is_file() && index >= outputs.len() {
            entries.push(Value::Null);
            continue;
        }
        entries.push(json!(content_digest(path)?));
        sources.push((index, path));
    }
    let record = directory.join("record.json");
    let mut owned = sources
        .iter()
        .map(|(i, _)| directory.join(i.to_string()))
        .collect::<Vec<_>>();
    owned.push(record.clone());
    let batch = state::artifacts::Batch::prepare(root, owned)
        .map_err(|e| format!("cannot own cache: {e}"))?;
    create_parents(root, &record)?;
    for (index, path) in sources {
        fs::copy(path, directory.join(index.to_string())).map_err(|e| e.to_string())?;
    }
    state::edit::replace(
        &record,
        &json!({"version":1,"key":key,"files":entries}).to_string(),
    )?;
    batch.verify_outputs()?;
    batch.finish()
}
