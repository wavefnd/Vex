// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use compiler::Compiler;
use diagnostic::Error;
use manifest::{Manifest, TargetKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const USAGE: &str = "usage: vex doctor [--format=json] [--target <triple>]";

#[derive(Default)]
struct Report {
    checks: Vec<Value>,
    first_error: Option<Error>,
}
impl Report {
    fn check(&mut self, id: &str, status: &str, required: bool, message: &str, details: Value) {
        self.checks
            .push(json!({"id":id, "status":status, "required":required,
            "message":message, "details":details}));
    }
    fn failure(&mut self, id: &str, error: Error) {
        self.check(
            id,
            "error",
            true,
            &error,
            json!({"category":error.category.name()}),
        );
        if self.first_error.is_none() {
            self.first_error = Some(error);
        }
    }
    fn finish(self, format_json: bool) -> Result<(), Error> {
        let mut value = json!({"schema_version":1, "success":self.first_error.is_none(),
            "checks":self.checks});
        // First register every credential-bearing URL, then redact standalone
        // repetitions too. Never serialize a raw tool response or environment.
        for check in value["checks"].as_array_mut().unwrap() {
            for field in ["message", "details"] {
                visit_strings(&mut check[field], &mut |s| {
                    let _ = source::redact(s);
                });
            }
        }
        for check in value["checks"].as_array_mut().unwrap() {
            for field in ["message", "details"] {
                visit_strings(&mut check[field], &mut |s| *s = source::redact(s));
            }
        }
        if format_json {
            diagnostic::outln!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(Error::internal)?
            );
        } else {
            diagnostic::outln!("Vex doctor");
            for check in value["checks"].as_array().unwrap() {
                diagnostic::outln!(
                    "[{}] {}: {}",
                    check["status"].as_str().unwrap(),
                    check["id"].as_str().unwrap(),
                    check["message"].as_str().unwrap()
                );
            }
        }
        match self.first_error {
            Some(error) => Err(Error::new(
                error.category,
                "doctor found required checks that failed; see the diagnostic report",
            )),
            None => Ok(()),
        }
    }
}

fn visit_strings(value: &mut Value, visit: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(text) => visit(text),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|value| visit_strings(value, visit)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|value| visit_strings(value, visit)),
        _ => {}
    }
}

pub fn doctor(args: &[String], selection: &crate::project::Selection) -> Result<(), Error> {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        diagnostic::outln!("{USAGE}");
        return Ok(());
    }
    let (format_json, target) = options(args)?;
    let mut report = Report::default();
    let host = host_targets();
    report.check(
        "vex",
        "ok",
        true,
        &format!(
            "Vex {} on {}/{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        json!({"version":env!("CARGO_PKG_VERSION"),
        "os":std::env::consts::OS, "arch":std::env::consts::ARCH, "host_targets":host}),
    );
    match std::env::current_exe().and_then(|exe| std::env::current_dir().map(|cwd| (exe, cwd))) {
        Ok((exe, cwd)) => match (path_text(&exe), path_text(&cwd)) {
            (Ok(exe), Ok(cwd)) => report.check(
                "paths",
                "ok",
                true,
                &format!("Vex {exe}; invocation directory {cwd}"),
                json!({"vex":exe, "cwd":cwd}),
            ),
            (Err(e), _) | (_, Err(e)) => report.failure("paths", e),
        },
        Err(error) => report.failure("paths", Error::environment(error)),
    }

    // Resolve both tools in the invocation directory. A relative override/PATH
    // must retain its normal meaning when --manifest-path selects elsewhere.
    let selected = Compiler::select();
    check_git(&mut report);
    let mut compiler = match selected {
        Ok(mut compiler) => {
            match executable(compiler.path()).and_then(|()| path_text(compiler.path())) {
                Ok(path) => match compiler.version() {
                    Ok(version) => report.check("wavec", "ok", true,
                        &format!("wavec {version} at {path}"), json!({"version":version, "path":path,
                            "selection":if std::env::var_os("VEX_WAVEC").is_some() {"VEX_WAVEC"} else {"PATH_or_managed"}})),
                    Err(error) => report.failure("wavec", error),
                },
                Err(error) => report.failure("wavec", error),
            }
            Some(compiler)
        }
        Err(error) => {
            report.failure("wavec", error);
            None
        }
    };
    let project = select_project(selection).and_then(|project| {
        project
            .map(|project| {
                let path = path_text(&project.manifest_path)?;
                let manifest = project.primary();
                Ok((project, path, manifest))
            })
            .transpose()
    });
    let target = target.or_else(|| {
        project
            .as_ref()
            .ok()
            .and_then(|project| project.as_ref())
            .and_then(|(_, _, manifest)| manifest.build_target.clone())
    });
    let mut compatible_target = false;
    if let Some(compiler) = compiler.as_mut() {
        match compiler.supported_targets() {
            Ok(targets) => {
                report.check(
                    "capabilities",
                    "ok",
                    true,
                    &format!(
                        "{} supported target(s): {}",
                        targets.len(),
                        targets.join(", ")
                    ),
                    json!({"targets":targets}),
                );
                let accepted = target.as_ref().map_or_else(
                    || host.iter().any(|h| targets.contains(h)),
                    |t| targets.contains(t),
                );
                if accepted {
                    compatible_target = true;
                    report.check(
                        "target",
                        "ok",
                        true,
                        &format!(
                            "compiler supports {}",
                            target.as_deref().unwrap_or("the host target")
                        ),
                        json!({"requested":target}),
                    );
                } else if target.is_none() && host.is_empty() {
                    compatible_target = true;
                    report.check("target", "warning", false, "this host has no Vex platform mapping; compiler default target will be used", Value::Null);
                } else {
                    report.failure(
                        "target",
                        Error::resolution(format!(
                            "unsupported {} target; available targets: {}",
                            target.as_deref().unwrap_or("host"),
                            targets.join(", ")
                        )),
                    );
                }
            }
            Err(error) => report.failure("capabilities", error),
        }
    } else {
        report.check(
            "capabilities",
            "not_checked",
            true,
            "compiler selection failed",
            Value::Null,
        );
    }
    match project {
        Ok(Some((project, path, manifest))) => {
            report.check(
                "project", "ok", false, &format!("{} at {path}", manifest.name),
                json!({"manifest_path":path, "name":manifest.name, "manifest_format":manifest.format}),
            );
            for member in &project.selected {
                inspect_project(
                    &mut report,
                    member,
                    compiler.as_mut(),
                    compatible_target,
                    target.as_deref(),
                    &host,
                );
            }
        }
        Ok(None) => {
            report.check(
                "project",
                "not_checked",
                false,
                "no project manifest found; tool diagnostics remain available outside a project",
                Value::Null,
            );
            report.check(
                "dry_run",
                "not_checked",
                false,
                "select an existing project to check its compiler dry-run contract",
                Value::Null,
            );
        }
        Err(error) => {
            report.failure("project", error);
            report.check(
                "dry_run",
                "not_checked",
                false,
                "project selection or manifest loading failed",
                Value::Null,
            );
        }
    }
    report.finish(format_json)
}

fn options(args: &[String]) -> Result<(bool, Option<String>), Error> {
    let mut format_json = false;
    let mut target = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--format=json" => format_json = true,
            "--format" if args.next().is_some_and(|s| s == "json") => format_json = true,
            "--target" => set_target(&mut target, args.next().map(String::as_str).unwrap_or(""))?,
            _ if arg.starts_with("--target=") => set_target(&mut target, &arg[9..])?,
            _ => {
                return Err(Error::usage(format!(
                    "unsupported doctor option `{arg}`\n{USAGE}"
                )))
            }
        }
    }
    Ok((format_json, target))
}
fn set_target(slot: &mut Option<String>, value: &str) -> Result<(), Error> {
    if slot.is_some()
        || value.is_empty()
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(Error::usage(
            "doctor --target requires one nonempty target triple",
        ));
    }
    *slot = Some(value.to_owned());
    Ok(())
}
fn host_targets() -> Vec<String> {
    host_targets_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn host_targets_for(os: &str, arch: &str) -> Vec<String> {
    let platforms: Value = serde_json::from_str(include_str!("../../platforms.json"))
        .expect("validated platform table");
    platforms["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["os"] == os && p["arch"] == arch)
        .map(|p| {
            // wave_target names the release archive, not necessarily the
            // compiler capability. In particular Rust uses riscv64gc while
            // Wave advertises riscv64-unknown-linux-gnu.
            let mut targets = vec![
                p["wave_target"].as_str().unwrap().to_owned(),
                p["rust_target"].as_str().unwrap().to_owned(),
            ];
            if os == "linux" {
                targets.push(format!("{arch}-unknown-linux-gnu"));
            }
            targets.sort();
            targets.dedup();
            targets
        })
        .unwrap_or_default()
}
fn path_text(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::environment("doctor cannot represent a non-UTF-8 path"))
}
fn executable(path: &Path) -> Result<(), Error> {
    let metadata = path.metadata().map_err(|error| {
        Error::environment(format!(
            "cannot inspect executable `{}`: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(Error::environment(format!(
            "executable `{}` is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::environment(format!(
                "executable `{}` has no execute permission",
                path.display()
            )));
        }
    }
    Ok(())
}
fn find_tool(name: &Path) -> Result<PathBuf, Error> {
    let cwd = std::env::current_dir().map_err(Error::environment)?;
    let candidates = if name.is_absolute() || name.components().count() > 1 {
        vec![cwd.join(name)]
    } else {
        std::env::var_os("PATH")
            .map(|p| {
                std::env::split_paths(&p)
                    .map(|p| cwd.join(p).join(name))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut failure = None;
    for path in candidates {
        if path.is_file() {
            match executable(&path) {
                Ok(()) => return Ok(path),
                Err(error) => failure = Some(error),
            }
        }
        #[cfg(windows)]
        if path.extension().is_none() {
            let path = path.with_extension("exe");
            if path.is_file() {
                executable(&path)?;
                return Ok(path);
            }
        }
    }
    Err(failure.unwrap_or_else(|| {
        Error::environment(format!(
            "cannot find executable `{}`; install it or correct PATH",
            name.display()
        ))
    }))
}
fn check_git(report: &mut Report) {
    let result = (|| {
        let path = find_tool(Path::new("git"))?;
        let output = process::output(
            Command::new(&path)
                .arg("--version")
                .env("GIT_TERMINAL_PROMPT", "0"),
            Duration::from_secs(60),
        )
        .map_err(Error::environment)?;
        if !output.status.success() {
            return Err(Error::environment("git --version failed"));
        }
        let version = String::from_utf8(output.stdout).map_err(Error::environment)?;
        if !version.trim().starts_with("git version ") {
            return Err(Error::environment("malformed Git version response"));
        }
        Ok((path_text(&path)?, version.trim().to_owned()))
    })();
    match result {
        Ok((path, version)) => report.check(
            "git",
            "ok",
            true,
            &format!("{version} at {path}"),
            json!({"path":path, "version":version}),
        ),
        Err(error) => report.failure("git", error),
    }
}
fn select_project(
    selection: &crate::project::Selection,
) -> Result<Option<crate::project::Project>, Error> {
    if selection.manifest_path.is_some() {
        return selection.enter().map(Some);
    }
    let cwd = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map_err(Error::environment)?;
    for directory in cwd.ancestors() {
        match std::fs::symlink_metadata(directory.join(manifest::MANIFEST_FILE)) {
            Ok(_) => return selection.enter().map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::environment(error)),
        }
    }
    Ok(None)
}
fn inspect_project(
    report: &mut Report,
    manifest: &Manifest,
    mut compiler: Option<&mut Compiler>,
    compatible_target: bool,
    target: Option<&str>,
    host: &[String],
) {
    if let Some(compiler) = compiler.as_mut() {
        match compiler.validate_version(manifest.compiler.as_deref()) {
            Ok(()) => report.check(
                "project_compiler",
                "ok",
                true,
                "project compiler version requirement is satisfied",
                json!({"required_version":manifest.compiler}),
            ),
            Err(error) => report.failure("project_compiler", error),
        }
    }
    let mut targets = manifest.targets.iter().collect::<Vec<_>>();
    targets.sort_by_key(|target| (target.kind != TargetKind::Library, &target.name));
    let mut missing_entry = false;
    for package_target in &targets {
        if let Err(error) = super::build::validate_entry(manifest, package_target) {
            report.failure("entry", error);
            missing_entry = true;
        }
    }
    if missing_entry {
        report.check(
            "dry_run",
            "not_checked",
            false,
            "a declared package input is unavailable",
            Value::Null,
        );
        return;
    }
    let mut graph_selection = source::graph::Selection::default();
    if let Some(target) = target {
        match source::graph::Context::new(target) {
            Ok(context) => graph_selection.target = Some(context),
            Err(error) => {
                report.failure("local_graph", Error::resolution(error));
                return;
            }
        }
    }
    let mut resolution = match resolver::inspect_selected(
        manifest,
        false,
        graph_selection.clone(),
        |_, _| {},
    ) {
        Ok(resolution) => resolution,
        Err(error) => {
            report.check(
                "local_graph",
                "warning",
                false,
                &error,
                json!({"category":error.category.name()}),
            );
            report.check("dry_run", "not_checked", false, "local graph is unavailable; run vex fetch before checking the project dry-run contract", Value::Null);
            return;
        }
    };
    report.check(
        "local_graph",
        "ok",
        false,
        &format!(
            "{} locally available dependencies",
            resolution.package_count()
        ),
        json!({"packages":resolution.package_count()}),
    );
    let Some(compiler) = compiler.filter(|_| compatible_target) else {
        report.check(
            "dry_run",
            "not_checked",
            false,
            "compiler or target capability is unavailable",
            Value::Null,
        );
        return;
    };
    let profile = &manifest.profiles["dev"];
    let prerequisite = (|| {
        let spec = compiler.target_spec(target)?;
        Ok(spec["triple"].as_str().unwrap().to_owned())
    })();
    let triple = match prerequisite {
        Ok(triple) => triple,
        Err(error) => {
            planning_failure(report, error, target, host);
            return;
        }
    };
    let mut planned_paths = Vec::new();
    for package_target in targets {
        let result = (|| {
            let mut context = source::graph::Context::new(&triple).map_err(Error::resolution)?;
            if let Some(mode) = manifest.platform_for(package_target, &triple).mode {
                context.mode = mode.as_str().into();
            }
            graph_selection.target = Some(context);
            resolution.activate_root(&manifest.source_path, &graph_selection)?;
            for package in resolution.packages() {
                let directory = package.directory();
                let manifest = Manifest::load_from(directory.join(manifest::MANIFEST_FILE))?;
                compiler.validate_version(manifest.compiler.as_deref())?;
            }

            let native = crate::native::prepare_target(
                manifest,
                package_target,
                &resolution,
                compiler,
                &triple,
                "dev",
                false,
                None,
            )?;
            let mut args = resolution.dependency_args()?;
            if package_target.kind == TargetKind::Binary && manifest.lib {
                let root = std::env::current_dir().map_err(Error::environment)?;
                args.push(format!("--dep={}={}", manifest.name, path_text(&root)?));
            }
            args.push(format!("-O{}", profile.opt_level));
            args.extend(native.global);
            args.extend(["build".into(), path_text(&package_target.path)?]);
            args.extend(native.inputs);
            args.extend(native.output);
            compiler::prepare_build(compiler, &args, &[], false, None, None)
        })();
        match result {
            Ok(prepared) => {
                let plan = prepared.plan();
                planned_paths.extend(prepared.output_paths().cloned());
                let identity =
                    json!({"name":package_target.name, "kind":package_target.kind.as_str()});
                report.check("dry_run", "ok", true,
                    "compiler dry-run schema v1 and output paths are valid; compilation and linking were not executed",
                    json!({"schema_version":plan["schema_version"], "target":plan["target"],
                        "package_target":identity,"profile":"dev","opt_level":profile.opt_level}));
                if let Some(program) = plan["link"]["program"].as_str() {
                    match find_tool(Path::new(program)).and_then(|path| path_text(&path)) {
                        Ok(path) => report.check("linker", "ok", false,
                            &format!("planned linker is available at {path}; actual link support was not tested"),
                            json!({"path":path,"package_target":identity})),
                        Err(error) if target.is_some_and(|t| !host.iter().any(|h| h == t)) =>
                            report.check("linker", "warning", false, &format!("optional cross-target tool unavailable: {error}"),
                                json!({"program":program,"package_target":identity})),
                        Err(error) => report.failure("linker", error),
                    }
                } else {
                    report.check(
                        "linker",
                        "not_checked",
                        false,
                        "the dry-run plan does not require a linker",
                        json!({"package_target":identity}),
                    );
                }
            }
            Err(error) => planning_failure(
                report,
                error.context(format!(
                    "{} `{}`",
                    package_target.kind.as_str(),
                    package_target.name
                )),
                target,
                host,
            ),
        }
    }
    if let Err(error) = std::env::current_dir()
        .map_err(Error::environment)
        .and_then(|root| {
            state::artifacts::validate_output_paths(&root, &planned_paths).map_err(Error::compiler)
        })
    {
        report.failure("output_paths", error);
    }
    // Keep resolution's shared lease alive through compiler planning so updates
    // cannot replace dependency source while the compiler reads it.
    drop(resolution);
}

fn planning_failure(report: &mut Report, error: Error, target: Option<&str>, host: &[String]) {
    if error.category == diagnostic::Category::Environment
        && target.is_some_and(|t| !host.iter().any(|h| h == t))
    {
        report.check(
            "dry_run",
            "warning",
            false,
            &format!("optional cross-target environment unavailable: {error}"),
            json!({"category":error.category.name()}),
        );
    } else {
        report.failure("dry_run", error);
    }
}

#[cfg(test)]
mod host_target_tests {
    use super::host_targets_for;

    #[test]
    fn every_supported_host_matches_its_compiler_capability() {
        // Capability names from the public Wave 0.2.1 release, deliberately
        // independent from the archive-name mapping in platforms.json.
        for (os, arch, capability) in [
            ("linux", "x86_64", "x86_64-unknown-linux-gnu"),
            ("linux", "aarch64", "aarch64-unknown-linux-gnu"),
            ("linux", "riscv64", "riscv64-unknown-linux-gnu"),
            ("linux", "loongarch64", "loongarch64-unknown-linux-gnu"),
            ("macos", "x86_64", "x86_64-apple-darwin"),
            ("macos", "aarch64", "aarch64-apple-darwin"),
            ("windows", "x86_64", "x86_64-pc-windows-msvc"),
            ("windows", "aarch64", "aarch64-pc-windows-msvc"),
            ("freebsd", "x86_64", "x86_64-unknown-freebsd"),
        ] {
            let targets = host_targets_for(os, arch);
            assert!(
                targets.iter().any(|target| target == capability),
                "{os}/{arch}: {targets:?}"
            );
            assert!(
                targets.iter().all(|target| target.starts_with(arch)),
                "{targets:?}"
            );
        }
        assert!(host_targets_for("linux", "unsupported").is_empty());
    }
}
