// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use lockfile::{LockedPackage, LockedSource, Lockfile, LOCKFILE_NAME};
use manifest::{Dependency, DependencySource, Manifest, MANIFEST_FILE};

use crate::git;
use crate::paths::{relative_to_root, resolve_path};
use crate::transaction::Transaction;
use crate::ResolveOptions;

pub(crate) struct Resolver<'a> {
    options: ResolveOptions,
    preflight: bool,
    updating: bool,
    root: PathBuf,
    transaction: Option<&'a Transaction>,
    root_name: String,
    root_manifest: PathBuf,
    existing: &'a Lockfile,
    packages: BTreeMap<String, LockedPackage>,
    requests: HashMap<String, RequestKey>,
    request_origins: HashMap<String, (String, String)>,
    visiting: Vec<String>,
    git_selections: HashMap<String, String>,
    status: &'a mut dyn FnMut(&str, String),
    manifests: BTreeMap<String, Manifest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RequestKey {
    Path {
        package: String,
        resolved: PathBuf,
        version: Option<String>,
    },
    Git {
        package: String,
        subdir: Option<String>,
        url: String,
        branch: Option<String>,
        tag: Option<String>,
        rev: Option<String>,
        version: Option<String>,
    },
}

impl<'a> Resolver<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        options: ResolveOptions,
        root: PathBuf,
        _dep_root: PathBuf,
        transaction: Option<&'a Transaction>,
        root_name: String,
        root_manifest: PathBuf,
        existing: &'a Lockfile,
        status: &'a mut dyn FnMut(&str, String),
    ) -> Self {
        Self {
            options,
            preflight: false,
            updating: true,
            root,
            transaction,
            root_name,
            root_manifest,
            existing,
            packages: BTreeMap::new(),
            requests: HashMap::new(),
            request_origins: HashMap::new(),
            visiting: Vec::new(),
            git_selections: HashMap::new(),
            status,
            manifests: BTreeMap::new(),
        }
    }

    fn logical(&self, path: &Path) -> PathBuf {
        if let Some(transaction) = self.transaction {
            if let Ok(suffix) = path.strip_prefix(&transaction.stage) {
                return self.root.join(".vex/deps").join(suffix);
            }
        }
        path.to_owned()
    }

    fn resolve_path(&self, path: &str, parent: &Path) -> Result<PathBuf, Error> {
        let parent = self.logical(parent);
        let candidate = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            parent.parent().unwrap_or(Path::new(".")).join(path)
        };
        let mut lexical = PathBuf::new();
        for component in candidate.components() {
            match component {
                std::path::Component::ParentDir => {
                    lexical.pop();
                }
                std::path::Component::CurDir => {}
                other => lexical.push(other.as_os_str()),
            }
        }
        if let Some(transaction) = self.transaction {
            if let Ok(suffix) = lexical.strip_prefix(self.root.join(".vex/deps")) {
                if let Some(std::path::Component::Normal(name)) = suffix.components().next() {
                    let name = name
                        .to_str()
                        .ok_or_else(|| Error::resolution("non-UTF-8 managed package name"))?;
                    let tail: PathBuf = suffix.components().skip(1).collect();
                    let physical = transaction.path(name).join(tail);
                    return Ok(physical.canonicalize().unwrap_or(physical));
                }
            }
        }
        Ok(resolve_path(path, &parent))
    }

    pub(crate) fn local_preflight(&mut self) {
        self.preflight = true;
    }

    pub(crate) fn into_packages(self) -> (Vec<LockedPackage>, BTreeMap<String, Manifest>) {
        (self.packages.into_values().collect(), self.manifests)
    }

    pub(crate) fn set_updating(&mut self, value: bool) {
        self.updating = value;
    }

    pub(crate) fn resolve_manifest_dependencies(
        &mut self,
        manifest: &Manifest,
    ) -> Result<Vec<String>, Error> {
        let mut dependencies = Vec::new();
        let mut edges: Vec<_> = manifest.dependencies.iter().collect();
        edges.sort_by(|a, b| a.name.cmp(&b.name));
        for dependency in edges {
            let key = self
                .resolve_dependency(dependency, &manifest.source_path)
                .map_err(|error| {
                    error
                        .with_field("package", &dependency.name)
                        .with_field("manifest", manifest.source_path.display())
                        .context(format!(
                            "failed to resolve dependency `{}` from `{}`\n\nCaused by",
                            dependency.name,
                            manifest.source_path.display()
                        ))
                })?;
            dependencies.push(key);
        }
        dependencies.sort();
        Ok(dependencies)
    }

    fn binding_key(&self, dependency: &Dependency, parent: &Path) -> Result<String, Error> {
        if !dependency.modern && !self.options.force_bindings {
            return Ok(dependency.name.clone());
        }
        use sha2::{Digest, Sha256};
        let source = match &dependency.source {
            DependencySource::Path { path } => format!(
                "path:{:?}",
                crate::paths::portable_path(&relative_to_root(
                    &self.logical(&self.resolve_path(path, parent)?),
                    &self.root
                ))?
            ),
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => format!("git:{:?}|{branch:?}|{tag:?}|{rev:?}", source::identity(url)),
        };
        Ok(format!(
            "binding_{:x}",
            Sha256::digest(
                format!(
                    "{:?}|{:?}|{source}|{:?}",
                    dependency.name,
                    dependency.package_name(),
                    dependency.subdir
                )
                .as_bytes()
            )
        ))
    }
    fn edges_for(&self, manifest: &Manifest) -> Result<Vec<source::graph::Edge>, Error> {
        manifest
            .dependencies
            .iter()
            .map(|dep| Ok(dep.edge(self.binding_key(dep, &manifest.source_path)?)))
            .collect()
    }
    pub(crate) fn root_record(&self, manifest: &Manifest) -> Result<source::graph::Root, Error> {
        let path = if manifest.source_path.is_absolute() {
            manifest.source_path.clone()
        } else {
            self.root.join(&manifest.source_path)
        };
        Ok(source::graph::Root {
            manifest: crate::paths::portable_path(&relative_to_root(
                &self.logical(&path),
                &self.root,
            ))?,
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            edges: self.edges_for(manifest)?,
            features: manifest.features.clone(),
        })
    }
    fn resolve_dependency(
        &mut self,
        dependency: &Dependency,
        parent: &Path,
    ) -> Result<String, Error> {
        let mut binding = dependency.clone();
        binding.name = self.binding_key(dependency, parent)?;
        if dependency.modern || self.options.force_bindings {
            binding.modern = true;
            binding.binding_alias = Some(dependency.name.clone());
            binding.package = Some(dependency.package_name().into());
        }
        self.resolve_binding(&binding, parent)?;
        Ok(binding.name)
    }

    fn resolve_binding(
        &mut self,
        dependency: &Dependency,
        parent_manifest: &Path,
    ) -> Result<(), Error> {
        if dependency.name == self.root_name {
            let source = match &dependency.source {
                DependencySource::Path { path } => self
                    .resolve_path(path, parent_manifest)?
                    .to_string_lossy()
                    .into_owned(),
                DependencySource::Git { url, .. } => url.clone(),
            };
            return Err(Error::resolution(format!(
                "dependency `{}` declared in `{}` from source `{source}` reuses root package name `{}` from `{}`",
                dependency.name,
                parent_manifest.display(),
                self.root_name,
                self.root_manifest.display()
            )));
        }

        let key = match &dependency.source {
            DependencySource::Path { path } => RequestKey::Path {
                package: dependency.package_name().to_owned(),
                resolved: self.logical(&self.resolve_path(path, parent_manifest)?),
                version: if dependency.modern {
                    None
                } else {
                    dependency.version.clone()
                },
            },
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => RequestKey::Git {
                package: dependency.package_name().to_owned(),
                subdir: dependency.subdir.clone(),
                url: source::identity(url),
                branch: branch.clone(),
                tag: tag.clone(),
                rev: rev.clone(),
                version: if dependency.modern {
                    None
                } else {
                    dependency.version.clone()
                },
            },
        };

        let chain = std::iter::once(self.root_name.as_str())
            .chain(self.visiting.iter().map(String::as_str))
            .chain(std::iter::once(dependency.name.as_str()))
            .collect::<Vec<_>>()
            .join(" -> ");
        if let Some(previous) = self.requests.get(&dependency.name) {
            if previous != &key {
                return Err(Error::resolution(format!(
                    "package name `{}` refers to more than one source or version requirement\nfirst: {} ({})\nsecond: {} ({})\nWave currently exposes a global import-name map; conflicting transitive aliases require scoped compiler dependency mappings",
                    dependency.name, self.request_origins[&dependency.name].0,
                    self.request_origins[&dependency.name].1, chain, parent_manifest.display()
                )).with_field("first_dependency_path", &self.request_origins[&dependency.name].0)
                  .with_field("second_dependency_path", &chain)
                  .with_field("first_request", format!("{previous:?}"))
                  .with_field("second_request", format!("{key:?}")));
            }
            if let Some(package) = self.packages.get_mut(&dependency.name) {
                dependency.validate_version(&package.version).map_err(|e| {
                    Error::resolution(e)
                        .with_field(
                            "first_dependency_path",
                            &self.request_origins[&dependency.name].0,
                        )
                        .with_field("second_dependency_path", &chain)
                })?;
                if self.existing.package(&dependency.name).is_none() {
                    if let (LockedSource::Path { requested, .. }, DependencySource::Path { path }) =
                        (&mut package.source, &dependency.source)
                    {
                        if path < requested {
                            *requested = path.clone();
                        }
                    }
                }
                return Ok(());
            }
        } else {
            self.request_origins.insert(
                dependency.name.clone(),
                (chain.clone(), parent_manifest.display().to_string()),
            );
            self.requests.insert(dependency.name.clone(), key);
        }

        if let Some(index) = self
            .visiting
            .iter()
            .position(|name| name == &dependency.name)
        {
            let mut cycle = self.visiting[index..].to_vec();
            cycle.push(dependency.name.clone());
            return Err(Error::resolution(format!(
                "dependency cycle detected: {}",
                cycle.join(" -> ")
            ))
            .with_field("dependency_path", cycle.join(" -> "))
            .with_field("manifest", parent_manifest.display()));
        }

        let mut record_checkout = None;
        let (resolved_path, locked_source) = match &dependency.source {
            DependencySource::Path { path } => {
                let resolved = self.resolve_path(path, parent_manifest)?;
                let relative = relative_to_root(&self.logical(&resolved), &self.root);
                // `requested` is historical request spelling, not source identity.
                // Resolve every current edge before reusing this annotation.
                let requested = self
                    .existing
                    .package(&dependency.name)
                    .and_then(|package| match &package.source {
                        LockedSource::Path {
                            requested,
                            resolved,
                        } if resolved == &relative => Some(requested.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| path.clone());
                let source = LockedSource::Path {
                    requested,
                    resolved: relative,
                };
                (resolved, source)
            }
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => {
                let canonical = crate::paths::checkout_name(&dependency.name);
                let previous = self.existing.package(&dependency.name).and_then(|package| {
                    if let LockedSource::Git { resolved, .. } = &package.source {
                        Some(resolved.clone())
                    } else {
                        None
                    }
                });
                let legacy = PathBuf::from(".vex/deps").join(&dependency.name);
                let encoded = PathBuf::from(".vex/deps").join(&canonical);
                if previous
                    .as_ref()
                    .is_some_and(|p| p != &legacy && p != &encoded)
                {
                    return Err(Error::resolution(format!(
                        "invalid managed checkout path for `{}` in vex.lock",
                        dependency.name
                    )));
                }
                let preserve_layout = self.options.locked || self.options.dry_run;
                let target = if preserve_layout {
                    previous.as_ref().unwrap_or(&encoded)
                } else {
                    &encoded
                };
                let name = target.file_name().unwrap().to_str().unwrap();
                let live = self.root.join(target);
                let locked_commit = self.pinned_commit(dependency);
                let reuse = if let Some(commit) = locked_commit.as_ref() {
                    let checked = git::require_checkout_at(&live, url, &dependency.name, commit)
                        .and_then(|()| git::is_detached(&live));
                    match checked {
                        Ok(detached) => detached,
                        Err(error) if process::failure_code() != 1 => return Err(error),
                        Err(_) => false,
                    }
                } else {
                    false
                };
                let destination = if let (false, Some(transaction)) = (reuse, self.transaction) {
                    if live.exists() {
                        git::reject_dirty_checkout(&live, &dependency.name)?;
                    }
                    if let Some(previous) = previous.as_ref() {
                        let old = self.root.join(previous);
                        if old != live && old.exists() {
                            git::reject_dirty_checkout(&old, &dependency.name)?;
                        }
                    }
                    // A changed declaration uses a fresh candidate. Never reset
                    // or repoint the previous checkout to the new repository.
                    let same_source = self.existing.package(&dependency.name).is_some_and(|p| {
                        matches!(&p.source, LockedSource::Git { url: old, .. } if source::identity(old) == source::identity(url))
                    });
                    let fresh = !same_source && previous.is_some();
                    let migration =
                        !live.exists() && previous.as_ref().is_some_and(|p| p != target);
                    let copy_source = if fresh {
                        None
                    } else if migration {
                        Some(self.root.join(previous.as_ref().unwrap()))
                    } else {
                        Some(live.clone())
                    };
                    // Validate the live source before copying. Copying can
                    // normalize metadata (for example hard-link counts), which
                    // must never restore an invalid deletion-ownership record.
                    record_checkout = match copy_source.as_ref().filter(|path| path.exists()) {
                        None => Some(false),
                        Some(path) if crate::prune::trusted_checkout(path).is_ok() => Some(true),
                        Some(_) => None,
                    };
                    if fresh {
                        transaction
                            .prepare_fresh(name)
                            .map_err(Error::environment)?;
                    } else if migration {
                        transaction
                            .prepare_from(
                                name,
                                previous
                                    .as_ref()
                                    .unwrap()
                                    .file_name()
                                    .unwrap()
                                    .to_str()
                                    .unwrap(),
                            )
                            .map_err(Error::environment)?;
                    } else {
                        transaction.prepare(name).map_err(Error::environment)?;
                    }
                    let candidate = transaction.path(name);
                    if record_checkout.is_none() {
                        crate::prune::protect_checkout(&candidate)?;
                    }
                    candidate
                } else {
                    live
                };
                let commit = if reuse {
                    (self.status)(
                        "Detail",
                        format!(
                            "{}: reuse locked local checkout{}; no Git transport",
                            dependency.name,
                            if self.options.dry_run {
                                " (read-only)"
                            } else if self.options.offline {
                                " (offline)"
                            } else {
                                ""
                            }
                        ),
                    );
                    locked_commit.unwrap()
                } else {
                    self.resolve_git_commit(dependency, &destination)?
                };
                self.git_selections.insert(
                    format!("{url:?}|{branch:?}|{tag:?}|{rev:?}"),
                    commit.clone(),
                );
                git::reject_submodules(&destination, name, &commit)?;
                let logical_destination = self.logical(&destination);
                (self.status)(
                    "Trace",
                    format!(
                        "{}: selected Git commit {commit} at {}",
                        dependency.name,
                        logical_destination.display()
                    ),
                );
                let source = LockedSource::Git {
                    url: source::identity(url),
                    branch: branch.clone(),
                    tag: tag.clone(),
                    rev: rev.clone(),
                    commit,
                    resolved: relative_to_root(&self.logical(&destination), &self.root),
                };
                (destination, source)
            }
        };

        let checkout_path = resolved_path.clone();
        let resolved_path = if let Some(subdir) = &dependency.subdir {
            let selected = resolved_path
                .join(subdir)
                .canonicalize()
                .map_err(Error::resolution)?;
            let repository = resolved_path.canonicalize().map_err(Error::resolution)?;
            if !selected.starts_with(&repository) {
                return Err(Error::resolution(
                    "dependency subdir escapes its repository",
                ));
            }
            selected
        } else {
            resolved_path
        };
        let manifest_path = resolved_path.join(MANIFEST_FILE);
        if matches!(locked_source, LockedSource::Git { .. }) && manifest_path.exists() {
            let actual = manifest_path.canonicalize().map_err(Error::resolution)?;
            if !actual.starts_with(checkout_path.canonicalize().map_err(Error::resolution)?) {
                return Err(Error::resolution(
                    "dependency manifest escapes its Git repository",
                ));
            }
        }
        let package_manifest = Manifest::load_from(&manifest_path).map_err(|e| {
            e.with_field("package", &dependency.name)
                .with_field("operation", "load dependency manifest")
        })?;
        if package_manifest.name != dependency.package_name() {
            return Err(Error::resolution(format!(
                "dependency is named `{}` but `{}` declares package `{}`",
                dependency.name,
                manifest_path.display(),
                package_manifest.name
            )));
        }
        if !package_manifest.lib {
            return Err(Error::resolution(format!(
                "dependency `{}` is not a library package\nhelp: set `lib = true` in `{}` and provide `src/lib.wave`",
                dependency.name,
                manifest_path.display()
            )));
        }
        let library_entry = resolved_path.join(package_manifest.default_entry_path());
        if !library_entry.is_file() {
            return Err(Error::resolution(format!(
                "dependency `{}` has no canonical library entry `{}`\nhelp: library packages must expose `src/lib.wave`",
                dependency.name,
                library_entry.display()
            )));
        }
        if matches!(locked_source, LockedSource::Git { .. }) {
            let actual = library_entry.canonicalize().map_err(Error::resolution)?;
            if !actual.starts_with(checkout_path.canonicalize().map_err(Error::resolution)?) {
                return Err(Error::resolution(
                    "dependency library entry escapes its Git repository",
                ));
            }
        }
        dependency
            .validate_version(&package_manifest.version)
            .map_err(|e| Error::resolution(e).with_field("dependency_path", &chain))?;

        self.visiting.push(dependency.name.clone());
        let dependencies = self.resolve_manifest_dependencies(&package_manifest)?;
        self.visiting.pop();

        if let Some(replace) = record_checkout {
            if let LockedSource::Git { url, commit, .. } = &locked_source {
                crate::prune::record_checkout(
                    &checkout_path,
                    &dependency.name,
                    url,
                    commit,
                    replace,
                )?;
            }
        }

        // Deletion ownership is deliberately stricter than source verification.
        // A validated staged checkout still supplies immutable Git objects when
        // prune must preserve its layout or user-created repository metadata.
        if !self.options.dry_run && self.logical(&checkout_path) != checkout_path {
            if let LockedSource::Git { url: _, commit, .. } = &locked_source {
                if let DependencySource::Git { url, .. } = &dependency.source {
                    crate::cache::store(&checkout_path, url, commit)?;
                }
            }
        }

        let edges = self.edges_for(&package_manifest)?;
        self.manifests
            .insert(dependency.name.clone(), package_manifest.clone());
        self.packages.insert(
            dependency.name.clone(),
            LockedPackage {
                import_name: dependency
                    .modern
                    .then(|| dependency.import_name().to_owned()),
                edges: if dependency.modern || package_manifest.format >= 4 {
                    edges
                } else {
                    Vec::new()
                },
                features: package_manifest.features.clone(),
                package: dependency.modern.then(|| package_manifest.name.clone()),
                id: dependency.modern.then(|| {
                    lockfile::package_identity(
                        &package_manifest.name,
                        &package_manifest.version,
                        &locked_source,
                        dependency.subdir.as_deref(),
                    )
                }),
                subdir: dependency.subdir.clone(),
                name: dependency.name.clone(),
                version: package_manifest.version,
                source: locked_source,
                dependencies,
            },
        );
        Ok(())
    }

    fn pinned_commit(&self, dependency: &Dependency) -> Option<String> {
        let DependencySource::Git {
            url,
            branch,
            tag,
            rev,
        } = &dependency.source
        else {
            return None;
        };
        let pinned = self.existing_git(dependency).and_then(|package| {
            if let LockedSource::Git { commit, .. } = &package.source {
                Some(commit.clone())
            } else {
                None
            }
        });
        if self.preflight || self.options.dry_run || self.options.locked {
            return pinned;
        }
        if !(self.updating
            && self
                .options
                .updates_binding(&dependency.name, dependency.import_name()))
            && pinned.is_some()
        {
            return pinned;
        }
        self.git_selections
            .get(&format!("{url:?}|{branch:?}|{tag:?}|{rev:?}"))
            .cloned()
    }

    fn existing_git(&self, dependency: &Dependency) -> Option<&LockedPackage> {
        let DependencySource::Git {
            url,
            branch,
            tag,
            rev,
        } = &dependency.source
        else {
            return None;
        };
        let matches = |package: &&LockedPackage| {
            package.import_name() == dependency.import_name()
                && package.package_name() == dependency.package_name()
                && package.subdir == dependency.subdir
                && matches!(&package.source, LockedSource::Git { url: old, branch: b, tag: t, rev: r, .. } if source::identity(old) == source::identity(url) && b == branch && t == tag && r == rev)
        };
        if let Some(package) = self
            .existing
            .package(&dependency.name)
            .filter(|p| matches(p))
        {
            return Some(package);
        }
        self.existing.packages.iter().find(matches)
    }

    fn resolve_git_commit(
        &mut self,
        dependency: &Dependency,
        destination: &Path,
    ) -> Result<String, Error> {
        let DependencySource::Git {
            url,
            branch,
            tag,
            rev,
        } = &dependency.source
        else {
            return Err(Error::internal("expected Git dependency"));
        };

        let locked = self.pinned_commit(dependency);

        if self.options.locked && locked.is_none() {
            return Err(Error::resolution(format!(
                "`{LOCKFILE_NAME}` does not match Git dependency `{}`\nhelp: run `vex fetch` to update the lockfile",
                dependency.name
            )));
        }

        if self.options.dry_run {
            let commit = locked.ok_or_else(|| {
                Error::resolution(format!(
                    "Git dependency `{}` is not pinned in `{LOCKFILE_NAME}`\nhelp: run `vex fetch` first",
                    dependency.name
                ))
            })?;
            git::require_checkout_at(destination, url, &dependency.name, &commit)?;
            (self.status)(
                "Detail",
                format!(
                    "{}: inspect locked local commit without checkout or Git transport",
                    dependency.name
                ),
            );
            return Ok(commit);
        }

        // Entering a workspace changes binding layout, not an unchanged Git
        // selection. Import old managed objects without discarding user edits.
        if !destination.exists() {
            if let (Some(commit), Some(old)) = (&locked, self.existing_git(dependency)) {
                if let LockedSource::Git { resolved, .. } = &old.source {
                    manifest::validate_package_name(&old.name).map_err(Error::resolution)?;
                    let legacy = PathBuf::from(".vex/deps").join(&old.name);
                    let encoded =
                        PathBuf::from(".vex/deps").join(crate::paths::checkout_name(&old.name));
                    if resolved != &legacy && resolved != &encoded {
                        return Err(Error::resolution(
                            "invalid historical managed checkout path",
                        ));
                    }
                    let old_path = self.root.join(resolved);
                    if old_path.exists() {
                        git::verify_origin(&old_path, url)?;
                        git::reject_dirty_checkout(&old_path, dependency.import_name())?;
                        if git::has_commit(&old_path, commit)? {
                            crate::cache::store(&old_path, url, commit)?;
                        }
                    }
                }
            }
        }
        if let Some(commit) = &locked {
            if (!destination.join(".git").exists() || !git::has_commit(destination, commit)?)
                && crate::cache::restore(destination, url, commit)?
            {
                (self.status)(
                    "Detail",
                    format!(
                        "{}: restored exact commit from shared cache without network",
                        dependency.name
                    ),
                );
            }
        }

        if self.options.offline {
            let commit = locked.ok_or_else(|| {
                Error::resolution(format!(
                    "Git dependency `{}` is not pinned for offline use\nhelp: run `vex fetch` while online",
                    dependency.name
                ))
            })?;
            git::require_local_repository(destination, url, &dependency.name, &commit)?;
            (self.status)(
                "Detail",
                format!(
                    "{}: select locally available locked commit in offline mode",
                    dependency.name
                ),
            );
            git::checkout_commit(destination, &dependency.name, &commit, false)?;
            return Ok(commit);
        }

        let repository =
            git::ensure_repository(destination, url, &dependency.name, &mut *self.status)?;
        let unborn = repository == git::RepositoryState::Initialized;
        git::reject_dirty_checkout(destination, &dependency.name)?;

        if let Some(commit) = locked {
            if !git::has_commit(destination, &commit)? {
                (self.status)(
                    "Detail",
                    format!(
                        "{}: locked commit is absent locally; fetch the exact object",
                        dependency.name
                    ),
                );
                (self.status)("Fetching", format!("{} ({url})", dependency.name));
                git::fetch_exact(destination, url, &dependency.name, &commit)?;
            } else {
                (self.status)(
                    "Detail",
                    format!(
                        "{}: locked commit is already available; skip fetch",
                        dependency.name
                    ),
                );
            }
            git::checkout_commit(destination, &dependency.name, &commit, unborn)?;
            return Ok(commit);
        }

        let reference = if let Some(branch) = branch {
            format!("refs/remotes/origin/{branch}^{{commit}}")
        } else if let Some(tag) = tag {
            format!("refs/tags/{tag}^{{commit}}")
        } else if let Some(rev) = rev {
            format!("{rev}^{{commit}}")
        } else {
            "refs/remotes/origin/HEAD^{commit}".to_string()
        };
        // A real clone already transferred the advertised branch/tag objects
        // and recorded the default branch. Reuse that snapshot when it contains
        // the requested commit; init-only candidates and existing checkouts
        // still need fetch, especially for an explicit update.
        if repository == git::RepositoryState::Cloned {
            if let Some(commit) = git::try_resolve_reference(destination, &reference)? {
                (self.status)(
                    "Detail",
                    format!(
                        "{}: clone already contains the requested reference; skip redundant fetch",
                        dependency.name
                    ),
                );
                git::checkout_commit(destination, &dependency.name, &commit, false)?;
                return Ok(commit);
            }
        }

        let reason = match repository {
            git::RepositoryState::Cloned => "requested reference is missing from the clone",
            git::RepositoryState::Initialized => {
                "long-path checkout was initialized without objects"
            }
            git::RepositoryState::Existing
                if (self.updating
                    && self
                        .options
                        .updates_binding(&dependency.name, dependency.import_name())) =>
            {
                "explicit update requires refreshed remote refs"
            }
            git::RepositoryState::Existing => {
                "unlocked source selection requires refreshed remote refs"
            }
        };
        (self.status)(
            "Detail",
            format!("{}: fetch required because {reason}", dependency.name),
        );
        (self.status)("Fetching", format!("{} ({url})", dependency.name));
        git::fetch(destination, url)?;
        if branch.is_none() && tag.is_none() && rev.is_none() {
            git::refresh_default_branch(destination, url)?;
        }
        let commit = git::resolve_reference(destination, &reference)?;
        git::checkout_commit(destination, &dependency.name, &commit, unborn)?;
        Ok(commit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_identity_uses_resolved_location() {
        let first = RequestKey::Path {
            package: "package".into(),
            resolved: PathBuf::from("/tmp/package"),
            version: Some("1.0.0".to_string()),
        };
        let second = RequestKey::Path {
            package: "package".into(),
            resolved: PathBuf::from("/tmp/package"),
            version: Some("1.0.0".to_string()),
        };
        assert_eq!(first, second);
    }
}
