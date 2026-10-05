use diagnostic::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};
use std::time::Duration;

use lockfile::LOCKFILE_NAME;

use crate::paths::{git_cli_path, validate_managed_checkout_path};

pub(crate) fn ensure_repository(
    destination: &Path,
    url: &str,
    name: &str,
    status: &mut dyn FnMut(&str, String),
) -> Result<bool, Error> {
    validate_managed_checkout_path(destination)?;
    if destination.exists() {
        if !destination.join(".git").is_dir() {
            return Err(Error::resolution(format!(
                "managed dependency path `{}` exists but is not a Git checkout",
                destination.display()
            )));
        }
        verify_origin(destination, url)?;
        return Ok(false);
    }

    let parent = destination.parent().ok_or_else(|| {
        Error::resolution(format!(
            "invalid dependency path `{}`",
            destination.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::environment(format!("failed to create `{}`: {error}", parent.display()))
    })?;
    status("Cloning", format!("{name} ({url})"));
    // Cover both clone and the init/fetch path used for long destinations.
    inject_failure("clone Git dependency")?;
    // clone exports an absolute GIT_DIR to index-pack, whose Windows setup has
    // a separate fixed-length guard even with core.longpaths. Initialize deep
    // candidates without transport, then let resolution fetch/checkout through
    // command_in's relative .git. Apply this path on all hosts for coverage.
    let unborn = destination.as_os_str().len() > 200;
    if unborn {
        let advertised = stdout(
            command().args(["ls-remote", "--", url]),
            "inspect Git object format",
        )?;
        let format = advertised_object_format(&advertised)?;
        fs::create_dir(destination).map_err(Error::environment)?;
        run(
            command().current_dir(destination).args([
                "init",
                // Template copying precedes Git's long-path configuration.
                // Private candidates need objects/refs, not template hooks.
                "--template=",
                &format!("--object-format={format}"),
                "--",
                ".",
            ]),
            "initialize Git dependency",
        )?;
        run(
            command_in(destination).args([
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ]),
            "configure Git dependency refs",
        )?;
    } else {
        run(
            command()
                .args(["clone", "--", url])
                .arg(git_cli_path(destination).as_ref()),
            "clone Git dependency",
        )?;
    }
    // Let Git apply the user's transport rewrites to the original declaration,
    // then remove authentication before a candidate can be published.
    run(
        command_in(destination).args([
            "config",
            "--replace-all",
            "remote.origin.url",
            &source::identity(url),
        ]),
        "store credential-free Git origin",
    )?;
    Ok(unborn)
}

fn advertised_object_format(advertised: &str) -> Result<&'static str, Error> {
    let mut width = None;
    for line in advertised.lines() {
        let (oid, _) = line
            .split_once('\t')
            .ok_or_else(|| Error::resolution("invalid Git ref advertisement"))?;
        if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::resolution(
                "unsupported Git object format in ref advertisement",
            ));
        }
        if width.is_some_and(|value| value != oid.len()) {
            return Err(Error::resolution(
                "inconsistent Git object formats in ref advertisement",
            ));
        }
        width = Some(oid.len());
    }
    match width {
        Some(40) => Ok("sha1"),
        Some(64) => Ok("sha256"),
        _ => Err(Error::resolution("Git source advertises no revisions")),
    }
}

pub(crate) fn require_local_repository(
    destination: &Path,
    url: &str,
    name: &str,
    commit: &str,
) -> Result<(), Error> {
    validate_managed_checkout_path(destination)?;
    if !destination.join(".git").is_dir() {
        return Err(Error::resolution(format!(
            "locked dependency `{name}` is not available locally in offline mode\n\nCaused by:\n  checkout `{}` is missing\n\nhelp: run `vex fetch` while online",
            destination.display()
        )));
    }
    verify_origin(destination, url)?;
    if !has_commit(destination, commit)? {
        return Err(Error::resolution(format!(
            "locked dependency `{name}` is incomplete in offline mode\n\nCaused by:\n  commit `{commit}` was not found in `{}`\n\nhelp: run `vex fetch` while online",
            destination.display()
        )));
    }
    Ok(())
}

fn verify_origin(destination: &Path, expected: &str) -> Result<(), Error> {
    // Read declarations, not `remote get-url`, which expands user insteadOf rules.
    // NUL delimiters preserve URLs containing whitespace and detect multiple values.
    let output = command_in(destination)
        .args(["config", "--null", "--get-all", "remote.origin.url"])
        .supervised_output()
        .map_err(|error| {
            Error::environment(format!("failed to read Git dependency origin: {error}"))
        })?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(Error::environment(git_error(
            "read Git dependency origin",
            output.status,
            &output.stdout,
            &output.stderr,
        )));
    }
    let values: Vec<_> = output
        .stdout
        .strip_suffix(&[0])
        .map(|bytes| bytes.split(|byte| *byte == 0).collect())
        .unwrap_or_default();
    if values.len() == 1
        && source::identity(&String::from_utf8_lossy(values[0])) == source::identity(expected)
    {
        return Ok(());
    }
    let actual = if values.is_empty() {
        "<missing>".to_string()
    } else {
        values
            .iter()
            .map(|value| String::from_utf8_lossy(value))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(Error::resolution(format!(
        "managed checkout `{}` has origin `{actual}`, expected exactly one origin `{expected}`\nhelp: restore remote.origin.url to the declared source, then run `vex fetch`",
        destination.display()
    )))
}

pub(crate) fn fetch(destination: &Path, url: &str) -> Result<(), Error> {
    run(
        command_in(destination).args([
            "fetch",
            "--tags",
            "--prune",
            "--",
            url,
            "+refs/heads/*:refs/remotes/origin/*",
        ]),
        "fetch Git dependency",
    )
}

pub(crate) fn fetch_exact(
    destination: &Path,
    url: &str,
    name: &str,
    commit: &str,
) -> Result<(), Error> {
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::resolution("locked Git object ID is invalid"));
    }
    let result = run(
        command_in(destination).args(["fetch", "--no-tags", "--", url, commit]),
        "fetch exact locked Git object",
    );
    if has_commit(destination, commit)? {
        return Ok(());
    }
    if let Err(failure) = result {
        if process::cancelled() || process::failure_code() == 124 {
            return Err(failure);
        }
        // A failed exact fetch alone cannot prove pruning: servers may refuse
        // unadvertised objects. Probe connectivity without guessing from stderr.
        run(
            command_in(destination).args(["ls-remote", "--", url]),
            "access locked Git source",
        )?;
        return Err(Error::resolution(format!(
            "dependency `{name}`: server cannot provide locked commit `{commit}` (missing or unadvertised-object access refused)\n{failure}\nhelp: restore the object on the source host or use an existing checkout containing it; Vex will never substitute a newer commit")));
    }
    Err(Error::resolution(format!("dependency `{name}`: fetched source does not contain locked commit `{commit}\nhelp: the source host must retain locked objects")))
}

pub(crate) fn reject_submodules(destination: &Path, name: &str, commit: &str) -> Result<(), Error> {
    let listing = stdout(
        command_in(destination).args(["ls-tree", "-r", "-z", commit]),
        "inspect Git submodule entries",
    )?;
    if listing
        .split('\0')
        .any(|entry| entry.starts_with("160000 "))
    {
        return Err(Error::resolution(format!(
            "Git dependency `{name}` contains submodules, which this Vex release does not support\nhelp: replace them with explicit Git/path dependencies or publish a source tree containing the required files")));
    }
    Ok(())
}

pub(crate) fn refresh_default_branch(destination: &Path, url: &str) -> Result<(), Error> {
    let advertised = stdout(
        command_in(destination).args(["ls-remote", "--symref", "--", url, "HEAD"]),
        "read Git default branch",
    )?;
    let branch = advertised
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: ")
                .and_then(|line| line.strip_suffix("\tHEAD"))
        })
        .and_then(|reference| reference.strip_prefix("refs/heads/"))
        .ok_or_else(|| Error::resolution("remote HEAD does not advertise a default branch"))?;
    run(
        command_in(destination).args([
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{branch}"),
        ]),
        "refresh Git dependency default branch",
    )
}

pub(crate) fn has_commit(destination: &Path, commit: &str) -> Result<bool, Error> {
    let output = command_in(destination)
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .supervised_output()
        .map_err(|error| {
            Error::environment(format!("failed to inspect Git dependency commit: {error}"))
        })?;
    Ok(output.status.success())
}

pub(crate) fn require_checkout_at(
    destination: &Path,
    url: &str,
    name: &str,
    commit: &str,
) -> Result<(), Error> {
    validate_managed_checkout_path(destination)?;
    if !destination.join(".git").is_dir() {
        return Err(Error::resolution(format!(
            "locked Git dependency is not available at `{}`\nhelp: run `vex fetch`",
            destination.display()
        )));
    }
    crate::transaction::reject_git_metadata_links(&destination.join(".git"))
        .map_err(Error::environment)?;
    verify_origin(destination, url)?;
    reject_dirty_checkout(destination, name)?;
    let current = stdout(
        command_in(destination).args(["rev-parse", "HEAD"]),
        "read Git dependency HEAD",
    )?;
    if current != commit {
        return Err(Error::resolution(format!(
            "Git dependency at `{}` is checked out at `{current}`, but `{LOCKFILE_NAME}` pins `{commit}`\nhelp: run `vex fetch`",
            destination.display()
        )));
    }
    Ok(())
}

pub(crate) fn is_detached(destination: &Path) -> Result<bool, Error> {
    let output = command_in(destination)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .supervised_output()
        .map_err(|e| Error::environment(e.to_string()))?;
    match output.status.code() {
        Some(1) => Ok(true),
        Some(0) => Ok(false),
        _ => Err(Error::environment(git_error(
            "inspect Git dependency HEAD",
            output.status,
            &output.stdout,
            &output.stderr,
        ))),
    }
}

pub(crate) fn checkout_commit(
    destination: &Path,
    name: &str,
    commit: &str,
    unborn: bool,
) -> Result<(), Error> {
    reject_dirty_checkout(destination, name)?;
    let current = if unborn {
        None
    } else {
        Some(stdout(
            command_in(destination).args(["rev-parse", "HEAD"]),
            "read Git dependency HEAD",
        )?)
    };
    if current.as_deref() == Some(commit) {
        let head = command_in(destination)
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .supervised_output()
            .map_err(|error| {
                Error::environment(format!("failed to inspect Git dependency HEAD: {error}"))
            })?;
        match head.status.code() {
            Some(1) => return Ok(()), // Already detached at the exact locked commit.
            Some(0) => {}             // Matching branch tip still needs detaching.
            _ => {
                return Err(Error::environment(git_error(
                    "inspect Git dependency HEAD",
                    head.status,
                    &head.stdout,
                    &head.stderr,
                )))
            }
        }
    }
    run(
        command_in(destination).args(["checkout", "--detach", commit]),
        "checkout locked Git dependency commit",
    )?;
    reject_dirty_checkout(destination, name)
}

pub(crate) fn reject_dirty_checkout(destination: &Path, name: &str) -> Result<(), Error> {
    validate_managed_checkout_path(destination)?;
    let dirty = stdout(
        command_in(destination).args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ]),
        "inspect Git dependency checkout",
    )?;
    if !dirty.is_empty() {
        let message = format!(
            "managed Git dependency `{name}` at `{}` has local changes\nhelp: preserve those changes outside the managed checkout, then restore it and rerun `vex fetch`; Vex will not discard your files",
            destination.display()
        );
        #[cfg(windows)]
        let message = format!("{message}\nhelp: if these changes are unexpected, check `git config --show-origin --get-all core.longpaths`; an explicit false setting can make Git for Windows report long paths as missing");
        return Err(Error::resolution(message));
    }
    Ok(())
}

pub(crate) fn command_in(destination: &Path) -> Command {
    let mut command = command();
    // Let Rust/Win32 select the extended-length working directory. Git's -C
    // and init <absolute-path> chdir paths still have a MAX_PATH limitation.
    command.current_dir(destination);
    // Relative arguments avoid Git's fixed GIT_DIR length guard and remain
    // correct in its spawned Git children.
    command.args(["--git-dir", ".git", "--work-tree", "."]);
    command
}

fn command() -> Command {
    let mut command = Command::new("git");
    command.args(["-c", "protocol.ext.allow=never"]);
    // Canonical checkout names plus transaction staging can exceed MAX_PATH
    // even in ordinary Windows temp/project directories. Scope the setting to
    // our commands (including clone's index-pack child), not user Git config.
    #[cfg(windows)]
    command.args([
        "-c",
        "core.longpaths=true",
        // unpack-objects still fails on long loose-object paths. Retaining
        // fetched packs uses index-pack's long-path-aware file handling.
        "-c",
        "fetch.unpackLimit=1",
    ]);
    // Read-only graph discovery and dry-run status checks must not refresh the
    // live index as a side effect. Explicit checkout/fetch operations still work.
    command.env("GIT_OPTIONAL_LOCKS", "0");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    command
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_ASKPASS", "");
    // OpenSSH's BatchMode disables password and host-key prompts. Explicit user
    // SSH transports remain supported, with the enclosing timeout as a bound.
    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    // A hook or parent tool can export repository-local context that overrides the selected checkout.
    // Keep authentication, SSH, proxies, HOME, and user config (including URL
    // rewrites) intact; remove only repository selection/object/index context.
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_REPLACE_REF_BASE",
    ] {
        command.env_remove(variable);
    }
    command
}

fn inject_failure(_action: &str) -> Result<(), Error> {
    #[cfg(debug_assertions)]
    if std::env::var("VEX_TEST_GIT_FAIL_ACTION").as_deref() == Ok(_action) {
        return Err(
            Error::environment(format!("injected Git operation failure: {_action}"))
                .with_field("operation", _action),
        );
    }
    Ok(())
}

fn run(command: &mut Command, action: &str) -> Result<(), Error> {
    inject_failure(action)?;
    let output = command
        .supervised_output()
        .map_err(|error| Error::environment(format!("failed to start git to {action}: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::environment(git_error(
        action,
        output.status,
        &output.stdout,
        &output.stderr,
    )))
}

pub(crate) fn stdout(command: &mut Command, action: &str) -> Result<String, Error> {
    let output = command
        .supervised_output()
        .map_err(|error| Error::environment(format!("failed to start git to {action}: {error}")))?;
    if !output.status.success() {
        return Err(Error::environment(git_error(
            action,
            output.status,
            &output.stdout,
            &output.stderr,
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Quiet rev-parse uses status 1 for a missing/unresolvable revision. Other
/// failures remain environmental; never classify localized stderr text.
pub(crate) fn resolve_reference(destination: &Path, reference: &str) -> Result<String, Error> {
    let output = command_in(destination)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            reference,
        ])
        .supervised_output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let message = format!("cannot resolve Git dependency reference `{reference}`");
    if output.status.code() == Some(1) {
        Err(Error::resolution(message))
    } else {
        Err(Error::environment(git_error(
            &message,
            output.status,
            &output.stdout,
            &output.stderr,
        )))
    }
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
    let message = format!("could not {action} (status {status}): {details}");
    #[cfg(windows)]
    let message = if details.contains("Filename too long") || details.contains("File name too long")
    {
        format!("{message}\nhelp: use a shorter project path and check `git config --show-origin --get-all core.longpaths`; Git for Windows can retain an explicit false value during early path handling despite a command override; remove that explicit false setting or enable long paths in its reported configuration file")
    } else {
        message
    };
    source::redact(&message)
}

trait GitOutput {
    fn supervised_output(&mut self) -> Result<Output, Error>;
}
impl GitOutput for Command {
    fn supervised_output(&mut self) -> Result<Output, Error> {
        let seconds = match std::env::var("VEX_GIT_TIMEOUT") {
            Ok(value) => value
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0 && *n <= 86400)
                .ok_or_else(|| Error::environment("VEX_GIT_TIMEOUT must be 1..86400 seconds"))?,
            Err(std::env::VarError::NotPresent) => 300,
            Err(_) => return Err(Error::environment("VEX_GIT_TIMEOUT must be UTF-8")),
        };
        process::output(self, Duration::from_secs(seconds)).map_err(Error::environment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ref_advertisements_select_hash_format_and_reject_ambiguous_data() {
        for (width, expected) in [(40, "sha1"), (64, "sha256")] {
            let oid = "a".repeat(width);
            assert_eq!(
                advertised_object_format(&format!("{oid}\tHEAD\n{oid}\trefs/tags/v1^{{}}\n"))
                    .unwrap(),
                expected
            );
        }
        for invalid in [
            String::new(),
            "not-a-ref".into(),
            "zz\tHEAD\n".into(),
            format!(
                "{}\tHEAD\n{}\trefs/heads/main\n",
                "a".repeat(40),
                "b".repeat(64)
            ),
        ] {
            assert!(advertised_object_format(&invalid).is_err());
        }
    }
}
