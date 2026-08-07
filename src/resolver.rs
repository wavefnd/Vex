use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::lockfile::{
    read_lockfile, write_lockfile, LockedPackage, LockedSource, Lockfile, LOCKFILE_NAME,
    LOCKFILE_VERSION,
};
use crate::manifest::{Dependency, DependencySource, Manifest, MANIFEST_FILE};
use crate::ui;

#[derive(Clone, Copy, Debug, Default)]
pub struct ResolveOptions {
    pub dry_run: bool,
    pub update: bool,
    pub locked: bool,
    pub offline: bool,
}

#[derive(Debug)]
pub struct Resolution {
    packages: Vec<LockedPackage>,
}

impl Resolution {
    pub fn dependency_args(&self) -> Vec<String> {
        let mut args = vec!["--dep-root=.vex/deps".to_string()];
        for package in &self.packages {
            let path = match &package.source {
                LockedSource::Path { resolved, .. } | LockedSource::Git { resolved, .. } => {
                    resolved
                }
            };
            args.push(format!("--dep={}={}", package.name, path.to_string_lossy()));
        }
        args
    }

    pub fn package_count(&self) -> usize {
        self.packages.len()
    }
}

pub fn resolve(manifest: &Manifest, options: ResolveOptions) -> Result<Resolution, String> {
    if !manifest.dependencies.is_empty() {
        ui::status(
            "Resolving",
            format!("dependencies for {} v{}", manifest.name, manifest.version),
        );
    }

    if options.update && options.locked {
        return Err("`--locked` cannot be used while updating dependencies".to_string());
    }
    if options.update && options.offline {
        return Err("`--offline` cannot be used while updating Git dependencies".to_string());
    }

    let existing = read_lockfile()?;
    if options.locked {
        let lockfile = existing.as_ref().ok_or_else(|| {
            format!(
                "`{LOCKFILE_NAME}` is required by `--locked`\nhelp: run `vex fetch` and commit `{LOCKFILE_NAME}`"
            )
        })?;
        if lockfile.version != LOCKFILE_VERSION {
            return Err(format!(
                "`{LOCKFILE_NAME}` version {} cannot be used with `--locked`; expected version {LOCKFILE_VERSION}\nhelp: run `vex fetch` to regenerate the lockfile",
                lockfile.version
            ));
        }
    }
    let existing = existing.unwrap_or_else(Lockfile::empty);
    let root = env_root()?;
    let dep_root = root.join(".vex/deps");
    if !options.dry_run {
        fs::create_dir_all(&dep_root)
            .map_err(|e| format!("failed to create `{}`: {e}", dep_root.display()))?;
    }

    let mut resolver = Resolver {
        options,
        root,
        dep_root,
        existing: &existing,
        packages: BTreeMap::new(),
        requests: HashMap::new(),
        visiting: Vec::new(),
    };
    resolver.resolve_manifest_dependencies(manifest)?;

    let resolved = Lockfile {
        version: LOCKFILE_VERSION,
        packages: resolver.packages.into_values().collect(),
    }
    .normalized();

    if resolved != existing {
        if options.locked {
            return Err(format!(
                "`{LOCKFILE_NAME}` needs to be updated, but `--locked` prevents changes\nhelp: run `vex fetch` and commit the updated `{LOCKFILE_NAME}`"
            ));
        }
        if options.dry_run {
            return Err(format!(
                "dependency graph differs from `{LOCKFILE_NAME}`\nhelp: run `vex fetch` to resolve and lock dependencies"
            ));
        }
        ui::status(
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
        write_lockfile(&resolved)?;
    }

    Ok(Resolution {
        packages: resolved.packages,
    })
}

struct Resolver<'a> {
    options: ResolveOptions,
    root: PathBuf,
    dep_root: PathBuf,
    existing: &'a Lockfile,
    packages: BTreeMap<String, LockedPackage>,
    requests: HashMap<String, RequestKey>,
    visiting: Vec<String>,
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

impl Resolver<'_> {
    fn resolve_manifest_dependencies(
        &mut self,
        manifest: &Manifest,
    ) -> Result<Vec<String>, String> {
        let mut names = HashSet::new();
        let mut dependencies = Vec::new();
        for dependency in &manifest.dependencies {
            if !names.insert(dependency.name.clone()) {
                return Err(format!(
                    "manifest `{}` declares dependency `{}` more than once",
                    manifest.source_path.display(),
                    dependency.name
                ));
            }
            self.resolve_dependency(dependency, &manifest.source_path)
                .map_err(|err| {
                    format!(
                        "failed to resolve dependency `{}` from `{}`\n\nCaused by:\n  {}",
                        dependency.name,
                        manifest.source_path.display(),
                        indent_lines(&err)
                    )
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
    ) -> Result<(), String> {
        let key = match &dependency.source {
            DependencySource::Path { path } => RequestKey::Path {
                resolved: resolve_path(path, parent_manifest),
                version: dependency.version.clone(),
            },
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => RequestKey::Git {
                url: url.clone(),
                branch: branch.clone(),
                tag: tag.clone(),
                rev: rev.clone(),
                version: dependency.version.clone(),
            },
        };

        if let Some(previous) = self.requests.get(&dependency.name) {
            if previous != &key {
                return Err(format!(
                    "package name `{}` refers to more than one source or version requirement",
                    dependency.name
                ));
            }
            if self.packages.contains_key(&dependency.name) {
                return Ok(());
            }
        } else {
            self.requests.insert(dependency.name.clone(), key);
        }

        if let Some(index) = self
            .visiting
            .iter()
            .position(|name| name == &dependency.name)
        {
            let mut cycle = self.visiting[index..].to_vec();
            cycle.push(dependency.name.clone());
            return Err(format!("dependency cycle detected: {}", cycle.join(" -> ")));
        }

        let (resolved_path, locked_source) = match &dependency.source {
            DependencySource::Path { path } => {
                let resolved = resolve_path(path, parent_manifest);
                let source = LockedSource::Path {
                    requested: path.clone(),
                    resolved: resolved.clone(),
                };
                (resolved, source)
            }
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => {
                let destination = self.dep_root.join(&dependency.name);
                let commit = self.resolve_git_commit(dependency, &destination)?;
                let source = LockedSource::Git {
                    url: url.clone(),
                    branch: branch.clone(),
                    tag: tag.clone(),
                    rev: rev.clone(),
                    commit,
                    resolved: relative_to_root(&destination, &self.root),
                };
                (destination, source)
            }
        };

        let manifest_path = resolved_path.join(MANIFEST_FILE);
        let package_manifest = Manifest::load_from(&manifest_path)?;
        if package_manifest.name != dependency.name {
            return Err(format!(
                "dependency is named `{}` but `{}` declares package `{}`",
                dependency.name,
                manifest_path.display(),
                package_manifest.name
            ));
        }
        if let Some(required) = dependency.version.as_deref() {
            if package_manifest.version != required {
                return Err(format!(
                    "dependency `{}` requires version `{required}` but source contains version `{}`",
                    dependency.name, package_manifest.version
                ));
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

    fn resolve_git_commit(
        &self,
        dependency: &Dependency,
        destination: &Path,
    ) -> Result<String, String> {
        let DependencySource::Git {
            url,
            branch,
            tag,
            rev,
        } = &dependency.source
        else {
            return Err("internal error: expected Git dependency".to_string());
        };

        let locked = if self.options.update {
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
                    } if locked_url == url
                        && locked_branch == branch
                        && locked_tag == tag
                        && locked_rev == rev =>
                    {
                        Some(commit.clone())
                    }
                    _ => None,
                })
        };

        if self.options.locked && locked.is_none() {
            return Err(format!(
                "`{LOCKFILE_NAME}` does not match Git dependency `{}`\nhelp: run `vex fetch` to update the lockfile",
                dependency.name
            ));
        }

        if self.options.dry_run {
            let commit = locked.ok_or_else(|| {
                format!(
                    "Git dependency `{}` is not pinned in `{LOCKFILE_NAME}`\nhelp: run `vex fetch` first",
                    dependency.name
                )
            })?;
            require_checkout_at(destination, url, &commit)?;
            return Ok(commit);
        }

        if self.options.offline {
            let commit = locked.ok_or_else(|| {
                format!(
                    "Git dependency `{}` is not pinned for offline use\nhelp: run `vex fetch` while online",
                    dependency.name
                )
            })?;
            require_local_repository(destination, url, &dependency.name, &commit)?;
            checkout_commit(destination, &commit)?;
            return Ok(commit);
        }

        ensure_repository(destination, url, &dependency.name)?;

        if let Some(commit) = locked {
            if !git_has_commit(destination, &commit)? {
                ui::status("Fetching", format!("{} ({url})", dependency.name));
                git_fetch(destination)?;
            }
            checkout_commit(destination, &commit)?;
            return Ok(commit);
        }

        ui::status("Fetching", format!("{} ({url})", dependency.name));
        git_fetch(destination)?;
        let reference = if let Some(branch) = branch {
            format!("refs/remotes/origin/{branch}^{{commit}}")
        } else if let Some(tag) = tag {
            format!("refs/tags/{tag}^{{commit}}")
        } else if let Some(rev) = rev {
            format!("{rev}^{{commit}}")
        } else {
            "refs/remotes/origin/HEAD^{commit}".to_string()
        };
        let commit = git_stdout(
            Command::new("git").arg("-C").arg(destination).args([
                "rev-parse",
                "--verify",
                &reference,
            ]),
            "resolve Git dependency reference",
        )?;
        checkout_commit(destination, &commit)?;
        Ok(commit)
    }
}

fn env_root() -> Result<PathBuf, String> {
    std::env::current_dir()
        .map_err(|e| format!("failed to determine project directory: {e}"))
        .map(|path| path.canonicalize().unwrap_or(path))
}

fn resolve_path(path: &str, manifest_path: &Path) -> PathBuf {
    let candidate = PathBuf::from(path);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(candidate)
    };
    resolved.canonicalize().unwrap_or(resolved)
}

fn relative_to_root(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn ensure_repository(destination: &Path, url: &str, name: &str) -> Result<(), String> {
    if destination.exists() {
        if !destination.join(".git").is_dir() {
            return Err(format!(
                "managed dependency path `{}` exists but is not a Git checkout",
                destination.display()
            ));
        }
        verify_origin(destination, url)?;
        return Ok(());
    }

    let parent = destination
        .parent()
        .ok_or_else(|| format!("invalid dependency path `{}`", destination.display()))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("failed to create `{}`: {e}", parent.display()))?;
    ui::status("Cloning", format!("{name} ({url})"));
    run_git(
        Command::new("git").args(["clone", url]).arg(destination),
        "clone Git dependency",
    )
}

fn require_local_repository(
    destination: &Path,
    url: &str,
    name: &str,
    commit: &str,
) -> Result<(), String> {
    if !destination.join(".git").is_dir() {
        return Err(format!(
            "locked dependency `{name}` is not available locally in offline mode\n\nCaused by:\n  checkout `{}` is missing\n\nhelp: run `vex fetch` while online",
            destination.display()
        ));
    }
    verify_origin(destination, url)?;
    if !git_has_commit(destination, commit)? {
        return Err(format!(
            "locked dependency `{name}` is incomplete in offline mode\n\nCaused by:\n  commit `{commit}` was not found in `{}`\n\nhelp: run `vex fetch` while online",
            destination.display()
        ));
    }
    Ok(())
}

fn verify_origin(destination: &Path, expected: &str) -> Result<(), String> {
    let actual = git_stdout(
        Command::new("git")
            .arg("-C")
            .arg(destination)
            .args(["remote", "get-url", "origin"]),
        "read Git dependency origin",
    )?;
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "managed checkout `{}` has origin `{actual}`, expected `{expected}`\nhelp: remove that checkout and run `vex fetch` again",
        destination.display()
    ))
}

fn git_fetch(destination: &Path) -> Result<(), String> {
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(destination)
            .args(["fetch", "origin", "--tags", "--prune"]),
        "fetch Git dependency",
    )
}

fn git_has_commit(destination: &Path, commit: &str) -> Result<bool, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(destination)
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .output()
        .map_err(|e| format!("failed to inspect Git dependency commit: {e}"))?;
    Ok(output.status.success())
}

fn require_checkout_at(destination: &Path, url: &str, commit: &str) -> Result<(), String> {
    if !destination.join(".git").is_dir() {
        return Err(format!(
            "locked Git dependency is not available at `{}`\nhelp: run `vex fetch`",
            destination.display()
        ));
    }
    verify_origin(destination, url)?;
    let current = git_stdout(
        Command::new("git")
            .arg("-C")
            .arg(destination)
            .args(["rev-parse", "HEAD"]),
        "read Git dependency HEAD",
    )?;
    if current != commit {
        return Err(format!(
            "Git dependency at `{}` is checked out at `{current}`, but `{LOCKFILE_NAME}` pins `{commit}`\nhelp: run `vex fetch`",
            destination.display()
        ));
    }
    Ok(())
}

fn checkout_commit(destination: &Path, commit: &str) -> Result<(), String> {
    let current = git_stdout(
        Command::new("git")
            .arg("-C")
            .arg(destination)
            .args(["rev-parse", "HEAD"]),
        "read Git dependency HEAD",
    )?;
    if current == commit {
        return Ok(());
    }

    let dirty = git_stdout(
        Command::new("git")
            .arg("-C")
            .arg(destination)
            .args(["status", "--porcelain"]),
        "inspect Git dependency checkout",
    )?;
    if !dirty.is_empty() {
        return Err(format!(
            "managed dependency checkout `{}` has local changes\nhelp: preserve or remove those changes before running Vex",
            destination.display()
        ));
    }

    run_git(
        Command::new("git")
            .arg("-C")
            .arg(destination)
            .args(["checkout", "--detach", commit]),
        "checkout locked Git dependency commit",
    )
}

fn run_git(command: &mut Command, action: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|e| format!("failed to start git to {action}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(git_error(
        action,
        output.status,
        &output.stdout,
        &output.stderr,
    ))
}

fn git_stdout(command: &mut Command, action: &str) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|e| format!("failed to start git to {action}: {e}"))?;
    if !output.status.success() {
        return Err(git_error(
            action,
            output.status,
            &output.stdout,
            &output.stderr,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_error(action: &str, status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let details = if !stderr.trim().is_empty() {
        stderr.trim()
    } else if !stdout.trim().is_empty() {
        stdout.trim()
    } else {
        "<no output>"
    };
    format!("could not {action} (status {status}): {details}")
}

fn indent_lines(message: &str) -> String {
    message.replace('\n', "\n  ")
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
