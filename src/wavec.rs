use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use serde_json::Value;

use crate::manifest::{Dependency, DependencySource, Manifest, MANIFEST_FILE};

#[derive(Debug, Clone)]
pub struct SplitBuildArgs {
    pub global_args: Vec<String>,
    pub build_args: Vec<String>,
    pub run_args: Vec<String>,
    pub run_separator_seen: bool,
}

pub fn split_global_and_build_args(args: &[String]) -> Result<SplitBuildArgs, String> {
    let mut global_args = Vec::new();
    let mut build_args = Vec::new();
    let mut run_args = Vec::new();
    let mut run_separator_seen = false;

    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();

        if token == "--" {
            run_separator_seen = true;
            run_args.extend_from_slice(&args[i + 1..]);
            break;
        }

        if let Some(consumed) = parse_global_option(args, i, &mut global_args)? {
            i += consumed;
            continue;
        }

        build_args.push(args[i].clone());
        i += 1;
    }

    Ok(SplitBuildArgs {
        global_args,
        build_args,
        run_args,
        run_separator_seen,
    })
}

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
    let program = wavec_program();
    let mut dry_run_args = args.to_vec();

    if !contains_dry_run_flag(&dry_run_args) {
        insert_build_flag(&mut dry_run_args, "--dry-run");
    }
    insert_build_flag(&mut dry_run_args, "--error-format=json");

    let plan_output = Command::new(&program)
        .args(&dry_run_args)
        .output()
        .map_err(|e| {
            format!(
                "failed to execute `{program}` dry-run: {e}. install compiler with `vex setup wavec` or set VEX_WAVEC"
            )
        })?;

    if !plan_output.status.success() {
        return Err(format!(
            "wavec dry-run failed [{}]: {}",
            classify_exit(plan_output.status),
            combined_output(&plan_output.stdout, &plan_output.stderr)
        ));
    }

    validate_dry_run_json_output(&plan_output.stdout, &plan_output.stderr)?;

    if !user_requested_dry_run {
        let status = Command::new(&program)
            .args(args)
            .status()
            .map_err(|e| format!("failed to execute `{program} build`: {e}"))?;
        if !status.success() {
            return Err(format!("wavec build failed [{}]", classify_exit(status)));
        }
        return Ok(());
    }

    let status = Command::new(&program)
        .args(args)
        .status()
        .map_err(|e| format!("failed to execute `{program} build --dry-run`: {e}"))?;
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

fn parse_global_option(
    args: &[String],
    index: usize,
    output: &mut Vec<String>,
) -> Result<Option<usize>, String> {
    let token = args[index].as_str();

    if matches!(
        token,
        "-O0" | "-O1" | "-O2" | "-O3" | "-Os" | "-Oz" | "-Ofast" | "--llvm"
    ) {
        output.push(token.to_string());
        return Ok(Some(1));
    }

    if token.starts_with("--debug-wave=")
        || token.starts_with("--link=")
        || token.starts_with("--dep-root=")
        || token.starts_with("--dep=")
        || token.starts_with("--target=")
        || token.starts_with("--cpu=")
        || token.starts_with("--features=")
        || token.starts_with("--abi=")
        || token.starts_with("--sysroot=")
        || token.starts_with("-Lnative=")
        || (token.starts_with("-C") && token != "-C")
        || (token.starts_with("-L") && token != "-L")
    {
        output.push(token.to_string());
        return Ok(Some(1));
    }

    if matches!(
        token,
        "--debug-wave"
            | "--link"
            | "--dep-root"
            | "--dep"
            | "--target"
            | "--cpu"
            | "--features"
            | "--abi"
            | "--sysroot"
            | "-L"
            | "-C"
    ) {
        if index + 1 >= args.len() {
            return Err(format!("missing value for global option `{token}`"));
        }
        output.push(token.to_string());
        output.push(args[index + 1].clone());
        return Ok(Some(2));
    }

    Ok(None)
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

fn wavec_program() -> String {
    env::var("VEX_WAVEC")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "wavec".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

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
    fn split_preserves_wavec_global_build_and_run_args() {
        let split = split_global_and_build_args(&strings(&[
            "--target",
            "x86_64-unknown-none-elf",
            "-C",
            "relocation-model=static",
            "src/main.wave",
            "--emit=bin",
            "--",
            "one",
            "--flag",
        ]))
        .expect("split must succeed");

        assert_eq!(
            split.global_args,
            strings(&[
                "--target",
                "x86_64-unknown-none-elf",
                "-C",
                "relocation-model=static"
            ])
        );
        assert_eq!(split.build_args, strings(&["src/main.wave", "--emit=bin"]));
        assert_eq!(split.run_args, strings(&["one", "--flag"]));
        assert!(split.run_separator_seen);
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
