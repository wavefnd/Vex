use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use serde_json::Value;

use crate::manifest::{Dependency, DependencySource, Manifest, MANIFEST_FILE};

pub fn build_dependency_global_args(
    manifest: &Manifest,
    ensure_dirs: bool,
) -> Result<Vec<String>, String> {
    let dep_root = Path::new(".vex/deps");
    if ensure_dirs {
        fs::create_dir_all(dep_root)
            .map_err(|e| format!("failed to create `{}`: {e}", dep_root.display()))?;
    }

    let mut names = HashSet::new();
    let mut args = vec![format!("--dep-root={}", dep_root.to_string_lossy())];

    for dep in &manifest.dependencies {
        if !names.insert(dep.name.clone()) {
            return Err(format!(
                "duplicate dependency name in manifest: `{}`",
                dep.name
            ));
        }

        let resolved = resolve_dependency(dep, &manifest.source_path, ensure_dirs)?;
        args.push(format!("--dep={}={}", dep.name, resolved.to_string_lossy()));
    }

    Ok(args)
}

pub fn run_build_with_dry_run(args: &[String], user_requested_dry_run: bool) -> Result<(), String> {
    let mut dry_run_args = args.to_vec();

    if !contains_dry_run_flag(&dry_run_args) {
        insert_build_flag(&mut dry_run_args, "--dry-run");
    }
    insert_build_flag(&mut dry_run_args, "--error-format=json");

    let (candidate, validation_output) = select_compatible_wavec(&dry_run_args)?;
    validate_dry_run_json_output(&validation_output.stdout, &validation_output.stderr)?;

    if !user_requested_dry_run {
        let status = Command::new(&candidate.path)
            .args(args)
            .status()
            .map_err(|e| format!("failed to execute `{}` build: {e}", candidate.display()))?;
        if !status.success() {
            return Err(format!("wavec build failed [{}]", classify_exit(status)));
        }
        return Ok(());
    }

    let status = Command::new(&candidate.path)
        .args(args)
        .status()
        .map_err(|e| {
            format!(
                "failed to execute `{}` build --dry-run: {e}",
                candidate.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "wavec build dry-run failed [{}]",
            classify_exit(status)
        ));
    }

    Ok(())
}

pub fn contains_dry_run_flag(args: &[String]) -> bool {
    for arg in args {
        if arg == "--" {
            break;
        }
        if arg == "--dry-run" {
            return true;
        }
    }
    false
}

fn resolve_dependency(
    dependency: &Dependency,
    manifest_path: &Path,
    ensure_ready: bool,
) -> Result<PathBuf, String> {
    match &dependency.source {
        DependencySource::Path { path } => {
            let resolved = resolve_dependency_path(path, manifest_path);
            if ensure_ready {
                require_dependency_manifest(dependency, &resolved)?;
            }
            Ok(resolved)
        }
        DependencySource::Git { .. } => {
            let resolved = Path::new(".vex/deps").join(&dependency.name);
            if ensure_ready {
                sync_git_dependency(dependency, &resolved)?;
                require_dependency_manifest(dependency, &resolved)?;
            }
            Ok(resolved)
        }
    }
}

fn sync_git_dependency(dependency: &Dependency, destination: &Path) -> Result<(), String> {
    let DependencySource::Git {
        url,
        branch,
        tag,
        rev,
    } = &dependency.source
    else {
        return Ok(());
    };

    if destination.exists() {
        if !destination.join(".git").is_dir() {
            return Err(format!(
                "managed dependency path `{}` exists but is not a git checkout",
                destination.display()
            ));
        }

        run_git(
            Command::new("git")
                .arg("-C")
                .arg(destination)
                .args(["fetch", "--all", "--tags", "--prune"]),
            "update git dependency",
        )?;
    } else {
        let parent = destination
            .parent()
            .ok_or_else(|| format!("invalid dependency path `{}`", destination.display()))?;
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create `{}`: {e}", parent.display()))?;

        let mut clone = Command::new("git");
        clone.arg("clone");
        if let Some(branch) = branch.as_ref().or(tag.as_ref()) {
            clone.args(["--depth", "1", "--branch", branch]);
        }
        clone.arg(url).arg(destination);
        run_git(&mut clone, "clone git dependency")?;
    }

    if let Some(branch) = branch {
        run_git(
            Command::new("git")
                .arg("-C")
                .arg(destination)
                .args(["checkout", branch]),
            "checkout git dependency branch",
        )?;
        run_git(
            Command::new("git")
                .arg("-C")
                .arg(destination)
                .args(["pull", "--ff-only"]),
            "fast-forward git dependency branch",
        )?;
    } else if let Some(tag) = tag {
        run_git(
            Command::new("git")
                .arg("-C")
                .arg(destination)
                .args(["checkout", tag]),
            "checkout git dependency tag",
        )?;
    } else if let Some(rev) = rev {
        run_git(
            Command::new("git")
                .arg("-C")
                .arg(destination)
                .args(["checkout", rev]),
            "checkout git dependency revision",
        )?;
    } else if destination.exists() {
        run_git(
            Command::new("git")
                .arg("-C")
                .arg(destination)
                .args(["pull", "--ff-only"]),
            "fast-forward git dependency",
        )?;
    }

    Ok(())
}

fn run_git(command: &mut Command, action: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|e| format!("failed to start git for {action}: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    Err(format!(
        "git failed during {action} [{}]: {}",
        classify_exit(output.status),
        combined_output(&output.stdout, &output.stderr)
    ))
}

fn require_dependency_manifest(dependency: &Dependency, path: &Path) -> Result<(), String> {
    let manifest = path.join(MANIFEST_FILE);
    if manifest.is_file() {
        return Ok(());
    }

    Err(format!(
        "dependency `{}` at `{}` must contain `{}`",
        dependency.name,
        path.display(),
        MANIFEST_FILE
    ))
}

fn validate_dry_run_json_output(stdout: &[u8], stderr: &[u8]) -> Result<(), String> {
    let stdout_text = String::from_utf8_lossy(stdout);
    let stderr_text = String::from_utf8_lossy(stderr);

    let candidate = extract_json_object(&stdout_text)
        .or_else(|| extract_json_object(&stderr_text))
        .ok_or_else(|| "wavec dry-run did not return a JSON object plan".to_string())?;

    let value: Value = serde_json::from_str(candidate)
        .map_err(|e| format!("invalid wavec dry-run JSON plan: {e}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "wavec dry-run plan must be a JSON object".to_string())?;

    match object.get("schema_version").and_then(Value::as_u64) {
        Some(1) => {}
        Some(found) => {
            return Err(format!(
                "unsupported wavec dry-run schema_version `{found}`; expected `1`"
            ));
        }
        None => return Err("dry-run JSON is missing numeric key `schema_version`".to_string()),
    }

    for key in ["mode", "target", "emit"] {
        require_string(object, key)?;
    }

    for key in ["emit_kinds", "inputs", "emit_jobs", "compile"] {
        require_array(object, key)?;
    }

    require_string_or_null(object, "control_mode")?;
    require_string_or_null(object, "forced_input_type")?;
    require_link_or_null(object.get("link"))?;
    require_execute_or_null(object.get("execute"))?;

    Ok(())
}

fn require_string(object: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    match object.get(key).and_then(Value::as_str) {
        Some(_) => Ok(()),
        None => Err(format!("dry-run JSON is missing string key `{key}`")),
    }
}

fn require_string_or_null(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<(), String> {
    match object.get(key) {
        Some(value) if value.is_null() || value.as_str().is_some() => Ok(()),
        Some(_) => Err(format!("dry-run JSON key `{key}` must be string or null")),
        None => Err(format!("dry-run JSON is missing key `{key}`")),
    }
}

fn require_array<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a Vec<Value>, String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("dry-run JSON is missing array key `{key}`"))
}

fn require_link_or_null(value: Option<&Value>) -> Result<(), String> {
    let Some(value) = value else {
        return Err("dry-run JSON is missing key `link`".to_string());
    };
    if value.is_null() {
        return Ok(());
    }

    let object = value
        .as_object()
        .ok_or_else(|| "dry-run JSON key `link` must be object or null".to_string())?;
    require_string(object, "output")?;
    require_array(object, "inputs")?;
    require_string(object, "program")?;
    require_array(object, "args")?;
    Ok(())
}

fn require_execute_or_null(value: Option<&Value>) -> Result<(), String> {
    let Some(value) = value else {
        return Err("dry-run JSON is missing key `execute`".to_string());
    };
    if value.is_null() {
        return Ok(());
    }

    let object = value
        .as_object()
        .ok_or_else(|| "dry-run JSON key `execute` must be object or null".to_string())?;
    require_string(object, "program")?;
    require_array(object, "args")?;
    Ok(())
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&text[start..=end])
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);

    match (out.trim().is_empty(), err.trim().is_empty()) {
        (false, false) => format!("{}\n{}", out.trim(), err.trim()),
        (false, true) => out.trim().to_string(),
        (true, false) => err.trim().to_string(),
        (true, true) => "<no output>".to_string(),
    }
}

fn insert_build_flag(args: &mut Vec<String>, flag: &str) {
    if let Some(separator_index) = args.iter().position(|arg| arg == "--") {
        args.insert(separator_index, flag.to_string());
    } else {
        args.push(flag.to_string());
    }
}

fn classify_exit(status: ExitStatus) -> &'static str {
    match status.code() {
        Some(0) => "success",
        Some(1) => "compile/link/run failure",
        Some(2) => "usage error",
        Some(3) => "environment/toolchain/io failure",
        Some(_) => "unknown failure code",
        None => "terminated by signal",
    }
}

fn resolve_dependency_path(dep_path: &str, manifest_path: &Path) -> PathBuf {
    let candidate = PathBuf::from(dep_path);
    let path = if candidate.is_absolute() {
        candidate
    } else {
        let base_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        base_dir.join(candidate)
    };

    path.canonicalize().unwrap_or(path)
}

#[derive(Debug, Clone)]
struct WavecCandidate {
    path: PathBuf,
    source: &'static str,
    explicit: bool,
}

impl WavecCandidate {
    fn display(&self) -> String {
        format!("{} ({})", self.path.to_string_lossy(), self.source)
    }
}

struct DryRunOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn select_compatible_wavec(
    dry_run_args: &[String],
) -> Result<(WavecCandidate, DryRunOutput), String> {
    let candidates = wavec_candidates();
    let mut failures = Vec::new();

    for candidate in candidates {
        let output = match Command::new(&candidate.path).args(dry_run_args).output() {
            Ok(output) => output,
            Err(err) => {
                let message = format!("{}: failed to execute: {err}", candidate.display());
                if candidate.explicit {
                    return Err(format!("explicit VEX_WAVEC is not usable. {message}"));
                }
                failures.push(message);
                continue;
            }
        };

        if !output.status.success() {
            let text = combined_output(&output.stdout, &output.stderr);
            if looks_like_incompatible_wavec(&text) {
                let message = format!(
                    "{}: incompatible wavec [{}]: {}",
                    candidate.display(),
                    classify_exit(output.status),
                    text
                );
                if candidate.explicit {
                    return Err(message);
                }
                failures.push(message);
                continue;
            }

            return Err(format!(
                "wavec dry-run failed using {} [{}]: {}",
                candidate.display(),
                classify_exit(output.status),
                text
            ));
        }

        if let Err(err) = validate_dry_run_json_output(&output.stdout, &output.stderr) {
            let message = format!(
                "{}: incompatible dry-run contract: {err}",
                candidate.display()
            );
            if candidate.explicit {
                return Err(message);
            }
            failures.push(message);
            continue;
        }

        return Ok((
            candidate,
            DryRunOutput {
                stdout: output.stdout,
                stderr: output.stderr,
            },
        ));
    }

    let mut message = String::from(
        "no compatible wavec found. Vex requires `wavec build --dry-run --error-format=json` schema_version 1",
    );
    if !failures.is_empty() {
        message.push_str("\ntried:");
        for failure in failures {
            message.push_str("\n  - ");
            message.push_str(&failure);
        }
    }
    message.push_str("\nhelp: install the current Wave compiler or set VEX_WAVEC=/path/to/wavec");
    Err(message)
}

fn wavec_candidates() -> Vec<WavecCandidate> {
    if let Some(explicit) = env::var_os("VEX_WAVEC").filter(|value| !value.is_empty()) {
        return vec![WavecCandidate {
            path: PathBuf::from(explicit),
            source: "VEX_WAVEC",
            explicit: true,
        }];
    }

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let exe_name = wavec_executable_name();

    if let Ok(current_exe) = env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            push_candidate(
                &mut candidates,
                &mut seen,
                dir.join(exe_name),
                "next to vex",
                false,
            );
        }

        for ancestor in current_exe.ancestors() {
            push_candidate(
                &mut candidates,
                &mut seen,
                ancestor
                    .join("Wave")
                    .join("target")
                    .join("debug")
                    .join(exe_name),
                "workspace Wave debug build",
                false,
            );
            push_candidate(
                &mut candidates,
                &mut seen,
                ancestor
                    .join("Wave")
                    .join("target")
                    .join("release")
                    .join(exe_name),
                "workspace Wave release build",
                false,
            );
        }
    }

    if let Some(wave_home) = env::var_os("WAVE_HOME") {
        push_candidate(
            &mut candidates,
            &mut seen,
            PathBuf::from(wave_home).join("bin").join(exe_name),
            "WAVE_HOME",
            false,
        );
    }

    if let Some(home) = home_dir() {
        push_candidate(
            &mut candidates,
            &mut seen,
            home.join(".wave").join("bin").join(exe_name),
            "home wave install",
            false,
        );
    }

    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            push_candidate(
                &mut candidates,
                &mut seen,
                dir.join(exe_name),
                "PATH",
                false,
            );
        }
    }

    if candidates.is_empty() {
        candidates.push(WavecCandidate {
            path: PathBuf::from(exe_name),
            source: "PATH",
            explicit: false,
        });
    }

    candidates
}

fn push_candidate(
    candidates: &mut Vec<WavecCandidate>,
    seen: &mut HashSet<String>,
    path: PathBuf,
    source: &'static str,
    explicit: bool,
) {
    if !path.is_file() {
        return;
    }

    let key = path
        .canonicalize()
        .unwrap_or_else(|_| path.clone())
        .to_string_lossy()
        .to_string();
    if !seen.insert(key) {
        return;
    }

    candidates.push(WavecCandidate {
        path,
        source,
        explicit,
    });
}

fn looks_like_incompatible_wavec(output: &str) -> bool {
    output.contains("unknown option for build: --dry-run")
        || output.contains("unknown option") && output.contains("--dry-run")
        || output.contains("unknown option") && output.contains("--error-format")
}

fn wavec_executable_name() -> &'static str {
    if cfg!(windows) {
        "wavec.exe"
    } else {
        "wavec"
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_plan() -> &'static str {
        r#"{
            "schema_version": 1,
            "mode": "build",
            "target": "x86_64-unknown-linux-gnu",
            "emit": "bin",
            "emit_kinds": ["bin"],
            "control_mode": null,
            "forced_input_type": null,
            "inputs": [{"path":"src/main.wave","kind":"wave"}],
            "emit_jobs": [],
            "compile": [{"input":"src/main.wave","kind":"wave","output":"target/main.o","command":"wavec <internal>"}],
            "link": {"output":"target/main","inputs":["target/main.o"],"program":"ld.lld","args":["target/main.o"]},
            "execute": null
        }"#
    }

    #[test]
    fn dry_run_schema_v1_is_validated() {
        validate_dry_run_json_output(valid_plan().as_bytes(), b"")
            .expect("schema v1 dry-run plan must be accepted");

        let missing_execute = valid_plan().replace(",\n            \"execute\": null", "");
        let err = validate_dry_run_json_output(missing_execute.as_bytes(), b"")
            .expect_err("execute is required by the v1 contract");
        assert!(err.contains("execute"), "{err}");

        let schema_two = valid_plan().replace("\"schema_version\": 1", "\"schema_version\": 2");
        let err = validate_dry_run_json_output(schema_two.as_bytes(), b"")
            .expect_err("unknown schema version must be rejected");
        assert!(err.contains("schema_version"), "{err}");
    }

    #[test]
    fn dry_run_json_can_be_extracted_from_noisy_output() {
        let output = format!("debug line\n{}\n", valid_plan());
        validate_dry_run_json_output(output.as_bytes(), b"")
            .expect("JSON object should be extracted from mixed output");
    }
}
