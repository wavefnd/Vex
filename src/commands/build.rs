use std::fs;
use std::path::{Path, PathBuf};

use crate::lockfile::write_lockfile;
use crate::manifest::Manifest;
use crate::validate::{collect_inputs, validate_build_invocation, BuildValidationRequest};
use crate::wavec::{
    build_dependency_global_args, contains_dry_run_flag, run_build_with_dry_run,
    split_global_and_build_args,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildMode {
    Build,
    Run,
    Check,
}

pub fn build(mode: BuildMode, args: &[String]) {
    if let Err(err) = run_build(mode, args) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_build(mode: BuildMode, args: &[String]) -> Result<(), String> {
    let manifest = Manifest::load()?;
    let split = split_global_and_build_args(args)?;
    let user_global_args = split.global_args.clone();
    let mut build_args = split.build_args.clone();

    match mode {
        BuildMode::Build => {}
        BuildMode::Run => build_args.push("--run".to_string()),
        BuildMode::Check => build_args.push("--emit=check".to_string()),
    }

    let explicit_inputs = collect_inputs(&build_args);

    if explicit_inputs.is_empty() {
        let default_input = resolve_default_input(&manifest, mode)?;
        build_args.insert(0, default_input);
    }

    validate_build_invocation(BuildValidationRequest {
        inputs: &collect_inputs(&build_args),
        build_args: &build_args,
        run_args: &split.run_args,
        run_separator_seen: split.run_separator_seen,
        global_args: &split.global_args,
    })?;

    let user_requested_dry_run = contains_dry_run_flag(&build_args);
    if !user_requested_dry_run {
        fs::create_dir_all("target").map_err(|e| format!("failed to create target/: {e}"))?;
    }

    let dependency_args = build_dependency_global_args(&manifest, !user_requested_dry_run)?;
    if !user_requested_dry_run {
        write_lockfile(&manifest)?;
    }

    let mut wavec_args = Vec::new();
    wavec_args.extend(dependency_args);
    wavec_args.extend(user_global_args);
    wavec_args.push("build".to_string());
    wavec_args.extend(build_args);

    if split.run_separator_seen {
        wavec_args.push("--".to_string());
        wavec_args.extend(split.run_args);
    }

    run_build_with_dry_run(&wavec_args, user_requested_dry_run)
}

fn resolve_default_input(manifest: &Manifest, mode: BuildMode) -> Result<String, String> {
    if mode == BuildMode::Run && manifest.lib {
        return Err(
            "library manifest cannot be `vex run` default target. Pass an explicit .wave file."
                .to_string(),
        );
    }

    let preferred = manifest.default_entry_path();
    if preferred.exists() {
        return Ok(preferred.to_string_lossy().to_string());
    }

    if mode == BuildMode::Run {
        if let Some(path) = find_wave_file_with_main(Path::new("src"))? {
            return Ok(path.to_string_lossy().to_string());
        }
    }

    if let Some(path) = find_first_wave_file(Path::new("src"))? {
        return Ok(path.to_string_lossy().to_string());
    }

    Err(format!(
        "no default Wave input found. Expected `{}` or any `.wave` file in `src/`.",
        preferred.to_string_lossy()
    ))
}

fn find_first_wave_file(src_dir: &Path) -> Result<Option<PathBuf>, String> {
    if !src_dir.exists() {
        return Ok(None);
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(src_dir).map_err(|e| format!("failed to read src/: {e}"))? {
        let entry = entry.map_err(|e| format!("failed to read src entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("wave") {
            files.push(path);
        }
    }

    files.sort();
    Ok(files.into_iter().next())
}

fn find_wave_file_with_main(src_dir: &Path) -> Result<Option<PathBuf>, String> {
    if !src_dir.exists() {
        return Ok(None);
    }

    let mut candidates = Vec::new();

    for entry in fs::read_dir(src_dir).map_err(|e| format!("failed to read src/: {e}"))? {
        let entry = entry.map_err(|e| format!("failed to read src entry: {e}"))?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("wave") {
            continue;
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read `{}`: {e}", path.to_string_lossy()))?;

        if content.contains("fun main()") {
            candidates.push(path);
        }
    }

    candidates.sort();
    Ok(candidates.into_iter().next())
}
