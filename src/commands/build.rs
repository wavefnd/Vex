use std::fs;
use std::path::{Path, PathBuf};

use crate::lockfile::write_lockfile;
use crate::manifest::Manifest;
use crate::validate::{collect_inputs, validate_build_invocation, BuildValidationRequest};
use crate::wavec::{build_dependency_global_args, run_build_with_dry_run};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildMode {
    Build,
    Run,
    Check,
}

#[derive(Debug, Default)]
struct VexBuildOptions {
    target: Option<String>,
    release: bool,
    dry_run: bool,
    run_args: Vec<String>,
    run_separator_seen: bool,
}

pub fn build(mode: BuildMode, args: &[String]) {
    if let Err(err) = run_build(mode, args) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_build(mode: BuildMode, args: &[String]) -> Result<(), String> {
    let manifest = Manifest::load()?;
    let options = parse_vex_build_options(mode, args)?;
    let default_input = resolve_default_input(&manifest, mode)?;

    let mut global_args = Vec::new();
    if options.release {
        global_args.push("-O2".to_string());
    }
    if let Some(target) = options.target.as_ref() {
        global_args.push("--target".to_string());
        global_args.push(target.clone());
    }

    let mut build_args = vec![default_input];
    match mode {
        BuildMode::Build => {}
        BuildMode::Run => build_args.push("--run".to_string()),
        BuildMode::Check => build_args.push("--emit=check".to_string()),
    }
    if options.dry_run {
        build_args.push("--dry-run".to_string());
    }

    let inputs = collect_inputs(&build_args);
    validate_build_invocation(BuildValidationRequest {
        inputs: &inputs,
        build_args: &build_args,
        run_args: &options.run_args,
        run_separator_seen: options.run_separator_seen,
        global_args: &global_args,
    })?;

    if !options.dry_run {
        fs::create_dir_all("target").map_err(|e| format!("failed to create target/: {e}"))?;
    }

    let dependency_args = build_dependency_global_args(&manifest, !options.dry_run)?;
    if !options.dry_run {
        write_lockfile(&manifest)?;
    }

    let mut wavec_args = Vec::new();
    wavec_args.extend(dependency_args);
    wavec_args.extend(global_args);
    wavec_args.push("build".to_string());
    wavec_args.extend(build_args);

    if options.run_separator_seen {
        wavec_args.push("--".to_string());
        wavec_args.extend(options.run_args);
    }

    run_build_with_dry_run(&wavec_args, options.dry_run)
}

fn parse_vex_build_options(mode: BuildMode, args: &[String]) -> Result<VexBuildOptions, String> {
    let mut options = VexBuildOptions::default();
    let mut i = 0;

    while i < args.len() {
        let token = args[i].as_str();

        if token == "--" {
            options.run_separator_seen = true;
            if mode != BuildMode::Run {
                return Err(
                    "runtime arguments after `--` are only valid with `vex run`".to_string()
                );
            }
            options.run_args.extend_from_slice(&args[i + 1..]);
            return Ok(options);
        }

        match token {
            "--target" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for `--target`".to_string());
                }
                options.target = Some(args[i].clone());
            }
            "--release" => options.release = true,
            "--dry-run" => options.dry_run = true,
            "-h" | "--help" => return Err(build_usage(mode).to_string()),
            _ if token.starts_with("--target=") => {
                options.target = Some(token.trim_start_matches("--target=").to_string());
            }
            _ if token.starts_with('-') => {
                return Err(format!(
                    "unknown Vex option `{token}`. Vex does not accept raw wavec flags here"
                ));
            }
            _ => {
                return Err(format!(
                    "unexpected argument `{token}`. Vex builds the package described by vex.ws"
                ));
            }
        }

        i += 1;
    }

    Ok(options)
}

fn build_usage(mode: BuildMode) -> &'static str {
    match mode {
        BuildMode::Build => "usage: vex build [--target <triple>] [--release] [--dry-run]",
        BuildMode::Run => {
            "usage: vex run [--target <triple>] [--release] [--dry-run] [-- <args...>]"
        }
        BuildMode::Check => "usage: vex check [--target <triple>] [--release] [--dry-run]",
    }
}

fn resolve_default_input(manifest: &Manifest, mode: BuildMode) -> Result<String, String> {
    if mode == BuildMode::Run && manifest.lib {
        return Err(
            "library manifest cannot be `vex run` default target. Add a binary target to vex.ws."
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_small_vex_build_option_surface() {
        let options = parse_vex_build_options(
            BuildMode::Run,
            &strings(&[
                "--target",
                "x86_64-unknown-linux-gnu",
                "--release",
                "--",
                "arg",
            ]),
        )
        .expect("options must parse");

        assert_eq!(options.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert!(options.release);
        assert_eq!(options.run_args, strings(&["arg"]));
    }

    #[test]
    fn rejects_raw_wavec_flags_and_source_inputs() {
        let err = parse_vex_build_options(BuildMode::Build, &strings(&["--emit=obj"]))
            .expect_err("raw wavec flags are not Vex build options");
        assert!(err.contains("raw wavec flags"), "{err}");

        let err = parse_vex_build_options(BuildMode::Run, &strings(&["src/main.wave"]))
            .expect_err("Vex run is manifest-based");
        assert!(err.contains("vex.ws"), "{err}");
    }
}
