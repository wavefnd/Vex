// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Content snapshots and scheduler inputs, distinct from cache eligibility.
use diagnostic::Error;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
#[derive(Clone)]
pub struct Inputs {
    pub files: Vec<PathBuf>,
    pub directories: Vec<PathBuf>,
    digest: String,
    pub complete_sources: bool,
    complete_toolchain_paths: bool,
}
impl Inputs {
    pub fn capture(plan: &Value, manifests: &[PathBuf], project: &Path) -> Result<Self, Error> {
        let mut files = BTreeSet::new();
        let mut directories = BTreeSet::new();
        let source_inputs = plan["source_inputs"].as_array();
        if let Some(inputs) = source_inputs {
            for input in inputs {
                let path = input
                    .as_str()
                    .ok_or_else(|| Error::compiler("source_inputs must be UTF-8 paths"))?;
                files.insert(project.join(path));
            }
        }
        for input in plan["inputs"]
            .as_array()
            .ok_or_else(|| Error::compiler("compiler inputs are missing"))?
        {
            if let Some(path) = input["path"].as_str() {
                files.insert(project.join(path));
            }
        }
        for manifest in manifests {
            files.insert(project.join(manifest));
            if let Some(parent) = manifest.parent() {
                directories.insert(project.join(parent));
            }
        }
        for name in ["vex.lock", "vex.toolchain.lock"] {
            let path = project.join(name);
            if path.exists() {
                files.insert(path);
            }
        }
        if let Some(script) = plan["linker_script"].as_str() {
            files.insert(project.join(script));
        }
        if let Some(args) = plan["link"]["args"].as_array() {
            for arg in args.iter().filter_map(Value::as_str) {
                let path = Path::new(arg);
                if path.is_absolute() && path.is_file() {
                    files.insert(path.into());
                }
            }
        }
        for path in &files {
            if let Some(parent) = path.parent() {
                directories.insert(parent.into());
            }
        }
        let files = files.into_iter().collect();
        let directories = directories.into_iter().collect();
        let mut inputs = Self {
            files,
            directories,
            digest: String::new(),
            complete_sources: source_inputs.is_some(),
            complete_toolchain_paths: false,
        };
        inputs.digest = inputs.fingerprint()?;
        Ok(inputs)
    }
    pub fn include_files(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Result<(), Error> {
        self.files.extend(paths);
        self.files.sort();
        self.files.dedup();
        self.digest = self.fingerprint()?;
        Ok(())
    }
    pub fn include_toolchain(
        &mut self,
        compiler: &crate::Compiler,
        archiver: Option<&Path>,
    ) -> Result<(), Error> {
        self.files
            .push(compiler.path().canonicalize().map_err(Error::environment)?);
        if let Some(path) = archiver {
            self.files
                .push(path.canonicalize().map_err(Error::environment)?);
        }
        if let Some(root) = compiler.selected_standard() {
            let mut pending = vec![root.to_owned()];
            let mut count = 0;
            while let Some(directory) = pending.pop() {
                state::reject_link(&directory).map_err(Error::environment)?;
                self.directories.push(directory.clone());
                for entry in std::fs::read_dir(&directory).map_err(Error::environment)? {
                    let path = entry.map_err(Error::environment)?.path();
                    if path.file_name().is_some_and(|n| n == ".git") {
                        continue;
                    }
                    state::reject_link(&path).map_err(Error::environment)?;
                    let metadata = std::fs::metadata(&path).map_err(Error::environment)?;
                    if metadata.is_dir() {
                        pending.push(path);
                    } else if metadata.is_file() {
                        self.files.push(path);
                    } else {
                        return Err(Error::environment("std contains a special file"));
                    }
                    count += 1;
                    if count > 65536 {
                        return Err(Error::environment(
                            "std scheduler inputs exceed 65536 entries",
                        ));
                    }
                }
            }
        }
        self.files.sort();
        self.files.dedup();
        self.directories.sort();
        self.directories.dedup();
        self.complete_toolchain_paths = true;
        self.digest = self.fingerprint()?;
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String, Error> {
        let mut hash = Sha256::new();
        hash.update(b"vex.build-inputs.v1\0");
        for path in &self.files {
            let text = path
                .to_str()
                .ok_or_else(|| Error::resolution("build inputs require UTF-8 paths"))?;
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
            hash.update(crate::identity::file_digest(path)?.as_bytes());
        }
        for directory in &self.directories {
            let mut names = std::fs::read_dir(directory)
                .map_err(Error::environment)?
                .filter_map(|e| match e {
                    Ok(e) => {
                        if e.path().is_dir() || e.path().extension().is_some_and(|x| x == "wave") {
                            Some(Ok(e.file_name()))
                        } else {
                            None
                        }
                    }
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(Error::environment)?;
            names.sort();
            for name in names {
                let name = name
                    .to_str()
                    .ok_or_else(|| Error::resolution("source directory entries must be UTF-8"))?;
                if name == "target" || name == ".vex" || name == ".git" {
                    continue;
                }
                hash.update((name.len() as u64).to_le_bytes());
                hash.update(name.as_bytes());
            }
        }
        Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }
    pub fn verify(&self) -> Result<(), Error> {
        if self.fingerprint()? != self.digest {
            return Err(Error::resolution("build inputs changed while the compiler was running; previous generation preserved"));
        }
        Ok(())
    }
    pub fn portable(&self, roots: &[(PathBuf, String)]) -> Result<Value, Error> {
        let mut entries = std::collections::BTreeMap::new();
        for path in &self.files {
            let root = roots
                .iter()
                .filter(|(root, _)| {
                    state::layout::path_form(path).starts_with(state::layout::path_form(root))
                })
                .max_by_key(|(root, _)| root.components().count());
            let (root,logical)=root.ok_or_else(||Error::resolution("portable provenance cannot name an input outside the selected source/std roots"))?;
            let relative = state::layout::relative_output(root, path).map_err(Error::resolution)?;
            let suffix = relative
                .to_str()
                .ok_or_else(|| Error::resolution("portable input path is not UTF-8"))?
                .replace('\\', "/");
            let logical = format!("{logical}/{suffix}");
            if entries
                .insert(logical, crate::identity::file_digest(path)?)
                .is_some()
            {
                return Err(Error::resolution("portable input identities collide"));
            }
        }
        let bytes = serde_json::to_vec(&entries).map_err(Error::internal)?;
        let digest = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Ok(
            json!({"schema_version":1,"sha256":digest,"files":entries,"complete_sources":self.complete_sources}),
        )
    }
    pub fn value(&self) -> Value {
        json!({"fingerprint":self.digest,"complete_sources":self.complete_sources,"complete_toolchain_paths":self.complete_toolchain_paths,"files":self.files,"directories":self.directories})
    }
}
pub fn escape(path: &Path) -> Result<String, Error> {
    let text = path
        .to_str()
        .ok_or_else(|| Error::resolution("depfile paths must be UTF-8"))?;
    if text.contains(['\n', '\r', '\0']) {
        return Err(Error::resolution(
            "depfile cannot represent a path with a newline or NUL",
        ));
    }
    Ok(text
        .replace('\\', "\\\\")
        .replace('$', "$$")
        .replace(' ', "\\ ")
        .replace('#', "\\#")
        .replace(':', "\\:"))
}
pub fn depfile(targets: &[PathBuf], inputs: &[Inputs]) -> Result<String, Error> {
    if targets.is_empty() {
        return Err(Error::usage(
            "depfile requires at least one artifact target",
        ));
    }
    let targets = targets
        .iter()
        .map(|p| escape(p))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let dependencies = inputs
        .iter()
        .flat_map(|i| i.files.iter().chain(&i.directories))
        .collect::<BTreeSet<_>>();
    let dependencies = dependencies
        .into_iter()
        .map(|p| escape(p))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    Ok(format!("{targets}: {dependencies}\n"))
}
