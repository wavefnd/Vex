// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use std::collections::BTreeSet;

use lockfile::{
    read_lockfile, LockedPackage, LockedSource, Lockfile, LOCKFILE_NAME, LOCKFILE_VERSION,
};
use manifest::Manifest;

mod activation;
mod cache;
mod git;
mod graph;
mod paths;
mod prune;
mod transaction;

pub use activation::{root_identity, Request as ActivationRequest, View as RootView};
pub use cache::prune as prune_cache;
pub use prune::{prune, PruneEntry, PruneReport};

pub fn recover_project(guard: &state::Guard, dry_run: bool) -> Result<(), Error> {
    transaction::recover(guard.root(), dry_run).map_err(|error| {
        Error::environment(error)
            .with_field("operation", "recover dependency transaction")
            .with_field("path", guard.root().join(".vex/transaction.json").display())
            .with_field(
                "recovery",
                "run a normal project command to recover; preserve user edits",
            )
    })?;
    state::edit::recover(guard.root(), dry_run).map_err(Error::environment)
}

#[derive(Clone, Debug, Default)]
pub struct ResolveOptions {
    pub requests: Vec<ActivationRequest>,
    pub update_bindings: Option<BTreeSet<String>>,
    pub force_bindings: bool,
    pub additional_contexts: Vec<source::graph::Context>,
    pub selection: source::graph::Selection,
    pub dry_run: bool,
    pub update: UpdatePolicy,
    pub locked: bool,
    pub offline: bool,
}

impl ResolveOptions {
    pub(crate) fn updates_binding(&self, key: &str, alias: &str) -> bool {
        self.update_bindings
            .as_ref()
            .map_or_else(|| self.update.updates(alias), |keys| keys.contains(key))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum UpdatePolicy {
    #[default]
    ReuseLocked,
    UpdateAll,
    UpdateSelected(BTreeSet<String>),
}

impl UpdatePolicy {
    fn is_update(&self) -> bool {
        !matches!(self, Self::ReuseLocked)
    }

    pub(crate) fn updates(&self, package: &str) -> bool {
        match self {
            Self::ReuseLocked => false,
            Self::UpdateAll => true,
            Self::UpdateSelected(packages) => packages.contains(package),
        }
    }
}

#[derive(Debug)]
pub struct Resolution {
    packages: Vec<LockedPackage>,
    union: Vec<LockedPackage>,
    root: source::graph::Root,
    roots: Vec<source::graph::Root>,
    requests: Vec<ActivationRequest>,
    views: Vec<RootView>,
    selection: source::graph::Selection,
    selected_edges: Vec<String>,
    selected_features: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    _guard: state::Guard,
}

impl Resolution {
    pub fn selection(&self) -> &source::graph::Selection {
        &self.selection
    }
    pub fn root_dependencies(&self) -> &[String] {
        &self.selected_edges
    }
    pub fn selected_features(
        &self,
    ) -> &std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
        &self.selected_features
    }
    pub fn union_packages(&self) -> &[LockedPackage] {
        &self.union
    }
    pub fn root_records(&self) -> &[source::graph::Root] {
        &self.roots
    }
    pub fn views(&self) -> &[RootView] {
        &self.views
    }
    pub fn activate(&mut self, selection: &source::graph::Selection) -> Result<(), Error> {
        let path = self._guard.root().join(&self.root.manifest);
        self.activate_root(&path, selection)
    }
    pub fn activate_root(
        &mut self,
        manifest: &std::path::Path,
        selection: &source::graph::Selection,
    ) -> Result<(), Error> {
        let key = manifest_key(manifest)?;
        let mut selection = selection.clone();
        if selection.target.is_none() {
            selection.target = Some(source::graph::Context::host());
        }
        let mut requests = self.requests.clone();
        if !requests
            .iter()
            .any(|r| r.manifest == key && r.selection.target == selection.target)
        {
            requests.push(ActivationRequest {
                manifest: key.clone(),
                selection: selection.clone(),
            });
        }
        let views = activation::select(&self.roots, &self.union, &requests)?;
        let view = views
            .iter()
            .find(|v| v.root.manifest == key && v.selection.target == selection.target)
            .ok_or_else(|| Error::internal("missing selected root activation"))?;
        self.root = view.root.clone();
        self.selection = view.selection.clone();
        self.packages = view.packages.clone();
        self.selected_edges = view.dependencies.clone();
        self.selected_features = view.features.clone();
        self.views = views;
        Ok(())
    }

    pub fn dependency_args(&self) -> Result<Vec<String>, Error> {
        let mut args = vec!["--dep-root=.vex/deps".to_string()];
        let mut aliases = BTreeSet::new();
        for package in &self.packages {
            if !aliases.insert(package.import_name()) {
                continue;
            }
            let path = package.directory();
            let path = path.to_str().ok_or_else(|| {
                Error::resolution(format!(
                    "wavec JSON protocol cannot represent non-UTF-8 source path for `{}`",
                    package.name
                ))
            })?;
            args.push(format!("--dep={}={path}", package.import_name()));
        }
        Ok(args)
    }

    pub fn package_count(&self) -> usize {
        self.packages.len()
    }

    pub fn packages(&self) -> &[LockedPackage] {
        &self.packages
    }
}

pub fn manifest_key(path: &std::path::Path) -> Result<String, Error> {
    let root = paths::env_root()?;
    let path = root.join(path);
    paths::portable_path(&paths::relative_to_root(&path, &root))
}
fn aggregate(
    views: &[RootView],
) -> (
    Vec<LockedPackage>,
    Vec<String>,
    std::collections::BTreeMap<String, BTreeSet<String>>,
) {
    let mut packages: std::collections::BTreeMap<String, LockedPackage> = Default::default();
    let mut edges = BTreeSet::new();
    let mut features: std::collections::BTreeMap<String, BTreeSet<String>> = Default::default();
    for view in views {
        edges.extend(view.dependencies.iter().cloned());
        for (id, values) in &view.features {
            features
                .entry(id.clone())
                .or_default()
                .extend(values.iter().cloned());
        }
        for package in &view.packages {
            if let Some(existing) = packages.get_mut(&package.name) {
                existing
                    .dependencies
                    .extend(package.dependencies.iter().cloned());
                existing.dependencies.sort();
                existing.dependencies.dedup();
            } else {
                packages.insert(package.name.clone(), package.clone());
            }
        }
    }
    (
        packages.into_values().collect(),
        edges.into_iter().collect(),
        features,
    )
}
fn reachable(
    roots: &[source::graph::Root],
    selected: &BTreeSet<String>,
    packages: &[LockedPackage],
) -> BTreeSet<String> {
    let packages: std::collections::BTreeMap<_, _> =
        packages.iter().map(|p| (&p.name, p)).collect();
    let mut pending: Vec<_> = roots
        .iter()
        .filter(|r| selected.contains(&r.manifest))
        .flat_map(|r| r.edges.iter().map(|e| e.key.clone()))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(key) = pending.pop() {
        if seen.insert(key.clone()) {
            pending.extend(packages[&key].dependencies.iter().cloned());
        }
    }
    seen
}

pub fn resolve<F>(
    manifest: &Manifest,
    options: ResolveOptions,
    status: F,
) -> Result<Resolution, Error>
where
    F: FnMut(&str, String),
{
    resolve_validated(manifest, options, status, |_| Ok(()))
}

/// Validate every candidate manifest before publishing any checkout or lockfile.
/// The validator runs under the project lease, after pending recovery is handled.
pub fn resolve_validated<F, V>(
    manifest: &Manifest,
    options: ResolveOptions,
    mut status: F,
    mut validate: V,
) -> Result<Resolution, Error>
where
    F: FnMut(&str, String),
    V: FnMut(&Manifest) -> Result<(), Error>,
{
    if manifest.scope.is_none() {
        source::graph::expand(
            &manifest.features,
            &manifest
                .dependencies
                .iter()
                .map(|d| d.edge(d.name.clone()))
                .collect::<Vec<_>>(),
            &options.selection.features,
            !options.selection.no_default_features,
        )
        .map_err(Error::resolution)?;
    }
    if !manifest.dependencies.is_empty() {
        status(
            "Resolving",
            format!("dependencies for {} v{}", manifest.name, manifest.version),
        );
    }

    if options.update.is_update() && options.locked {
        return Err(Error::resolution(
            "`--locked` cannot be used while updating dependencies".to_string(),
        ));
    }
    if options.update.is_update() && options.offline {
        return Err(Error::resolution(
            "`--offline` cannot be used while updating Git dependencies".to_string(),
        ));
    }

    report_policy(&options, &mut status);
    status(
        "Trace",
        if options.dry_run {
            "project lease: acquiring shared access for read-only resolution"
        } else {
            "project lease: acquiring exclusive access for dependency resolution and publication"
        }
        .into(),
    );
    let guard = state::Guard::acquire(options.dry_run, &mut status).map_err(Error::environment)?;
    status(
        "Trace",
        "project lease: acquired before reading vex.lock".into(),
    );
    resolve_guarded(manifest, options, status, guard, &mut validate, true)
}

/// Inspect only the existing local graph under a read-only shared lease.
pub fn inspect(
    manifest: &Manifest,
    locked: bool,
    status: impl FnMut(&str, String),
) -> Result<Resolution, Error> {
    inspect_selected(
        manifest,
        locked,
        source::graph::Selection::default(),
        status,
    )
}

pub fn inspect_selected(
    manifest: &Manifest,
    locked: bool,
    selection: source::graph::Selection,
    mut status: impl FnMut(&str, String),
) -> Result<Resolution, Error> {
    let options = ResolveOptions {
        requests: Vec::new(),
        update_bindings: None,
        force_bindings: false,
        selection,
        additional_contexts: Vec::new(),
        dry_run: true,
        offline: true,
        locked,
        update: UpdatePolicy::ReuseLocked,
    };
    report_policy(&options, &mut status);
    status(
        "Trace",
        "project lease: acquiring existing shared access for read-only local inspection".into(),
    );
    let guard = state::Guard::acquire_existing(&mut status).map_err(Error::resolution)?;
    status(
        "Trace",
        "project lease: acquired before reading vex.lock".into(),
    );
    resolve_guarded(manifest, options, status, guard, &mut |_| Ok(()), true).map_err(|e| {
        Error::new(
            e.category,
            format!("{e}\nhelp: run `vex fetch` to prepare the local graph"),
        )
    })
}

fn report_policy(options: &ResolveOptions, status: &mut impl FnMut(&str, String)) {
    let update = match &options.update {
        UpdatePolicy::ReuseLocked => "reuse locked source selections".to_owned(),
        UpdatePolicy::UpdateAll => "refresh all Git source selections".to_owned(),
        UpdatePolicy::UpdateSelected(packages) => {
            format!("refresh {} selected Git source(s)", packages.len())
        }
    };
    status("Detail", format!("dependency policy: {update}"));
    status(
        "Detail",
        if options.offline {
            "network: disabled by offline mode; only local dependency sources may be used"
        } else if options.dry_run {
            "network: disabled by read-only resolution; fetch missing sources first"
        } else {
            "network: allowed for dependency retrieval and requested updates"
        }
        .into(),
    );
    status(
        "Trace",
        format!(
            "resolver options: dry_run={}, locked={}, offline={}",
            options.dry_run, options.locked, options.offline
        ),
    );
}

fn resolve_guarded<F: FnMut(&str, String)>(
    manifest: &Manifest,
    mut options: ResolveOptions,
    mut status: F,
    guard: state::Guard,
    validate: &mut dyn FnMut(&Manifest) -> Result<(), Error>,
    recover: bool,
) -> Result<Resolution, Error> {
    if recover {
        recover_project(&guard, options.dry_run)?;
    }
    // Official edits only change dependency spans. Reload them after acquiring
    // the lease so no command uses an old manifest with a newly published lock.
    let mut refreshed = Manifest::load_from(&manifest.source_path)?;
    refreshed.scope = manifest.scope.clone();
    let manifest = &refreshed;
    let existing = read_lockfile()?;
    let missing_lockfile = existing.is_none();
    status(
        "Trace",
        match &existing {
            Some(lockfile) => format!(
                "lockfile: read version {} with {} package(s)",
                lockfile.version,
                lockfile.packages.len()
            ),
            None => "lockfile: vex.lock does not exist".into(),
        },
    );
    if options.locked
        && existing.as_ref().is_some_and(|lock| {
            lock.packages.iter().any(|package| {
        matches!(&package.source, LockedSource::Git { url, .. } if source::identity(url) != *url)
    })
        })
    {
        return Err(Error::resolution("vex.lock contains source authentication; --locked preserves it unchanged\nhelp: run vex fetch to migrate the lockfile, then remove credentials from version-control history"));
    }
    if options.locked {
        let lockfile = existing.as_ref().ok_or_else(|| {
            Error::resolution(format!(
                "`{LOCKFILE_NAME}` is required by `--locked`\nhelp: run `vex fetch` and commit `{LOCKFILE_NAME}`"
            ))
        })?;
        if lockfile.version != 2 && lockfile.version != 3 && lockfile.version != LOCKFILE_VERSION {
            return Err(Error::resolution(format!(
                "`{LOCKFILE_NAME}` version {} cannot be used with `--locked`; expected version {LOCKFILE_VERSION}\nhelp: run `vex fetch` to regenerate the lockfile",
                lockfile.version
            )));
        }
    }
    let existing = existing.unwrap_or_else(Lockfile::empty);
    let root = paths::env_root()?;
    let dep_root = root.join(".vex/deps");
    let root_manifest = if manifest.source_path.is_absolute() {
        manifest.source_path.clone()
    } else {
        root.join(&manifest.source_path)
    };
    let locked = options.locked;
    let dry_run = options.dry_run;
    paths::validate_managed_root(&root, &dep_root)?;

    let mut root_manifests = if let Some(scope) = &manifest.scope {
        manifest::workspace::load_context(scope)?
    } else {
        vec![manifest.clone()]
    };
    options.force_bindings = manifest.scope.is_some();
    let selected_paths: BTreeSet<_> = if let Some(scope) = &manifest.scope {
        scope
            .selected
            .iter()
            .map(|p| manifest_key(p))
            .collect::<Result<_, _>>()?
    } else {
        BTreeSet::from([manifest_key(&manifest.source_path)?])
    };
    // Resolve selected roots first: an update of a shared binding is then seen
    // by other roots, while unrelated pinned bindings remain unchanged.
    let root_keys = root_manifests
        .iter()
        .map(|m| Ok((m.source_path.clone(), manifest_key(&m.source_path)?)))
        .collect::<Result<std::collections::BTreeMap<_, _>, Error>>()?;
    root_manifests.sort_by_key(|m| {
        (
            !selected_paths.contains(&root_keys[&m.source_path]),
            m.name.clone(),
        )
    });
    let mut requests = options.requests.clone();
    if requests.is_empty() {
        for member in root_manifests
            .iter()
            .filter(|m| selected_paths.contains(&manifest_key(&m.source_path).unwrap()))
        {
            let mut selection = options.selection.clone();
            if selection.target.is_none() {
                selection.target = Some(match &member.build_target {
                    Some(t) => source::graph::Context::new(t).map_err(Error::resolution)?,
                    None => source::graph::Context::host(),
                });
            }
            requests.push(ActivationRequest {
                manifest: manifest_key(&member.source_path)?,
                selection: selection.clone(),
            });
            for context in &options.additional_contexts {
                selection.target = Some(context.clone());
                requests.push(ActivationRequest {
                    manifest: manifest_key(&member.source_path)?,
                    selection: selection.clone(),
                });
            }
        }
    }
    let provisional_roots = root_manifests
        .iter()
        .map(|m| {
            Ok(source::graph::Root {
                manifest: manifest_key(&m.source_path)?,
                name: m.name.clone(),
                version: m.version.clone(),
                edges: m
                    .dependencies
                    .iter()
                    .map(|d| d.edge(d.name.clone()))
                    .collect(),
                features: m.features.clone(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let qualified = activation::qualify(&provisional_roots, &requests)?;
    for request in &qualified {
        let candidate = root_manifests
            .iter()
            .find(|m| manifest_key(&m.source_path).as_deref() == Ok(request.manifest.as_str()))
            .unwrap();
        source::graph::expand(
            &candidate.features,
            &candidate
                .dependencies
                .iter()
                .map(|d| d.edge(d.name.clone()))
                .collect::<Vec<_>>(),
            &request.selection.features,
            !request.selection.no_default_features,
        )
        .map_err(Error::resolution)?;
        validate(candidate)?;
    }
    if let UpdatePolicy::UpdateSelected(wanted) = &options.update {
        let mut local_options = options.clone();
        local_options.dry_run = true;
        local_options.offline = true;
        local_options.locked = false;
        let mut preflight = graph::Resolver::new(
            local_options,
            root.clone(),
            dep_root.clone(),
            None,
            manifest.name.clone(),
            root_manifest.clone(),
            &existing,
            &mut status,
        );
        preflight.local_preflight();
        let mut roots = Vec::new();
        for member in &root_manifests {
            preflight.resolve_manifest_dependencies(member).map_err(|e| Error::new(e.category, format!("cannot validate update names from the local current graph: {e}\nhelp: run `vex fetch` first")))?;
            roots.push(preflight.root_record(member)?);
        }
        let (packages, _) = preflight.into_packages();
        let eligible = reachable(&roots, &selected_paths, &packages);
        let git: Vec<_> = packages
            .iter()
            .filter(|p| eligible.contains(&p.name) && matches!(p.source, LockedSource::Git { .. }))
            .collect();
        let available: BTreeSet<_> = git.iter().map(|p| p.import_name()).collect();
        for name in wanted {
            if !git
                .iter()
                .any(|p| p.import_name() == name || p.name == *name)
            {
                return Err(Error::resolution(format!("cannot update package `{name}`: not a Git dependency in the selected current graph\nhelp: available Git packages: {}\nhelp: run `vex update <package>...` using an available dependency alias", available.iter().copied().collect::<Vec<_>>().join(", "))));
            }
        }
        options.update_bindings = Some(
            git.into_iter()
                .filter(|p| wanted.contains(p.import_name()) || wanted.contains(&p.name))
                .map(|p| p.name.clone())
                .collect(),
        );
    }
    let transaction = if dry_run {
        None
    } else {
        Some(transaction::Transaction::new(&root).map_err(Error::environment)?)
    };
    let physical_deps = transaction
        .as_ref()
        .map(|t| t.stage.clone())
        .unwrap_or(dep_root);
    let ((packages, manifests), roots) = {
        let mut resolver = graph::Resolver::new(
            options,
            root.clone(),
            physical_deps,
            transaction.as_ref(),
            manifest.name.clone(),
            root_manifest,
            &existing,
            &mut status,
        );
        let mut roots = Vec::new();
        for member in &root_manifests {
            resolver.set_updating(selected_paths.contains(&manifest_key(&member.source_path)?));
            resolver.resolve_manifest_dependencies(member)?;
            roots.push(resolver.root_record(member)?);
        }
        (resolver.into_packages(), roots)
    };
    let requires_v4 = manifest.scope.is_some()
        || manifest.format >= 4
        || packages
            .iter()
            .any(|p| p.id.is_some() || p.subdir.is_some());
    let resolved = Lockfile {
        roots: if requires_v4 {
            roots.clone()
        } else {
            Vec::new()
        },
        version: if !requires_v4 && (locked || dry_run) && matches!(existing.version, 2 | 3) {
            existing.version
        } else if !requires_v4 && (missing_lockfile || existing.version < 4) {
            3
        } else {
            LOCKFILE_VERSION
        },
        packages,
    }
    .normalized();

    // WSON and the compiler's JSON interface cannot encode arbitrary OS bytes.
    // Reject before publishing rather than serializing replacement characters.
    for package in &resolved.packages {
        let (LockedSource::Path { resolved, .. } | LockedSource::Git { resolved, .. }) =
            &package.source;
        if resolved.to_str().is_none() {
            return Err(Error::resolution(format!(
                "non-UTF-8 source path for `{}` cannot be represented in vex.lock",
                package.name
            )));
        }
    }

    let views = activation::select(&roots, &resolved.packages, &requests)?;
    let (active_packages, selected_edges, selected_features) = aggregate(&views);
    let validate_keys = active_packages
        .iter()
        .map(|p| p.name.clone())
        .collect::<BTreeSet<_>>();
    for key in validate_keys {
        let candidate = &manifests[&key];
        validate(candidate).map_err(|error| {
            error
                .with_field("package", &candidate.name)
                .with_field("manifest", candidate.source_path.display())
                .context(format!(
                    "failed to validate selected dependency `{}`",
                    candidate.name
                ))
        })?;
    }
    let needs_lockfile = missing_lockfile || resolved != existing;
    let lockfile_decision = if missing_lockfile {
        "lockfile: write required because vex.lock is missing".to_owned()
    } else if resolved.version != existing.version {
        format!(
            "lockfile: write required because format changes from {} to {}",
            existing.version, resolved.version
        )
    } else if needs_lockfile {
        "lockfile: write required because dependency graph or exact source selections changed"
            .to_owned()
    } else {
        "lockfile: preserved because the resolved graph matches vex.lock".to_owned()
    };
    status("Detail", lockfile_decision);
    if needs_lockfile {
        if locked {
            return Err(Error::resolution(format!(
                "`{LOCKFILE_NAME}` needs to be updated, but `--locked` prevents changes\nhelp: run `vex fetch` and commit the updated `{LOCKFILE_NAME}`"
            )));
        }
        if dry_run {
            return Err(Error::resolution(format!(
                "dependency graph differs from `{LOCKFILE_NAME}`\nhelp: run `vex fetch` to resolve and lock dependencies"
            )));
        }
        status(
            "Locking",
            format!(
                "{} package{} to exact sources",
                resolved.packages.len(),
                if resolved.packages.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
        );
    }

    if let Some(transaction) = transaction {
        if needs_lockfile {
            state::edit::expect_lock(guard.root(), lockfile::encode(resolved.clone()))
                .map_err(Error::environment)?;
        }
        status(
            "Trace",
            format!(
                "dependency transaction: publishing staged state; update vex.lock={needs_lockfile}"
            ),
        );
        transaction
            .publish(needs_lockfile.then(|| lockfile::encode(resolved.clone())))
            .map_err(Error::environment)?;
    }

    Ok(Resolution {
        packages: active_packages,
        union: resolved.packages,
        root: views[0].root.clone(),
        selection: views[0].selection.clone(),
        roots,
        requests,
        views,
        selected_edges,
        selected_features,
        _guard: guard,
    })
}

/// Edit a direct dependency and resolve the pair under one exclusive lease.
pub fn edit_manifest(
    manifest: &Manifest,
    offline: bool,
    transform: impl FnOnce(&Manifest, &str) -> Result<String, Error>,
    mut status: impl FnMut(&str, String),
) -> Result<(), Error> {
    let guard = state::Guard::acquire(false, &mut status).map_err(Error::environment)?;
    recover_project(&guard, false)?;
    let mut current = Manifest::load_from(&manifest.source_path)?;
    current.scope = manifest.scope.clone();
    let raw = wson::read_document(&current.source_path)?;
    let next = transform(&current, &raw)?;
    // Validate before starting the durable edit.
    manifest::decode(&next, current.source_path.clone()).map_err(Error::resolution)?;
    state::edit::begin(&guard, &current.source_path, next).map_err(Error::environment)?;
    #[cfg(debug_assertions)]
    if std::env::var_os("VEX_TEST_EDIT_CRASH_MANIFEST").is_some() {
        std::process::exit(99);
    }
    match resolve_guarded(
        &current,
        ResolveOptions {
            offline,
            ..Default::default()
        },
        &mut status,
        guard,
        &mut |_| Ok(()),
        false,
    ) {
        Ok(resolution) => {
            #[cfg(debug_assertions)]
            if std::env::var_os("VEX_TEST_EDIT_CRASH_LOCK").is_some() {
                std::process::exit(99);
            }
            state::edit::commit(&resolution._guard).map_err(Error::environment)
        }
        Err(error) => {
            let guard = state::Guard::acquire(false, &mut status).map_err(Error::environment)?;
            recover_project(&guard, false)?;
            Err(error)
        }
    }
}

/// The same sanitized Git environment is used by explicit project creation.
pub fn project_git_command() -> std::process::Command {
    git::command()
}
