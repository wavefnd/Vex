// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use crate::{messages::Messages, outcome::Outcome};
use diagnostic::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ui;
use compiler::{prepare_build, Compiler};
use manifest::Manifest;
use manifest::{PackageTarget, TargetKind};
use resolver::{resolve_validated, ResolveOptions, UpdatePolicy};
use serde_json::json;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildMode {
    Build,
    Run,
    Check,
    Test,
}

#[derive(Clone, Debug, Default)]
struct VexBuildOptions {
    target: Option<String>,
    release: bool,
    profile: Option<String>,
    library: bool,
    binaries: Vec<String>,
    filter: Option<String>,
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
    run_build(
        mode,
        &_project.selected,
        options,
        runtime,
        compiler,
        messages,
        selection.graph.clone(),
    )
}

struct MemberBuild {
    manifest: Manifest,
    targets: Vec<PackageTarget>,
}
struct PlannedTarget {
    package: String,
    manifest_path: PathBuf,
    directory: PathBuf,
    target: PackageTarget,
    plan: compiler::PreparedBuild,
}
fn run_build(
    mode: BuildMode,
    manifests: &[Manifest],
    options: VexBuildOptions,
    runtime: &[std::ffi::OsString],
    compiler: Result<Compiler, Error>,
    messages: &mut Messages,
    mut graph_selection: source::graph::Selection,
) -> Result<Outcome, Error> {
    graph_selection.include_tests = mode == BuildMode::Test;
    let started = Instant::now();
    let root = std::env::current_dir().map_err(Error::environment)?;
    root.to_str().ok_or_else(|| {
        Error::resolution("wavec JSON protocol cannot represent a non-UTF-8 project root")
    })?;
    let requested_selection = options.library || !options.binaries.is_empty();
    if manifests.len() > 1 {
        for name in &options.binaries {
            if !manifests.iter().any(|m| {
                m.targets
                    .iter()
                    .any(|t| t.kind == TargetKind::Binary && t.name == *name)
            }) {
                return Err(Error::resolution(format!("unknown binary target `{name}`")));
            }
        }
    }
    let mut members = Vec::new();
    for manifest in manifests {
        if mode == BuildMode::Test
            && !manifest
                .tests
                .iter()
                .any(|t| options.filter.as_ref().is_none_or(|f| t.name.contains(f)))
        {
            continue;
        }
        let mut local = options.clone();
        if manifests.len() > 1 {
            local.library &= manifest.lib;
            local.binaries.retain(|name| {
                manifest
                    .targets
                    .iter()
                    .any(|t| t.kind == TargetKind::Binary && t.name == *name)
            });
            if requested_selection && !local.library && local.binaries.is_empty() {
                continue;
            }
            if mode == BuildMode::Run
                && !manifest
                    .targets
                    .iter()
                    .any(|t| t.kind == TargetKind::Binary)
            {
                continue;
            }
        }
        let targets = select_targets(manifest, mode, &local)?
            .into_iter()
            .cloned()
            .collect();
        members.push(MemberBuild {
            manifest: manifest.clone(),
            targets,
        });
    }
    if mode == BuildMode::Test {
        members.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    }
    if members.is_empty() {
        return Err(Error::resolution("no package targets match the selection"));
    }
    if mode == BuildMode::Run && (members.len() != 1 || members[0].targets.len() != 1) {
        return Err(Error::resolution("vex run requires one binary across the selected packages; use -p <package> and --bin <name>"));
    }
    let selected_paths: Vec<_> = members
        .iter()
        .map(|m| m.manifest.source_path.clone())
        .collect();
    let profile_name =
        options
            .profile
            .as_deref()
            .unwrap_or(if options.release { "release" } else { "dev" });
    for member in &mut members {
        if let Some(scope) = &mut member.manifest.scope {
            scope.selected = selected_paths.clone();
        }
        member
            .manifest
            .profile(profile_name)
            .map_err(Error::resolution)?;
        for target in &member.targets {
            validate_entry(&member.manifest, target)?;
        }
    }
    let primary = &members[0].manifest;
    let mut selected_compiler = compiler?;
    let compiler = &mut selected_compiler;
    let requested_triple = options
        .target
        .as_deref()
        .or(primary.build_target.as_deref());
    if let Some(target) = requested_triple {
        compiler.validate_target(target)?;
    }
    for member in &members {
        compiler.validate_version(member.manifest.compiler.as_deref())?;
    }
    let recovery_guard =
        state::Guard::acquire(options.dry_run, ui::status).map_err(Error::environment)?;
    resolver::recover_project(&recovery_guard, options.dry_run)?;
    drop(recovery_guard);
    let spec = compiler.target_spec(requested_triple)?;
    let triple = spec["triple"].as_str().unwrap().to_owned();
    let mut requests = Vec::new();
    for member in &members {
        for target in &member.targets {
            let mut selection = graph_selection.clone();
            let mut context = source::graph::Context::new(&triple).map_err(Error::resolution)?;
            if let Some(mode) = member.manifest.platform_for(target, &triple).mode {
                context.mode = mode.as_str().into();
            }
            selection.target = Some(context);
            requests.push(resolver::ActivationRequest {
                manifest: resolver::manifest_key(&member.manifest.source_path)?,
                selection,
            });
        }
    }
    let mut resolution = resolve_validated(
        primary,
        ResolveOptions {
            requests,
            selection: graph_selection.clone(),
            dry_run: options.dry_run,
            update: UpdatePolicy::ReuseLocked,
            locked: options.locked,
            offline: options.offline,
            ..Default::default()
        },
        ui::status,
        |candidate| compiler.validate_version(candidate.compiler.as_deref()),
    )?;
    ui::verbose(format!("target: {triple}"));
    ui::verbose(format!("compiler: {}", compiler.path().display()));
    ui::verbose(format!(
        "resolved {} dependency bindings",
        resolution.package_count()
    ));
    if matches!(mode, BuildMode::Run | BuildMode::Test)
        && !runtime.is_empty()
        && spec["object_format"] == "wasm"
        && spec["hosted"] == false
    {
        return Err(Error::resolution(
            "bare WebAssembly has no process argument ABI; use a WASI target for program arguments",
        ));
    }
    let mut run_lease = None;
    let generation = if matches!(mode, BuildMode::Run | BuildMode::Test) {
        Some(if options.dry_run {
            PathBuf::from("target/.vex-run/planned")
        } else {
            let path = compiler::create_run_generation()?;
            run_lease =
                Some(state::artifacts::RunLease::create(&root, &path).map_err(Error::environment)?);
            path
        })
    } else {
        None
    };
    let mut prepared = Vec::new();
    let reporting = messages.enabled();
    messages.status("compiler")?;
    for member in &members {
        let profile = member
            .manifest
            .profile(profile_name)
            .map_err(Error::resolution)?;
        let directory = member
            .manifest
            .source_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .map_err(Error::environment)?;
        for target in &member.targets {
            let mut selection = graph_selection.clone();
            let mut context = source::graph::Context::new(&triple).map_err(Error::resolution)?;
            if let Some(mode) = member.manifest.platform_for(target, &triple).mode {
                context.mode = mode.as_str().into();
            }
            selection.target = Some(context);
            resolution.activate_root(&member.manifest.source_path, &selection)?;
            let configuration = crate::native::prepare_target(
                &member.manifest,
                target,
                &resolution,
                compiler,
                &triple,
                profile_name,
                mode == BuildMode::Check,
                generation.as_deref(),
            )?;
            let mut args = resolution.dependency_args()?;
            if target.kind == TargetKind::Binary && member.manifest.lib {
                args.push(format!(
                    "--dep={}={}",
                    member.manifest.name,
                    input_string(&directory)?
                ));
            }
            args.push(format!("-O{}", profile.opt_level));
            args.extend(configuration.global);
            args.push("build".into());
            args.push(input_string(&target.path)?);
            if matches!(mode, BuildMode::Run | BuildMode::Test) {
                args.push("--run".into());
            }
            if mode == BuildMode::Check {
                args.push("--emit=check".into());
            }
            args.extend(configuration.inputs);
            args.extend(configuration.output);
            let identity = json!({"name":target.name,"kind":target.kind.as_str()});
            let mut report = |mut event: serde_json::Value| {
                event["package"] = json!(member.manifest.name);
                event["package_target"] = identity.clone();
                event["profile"] = json!(profile_name);
                messages.emit(event)
            };
            let plan = prepare_build(
                compiler,
                &args,
                runtime,
                options.dry_run,
                generation.as_deref(),
                if reporting { Some(&mut report) } else { None },
            )
            .map_err(|e| target_error(e, target, profile_name))?;
            prepared.push(PlannedTarget {
                package: member.manifest.name.clone(),
                manifest_path: member.manifest.source_path.clone(),
                directory: directory.clone(),
                target: target.clone(),
                plan,
            });
        }
    }
    let paths = prepared
        .iter()
        .flat_map(|p| p.plan.output_paths().cloned())
        .collect::<Vec<_>>();
    state::artifacts::validate_output_paths(&root, &paths).map_err(Error::compiler)?;
    if options.dry_run {
        ui::verbose("freshness: planning only; compiler will not execute");
        let plans = prepared.iter().map(|task| json!({"package":task.package,"manifest_path":task.manifest_path,"build_directory":root,"run_directory":if matches!(mode, BuildMode::Run|BuildMode::Test) { Some(&task.directory) } else { None },"package_target":{"name":task.target.name,"kind":task.target.kind.as_str()},"compiler_plan":task.plan.plan()})).collect::<Vec<_>>();
        for task in &prepared {
            ui::status(
                "Planning",
                format!(
                    "{} {} ({})",
                    task.target.kind.as_str(),
                    task.target.name,
                    task.package
                ),
            );
        }
        let profile = primary.profile(profile_name).map_err(Error::resolution)?;
        diagnostic::outln!("{}", serde_json::to_string_pretty(&json!({"schema":"vex.build-plan","schema_version":1,"target":triple,"profile":{"name":profile_name,"opt_level":profile.opt_level},"plans":plans})).map_err(Error::internal)?);
        return Ok(Outcome::success());
    }
    for task in &prepared {
        task.plan
            .preflight_ownership()
            .map_err(|e| target_error(e, &task.target, profile_name))?;
    }
    let mut source_roots = members
        .iter()
        .map(|m| m.manifest.source_path.parent().unwrap().to_path_buf())
        .collect::<Vec<_>>();
    source_roots.push(root.clone());
    source_roots.extend(resolution.union_packages().iter().map(|p| p.directory()));
    let freshness = if prepared
        .iter()
        .any(|task| task.plan.cache_eligibility().is_ok())
    {
        compiler::freshness::Inputs::capture(compiler, source_roots, &json!({"capabilities":spec,"selection":graph_selection,"features":resolution.views().iter().map(|view|&view.features).collect::<Vec<_>>()} ).to_string())
        .and_then(|inputs| inputs.fingerprint().map(|base| (inputs, base)))
    } else {
        Err(prepared
            .first()
            .and_then(|task| task.plan.cache_eligibility().err())
            .unwrap_or_else(|| "no reusable outputs".into()))
    };
    match &freshness {Ok(_)=>ui::verbose("freshness: source, dependency, compiler runtime and standard library contents fingerprinted"), Err(reason)=>ui::verbose(format!("freshness: compiling; {reason}"))};
    let mut executions = Vec::new();
    for mut task in prepared {
        if let Ok((inputs, base)) = &freshness {
            let (plan, status) = task.plan.with_fingerprint(base, inputs);
            task.plan = plan;
            match status {
                Ok(()) => ui::verbose("freshness: verify cached artifacts before reuse"),
                Err(reason) => ui::verbose(format!("freshness: compiling; {reason}")),
            };
        }
        ui::status(
            if mode == BuildMode::Check {
                "Checking"
            } else {
                "Compiling"
            },
            format!(
                "{} ({} {})",
                task.package,
                task.target.kind.as_str(),
                task.target.name
            ),
        );
        let final_paths = if let Some(path) = task.plan.plan()["link"]["output"].as_str() {
            vec![path.to_owned()]
        } else {
            task.plan.plan()["compile"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|job| job["output"].as_str().map(str::to_owned))
                .collect()
        };
        let identity = json!({"name":task.target.name,"kind":task.target.kind.as_str()});
        let artifact_kind = task.target.output.map(|v| v.as_str()).unwrap_or("binary");
        let mut report = |mut event: serde_json::Value| {
            event["package"] = json!(task.package);
            event["package_target"] = identity.clone();
            event["profile"] = json!(profile_name);
            if event["event"] == "artifact" {
                event["artifact_kind"] = json!(artifact_kind);
            }
            messages.emit(event)
        };
        let execution = task
            .plan
            .execute_reported(
                compiler,
                if reporting { Some(&mut report) } else { None },
                &mut |message| ui::verbose(message),
            )
            .map_err(|e| target_error(e, &task.target, profile_name))?
            .map(|execution| execution.in_directory(&task.directory));
        if let Some(execution) = execution {
            executions.push((task.package.clone(), task.target.name.clone(), execution));
        }
        for path in final_paths {
            ui::status("Artifact", path);
        }
    }
    drop(resolution);
    let mut outcome = Outcome::success();
    for (package, name, execution) in executions {
        ui::status(
            if mode == BuildMode::Test {
                "Testing"
            } else {
                "Running"
            },
            &name,
        );
        if mode == BuildMode::Test && outcome.origin == "program" {
            messages.program_event(json!({"event":"status","phase":"running"}));
        } else {
            messages.status("running")?;
        }
        let result = Outcome::program(execution.execute()?);
        if mode == BuildMode::Test {
            ui::result(format!(
                "test {package}::{name} ... {} (exit {})",
                if result.code == 0 { "ok" } else { "FAILED" },
                result.code
            ));
            messages.program_event(json!({"event":"test_result","package":package,"test":name,"origin":"program","exit_code":result.code,"signal":result.signal,"success":result.code==0}));
        }
        if outcome.code == 0 {
            outcome = result;
        }
    }
    drop(run_lease);
    if outcome.code == 0 {
        ui::status(
            "Finished",
            format!(
                "{profile_name} profile in {:.2}s",
                started.elapsed().as_secs_f64()
            ),
        );
    }
    Ok(outcome)
}

fn target_error(error: Error, target: &PackageTarget, profile: &str) -> Error {
    error
        .with_field(
            "package_target",
            format!("{}:{}", target.kind.as_str(), target.name),
        )
        .with_field("profile", profile)
}

fn select_targets<'a>(
    manifest: &'a Manifest,
    mode: BuildMode,
    options: &VexBuildOptions,
) -> Result<Vec<&'a PackageTarget>, Error> {
    if mode == BuildMode::Test {
        let mut tests = manifest
            .tests
            .iter()
            .filter(|target| {
                options
                    .filter
                    .as_ref()
                    .is_none_or(|filter| target.name.contains(filter))
            })
            .collect::<Vec<_>>();
        tests.sort_by(|a, b| a.name.cmp(&b.name));
        if tests.is_empty() {
            return Err(Error::resolution(
                "no executable tests match the name filter",
            ));
        }
        return Ok(tests);
    }
    let binaries = manifest
        .targets
        .iter()
        .filter(|t| t.kind == TargetKind::Binary)
        .collect::<Vec<_>>();
    if mode == BuildMode::Run {
        if options.library {
            return Err(Error::usage("vex run requires a binary; --lib is invalid"));
        }
        if options.binaries.len() > 1 {
            return Err(Error::usage("vex run selects exactly one binary"));
        }
        let name = options
            .binaries
            .first()
            .map(String::as_str)
            .or(manifest.default_run.as_deref());
        return if let Some(name) = name {
            binaries
                .into_iter()
                .find(|t| t.name == name)
                .map(|t| vec![t])
                .ok_or_else(|| Error::resolution(format!("unknown binary target `{name}`")))
        } else if binaries.len() == 1 {
            Ok(binaries)
        } else if binaries.is_empty() {
            Err(Error::resolution(
                "library packages cannot be run; select a package with a binary target",
            ))
        } else {
            Err(Error::resolution(
                "multiple binary targets; select one with --bin <name> or set default_run",
            ))
        };
    }
    for name in &options.binaries {
        if !binaries.iter().any(|t| t.name == *name) {
            return Err(Error::resolution(format!("unknown binary target `{name}`")));
        }
    }
    if options.library && !manifest.lib {
        return Err(Error::resolution("package has no library target"));
    }
    let all = !options.library && options.binaries.is_empty();
    let mut targets = manifest
        .targets
        .iter()
        .filter(|t| {
            all || match t.kind {
                TargetKind::Library => options.library,
                TargetKind::Binary => options.binaries.contains(&t.name),
            }
        })
        .collect::<Vec<_>>();
    targets.sort_by_key(|t| (t.kind != TargetKind::Library, &t.name));
    Ok(targets)
}

pub(crate) fn validate_entry(manifest: &Manifest, target: &PackageTarget) -> Result<(), Error> {
    let path = &target.path;
    if !path.is_file() {
        return Err(Error::resolution(format!(
            "missing canonical package entry `{}` for {} `{}`",
            path.display(),
            target.kind.as_str(),
            target.name
        )));
    }
    if manifest.format >= 3 {
        let root = manifest
            .source_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .map_err(Error::environment)?;
        let actual = path.canonicalize().map_err(Error::environment)?;
        if !actual.starts_with(root) {
            return Err(Error::resolution(
                "package target entry escapes its package directory",
            ));
        }
    }
    input_string(path).map(|_| ())
}

fn parse_vex_build_options(mode: BuildMode, args: &[String]) -> Result<VexBuildOptions, String> {
    let mut options = VexBuildOptions::default();
    let mut i = 0;

    while i < args.len() {
        let token = args[i].as_str();

        if token == "--" {
            if !matches!(mode, BuildMode::Run | BuildMode::Test) {
                return Err(
                    "runtime arguments after `--` are only valid with `vex run`".to_string()
                );
            }
            break;
        }

        match token {
            "--target" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for `--target`".to_string());
                }
                set_target(&mut options, &args[i])?;
            }
            "--release" => {
                if options.release {
                    return Err("--release may only be specified once".into());
                }
                options.release = true;
            }
            "--profile" | "--bin" => {
                i += 1;
                let value = args
                    .get(i)
                    .filter(|s| !s.is_empty() && !s.starts_with('-'))
                    .ok_or_else(|| format!("missing value for `{token}`"))?;
                set_selection(&mut options, token, value)?;
            }
            "--lib" => {
                if options.library {
                    return Err("--lib may only be specified once".into());
                }
                options.library = true;
            }
            _ if token.starts_with("--profile=") => {
                set_selection(&mut options, "--profile", &token[10..])?
            }
            _ if token.starts_with("--bin=") => set_selection(&mut options, "--bin", &token[6..])?,
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
            _ if mode == BuildMode::Test && options.filter.is_none() => {
                options.filter = Some(token.into())
            }
            _ => {
                return Err(format!(
                    "unexpected argument `{token}`. Vex builds the package described by vex.ws"
                ));
            }
        }

        i += 1;
    }

    if options.release && options.profile.is_some() {
        return Err("--release and --profile select the same setting; use only one".into());
    }
    if mode == BuildMode::Test && (options.library || !options.binaries.is_empty()) {
        return Err(
            "vex test selects explicit test targets with a name filter, not --lib/--bin".into(),
        );
    }
    if mode == BuildMode::Run && (options.library || options.binaries.len() > 1) {
        return Err(
            "vex run selects one binary with --bin; --lib and multiple binaries are invalid".into(),
        );
    }
    Ok(options)
}

fn set_selection(options: &mut VexBuildOptions, option: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.starts_with('-') {
        return Err(format!("missing value for `{option}`"));
    }
    if option == "--profile" {
        if options.profile.is_some() {
            return Err("--profile may only be specified once".into());
        }
        options.profile = Some(value.into());
    } else {
        if options.binaries.iter().any(|name| name == value) {
            return Err(format!("duplicate binary selection `{value}`"));
        }
        options.binaries.push(value.into());
    }
    Ok(())
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
        BuildMode::Test => "usage: vex test [filter] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline] [-- <args...>]",
        BuildMode::Build => {
            "usage: vex build [--lib] [--bin <name>] [--target <triple>] [--profile <name> | --release] [--dry-run] [--locked] [--offline]"
        }
        BuildMode::Run => {
            "usage: vex run [--bin <name>] [--target <triple>] [--profile <name> | --release] [--dry-run] [--locked] [--offline] [-- <args...>]"
        }
        BuildMode::Check => {
            "usage: vex check [--lib] [--bin <name>] [--target <triple>] [--profile <name> | --release] [--dry-run] [--locked] [--offline]"
        }
    }
}

fn input_string(path: &Path) -> Result<String, Error> {
    let value = path.to_str().ok_or_else(|| {
        Error::resolution("wavec JSON protocol cannot represent a non-UTF-8 input path")
    })?;
    #[cfg(windows)]
    {
        if let Some(value) = value.strip_prefix(r"\\?\UNC\") {
            return Ok(format!("//{}", value.replace('\\', "/")));
        }
        Ok(value
            .strip_prefix(r"\\?\")
            .unwrap_or(value)
            .replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        Ok(value.to_owned())
    }
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
