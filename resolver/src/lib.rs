use diagnostic::Error;
use std::collections::BTreeSet;

use lockfile::{
    read_lockfile, LockedPackage, LockedSource, Lockfile, LOCKFILE_NAME, LOCKFILE_VERSION,
};
use manifest::Manifest;

mod git;
mod graph;
mod paths;
mod transaction;

pub fn recover_project(guard: &state::Guard, dry_run: bool) -> Result<(), Error> {
    transaction::recover(guard.root(), dry_run).map_err(|error| {
        Error::environment(error)
            .with_field("operation", "recover dependency transaction")
            .with_field("path", guard.root().join(".vex/transaction.json").display())
            .with_field(
                "recovery",
                "run a normal project command to recover; preserve user edits",
            )
    })
}

#[derive(Clone, Debug, Default)]
pub struct ResolveOptions {
    pub dry_run: bool,
    pub update: UpdatePolicy,
    pub locked: bool,
    pub offline: bool,
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
    _guard: state::Guard,
}

impl Resolution {
    pub fn dependency_args(&self) -> Result<Vec<String>, Error> {
        let mut args = vec!["--dep-root=.vex/deps".to_string()];
        for package in &self.packages {
            let path = match &package.source {
                LockedSource::Path { resolved, .. } | LockedSource::Git { resolved, .. } => {
                    resolved
                }
            };
            let path = path.to_str().ok_or_else(|| {
                Error::resolution(format!(
                    "wavec JSON protocol cannot represent non-UTF-8 source path for `{}`",
                    package.name
                ))
            })?;
            args.push(format!("--dep={}={path}", package.name));
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

    let guard = state::Guard::acquire(options.dry_run, &mut status).map_err(Error::environment)?;
    resolve_guarded(manifest, options, status, guard, &mut validate)
}

/// Inspect only the existing local graph under a read-only shared lease.
pub fn inspect(
    manifest: &Manifest,
    locked: bool,
    status: impl FnMut(&str, String),
) -> Result<Resolution, Error> {
    let guard = state::Guard::acquire_existing(status).map_err(Error::resolution)?;
    resolve_guarded(
        manifest,
        ResolveOptions {
            dry_run: true,
            offline: true,
            locked,
            update: UpdatePolicy::ReuseLocked,
        },
        |_, _| {},
        guard,
        &mut |_| Ok(()),
    )
    .map_err(|e| {
        Error::new(
            e.category,
            format!("{e}\nhelp: run `vex fetch` to prepare the local graph"),
        )
    })
}

fn resolve_guarded<F: FnMut(&str, String)>(
    manifest: &Manifest,
    options: ResolveOptions,
    mut status: F,
    guard: state::Guard,
    validate: &mut dyn FnMut(&Manifest) -> Result<(), Error>,
) -> Result<Resolution, Error> {
    recover_project(&guard, options.dry_run)?;
    let existing = read_lockfile()?;
    let missing_lockfile = existing.is_none();
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
        if lockfile.version != 2 && lockfile.version != LOCKFILE_VERSION {
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

    if let UpdatePolicy::UpdateSelected(selected) = &options.update {
        let mut preflight = graph::Resolver::new(
            ResolveOptions {
                dry_run: true,
                update: UpdatePolicy::UpdateSelected(selected.clone()),
                locked: false,
                offline: true,
            },
            root.clone(),
            dep_root.clone(),
            None,
            manifest.name.clone(),
            root_manifest.clone(),
            &existing,
            &mut status,
            validate,
        );
        // Local discovery must reuse pinned sources even for selected names.
        preflight.local_preflight();
        preflight.resolve_manifest_dependencies(manifest).map_err(|e| Error::new(e.category, format!("cannot validate update names from the local current graph: {e}\nhelp: run `vex fetch` first")))?;
        preflight.validate_selected_packages()?;
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
    let packages = {
        let mut resolver = graph::Resolver::new(
            options,
            root,
            physical_deps,
            transaction.as_ref(),
            manifest.name.clone(),
            root_manifest,
            &existing,
            &mut status,
            validate,
        );
        resolver.resolve_manifest_dependencies(manifest)?;
        resolver.validate_selected_packages()?;
        resolver.into_packages()
    };
    let resolved = Lockfile {
        version: if (locked || dry_run) && existing.version == 2 {
            2
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

    let needs_lockfile = missing_lockfile || resolved != existing;
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
        transaction
            .publish(needs_lockfile.then(|| lockfile::encode(resolved.clone())))
            .map_err(Error::environment)?;
    }

    Ok(Resolution {
        packages: resolved.packages,
        _guard: guard,
    })
}
