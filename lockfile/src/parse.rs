// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use wson::{loads_versions, WsonMap, WsonValue};

use crate::{LockedPackage, LockedSource, Lockfile, LOCKFILE_NAME, LOCKFILE_VERSION};

pub fn parse_lockfile(raw: &str) -> Result<Lockfile, String> {
    parse_lockfile_inner(raw).map_err(|error| format!("{error}\nhelp: restore a valid `{LOCKFILE_NAME}` from version control; to regenerate intentionally, preserve the invalid file elsewhere and run `vex fetch`"))
}

fn parse_lockfile_inner(raw: &str) -> Result<Lockfile, String> {
    let data = loads_versions(raw, "version", &[3, 4])
        .map_err(|e| format!("failed to parse `{LOCKFILE_NAME}`: {e}"))?;
    let version = match data.get("version") {
        Some(WsonValue::Int(value)) => *value,
        _ => {
            return Err(format!(
                "`{LOCKFILE_NAME}` field `version` must be an integer"
            ))
        }
    };

    if version == 1 {
        return Ok(Lockfile {
            version,
            packages: Vec::new(),
            roots: Vec::new(),
        });
    }
    if version != 2 && version != 3 && version != LOCKFILE_VERSION {
        return Err(format!(
            "unsupported `{LOCKFILE_NAME}` version `{version}`; expected `{LOCKFILE_VERSION}`"
        ));
    }
    reject_unknown_fields(&data, &["version", "package", "roots"], "root")?;

    let packages = match data.get("package") {
        Some(WsonValue::Array(items)) => items
            .iter()
            .map(|value| parse_package(value, version))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(format!(
                "`{LOCKFILE_NAME}` field `package` must be an array"
            ))
        }
        None => Vec::new(),
    };

    let roots: Vec<source::graph::Root> = match data.get("roots") {
        Some(value) if version >= 4 => serde_json::from_value(wson::json_value(value)?)
            .map_err(|e| format!("invalid lockfile roots: {e}"))?,
        Some(_) => return Err("lockfile roots require version 4".into()),
        None => Vec::new(),
    };
    let lockfile = Lockfile {
        version,
        packages,
        roots,
    };
    validate_graph(&lockfile)?;
    let normalized = lockfile.normalized();
    Ok(normalized)
}

fn parse_package(value: &WsonValue, lock_version: i64) -> Result<LockedPackage, String> {
    let WsonValue::Object(object) = value else {
        return Err("lockfile package entry must be an object".to_string());
    };
    let name = required_string(object, "name")?;
    let version = required_string(object, "version")?;
    let source = required_string(object, "source")?;
    let dependencies = optional_string_array(object, "dependencies")?;

    let fields: &[&str] = match source.as_str() {
        "path" => &[
            "name",
            "id",
            "package",
            "import_name",
            "edges",
            "features",
            "version",
            "source",
            "path",
            "resolved",
            "dependencies",
        ],
        "git" => &[
            "name",
            "id",
            "package",
            "import_name",
            "edges",
            "features",
            "version",
            "source",
            "git",
            "subdir",
            "branch",
            "tag",
            "rev",
            "commit",
            "resolved",
            "dependencies",
        ],
        _ => &[],
    };
    if !fields.is_empty() {
        reject_unknown_fields(object, fields, &format!("package `{name}`"))?;
    }

    let source = match source.as_str() {
        "path" => LockedSource::Path {
            requested: required_string(object, "path")?,
            resolved: PathBuf::from(required_string(object, "resolved")?),
        },
        "git" => {
            if ["branch", "tag", "rev"]
                .iter()
                .filter(|field| object.contains_key(**field))
                .count()
                > 1
            {
                return Err(format!("lockfile package `{name}` must specify at most one of `branch`, `tag`, or `rev`"));
            }
            let commit = required_string(object, "commit")?;
            validate_commit(&commit, &name)?;
            LockedSource::Git {
                url: required_string(object, "git")?,
                branch: optional_string(object, "branch")?,
                tag: optional_string(object, "tag")?,
                rev: optional_string(object, "rev")?,
                commit: commit.to_ascii_lowercase(),
                resolved: PathBuf::from(required_string(object, "resolved")?),
            }
        }
        other => {
            return Err(format!(
                "unknown lockfile source `{other}` for package `{name}`"
            ))
        }
    };
    let import_name = optional_string(object, "import_name")?;
    let edges = match object.get("edges") {
        Some(value) => serde_json::from_value(wson::json_value(value)?)
            .map_err(|e| format!("invalid lockfile edges: {e}"))?,
        None => Vec::new(),
    };
    let features = match object.get("features") {
        Some(value) => serde_json::from_value(wson::json_value(value)?)
            .map_err(|e| format!("invalid locked features: {e}"))?,
        None => source::graph::Features::new(),
    };
    if lock_version < 4
        && (import_name.is_some()
            || object.contains_key("edges")
            || object.contains_key("features"))
    {
        return Err("activation graph requires version 4".into());
    }
    source::graph::validate(&features, &edges)?;
    let package = optional_string(object, "package")?;
    let id = optional_string(object, "id")?;
    if (package.is_some() || id.is_some()) && lock_version < 4 {
        return Err("package identity requires lockfile version 4".into());
    }
    if let Some(package) = &package {
        if package.is_empty()
            || !package
                .bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
        {
            return Err("invalid package name in lockfile".into());
        }
    }
    if id
        .as_ref()
        .is_some_and(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("invalid package identity in lockfile".into());
    }
    let subdir = optional_string(object, "subdir")?;
    if let Some(value) = &subdir {
        if lock_version < 4 {
            return Err("subdir requires lockfile version 4".into());
        }
        source::validate_subdir(value)?;
    }
    Ok(LockedPackage {
        import_name,
        edges,
        features,
        package,
        id,
        subdir,
        name,
        version,
        source,
        dependencies,
    })
}

fn reject_unknown_fields(object: &WsonMap, allowed: &[&str], context: &str) -> Result<(), String> {
    for field in object.keys() {
        if !allowed.contains(&field.as_str()) {
            return Err(format!(
                "unknown or source-inapplicable lockfile field `{field}` in {context}"
            ));
        }
    }
    Ok(())
}

fn validate_graph(lockfile: &Lockfile) -> Result<(), String> {
    let mut packages = BTreeMap::new();
    for package in &lockfile.packages {
        if package.id.is_some() != package.package.is_some() {
            return Err(
                "lockfile package identity and declared package must appear together".into(),
            );
        }
        if let Some(id) = &package.id {
            if *id
                != crate::package_identity(
                    package.package_name(),
                    &package.version,
                    &package.source,
                    package.subdir.as_deref(),
                )
            {
                return Err(format!(
                    "lockfile identity does not match source for `{}`",
                    package.name
                ));
            }
        }
        if packages.insert(package.name.as_str(), package).is_some() {
            return Err(format!(
                "`{LOCKFILE_NAME}` contains duplicate package `{}`",
                package.name
            ));
        }
    }
    let validate_edges = |edges: &[source::graph::Edge]| -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for edge in edges {
            if !seen.insert(serde_json::to_string(edge).unwrap()) {
                return Err("duplicate locked activation edge".into());
            }
            if !packages.contains_key(edge.key.as_str()) {
                return Err(format!(
                    "locked activation edge references missing package `{}`",
                    edge.key
                ));
            }
            manifest_alias(&edge.alias)?;
        }
        Ok(())
    };
    let mut root_paths = BTreeSet::new();
    for root in &lockfile.roots {
        source::validate_subdir(&root.manifest)?;
        manifest_alias(&root.name)?;
        if !root_paths.insert(&root.manifest) {
            return Err("duplicate root manifest in lockfile".into());
        }
        source::graph::validate(&root.features, &root.edges)?;
        validate_edges(&root.edges)?;
    }
    for package in &lockfile.packages {
        validate_edges(&package.edges)?;
        if !package.edges.is_empty()
            && package
                .edges
                .iter()
                .map(|e| e.key.clone())
                .collect::<BTreeSet<_>>()
                != package.dependencies.iter().cloned().collect()
        {
            return Err("locked dependencies disagree with activation edges".into());
        }
        if let Some(alias) = &package.import_name {
            manifest_alias(alias)?;
        }
    }
    let mut indegree: BTreeMap<&str, usize> = packages.keys().map(|name| (*name, 0)).collect();
    for package in &lockfile.packages {
        let mut seen = BTreeSet::new();
        for dependency in &package.dependencies {
            if !seen.insert(dependency) {
                return Err(format!(
                    "lockfile graph has duplicate edge: {} -> {dependency}",
                    package.name
                ));
            }
            if dependency == &package.name {
                return Err(format!(
                    "lockfile graph has self-dependency: {} -> {dependency}",
                    package.name
                ));
            }
            let count = indegree.get_mut(dependency.as_str()).ok_or_else(|| {
                format!(
                    "lockfile graph references missing package: {} -> {dependency}",
                    package.name
                )
            })?;
            *count += 1;
        }
    }
    // Iterative traversal avoids stack overflow for long hand-edited chains.
    let mut ready: Vec<_> = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| *name)
        .collect();
    while let Some(name) = ready.pop() {
        for dependency in &packages[name].dependencies {
            let count = indegree
                .get_mut(dependency.as_str())
                .expect("edges checked above");
            *count -= 1;
            if *count == 0 {
                ready.push(dependency.as_str());
            }
        }
        indegree.remove(name);
    }
    if let Some((&start, _)) = indegree.first_key_value() {
        // Every remaining node has a remaining predecessor. Follow those
        // predecessors until one repeats, then reverse to show directed edges.
        let mut predecessor = BTreeMap::new();
        for &name in indegree.keys() {
            for dependency in &packages[name].dependencies {
                if indegree.contains_key(dependency.as_str()) {
                    predecessor.insert(dependency.as_str(), name);
                }
            }
        }
        let mut positions = BTreeMap::new();
        let mut path = Vec::new();
        let mut current = start;
        loop {
            if let Some(&position) = positions.get(current) {
                let mut cycle = path[position..].to_vec();
                cycle.push(current);
                cycle.reverse();
                return Err(format!(
                    "lockfile graph contains a cycle: {}",
                    cycle.join(" -> ")
                ));
            }
            positions.insert(current, path.len());
            path.push(current);
            current = predecessor[current];
        }
    }
    Ok(())
}

fn manifest_alias(alias: &str) -> Result<(), String> {
    if alias.is_empty()
        || !alias
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
    {
        return Err("invalid locked import alias".into());
    }
    Ok(())
}

fn validate_commit(commit: &str, package: &str) -> Result<(), String> {
    if matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(format!(
        "lockfile package `{package}` has invalid Git commit `{commit}`; expected a full hexadecimal object ID"
    ))
}

fn required_string(object: &WsonMap, key: &str) -> Result<String, String> {
    match object.get(key) {
        Some(WsonValue::String(value)) => Ok(restore_url(value)),
        _ => Err(format!("lockfile field `{key}` must be a string")),
    }
}

fn optional_string(object: &WsonMap, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        Some(WsonValue::String(value)) => Ok(Some(restore_url(value))),
        Some(_) => Err(format!("lockfile field `{key}` must be a string")),
        None => Ok(None),
    }
}

fn optional_string_array(object: &WsonMap, key: &str) -> Result<Vec<String>, String> {
    match object.get(key) {
        Some(WsonValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                WsonValue::String(value) => Ok(restore_url(value)),
                _ => Err(format!("lockfile field `{key}` must contain only strings")),
            })
            .collect(),
        Some(_) => Err(format!("lockfile field `{key}` must be an array")),
        None => Ok(Vec::new()),
    }
}

fn restore_url(value: &str) -> String {
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_git_graph_fixture_preserves_object_formats_and_edges() {
        let raw = include_str!("../../tests/fixtures/lockfile/v2-git-graph.ws");
        let parsed = parse_lockfile(raw).unwrap();
        assert_eq!(parsed.packages[0].dependencies, ["beta"]);
        for (package, length) in parsed.packages.iter().zip([40, 64]) {
            let LockedSource::Git { commit, .. } = &package.source else {
                panic!("expected Git fixture");
            };
            assert_eq!(commit.len(), length);
        }
        let rendered = crate::render::render_lockfile(parsed.clone());
        assert_eq!(parse_lockfile(&rendered).unwrap().packages, parsed.packages);
        assert_eq!(
            crate::render::render_lockfile(parse_lockfile(&rendered).unwrap()),
            rendered
        );
    }

    fn git_lock(commit: &str) -> String {
        format!(
            r#"{{ version = 2, package = [{{
                name = "dep", version = "1.0.0", source = "git",
                git = "https://example.com/dep.git", commit = "{commit}",
                resolved = ".vex/deps/dep", dependencies = []
            }}] }}"#
        )
    }

    #[test]
    fn version_one_lockfile_is_treated_as_unresolved() {
        let parsed = parse_lockfile("{ version = 1, package = [] }")
            .expect("legacy lockfile should trigger regeneration");
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn rejects_non_object_id_git_commits() {
        let error =
            parse_lockfile(&git_lock("--help")).expect_err("Git commits must be full object IDs");
        assert!(error.contains("invalid Git commit"), "{error}");
    }

    #[test]
    fn normalizes_sha1_and_sha256_git_commits_to_lowercase() {
        for spelling in [
            "0123456789ABCDEF0123456789ABCDEF01234567",
            "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
        ] {
            let parsed = parse_lockfile(&git_lock(spelling)).expect("object ID must parse");
            let LockedSource::Git { commit, .. } = &parsed.packages[0].source else {
                unreachable!();
            };
            assert_eq!(commit, &spelling.to_ascii_lowercase());
        }
    }

    #[test]
    fn rejects_abbreviated_invalid_and_unsupported_length_commits() {
        for invalid in [
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdef0123456",
            "0123456789abcdef0123456789abcdef012345678",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdef0123456g",
            "0123456789ABCDEF0123456789ABCDEF0123456G",
            "0123456789abcdef0123456789abcdef012345 7",
        ] {
            let result = parse_lockfile(&git_lock(invalid));
            assert!(result.is_err(), "commit `{invalid}` must be rejected");
            let error = result.unwrap_err();
            assert!(error.contains("invalid Git commit"), "{error}");
        }
    }
}
