// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use std::{fs, path::PathBuf};

pub fn new(args: &[String]) -> Result<(), Error> {
    let destination = args
        .first()
        .filter(|s| !s.starts_with('-'))
        .ok_or_else(|| Error::usage("usage: vex new <path> [--name <name>] [--lib] [--git]"))?;
    let mut name = None;
    let mut library = false;
    let mut git = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--lib" if !library => library = true,
            "--git" if !git => git = true,
            "--name" if name.is_none() => {
                i += 1;
                name = Some(
                    args.get(i)
                        .filter(|s| !s.starts_with('-'))
                        .ok_or_else(|| Error::usage("--name requires a package name"))?
                        .clone(),
                );
            }
            other => {
                return Err(Error::usage(format!(
                    "unknown or repeated new option `{other}`"
                )))
            }
        }
        i += 1;
    }
    let destination = PathBuf::from(destination);
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(Error::environment(
            "destination already exists; preserving it",
        ));
    }
    let inferred = destination
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::usage("destination needs a UTF-8 package name; use --name"))?;
    let name = name.as_deref().unwrap_or(inferred);
    manifest::validate_package_name(name)
        .map_err(|e| Error::usage(format!("{e}; choose an explicit valid --name")))?;
    manifest::validate_target_name(name).map_err(Error::usage)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .canonicalize()
        .map_err(Error::environment)?;
    let destination = parent.join(
        destination
            .file_name()
            .ok_or_else(|| Error::usage("destination must name a directory"))?,
    );
    // Exclusive directory creation never replaces even an empty destination.
    // The existing scaffold journal recovers interrupted file publication.
    fs::create_dir(&destination)?;
    let stage = destination.clone();
    let previous = std::env::current_dir()?;
    let result = (|| {
        std::env::set_current_dir(&stage)?;
        let guard = state::Guard::acquire(false, crate::ui::status).map_err(Error::environment)?;
        let manifest = manifest::render_new_manifest(name, "unknown", library)
            .replacen("format = 3", "format = 4", 1)
            .replacen("version = 0.1.0", "version = \"0.1.0\"", 1);
        let source = if library {
            "pub fun greet() {\n    println(\"Hello from library\");\n}\n"
        } else {
            "fun main() {\n    println(\"Hello World\");\n}\n"
        };
        let root = source::graph::Root {
            manifest: "vex.ws".into(),
            name: name.into(),
            version: "0.1.0".into(),
            edges: vec![],
            features: Default::default(),
        };
        let files = vec![
            (
                if library {
                    "src/lib.wave"
                } else {
                    "src/main.wave"
                },
                source.into(),
            ),
            (
                "vex.lock",
                lockfile::encode(lockfile::Lockfile {
                    version: 4,
                    packages: vec![],
                    roots: vec![root],
                }),
            ),
            (".gitignore", "/target/\n/.vex/\n".into()),
            ("vex.ws", manifest),
        ];
        state::initialize(&guard, &files).map_err(Error::environment)?;
        drop(guard);
        if git {
            let mut command = resolver::project_git_command();
            command.args(["init", "--quiet"]);
            command.current_dir(&stage);
            let status = process::status(
                &mut command,
                Some(std::time::Duration::from_secs(120)),
                false,
            )
            .map_err(Error::environment)?;
            if !status.success() {
                return Err(Error::environment("git init failed"));
            }
        }
        state::sync_dir(&parent)?;
        Ok(())
    })();
    let _ = std::env::set_current_dir(previous);
    // A failure preserves the new directory and journal for explicit recovery.
    // Never recursively remove it: a user may have placed files in it.
    result?;
    diagnostic::outln!("created Wave project at {}", destination.display());
    Ok(())
}
