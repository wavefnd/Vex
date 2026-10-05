use diagnostic::Error;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use lockfile::{LockedPackage, LockedSource, Lockfile, LOCKFILE_NAME};
use manifest::{Dependency, DependencySource, Manifest, MANIFEST_FILE};

use crate::git;
use crate::paths::{relative_to_root, resolve_path};
use crate::transaction::Transaction;
use crate::{ResolveOptions, UpdatePolicy};

pub(crate) struct Resolver<'a> {
    options: ResolveOptions,
    preflight: bool,
    root: PathBuf,
    transaction: Option<&'a Transaction>,
    root_name: String,
    root_manifest: PathBuf,
    existing: &'a Lockfile,
    packages: BTreeMap<String, LockedPackage>,
    requests: HashMap<String, RequestKey>,
    request_origins: HashMap<String, (String, String)>,
    visiting: Vec<String>,
    status: &'a mut dyn FnMut(&str, String),
    validate: &'a mut dyn FnMut(&Manifest) -> Result<(), Error>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RequestKey {
    Path {
        resolved: PathBuf,
        version: Option<String>,
    },
    Git {
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
        validate: &'a mut dyn FnMut(&Manifest) -> Result<(), Error>,
    ) -> Self {
        Self {
            options,
            preflight: false,
            root,
            transaction,
            root_name,
            root_manifest,
            existing,
            packages: BTreeMap::new(),
            requests: HashMap::new(),
            request_origins: HashMap::new(),
            visiting: Vec::new(),
            status,
            validate,
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

    pub(crate) fn into_packages(self) -> Vec<LockedPackage> {
        self.packages.into_values().collect()
    }

    pub(crate) fn validate_selected_packages(&self) -> Result<(), Error> {
        let UpdatePolicy::UpdateSelected(selected) = &self.options.update else {
            return Ok(());
        };

        let available = self
            .packages
            .values()
            .filter(|package| matches!(package.source, LockedSource::Git { .. }))
            .map(|package| package.name.as_str())
            .collect::<BTreeSet<_>>();
        let unavailable = selected
            .iter()
            .filter(|name| !available.contains(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();

        if unavailable.is_empty() {
            return Ok(());
        }

        let requested = unavailable
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let package_label = if unavailable.len() == 1 {
            "package"
        } else {
            "packages"
        };
        let available = if available.is_empty() {
            "<none>".to_string()
        } else {
            available.into_iter().collect::<Vec<_>>().join(", ")
        };
        Err(Error::resolution(format!(
            "cannot update {package_label} {requested}: one or more requested names are not Git dependencies in the current graph\nhelp: available Git packages: {available}\nhelp: run `vex update <package>...` using one or more available package names"
        )))
    }

    pub(crate) fn resolve_manifest_dependencies(
        &mut self,
        manifest: &Manifest,
    ) -> Result<Vec<String>, Error> {
        (self.validate)(manifest).map_err(|error| {
            error
                .with_field("package", &manifest.name)
                .with_field("manifest", manifest.source_path.display())
        })?;
        let mut dependencies = Vec::new();
        let mut edges: Vec<_> = manifest.dependencies.iter().collect();
        edges.sort_by(|a, b| a.name.cmp(&b.name));
        for dependency in edges {
            self.resolve_dependency(dependency, &manifest.source_path)
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
            dependencies.push(dependency.name.clone());
        }
        dependencies.sort();
        Ok(dependencies)
    }

    fn resolve_dependency(
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
                resolved: self.logical(&self.resolve_path(path, parent_manifest)?),
                version: dependency.version.clone(),
            },
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => RequestKey::Git {
                url: source::identity(url),
                branch: branch.clone(),
                tag: tag.clone(),
                rev: rev.clone(),
                version: dependency.version.clone(),
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
                    "package name `{}` refers to more than one source or version requirement\nfirst: {} ({})\nsecond: {} ({})",
                    dependency.name, self.request_origins[&dependency.name].0,
                    self.request_origins[&dependency.name].1, chain, parent_manifest.display()
                )).with_field("first_dependency_path", &self.request_origins[&dependency.name].0)
                  .with_field("second_dependency_path", &chain)
                  .with_field("first_request", format!("{previous:?}"))
                  .with_field("second_request", format!("{key:?}")));
            }
            if let Some(package) = self.packages.get_mut(&dependency.name) {
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
                    if !same_source && previous.is_some() {
                        transaction
                            .prepare_fresh(name)
                            .map_err(Error::environment)?;
                    } else if !live.exists() && previous.as_ref().is_some_and(|p| p != target) {
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
                    transaction.path(name)
                } else {
                    live
                };
                let commit = if reuse {
                    locked_commit.unwrap()
                } else {
                    self.resolve_git_commit(dependency, &destination)?
                };
                git::reject_submodules(&destination, name, &commit)?;
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

        let manifest_path = resolved_path.join(MANIFEST_FILE);
        let package_manifest = Manifest::load_from(&manifest_path).map_err(|e| {
            e.with_field("package", &dependency.name)
                .with_field("operation", "load dependency manifest")
        })?;
        if package_manifest.name != dependency.name {
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
        if let Some(required) = dependency.version.as_deref() {
            if package_manifest.version != required {
                return Err(Error::resolution(format!(
                    "dependency `{}` requires version `{required}` but source contains version `{}`",
                    dependency.name, package_manifest.version
                )));
            }
        }

        self.visiting.push(dependency.name.clone());
        let dependencies = self.resolve_manifest_dependencies(&package_manifest)?;
        self.visiting.pop();

        self.packages.insert(
            dependency.name.clone(),
            LockedPackage {
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
        if !self.preflight && self.options.update.updates(&dependency.name) {
            None
        } else {
            self.existing
                .package(&dependency.name)
                .and_then(|package| match &package.source {
                    LockedSource::Git {
                        url: locked_url,
                        branch: locked_branch,
                        tag: locked_tag,
                        rev: locked_rev,
                        commit,
                        ..
                    } if source::identity(locked_url) == source::identity(url)
                        && locked_branch == branch
                        && locked_tag == tag
                        && locked_rev == rev =>
                    {
                        Some(commit.clone())
                    }
                    _ => None,
                })
        }
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
            return Ok(commit);
        }

        if self.options.offline {
            let commit = locked.ok_or_else(|| {
                Error::resolution(format!(
                    "Git dependency `{}` is not pinned for offline use\nhelp: run `vex fetch` while online",
                    dependency.name
                ))
            })?;
            git::require_local_repository(destination, url, &dependency.name, &commit)?;
            git::checkout_commit(destination, &dependency.name, &commit, false)?;
            return Ok(commit);
        }

        let unborn = git::ensure_repository(destination, url, &dependency.name, &mut *self.status)?;
        git::reject_dirty_checkout(destination, &dependency.name)?;

        if let Some(commit) = locked {
            if !git::has_commit(destination, &commit)? {
                (self.status)("Fetching", format!("{} ({url})", dependency.name));
                git::fetch_exact(destination, url, &dependency.name, &commit)?;
            }
            git::checkout_commit(destination, &dependency.name, &commit, unborn)?;
            return Ok(commit);
        }

        (self.status)("Fetching", format!("{} ({url})", dependency.name));
        git::fetch(destination, url)?;
        let reference = if let Some(branch) = branch {
            format!("refs/remotes/origin/{branch}^{{commit}}")
        } else if let Some(tag) = tag {
            format!("refs/tags/{tag}^{{commit}}")
        } else if let Some(rev) = rev {
            format!("{rev}^{{commit}}")
        } else {
            git::refresh_default_branch(destination, url)?;
            "refs/remotes/origin/HEAD^{commit}".to_string()
        };
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
            resolved: PathBuf::from("/tmp/package"),
            version: Some("1.0.0".to_string()),
        };
        let second = RequestKey::Path {
            resolved: PathBuf::from("/tmp/package"),
            version: Some("1.0.0".to_string()),
        };
        assert_eq!(first, second);
    }
}
