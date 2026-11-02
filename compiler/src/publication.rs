// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! A durable, per-target generation commit. The result pointer is authoritative.
use diagnostic::Error;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};
const JOURNAL: &str = ".vex/build-publication.json";
fn error(value: impl std::fmt::Display) -> Error {
    Error::environment(value)
}
fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
pub fn key(paths: &[PathBuf], root: &Path) -> Result<String, Error> {
    let names = state::artifacts::validate_output_paths(root, paths).map_err(error)?;
    Ok(hex(Sha256::digest(
        serde_json::to_vec(&names).map_err(Error::internal)?,
    )))
}
pub fn pointer(root: &Path, key: &str) -> PathBuf {
    root.join("target/.vex-results").join(format!("{key}.json"))
}
fn bounded_json(path: &Path) -> Result<Option<Value>, Error> {
    state::reject_link(path).map_err(error)?;
    let file = match state::open_regular(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(e)),
    };
    if !file.metadata().map_err(error)?.is_file() {
        return Err(error("publication record is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(error("publication record exceeds 8 MiB"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(error)
}
fn safe(root: &Path, value: &Value) -> Result<PathBuf, Error> {
    let name = value
        .as_str()
        .ok_or_else(|| error("publication path must be a string"))?;
    let path = Path::new(name);
    state::artifacts::validate_output_paths(root, &[path.to_path_buf()]).map_err(error)?;
    let mut checked = root.to_path_buf();
    for part in path.components() {
        if !matches!(part, Component::Normal(_)) {
            return Err(error("invalid publication path"));
        }
        checked.push(part);
        state::reject_link(&checked).map_err(error)?;
    }
    Ok(root.join(path))
}
fn relative(root: &Path, path: &Path) -> Result<String, Error> {
    let absolute = root.join(path);
    state::layout::relative_output(root, &absolute)
        .map_err(error)?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| error("publication path is not UTF-8"))
}
pub fn atomic_copy(source: &Path, destination: &Path) -> Result<(), Error> {
    state::reject_link(source).map_err(error)?;
    state::reject_link(destination).map_err(error)?;
    let parent = destination
        .parent()
        .ok_or_else(|| error("missing output parent"))?;
    state::ensure_tree(parent).map_err(error)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(error)?
        .as_nanos();
    let temporary = parent.join(format!(".vex-publish-{}-{nonce}", std::process::id()));
    let result = (|| {
        let mut input = fs::File::open(source).map_err(error)?;
        let metadata = input.metadata().map_err(error)?;
        if !metadata.is_file() {
            return Err(error("staged output must be a regular file"));
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(error)?;
        std::io::copy(&mut input, &mut output).map_err(error)?;
        output
            .set_permissions(metadata.permissions())
            .map_err(error)?;
        output.sync_all().map_err(error)?;
        drop(output);
        state::atomic_rename(&temporary, destination).map_err(error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn record_text(path: &Path, value: &Value) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| error("missing record parent"))?;
    state::ensure_tree(parent).map_err(error)?;
    let mut text = serde_json::to_string(value).map_err(Error::internal)?;
    text.push('\n');
    state::edit::replace(path, &text).map_err(error)
}
fn fingerprint(path: &Path) -> Result<Value, Error> {
    state::artifacts::file_fingerprint(path)
        .map(|v| json!(v))
        .map_err(error)
}
pub fn recover(root: &Path, readonly: bool) -> Result<(), Error> {
    let journal = root.join(JOURNAL);
    let Some(record) = bounded_json(&journal)? else {
        return Ok(());
    };
    if readonly {
        return Err(error(
            "artifact publication recovery is pending; run a normal build first",
        ));
    }
    if record["schema_version"] != 1 {
        return Err(error("unsupported artifact publication journal"));
    }
    let point = safe(root, &record["pointer"])?;
    let entries = record["entries"]
        .as_array()
        .ok_or_else(|| error("invalid publication journal entries"))?;
    if entries.is_empty() || entries.len() > 4096 {
        return Err(error("invalid publication entry count"));
    }
    let valid_hash = |value: &Value| {
        value.is_null()
            || value.as_str().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
    };
    if record["pointer_after"].is_null() || !valid_hash(&record["pointer_after"]) {
        return Err(error("invalid publication commit identity"));
    }
    let last = entries.last().unwrap();
    if safe(root, &last["path"])? != point || last["after"] != record["pointer_after"] {
        return Err(error("publication commit point is not the final entry"));
    }
    let destinations = entries[..entries.len() - 1]
        .iter()
        .map(|e| safe(root, &e["path"]))
        .collect::<Result<Vec<_>, _>>()?;
    if destinations.is_empty() || pointer(root, &key(&destinations, root)?) != point {
        return Err(error(
            "publication pointer does not match its artifact destinations",
        ));
    }
    let committed = fingerprint(&point)? == record["pointer_after"];
    let mut paths = Vec::new();
    let mut identities = std::collections::BTreeSet::new();
    let mut backups = std::collections::BTreeSet::new();
    // Validate the complete journal before moving a single byte.
    for entry in entries {
        let destination = safe(root, &entry["path"])?;
        if !identities.insert(destination.clone()) {
            return Err(error("duplicate publication destination"));
        }
        if !valid_hash(&entry["before"]) || !valid_hash(&entry["after"]) {
            return Err(error("invalid publication content identity"));
        }
        let owned =
            json!(state::artifacts::recorded_fingerprint(root, &destination).map_err(error)?);
        if !entry["before"].is_null()
            && owned != entry["before"]
            && !(committed && owned == entry["after"])
        {
            return Err(error("publication journal lacks previous output ownership"));
        }
        let current = fingerprint(&destination)?;
        if current != entry["before"] && current != entry["after"] {
            return Err(error("published artifact was modified; preserve build-publication.json for manual recovery"));
        }
        if !entry["backup"].is_null() {
            let backup = safe(root, &entry["backup"])?;
            if entry["before"].is_null()
                || !backups.insert(backup.clone())
                || destinations.iter().any(|p| p == &backup)
                || !backup.starts_with(root.join("target/.vex-run"))
                || fingerprint(&backup)? != entry["before"]
            {
                return Err(error("publication backup is missing or modified"));
            }
        } else if !entry["before"].is_null() {
            return Err(error("publication entry lacks its backup"));
        }
        if committed && current != entry["after"] {
            return Err(error(
                "committed artifact generation is incomplete; preserve recovery state",
            ));
        }
        paths.push(destination);
    }
    if !committed {
        for entry in entries.iter().rev() {
            let destination = safe(root, &entry["path"])?;
            if fingerprint(&destination)? == entry["before"] {
                continue;
            }
            if entry["backup"].is_null() {
                fs::remove_file(&destination).map_err(error)?;
                state::sync_dir(destination.parent().unwrap()).map_err(error)?;
            } else {
                atomic_copy(&safe(root, &entry["backup"])?, &destination)?;
            }
        }
    }
    state::artifacts::record_verified(root, &paths).map_err(error)?;
    fs::remove_file(&journal).map_err(error)?;
    state::sync_dir(journal.parent().unwrap()).map_err(error)
}

pub fn publish(
    root: &Path,
    generation: &Path,
    final_paths: &[PathBuf],
    staged: &[PathBuf],
    metadata: Value,
) -> Result<Value, Error> {
    recover(root, false)?;
    if final_paths.len() != staged.len() || final_paths.is_empty() {
        return Err(Error::internal("invalid generation output shape"));
    }
    let final_paths = final_paths.iter().map(|p| root.join(p)).collect::<Vec<_>>();
    let staged = staged.iter().map(|p| root.join(p)).collect::<Vec<_>>();
    let name = key(&final_paths, root)?;
    let point = pointer(root, &name);
    let mut artifacts = Vec::new();
    for (destination, source) in final_paths.iter().zip(&staged) {
        if !source.is_file() {
            continue;
        }
        let role = if destination
            .components()
            .any(|p| p.as_os_str() == ".vex-obj")
        {
            "intermediate"
        } else if destination.extension().is_some_and(|e| e == "pdb") {
            "debug"
        } else if metadata["artifact_kind"] == "shared"
            && destination.extension().is_some_and(|e| e == "lib")
        {
            "import_library"
        } else {
            "primary"
        };
        artifacts.push(json!({"role":role,"path":source,"published_path":destination,"sha256":crate::identity::file_digest(source)?}));
    }
    let mut protected = final_paths.clone();
    protected.push(point.clone());
    let _ = state::artifacts::Batch::prepare_with_optional(root, Vec::<PathBuf>::new(), protected)
        .map_err(error)?;
    if let Some(previous) = bounded_json(&point)? {
        let old = previous["artifacts"].as_array();
        if previous["metadata"] == metadata
            && old.is_some_and(|old| {
                old.len() == artifacts.len()
                    && old.iter().zip(&artifacts).all(|(a, b)| {
                        a["published_path"] == b["published_path"]
                            && a["sha256"] == b["sha256"]
                            && a["path"].as_str().is_some_and(|p| {
                                crate::identity::file_digest(Path::new(p)).ok().as_deref()
                                    == a["sha256"].as_str()
                            })
                    })
            })
        {
            return Ok(previous);
        }
    }
    let archive_identity = metadata
        .get("archive")
        .map(|a| json!({"sha256":a["sha256"],"format":a["format"],"target":a["target"]}));
    let provenance = json!({"schema_version":1,"package":metadata["package"],"package_target":metadata["package_target"],"profile":metadata["profile"],"target":metadata["target"],"features":metadata["features"],"settings":{"mode":metadata["settings"]["mode"],"relocation":metadata["settings"]["relocation"],"entry":metadata["settings"]["entry"],"startup":metadata["settings"]["startup"],"native":metadata["settings"]["native"],"reproducible":metadata["settings"]["reproducible"],"sysroot_configured":!metadata["settings"]["sysroot"].is_null(),"explicit_linker":!metadata["settings"]["linker"].is_null()},"compiler":metadata["compiler"],"inputs":metadata["portable_inputs"],"native_requirements":metadata["native_requirements"].as_array().map(|entries|entries.iter().map(|r|json!({"package_id":r["package_id"],"package":r["package"],"version":r["version"],"path":r["declared_path"],"name":r["name"],"target":r["target"]})).collect::<Vec<_>>()),"archive":archive_identity});
    let record = json!({"schema":"vex.artifact-generation","schema_version":1,"generation":generation,"metadata":metadata,"provenance":provenance,"artifacts":artifacts});
    let staged_point = generation.join("result.json");
    state::artifacts::Batch::prepare(root, [staged_point.clone()])
        .map_err(error)?
        .finish()
        .map_err(error)?;
    record_text(&staged_point, &record)?;
    state::artifacts::record_verified(root, std::slice::from_ref(&staged_point)).map_err(error)?;
    let mut destinations = final_paths.clone();
    destinations.push(point.clone());
    let mut sources = staged;
    sources.push(staged_point);
    let _ = state::artifacts::Batch::prepare_with_optional(
        root,
        Vec::<PathBuf>::new(),
        destinations.clone(),
    )
    .map_err(error)?;
    let backup_dir = generation.join("backup");
    let mut entries = Vec::new();
    for (index, (destination, source)) in destinations.iter().zip(&sources).enumerate() {
        let before = fingerprint(destination)?;
        let after = fingerprint(source)?;
        let backup = if before.is_null() {
            None
        } else {
            let backup = backup_dir.join(index.to_string());
            let batch = state::artifacts::Batch::prepare(root, [backup.clone()]).map_err(error)?;
            state::ensure_tree(backup.parent().unwrap()).map_err(error)?;
            fs::copy(destination, &backup).map_err(error)?;
            batch.finish().map_err(error)?;
            Some(relative(root, &backup)?)
        };
        entries.push(json!({"path":relative(root,destination)?,"before":before,"after":after,"backup":backup}));
    }
    let journal = json!({"schema_version":1,"pointer":relative(root,&point)?,"pointer_after":fingerprint(sources.last().unwrap())?,"entries":entries});
    record_text(&root.join(JOURNAL), &journal)?;
    let outcome = (|| {
        for (index, (destination, source)) in destinations.iter().zip(&sources).enumerate() {
            let entry = &journal["entries"][index];
            if fingerprint(destination)? != entry["before"] {
                return Err(error("output changed during publication"));
            }
            if entry["before"] == entry["after"] {
                continue;
            }
            if entry["after"].is_null() {
                if destination.exists() {
                    fs::remove_file(destination).map_err(error)?;
                }
            } else {
                atomic_copy(source, destination)?;
            }
            #[cfg(debug_assertions)]
            if std::env::var("VEX_TEST_PUBLICATION_CRASH_AFTER")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                == Some(index)
            {
                std::process::exit(97);
            }
            #[cfg(debug_assertions)]
            if std::env::var("VEX_TEST_PUBLICATION_FAIL_AFTER")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                == Some(index)
            {
                return Err(error("injected artifact publication failure"));
            }
        }
        Ok(())
    })();
    let recovery = recover(root, false);
    match (outcome, recovery) {
        (Ok(()), Ok(())) => Ok(record),
        (Err(primary), Ok(())) => Err(primary),
        (Err(primary), Err(secondary)) => {
            Err(primary.with_help(format!("recovery also failed: {secondary}")))
        }
        (Ok(()), Err(e)) => Err(e),
    }
}
