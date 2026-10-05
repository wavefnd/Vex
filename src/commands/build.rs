use crate::{messages::Messages, outcome::Outcome};
use diagnostic::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ui;
use compiler::{run_build_with_dry_run, Compiler};
use manifest::Manifest;
use resolver::{resolve_validated, ResolveOptions, UpdatePolicy};

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
    locked: bool,
    offline: bool,
}

pub fn build(
    mode: BuildMode,
    args: &[String],
    runtime: &[std::ffi::OsString],
    selection: &crate::project::Selection,
    messages: &mut Messages,
) -> Result<Outcome, Error> {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        diagnostic::outln!("{}", build_usage(mode));
        return Ok(Outcome::success());
    }
    let options = parse_vex_build_options(mode, args).map_err(Error::usage)?;
    let compiler = Compiler::select();
    let _project = selection.enter()?;
    let manifest = Manifest::load()?;
    let mut compiler = compiler?;
    run_build(mode, &manifest, options, runtime, &mut compiler, messages)
}

fn run_build(
    mode: BuildMode,
    manifest: &Manifest,
    options: VexBuildOptions,
    runtime: &[std::ffi::OsString],
    compiler: &mut Compiler,
    messages: &mut Messages,
) -> Result<Outcome, Error> {
    let started = Instant::now();
    std::env::current_dir()
        .map_err(Error::environment)?
        .to_str()
        .ok_or_else(|| {
            Error::resolution("wavec JSON protocol cannot represent a non-UTF-8 project root")
        })?;
    if let Some(target) = &options.target {
        compiler.validate_target(target)?;
    }
    compiler.validate_version(manifest.compiler.as_deref())?;
    let default_input = resolve_default_input(manifest, mode)?;

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

    let resolution = resolve_validated(
        manifest,
        ResolveOptions {
            dry_run: options.dry_run,
            update: UpdatePolicy::ReuseLocked,
            locked: options.locked,
            offline: options.offline,
        },
        ui::status,
        |candidate| compiler.validate_version(candidate.compiler.as_deref()),
    )?;

    let generation = if mode == BuildMode::Run {
        let path = if options.dry_run {
            PathBuf::from("target/.vex-run/planned")
        } else {
            compiler::create_run_generation()?
        };
        build_args.push(format!("--target-dir={}", path.display()));
        Some(path)
    } else {
        None
    };
    let mut wavec_args = Vec::new();
    wavec_args.extend(resolution.dependency_args()?);
    wavec_args.extend(global_args);
    wavec_args.push("build".to_string());
    wavec_args.extend(build_args);

    let package = format!("{} v{}", manifest.name, manifest.version);
    if options.dry_run {
        ui::status("Planning", &package);
    } else {
        ui::status(
            match mode {
                BuildMode::Check => "Checking",
                BuildMode::Build | BuildMode::Run => "Compiling",
            },
            &package,
        );
    }

    messages.status("compiler")?;
    let reporting = messages.enabled();
    let mut report = |event| messages.emit(event);
    let execution = run_build_with_dry_run(
        compiler,
        &wavec_args,
        runtime,
        options.dry_run,
        generation.as_deref(),
        if reporting { Some(&mut report) } else { None },
    )?;
    drop(resolution);
    let mut outcome = Outcome::success();
    if let Some(execution) = execution {
        ui::status("Running", &manifest.name);
        messages.status("running")?;
        let status = execution.execute()?;
        outcome = Outcome::program(status);
    }

    if !options.dry_run && outcome.code == 0 {
        ui::status(
            "Finished",
            format!(
                "{} profile in {:.2}s",
                if options.release { "release" } else { "dev" },
                started.elapsed().as_secs_f64()
            ),
        );
    }
    Ok(outcome)
}

fn parse_vex_build_options(mode: BuildMode, args: &[String]) -> Result<VexBuildOptions, String> {
    let mut options = VexBuildOptions::default();
    let mut i = 0;

    while i < args.len() {
        let token = args[i].as_str();

        if token == "--" {
            if mode != BuildMode::Run {
                return Err(
                    "runtime arguments after `--` are only valid with `vex run`".to_string()
                );
            }
            return Ok(options);
        }

        match token {
            "--target" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for `--target`".to_string());
                }
                set_target(&mut options, &args[i])?;
            }
            "--release" => options.release = true,
            "--dry-run" => options.dry_run = true,
            "--locked" => options.locked = true,
            "--offline" => options.offline = true,
            "-h" | "--help" => return Err(build_usage(mode).to_string()),
            _ if token.starts_with("--target=") => {
                set_target(&mut options, token.trim_start_matches("--target="))?;
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

fn set_target(options: &mut VexBuildOptions, target: &str) -> Result<(), String> {
    if target.is_empty() {
        return Err("missing value for `--target`: expected a target triple".to_string());
    }
    if target.starts_with('-') {
        return Err(format!(
            "missing value for `--target`: expected a target triple, found option `{target}`"
        ));
    }
    if !target
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "invalid value `{target}` for `--target`: expected a target triple containing only ASCII letters, digits, `-`, `_`, or `.`"
        ));
    }
    if options.target.is_some() {
        return Err("`--target` may only be specified once".to_string());
    }
    options.target = Some(target.to_string());
    Ok(())
}

fn build_usage(mode: BuildMode) -> &'static str {
    match mode {
        BuildMode::Build => {
            "usage: vex build [--target <triple>] [--release] [--dry-run] [--locked] [--offline]"
        }
        BuildMode::Run => {
            "usage: vex run [--target <triple>] [--release] [--dry-run] [--locked] [--offline] [-- <args...>]"
        }
        BuildMode::Check => {
            "usage: vex check [--target <triple>] [--release] [--dry-run] [--locked] [--offline]"
        }
    }
}

fn resolve_default_input(manifest: &Manifest, mode: BuildMode) -> Result<String, Error> {
    if mode == BuildMode::Run && manifest.lib {
        return Err(Error::resolution(
            "library packages cannot be run; use a binary package with src/main.wave.".to_string(),
        ));
    }

    let preferred = manifest.default_entry_path();
    if preferred.is_file() {
        return input_string(&preferred);
    }

    Err(Error::resolution(format!(
        "missing canonical package entry `{}`\nhelp: create this file; Vex does not select arbitrary .wave files",
        preferred.display()
    )))
}

fn input_string(path: &Path) -> Result<String, Error> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        Error::resolution("wavec JSON protocol cannot represent a non-UTF-8 input path")
    })
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
                "--locked",
                "--offline",
                "--",
            ]),
        )
        .expect("options must parse");

        assert_eq!(options.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert!(options.release);
        assert!(options.locked);
        assert!(options.offline);
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

    #[test]
    fn rejects_empty_and_duplicate_targets() {
        for arguments in [
            strings(&["--target="]),
            strings(&["--target", ""]),
            strings(&["--target", "host", "--target=other"]),
        ] {
            parse_vex_build_options(BuildMode::Build, &arguments)
                .expect_err("invalid target options must be rejected");
        }
    }

    #[test]
    fn rejects_option_tokens_and_invalid_target_syntax() {
        for mode in [BuildMode::Build, BuildMode::Run, BuildMode::Check] {
            for token in ["--release", "--dry-run", "--locked", "--offline", "--"] {
                let err = parse_vex_build_options(mode, &strings(&["--target", token]))
                    .expect_err("another option cannot be a target value");
                assert!(err.contains("missing value for `--target`"), "{err}");
                assert!(err.contains(token), "{err}");
            }
            let err = parse_vex_build_options(mode, &strings(&["--target=bad/target"]))
                .expect_err("target syntax must be checked by Vex");
            assert!(err.contains("invalid value"), "{err}");
        }
    }
}
