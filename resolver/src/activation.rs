// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Pure activation of fully locked union graphs. No filesystem or network I/O.
use diagnostic::Error;
use lockfile::{LockedPackage, LockedSource};
use source::graph::{Context, Edge, Root, Selection};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Request {
    pub manifest: String,
    pub selection: Selection,
}
#[derive(Clone, Debug)]
pub struct View {
    pub root: Root,
    pub selection: Selection,
    pub packages: Vec<LockedPackage>,
    pub dependencies: Vec<String>,
    pub features: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Default, Clone, Eq, PartialEq)]
struct Features {
    features: BTreeSet<String>,
    defaults: bool,
}
fn identity(package: &LockedPackage) -> String {
    package.id.clone().unwrap_or_else(|| {
        lockfile::package_identity(
            package.package_name(),
            &package.version,
            &package.source,
            package.subdir.as_deref(),
        )
    })
}
pub fn root_identity(root: &Root) -> String {
    let directory = Path::new(&root.manifest)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    lockfile::package_identity(
        &root.name,
        &root.version,
        &LockedSource::Path {
            requested: directory.to_string_lossy().into_owned(),
            resolved: PathBuf::from(directory),
        },
        None,
    )
}
fn edges(package: &LockedPackage, packages: &BTreeMap<&str, &LockedPackage>) -> Vec<Edge> {
    if !package.edges.is_empty() {
        return package.edges.clone();
    }
    package
        .dependencies
        .iter()
        .map(|key| Edge::unconditional(key.clone(), packages[key.as_str()].import_name().into()))
        .collect()
}
fn selected(
    edges: &[Edge],
    enabled: &BTreeSet<String>,
    target: &Context,
    tests: bool,
) -> Vec<Edge> {
    edges
        .iter()
        .filter(|edge| {
            (!edge.development || tests)
                && edge.target.as_ref().is_none_or(|p| p.matches(target))
                && (!edge.optional || enabled.contains(&edge.alias))
        })
        .cloned()
        .collect()
}
/// Workspace package qualification is resolved before feature expansion. With
/// several selected roots, unqualified names would silently affect the wrong
/// package and are rejected.
pub(crate) fn qualify(roots: &[Root], requests: &[Request]) -> Result<Vec<Request>, Error> {
    let selected: BTreeSet<_> = requests.iter().map(|r| r.manifest.as_str()).collect();
    let mut result = Vec::new();
    for request in requests {
        let root = roots
            .iter()
            .find(|root| root.manifest == request.manifest)
            .ok_or_else(|| Error::resolution("selected root is not in the workspace graph"))?;
        let mut selection = request.selection.clone();
        selection.features.clear();
        for feature in &request.selection.features {
            if let Some((package, value)) = feature.split_once('/') {
                if let Some(owner) = roots.iter().find(|root| root.name == package) {
                    if !selected.contains(owner.manifest.as_str()) {
                        return Err(Error::resolution(format!(
                            "feature names unselected package `{package}`"
                        )));
                    }
                    if owner.manifest == root.manifest {
                        selection.features.insert(value.into());
                    }
                    continue;
                }
            }
            if selected.len() > 1 {
                return Err(Error::resolution(
                    "features for several workspace packages require package/feature qualification",
                ));
            }
            selection.features.insert(feature.clone());
        }
        if selection.target.is_none() {
            selection.target = Some(Context::host());
        }
        result.push(Request {
            manifest: request.manifest.clone(),
            selection,
        });
    }
    Ok(result)
}

pub(crate) fn select(
    roots: &[Root],
    union: &[LockedPackage],
    requests: &[Request],
) -> Result<Vec<View>, Error> {
    let requests = qualify(roots, requests)?;
    let packages: BTreeMap<_, _> = union.iter().map(|p| (p.name.as_str(), p)).collect();
    let roots: BTreeMap<_, _> = roots
        .iter()
        .map(|root| (root.manifest.as_str(), root))
        .collect();
    let mut reached: BTreeSet<(String, Context)> = BTreeSet::new();
    let mut groups: BTreeMap<String, Features> = BTreeMap::new();
    for request in &requests {
        let group = groups
            .entry(root_identity(roots[request.manifest.as_str()]))
            .or_default();
        group
            .features
            .extend(request.selection.features.iter().cloned());
        group.defaults |= !request.selection.no_default_features;
    }
    loop {
        let previous = (reached.clone(), groups.clone());
        let mut parents = Vec::new();
        for request in &requests {
            let root = roots[request.manifest.as_str()];
            let context = request.selection.target.as_ref().unwrap();
            let group = &groups[&root_identity(root)];
            let expanded =
                source::graph::expand(&root.features, &root.edges, &group.features, group.defaults)
                    .map_err(|e| Error::resolution(e).with_field("package", &root.name))?;
            parents.push((
                selected(
                    &root.edges,
                    &expanded.enabled,
                    context,
                    request.selection.include_tests,
                ),
                expanded.forwarded,
                context.clone(),
            ));
        }
        for (key, context) in &reached {
            let package = packages[key.as_str()];
            let group = &groups[&identity(package)];
            let all_edges = edges(package, &packages);
            let expanded = source::graph::expand(
                &package.features,
                &all_edges,
                &group.features,
                group.defaults,
            )
            .map_err(|e| Error::resolution(e).with_field("package", package.package_name()))?;
            parents.push((
                selected(&all_edges, &expanded.enabled, context, false),
                expanded.forwarded,
                context.clone(),
            ));
        }
        for (edges, forwarded, context) in parents {
            for edge in edges {
                let child = packages
                    .get(edge.key.as_str())
                    .ok_or_else(|| Error::resolution("activation edge has no locked source"))?;
                reached.insert((edge.key, context.clone()));
                let group = groups.entry(identity(child)).or_default();
                group.defaults |= edge.default_features;
                group.features.extend(edge.features);
                if let Some(features) = forwarded.get(&edge.alias) {
                    group.features.extend(features.iter().cloned());
                }
            }
        }
        if previous == (reached.clone(), groups.clone()) {
            break;
        }
    }
    let mut views = Vec::new();
    for request in requests {
        let root = roots[request.manifest.as_str()];
        let context = request.selection.target.as_ref().unwrap();
        let group = &groups[&root_identity(root)];
        let expanded =
            source::graph::expand(&root.features, &root.edges, &group.features, group.defaults)
                .map_err(Error::resolution)?;
        let root_edges = selected(
            &root.edges,
            &expanded.enabled,
            context,
            request.selection.include_tests,
        );
        let mut features = BTreeMap::from([(root_identity(root), expanded.features)]);
        let mut pending: Vec<_> = root_edges.iter().map(|edge| edge.key.clone()).collect();
        let mut seen = BTreeSet::new();
        let mut aliases = BTreeMap::new();
        let mut active = Vec::new();
        while let Some(key) = pending.pop() {
            if !seen.insert(key.clone()) {
                continue;
            }
            let package = packages[key.as_str()];
            let id = identity(package);
            let alias = package.import_name();
            if alias == root.name {
                return Err(Error::resolution(format!(
                    "dependency alias `{alias}` conflicts with root package `{}`",
                    root.name
                )));
            }
            if aliases
                .insert(alias, id.clone())
                .is_some_and(|previous| previous != id)
            {
                return Err(Error::resolution(format!("import alias `{alias}` refers to multiple active package sources; Wave currently exposes a global import-name map\nhelp: choose distinct aliases; scoped compiler dependency mappings are required for conflicting transitive imports")));
            }
            let group = &groups[&id];
            let all_edges = edges(package, &packages);
            let expanded = source::graph::expand(
                &package.features,
                &all_edges,
                &group.features,
                group.defaults,
            )
            .map_err(Error::resolution)?;
            features.insert(id, expanded.features);
            let mut package = package.clone();
            package.dependencies = selected(&all_edges, &expanded.enabled, context, false)
                .into_iter()
                .map(|e| e.key)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            pending.extend(package.dependencies.iter().cloned());
            active.push(package);
        }
        active.sort_by(|a, b| a.name.cmp(&b.name));
        views.push(View {
            root: root.clone(),
            selection: request.selection,
            packages: active,
            dependencies: root_edges
                .into_iter()
                .map(|e| e.key)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            features,
        });
    }
    Ok(views)
}
