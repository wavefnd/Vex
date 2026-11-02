// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Exclusive ownership and recoverable replacement of reusable consumer reports.
use diagnostic::Error;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
const OWNERS: &str = ".vex/report-ownership.json";
const JOURNAL: &str = ".vex/report-publication.json";
fn error(e: impl std::fmt::Display) -> Error {
    Error::environment(e)
}
fn read(path: &Path) -> Result<Option<Value>, Error> {
    state::reject_link(path).map_err(error)?;
    let file = match state::open_regular(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(e)),
    };
    if !file.metadata().map_err(error)?.is_file() {
        return Err(error("consumer report metadata is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(error("consumer report metadata exceeds 8 MiB"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(error)
}
fn valid_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn validate_registry(value: &Value) -> Result<(), Error> {
    if value["schema_version"] != 1
        || value["files"].as_object().is_none_or(|files| {
            files.iter().any(|(path, hash)| {
                !Path::new(path).is_absolute()
                    || Path::new(path)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    || !valid_hash(hash)
            })
        })
    {
        return Err(error("invalid report ownership registry"));
    }
    Ok(())
}
fn owners(root: &Path) -> Result<Value, Error> {
    let value = read(&root.join(OWNERS))?.unwrap_or_else(|| json!({"schema_version":1,"files":{}}));
    validate_registry(&value)?;
    Ok(value)
}
fn name(path: &Path) -> Result<&str, Error> {
    path.to_str()
        .ok_or_else(|| Error::usage("consumer report paths must be UTF-8"))
}
fn digest(path: &Path) -> Result<Value, Error> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(e) => Err(error(e)),
        Ok(m) => {
            if !m.is_file() {
                return Err(error("consumer report must be a regular file"));
            }
            crate::identity::file_digest(path).map(|s| json!(s))
        }
    }
}
fn check_parents(path: &Path) -> Result<(), Error> {
    for parent in path.ancestors() {
        state::reject_link(parent).map_err(error)?;
    }
    Ok(())
}
pub fn destination(path: &Path) -> Result<PathBuf, Error> {
    check_parents(path)?;
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Error::usage("report path escapes filesystem root"));
                }
            }
            part => normalized.push(part),
        }
    }
    let mut ancestor = normalized.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::usage("report destination has no existing ancestor"))?;
    }
    let real = ancestor.canonicalize().map_err(error)?;
    let tail = normalized.strip_prefix(ancestor).map_err(error)?;
    Ok(if tail.as_os_str().is_empty() {
        real
    } else {
        real.join(tail)
    })
}
pub fn preflight(root: &Path, paths: &[PathBuf]) -> Result<(), Error> {
    let registry = owners(root)?;
    let mut unique = std::collections::BTreeSet::new();
    for path in paths {
        if !path.is_absolute() {
            return Err(Error::usage(
                "consumer report destination must resolve to an absolute path",
            ));
        }
        check_parents(path)?;
        if !unique.insert(path) {
            return Err(Error::usage(
                "artifact manifest and depfile need different destinations",
            ));
        }
        let key = name(path)?;
        let current = digest(path)?;
        if !current.is_null() && registry["files"][key] != current {
            return Err(error(
                "refusing to overwrite an unowned or modified consumer report",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| Error::usage("consumer report lacks a parent directory"))?;
        if !parent.is_dir() && !parent.starts_with(root) {
            return Err(error(
                "consumer report parent must already exist outside the build root",
            ));
        }
        if path.starts_with(root.join(".vex")) || path.starts_with(root.join("target/.vex-run")) {
            return Err(Error::usage("consumer report overlaps private Vex state"));
        }
    }
    Ok(())
}
fn replace(path: &Path, value: &Value) -> Result<(), Error> {
    let mut text = serde_json::to_string(value).map_err(Error::internal)?;
    text.push('\n');
    state::edit::replace(path, &text).map_err(error)
}
pub fn recover(root: &Path, readonly: bool) -> Result<(), Error> {
    let Some(record) = read(&root.join(JOURNAL))? else {
        return Ok(());
    };
    if readonly {
        return Err(error(
            "consumer report recovery pending; run a normal build",
        ));
    }
    if record["schema_version"] != 1 {
        return Err(error("unsupported consumer report journal"));
    }
    let entries = record["entries"]
        .as_array()
        .ok_or_else(|| error("invalid report transaction entries"))?;
    if entries.len() > 2 || entries.is_empty() {
        return Err(error("invalid report transaction size"));
    }
    validate_registry(&record["before_registry"])?;
    validate_registry(&record["after_registry"])?;
    let registry = owners(root)?;
    let committed = registry == record["after_registry"];
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let path = Path::new(
            entry["path"]
                .as_str()
                .ok_or_else(|| error("missing report journal path"))?,
        );
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
            || path.starts_with(root.join(".vex"))
            || path.starts_with(root.join("target/.vex-run"))
            || !path.is_absolute()
            || !seen.insert(path)
        {
            return Err(error("invalid or duplicate report journal path"));
        }
        check_parents(path)?;
        if (!entry["before"].is_null() && !valid_hash(&entry["before"]))
            || !valid_hash(&entry["after"])
            || record["after_registry"]["files"][name(path)?] != entry["after"]
        {
            return Err(error("invalid report transaction identity"));
        }
        if entry["before"].is_null() != entry["backup"].is_null() {
            return Err(error("report transaction lacks a valid backup"));
        }
        let current = digest(path)?;
        if current != entry["before"] && current != entry["after"] {
            return Err(error(
                "consumer report changed during recovery; preserve the journal",
            ));
        }
        if !entry["before"].is_null()
            && record["before_registry"]["files"][name(path)?] != entry["before"]
        {
            return Err(error("journal lacks previous report ownership"));
        }
        if !entry["backup"].is_null() {
            let backup = Path::new(
                entry["backup"]
                    .as_str()
                    .ok_or_else(|| error("invalid report backup"))?,
            );
            check_parents(backup)?;
            if !backup.starts_with(root.join("target/.vex-run"))
                || backup
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                || digest(backup)? != entry["before"]
            {
                return Err(error("report backup is unavailable or modified"));
            }
        }
        if committed && current != entry["after"] {
            return Err(error("committed consumer report is incomplete"));
        }
    }
    let mut expected = record["before_registry"].clone();
    for entry in entries {
        expected["files"][entry["path"].as_str().unwrap()] = entry["after"].clone();
    }
    if expected != record["after_registry"] {
        return Err(error("report journal changes unrelated ownership"));
    }
    if !committed {
        if registry != record["before_registry"] {
            return Err(error(
                "report ownership changed outside pending transaction",
            ));
        }
        for entry in entries.iter().rev() {
            let path = Path::new(entry["path"].as_str().unwrap());
            if digest(path)? == entry["before"] {
                continue;
            }
            if entry["before"].is_null() {
                fs::remove_file(path).map_err(error)?;
                state::sync_dir(path.parent().unwrap()).map_err(error)?;
            } else {
                crate::publication::atomic_copy(
                    Path::new(
                        entry["backup"]
                            .as_str()
                            .ok_or_else(|| error("missing report backup"))?,
                    ),
                    path,
                )?;
            }
        }
    }
    fs::remove_file(root.join(JOURNAL)).map_err(error)?;
    state::sync_dir(&root.join(".vex")).map_err(error)
}
pub fn publish(root: &Path, files: &[(PathBuf, String)]) -> Result<(), Error> {
    if files.is_empty() {
        return Ok(());
    }
    recover(root, false)?;
    preflight(
        root,
        &files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
    )?;
    let generation = crate::create_run_generation()?;
    let generation = root.join(generation);
    let _lease = state::artifacts::RunLease::create(root, &generation).map_err(error)?;
    let before = owners(root)?;
    let mut after = before.clone();
    let mut entries = Vec::new();
    let mut sources = Vec::new();
    for (index, (path, text)) in files.iter().enumerate() {
        let staged = generation.join(format!("report-{index}"));
        let batch = state::artifacts::Batch::prepare(root, [staged.clone()]).map_err(error)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(error)?;
        // Windows requires write access for sync_all (FlushFileBuffers).
        file.write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(error)?;
        drop(file);
        batch.finish().map_err(error)?;
        let previous = digest(path)?;
        let next = digest(&staged)?;
        let backup = if previous.is_null() {
            None
        } else {
            let backup = generation.join(format!("report-backup-{index}"));
            let batch = state::artifacts::Batch::prepare(root, [backup.clone()]).map_err(error)?;
            fs::copy(path, &backup).map_err(error)?;
            batch.finish().map_err(error)?;
            Some(backup)
        };
        after["files"][name(path)?] = next.clone();
        entries.push(json!({"path":path,"before":previous,"after":next,"backup":backup}));
        sources.push(staged);
    }
    if after == before {
        return Ok(());
    }
    let journal = json!({"schema_version":1,"before_registry":before,"after_registry":after,"entries":entries});
    replace(&root.join(JOURNAL), &journal)?;
    let result = (|| {
        for (index, ((destination, _), source)) in files.iter().zip(sources).enumerate() {
            if digest(destination)? != journal["entries"][index]["before"] {
                return Err(error("consumer report was modified during publication"));
            }
            if journal["entries"][index]["before"] != journal["entries"][index]["after"] {
                crate::publication::atomic_copy(&source, destination)?;
            }
            #[cfg(debug_assertions)]
            if std::env::var("VEX_TEST_REPORT_CRASH_AFTER")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                == Some(index)
            {
                std::process::exit(97);
            }
            #[cfg(debug_assertions)]
            if std::env::var("VEX_TEST_REPORT_FAIL_AFTER")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                == Some(index)
            {
                return Err(error("injected report publication failure"));
            }
        }
        replace(&root.join(OWNERS), &after)
    })();
    let recovery = recover(root, false);
    match (result, recovery) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) => Err(e),
        (Ok(()), Err(e)) => Err(e),
        (Err(e), Err(other)) => Err(e.with_help(format!("report recovery also failed: {other}"))),
    }
}
pub fn load_result(root: &Path, key: &str) -> Result<Value, Error> {
    let value = read(&crate::publication::pointer(root, key))?
        .ok_or_else(|| error("completed build has no artifact result"))?;
    if value["schema_version"] != 1 || value["schema"] != "vex.artifact-generation" {
        return Err(error("invalid artifact generation result"));
    }
    for artifact in value["artifacts"]
        .as_array()
        .ok_or_else(|| error("missing artifact result entries"))?
    {
        let path = Path::new(
            artifact["path"]
                .as_str()
                .ok_or_else(|| error("missing artifact path"))?,
        );
        if !state::layout::path_form(path)
            .starts_with(state::layout::path_form(&root.join("target/.vex-run")))
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(error("artifact result escapes its generation"));
        }
        check_parents(path)?;
        if !valid_hash(&artifact["sha256"]) || digest(path)? != artifact["sha256"] {
            return Err(error("completed artifact is missing or corrupt"));
        }
    }
    Ok(value)
}
