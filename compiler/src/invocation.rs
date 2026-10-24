// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::plan;

/// Compilation ends before this value is returned. No project guard is owned
/// here: the caller must drop its resolution/guard before calling execute().
pub struct Execution {
    directory: PathBuf,
    program: String,
    args: Vec<OsString>,
}
impl Execution {
    pub fn in_directory(mut self, directory: &Path) -> Self {
        self.directory = directory.to_owned();
        self
    }
    pub fn execute(self) -> Result<ExitStatus, Error> {
        let program = runner_program(&self.program)?;
        process::status(
            Command::new(&program)
                .args(&self.args)
                .current_dir(&self.directory),
            None,
            true,
        )
        .map_err(|e| Error::environment(format!("failed to run `{}`: {e}", self.program)))
    }
}

fn runner_program(program: &str) -> Result<PathBuf, Error> {
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() > 1 {
        return Ok(path.to_path_buf());
    }
    let root = std::env::current_dir().map_err(Error::environment)?;
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = root.join(directory).join(path);
        #[cfg(windows)]
        let candidate = if candidate.extension().is_none() {
            candidate.with_extension("exe")
        } else {
            candidate
        };
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&candidate)
                .map_err(Error::environment)?
                .permissions()
                .mode()
                & 0o111
                == 0
            {
                continue;
            }
        }
        return Ok(candidate);
    }
    Err(Error::environment(format!(
        "program runner `{program}` is unavailable on PATH"
    )))
}

/// A validated compiler plan. Constructing it does not create build outputs.
pub struct PreparedBuild {
    freshness: Option<(String, String, crate::freshness::Inputs)>,
    plan: serde_json::Value,
    args: Vec<String>,
    root: PathBuf,
    outputs: Vec<PathBuf>,
    optional_outputs: Vec<PathBuf>,
    execution: Option<(Execution, PathBuf)>,
    generation: Option<PathBuf>,
}
impl PreparedBuild {
    pub fn with_fingerprint(
        mut self,
        base: &str,
        inputs: &crate::freshness::Inputs,
    ) -> (Self, Result<(), String>) {
        let key = crate::freshness::key(base, &self.plan, &self.args, self.generation.as_deref());
        let status = key.as_ref().map(|_| ()).map_err(Clone::clone);
        self.freshness = key.ok().map(|key| (key, base.to_owned(), inputs.clone()));
        (self, status)
    }

    pub fn cache_eligibility(&self) -> Result<(), String> {
        crate::freshness::key("", &self.plan, &self.args, self.generation.as_deref()).map(|_| ())
    }
    pub fn plan(&self) -> &serde_json::Value {
        &self.plan
    }
    pub fn output_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.outputs.iter().chain(&self.optional_outputs)
    }
    /// Validate ownership for the full batch before any target is compiled.
    pub fn preflight_ownership(&self) -> Result<(), Error> {
        state::artifacts::Batch::prepare_with_optional(
            &self.root,
            self.outputs.clone(),
            self.optional_outputs.iter().cloned(),
        )
        .map(|_| ())
        .map_err(Error::environment)
    }
    pub fn execute(
        self,
        compiler: &crate::Compiler,
        report: Option<&mut dyn FnMut(serde_json::Value) -> Result<(), Error>>,
    ) -> Result<Option<Execution>, Error> {
        self.execute_reported(compiler, report, &mut |_| {})
    }
    pub fn execute_reported(
        self,
        compiler: &crate::Compiler,
        mut report: Option<&mut dyn FnMut(serde_json::Value) -> Result<(), Error>>,
        decision: &mut dyn FnMut(String),
    ) -> Result<Option<Execution>, Error> {
        let Self {
            freshness,
            plan,
            args,
            root,
            outputs,
            optional_outputs,
            execution,
            generation,
        } = self;
        let wavec = &compiler.path;
        state::ensure_dir(Path::new("target")).map_err(Error::environment)?;
        let mut compile_args: Vec<_> = args[..]
            .iter()
            .filter(|a| a.as_str() != "--run")
            .cloned()
            .collect();
        let ownership = state::artifacts::Batch::prepare_with_optional(
            &root,
            outputs.clone(),
            optional_outputs.iter().cloned(),
        )
        .map_err(Error::environment)?;
        let reused = match freshness.as_ref() {
            Some((key, base, inputs))
                if inputs.fingerprint().as_deref() == Ok(base.as_str())
                    && crate::freshness::key(base, &plan, &args, generation.as_deref())
                        .as_deref()
                        == Ok(key.as_str()) =>
            {
                match crate::freshness::restore(&root, key, &outputs, &optional_outputs) {
                    Ok(true) => {
                        decision(
                            "freshness: fresh; verified cached artifacts copied into owned outputs"
                                .into(),
                        );
                        true
                    }
                    Ok(false) => {
                        decision("freshness: dirty; cache entry is absent".into());
                        false
                    }
                    Err(reason) => {
                        decision(format!("freshness: dirty; {reason}"));
                        false
                    }
                }
            }
            _ => false,
        };
        if let Some(sink) = report.as_mut() {
            sink(serde_json::json!({"event":"freshness","reused":reused,"origin":"vex"}))?;
        }
        let compile_result = if reused {
            ownership.verify_outputs().map_err(Error::compiler)
        } else {
            let result = (|| -> Result<ExitStatus, Error> {
                let status = if let Some(sink) = report.as_mut() {
                    insert_build_flag(&mut compile_args, "--error-format=json");
                    let output =
                        process::output_without_deadline(Command::new(wavec).args(&compile_args))
                            .map_err(Error::environment)?;
                    diagnostic::output::stdout(format_args!(
                        "{}",
                        String::from_utf8_lossy(&output.stdout)
                    ))?;
                    // Compiler diagnostics retain their original structured payload and spans.
                    // User-program streams never pass through this compiler-only capture.
                    diagnostic::output::stderr(format_args!(
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    ))?;
                    emit_diagnostics(&output.stderr, &mut **sink)?;
                    output.status
                } else {
                    process::status(Command::new(wavec).args(&compile_args), None, false).map_err(
                        |e| {
                            Error::environment(format!(
                                "failed to execute `{}` build: {e}",
                                wavec.display()
                            ))
                        },
                    )?
                };
                Ok(status)
            })();
            result.and_then(|status| {
                if status.success() {
                    ownership.verify_outputs().map_err(Error::compiler)
                } else {
                    Err(compiler_error(
                        status,
                        format!("wavec build failed [{}]", classify_exit(status)),
                    ))
                }
            })
        };
        finish_compilation(compile_result, ownership.finish())?;
        if !reused {
            if let Some((key, base, inputs)) = freshness.as_ref() {
                if inputs.fingerprint().as_deref() == Ok(base.as_str())
                    && crate::freshness::key(base, &plan, &args, generation.as_deref()).as_deref()
                        == Ok(key.as_str())
                {
                    match crate::freshness::store(&root, key, &outputs, &optional_outputs) {
                        Ok(()) => decision(
                            "freshness: rebuilt; successful output snapshot recorded".into(),
                        ),
                        Err(reason) => decision(format!(
                            "freshness: rebuilt; cache was not recorded: {reason}"
                        )),
                    }
                }
            }
        }
        if let Some(sink) = report.as_mut() {
            let mut outputs: Vec<_> = if let Some(output) = plan["link"]["output"].as_str() {
                vec![output.to_owned()]
            } else {
                plan["compile"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|job| job["output"].as_str().map(str::to_owned))
                    .collect()
            };
            outputs.extend(
                optional_outputs
                    .iter()
                    .filter(|path| path.is_file())
                    .map(|path| path.to_string_lossy().into_owned()),
            );
            sink(
                serde_json::json!({"event":"artifact", "origin":"vex", "target":plan["target"],
            "emit":plan["emit"], "paths":outputs, "executable":execution.as_ref().map(|(_,path)| path)}),
            )?;
        }
        if let Some((execution, output)) = execution {
            state::reject_link(&output).map_err(Error::environment)?;
            let actual = output.canonicalize().map_err(|e| {
                Error::new(
                    if e.kind() == std::io::ErrorKind::NotFound {
                        diagnostic::Category::Compiler
                    } else {
                        diagnostic::Category::Environment
                    },
                    format!("compiler did not produce {}: {e}", output.display()),
                )
            })?;
            let generation = generation
                .as_deref()
                .unwrap()
                .canonicalize()
                .map_err(|e| Error::environment(e.to_string()))?;
            if !actual.starts_with(&generation) || !actual.is_file() {
                return Err(Error::compiler(
                    "compiler output escaped the run generation",
                ));
            }
            return Ok(Some(execution));
        }
        Ok(None)
    }
}

pub fn prepare_build(
    compiler: &crate::Compiler,
    args: &[String],
    runtime: &[OsString],
    user_requested_dry_run: bool,
    generation: Option<&Path>,
    mut report: Option<&mut dyn FnMut(serde_json::Value) -> Result<(), Error>>,
) -> Result<PreparedBuild, Error> {
    let mut dry_run_args = args.to_vec();
    if !contains_dry_run_flag(&dry_run_args) {
        insert_build_flag(&mut dry_run_args, "--dry-run");
    }
    insert_build_flag(&mut dry_run_args, "--error-format=json");
    let wavec = &compiler.path;
    let validation_output = run_wavec_dry_run(wavec, &dry_run_args, &mut report)?;
    let mut plan = plan::validate_dry_run_json_output(&validation_output.stdout, &validation_output.stderr)
        .map_err(|error| Error::compiler(format!("installed wavec is incompatible with Vex: {error}\nhelp: update wavec or set VEX_WAVEC=/path/to/wavec")))?;
    if !validation_output.stderr.is_empty() {
        diagnostic::output::stderr(format_args!(
            "{}",
            String::from_utf8_lossy(&validation_output.stderr)
        ))?;
    }
    let root = std::env::current_dir().map_err(Error::environment)?;
    let separator = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let is_run = args[..separator].iter().any(|a| a == "--run");
    let execution = if is_run {
        let generation =
            generation.ok_or_else(|| Error::internal("missing per-run output generation"))?;
        if plan["mode"] != "build+run" || plan["emit"] != "bin" {
            return Err(Error::compiler(
                "run requires a build plan that emits a binary",
            ));
        }
        for job in plan["compile"].as_array().unwrap() {
            validate_output_path(Path::new(job["output"].as_str().unwrap()), generation)?;
        }
        let output = plan["link"]["output"]
            .as_str()
            .ok_or_else(|| Error::compiler("run plan is missing link.output"))?;
        validate_output_path(Path::new(output), generation)?;
        let program = plan["execute"]["program"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::compiler("run plan is missing execute.program"))?
            .to_owned();
        let run_args = plan::string_array(&plan["execute"]["args"], "execute.args")
            .map_err(Error::compiler)?;
        if program == output {
            if !run_args.is_empty() {
                return Err(Error::compiler(
                    "native execution plan added runtime arguments",
                ));
            }
        } else if !run_args.iter().any(|a| a == output) {
            return Err(Error::compiler(
                "runner plan does not reference the generated artifact",
            ));
        }
        let absolute_output = root
            .join(output)
            .to_str()
            .ok_or_else(|| Error::resolution("run output requires a UTF-8 path"))?
            .to_owned();
        let executable = if program == output {
            absolute_output.clone()
        } else if Path::new(&program).components().count() > 1 {
            root.join(&program)
                .to_str()
                .ok_or_else(|| Error::resolution("runner path requires UTF-8"))?
                .to_owned()
        } else {
            program.clone()
        };
        let mut execution_args: Vec<OsString> = run_args
            .into_iter()
            .map(|arg| {
                OsString::from(if arg == output {
                    absolute_output.clone()
                } else {
                    arg
                })
            })
            .collect();
        execution_args.extend_from_slice(runtime);
        Some((
            Execution {
                directory: root.clone(),
                program: executable,
                args: execution_args,
            },
            PathBuf::from(output),
        ))
    } else {
        None
    };
    let root = std::env::current_dir().map_err(Error::environment)?;
    let (outputs, optional_outputs) = validate_plan_outputs(&plan, &root, &args[..separator])?;
    validate_requested_outputs(&plan, &root, &args[..separator])?;
    if user_requested_dry_run && !runtime.is_empty() {
        let values = runtime
            .iter()
            .map(|arg| {
                arg.to_str().map(str::to_owned).ok_or_else(|| {
                    Error::usage("dry-run JSON cannot represent non-UTF-8 program arguments")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let args = plan["execute"]["args"]
            .as_array_mut()
            .ok_or_else(|| Error::compiler("run plan is missing execute.args"))?;
        args.extend(values.into_iter().map(serde_json::Value::String));
    }
    Ok(PreparedBuild {
        freshness: None,
        plan,
        args: args[..separator].to_vec(),
        root,
        outputs,
        optional_outputs,
        execution,
        generation: generation.map(Path::to_path_buf),
    })
}

/// Compatibility entry point for callers that need only one compiler plan.
pub fn run_build_with_dry_run(
    compiler: &crate::Compiler,
    args: &[String],
    runtime: &[OsString],
    user_requested_dry_run: bool,
    generation: Option<&Path>,
    mut report: Option<&mut dyn FnMut(serde_json::Value) -> Result<(), Error>>,
) -> Result<Option<Execution>, Error> {
    let prepared = if let Some(sink) = report.as_mut() {
        prepare_build(
            compiler,
            args,
            runtime,
            user_requested_dry_run,
            generation,
            Some(&mut **sink),
        )?
    } else {
        prepare_build(
            compiler,
            args,
            runtime,
            user_requested_dry_run,
            generation,
            None,
        )?
    };
    if user_requested_dry_run {
        diagnostic::outln!(
            "{}",
            serde_json::to_string_pretty(prepared.plan()).map_err(Error::internal)?
        );
        Ok(None)
    } else {
        prepared.execute(compiler, report)
    }
}

/// Bind compiler destinations to the artifact identity selected by Vex. Merely
/// remaining somewhere under target/ is insufficient for a multi-target batch.
fn validate_requested_outputs(
    plan: &serde_json::Value,
    root: &Path,
    args: &[String],
) -> Result<(), Error> {
    if args.iter().any(|arg| arg == "--emit=check") {
        if plan["emit"] != "check"
            || !plan["link"].is_null()
            || !plan["compile"].as_array().unwrap().is_empty()
            || !plan["emit_jobs"].as_array().unwrap().is_empty()
        {
            return Err(Error::compiler(
                "compiler check plan must not emit artifacts",
            ));
        }
        return Ok(());
    }
    let Some(expected) = args
        .windows(2)
        .find(|pair| pair[0] == "-o")
        .map(|pair| &pair[1])
    else {
        return Ok(());
    };
    let normalize = |path: &Path| {
        let path = root.join(path);
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    result.pop();
                }
                _ => result.push(component.as_os_str()),
            }
        }
        result
    };
    let expected = normalize(Path::new(expected));
    let compile = plan["compile"].as_array().unwrap();
    let object = args.iter().any(|arg| arg == "--emit=obj");
    if object && (plan["emit"] != "obj" || !plan["link"].is_null()) {
        return Err(Error::compiler(
            "compiler object plan does not match the selected package target output kind",
        ));
    }
    if !object && (plan["emit"] != "bin" || plan["link"].is_null()) {
        return Err(Error::compiler(
            "compiler linked plan does not match the selected package target output kind",
        ));
    }
    if let Some(output) = plan["link"]["output"].as_str() {
        if normalize(Path::new(output)) != expected {
            return Err(Error::compiler(
                "compiler plan final output does not match the selected package target output",
            ));
        }
        if let Some(directory) = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--target-dir="))
        {
            let directory = normalize(Path::new(directory));
            for job in compile {
                let output = normalize(Path::new(job["output"].as_str().unwrap()));
                if output == directory || !output.starts_with(&directory) {
                    return Err(Error::compiler("compiler plan intermediate output escapes the selected package target directory"));
                }
            }
        }
    } else if plan["emit"] != "obj"
        || compile.len() != 1
        || normalize(Path::new(compile[0]["output"].as_str().unwrap())) != expected
    {
        return Err(Error::compiler(
            "compiler object plan does not match the selected package target output",
        ));
    }
    Ok(())
}

fn finish_compilation(
    compilation: Result<(), Error>,
    ownership: Result<(), String>,
) -> Result<(), Error> {
    match (compilation, ownership) {
        (Err(primary), Err(secondary)) => {
            // Bookkeeping must not replace compiler, message-file, or
            // supervision failures. The secondary cause remains in JSONL too.
            let _ = diagnostic::output::stderr(format_args!(
                "warning: artifact ownership could not be recorded: {secondary}\n"
            ));
            Err(primary.with_field("artifact_ownership_error", secondary))
        }
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(error)) => Err(Error::environment(error)),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub(crate) fn validate_plan_outputs(
    plan: &serde_json::Value,
    root: &Path,
    compiler_args: &[String],
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), Error> {
    let compile = plan["compile"].as_array().unwrap();
    let outputs: Vec<_> = compile
        .iter()
        .filter_map(|job| job["output"].as_str())
        .chain(plan["link"]["output"].as_str())
        .map(PathBuf::from)
        .collect();
    let optional = optional_link_outputs(plan);
    let all_outputs: Vec<_> = outputs.iter().chain(&optional).cloned().collect();
    let names =
        state::artifacts::validate_output_paths(root, &all_outputs).map_err(Error::compiler)?;
    let identity = |path: &Path| {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            root.join(path)
        };
        let path = path.canonicalize().unwrap_or_else(|_| {
            let mut normalized = PathBuf::new();
            for component in path.components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        normalized.pop();
                    }
                    _ => normalized.push(component.as_os_str()),
                }
            }
            normalized
        });
        let value = path.to_string_lossy().into_owned();
        if cfg!(any(windows, target_os = "macos")) {
            value.to_lowercase()
        } else {
            value
        }
    };
    let output_ids: Vec<_> = names.iter().map(|name| identity(Path::new(name))).collect();
    let compile_ids: Vec<_> = compile
        .iter()
        .map(|job| identity(Path::new(job["output"].as_str().unwrap())))
        .collect();
    let inputs = plan["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|input| input["path"].as_str().unwrap())
        .chain(compile.iter().map(|job| job["input"].as_str().unwrap()));
    for input in inputs {
        if output_ids.contains(&identity(Path::new(input))) {
            return Err(Error::compiler(format!(
                "compiler output aliases input `{input}`"
            )));
        }
    }
    // Native absolute file arguments are generated by Vex, not accepted from
    // the CLI. Schema v1 omits raw linker arguments from link.inputs, so protect
    // them explicitly before any compile job could replace them.
    for input in compiler_args
        .iter()
        .filter_map(|argument| argument.strip_prefix("-Clink-arg="))
        .filter(|input| Path::new(input).is_absolute())
    {
        if output_ids.contains(&identity(Path::new(input))) {
            return Err(Error::compiler(format!(
                "compiler output aliases native linker input `{input}`"
            )));
        }
    }
    if let Some(inputs) = plan["link"]["inputs"].as_array() {
        for input in inputs {
            let input = input.as_str().unwrap();
            let id = identity(Path::new(input));
            // Objects generated in this same plan are intentionally link inputs.
            if !compile_ids.contains(&id) && output_ids.contains(&id) {
                return Err(Error::compiler(format!(
                    "compiler output aliases link input `{input}`"
                )));
            }
        }
    }
    Ok((outputs, optional))
}

fn optional_link_outputs(plan: &serde_json::Value) -> Vec<PathBuf> {
    if !plan["target"].as_str().unwrap().ends_with("-windows-msvc") {
        return Vec::new();
    }
    let Some(output) = plan["link"]["output"].as_str().map(PathBuf::from) else {
        return Vec::new();
    };
    let args = plan["link"]["args"].as_array().unwrap();
    let mut paths = Vec::new();
    // Schema v1 lists the image but omits MSVC linker companions. Reserve the
    // compiler's documented destinations without parsing display-only commands.
    for (option, default) in [
        ("IMPLIB", output.with_extension("lib")),
        ("PDB", output.with_extension("pdb")),
        ("ILK", output.with_extension("ilk")),
        ("MAP", output.with_extension("map")),
        (
            "MANIFESTFILE",
            PathBuf::from(format!("{}.manifest", output.display())),
        ),
    ] {
        let path = args
            .iter()
            .rev()
            .find_map(|arg| {
                let arg = arg.as_str().unwrap();
                let (name, value) = arg
                    .strip_prefix('/')
                    .or_else(|| arg.strip_prefix('-'))?
                    .split_once(':')?;
                name.eq_ignore_ascii_case(option)
                    .then(|| PathBuf::from(value))
            })
            .unwrap_or(default);
        if option == "PDB"
            && path
                .to_str()
                .is_some_and(|path| path.eq_ignore_ascii_case("NONE"))
        {
            continue;
        }
        if option == "IMPLIB" {
            paths.push(path.with_extension("exp"));
        }
        paths.push(path);
    }
    paths
}

fn validate_output_path(output: &Path, generation: &Path) -> Result<(), Error> {
    if !output.starts_with(generation)
        || output
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::compiler(
            "link.output must be inside the per-run output generation",
        ));
    }
    Ok(())
}

pub fn create_run_generation() -> Result<PathBuf, Error> {
    state::ensure_dir(Path::new("target")).map_err(Error::environment)?;
    let parent = Path::new("target/.vex-run");
    state::ensure_dir(parent).map_err(Error::environment)?;
    loop {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::environment(e.to_string()))?
            .as_nanos();
        let path = parent.join(format!("{}-{time}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::environment(e)),
        }
    }
}

pub fn contains_dry_run_flag(args: &[String]) -> bool {
    args.iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument == "--dry-run")
}

fn insert_build_flag(args: &mut Vec<String>, flag: &str) {
    if let Some(separator_index) = args.iter().position(|argument| argument == "--") {
        args.insert(separator_index, flag.to_string());
    } else {
        args.push(flag.to_string());
    }
}

pub(crate) fn compiler_error(status: ExitStatus, message: String) -> Error {
    // wavec reserves 3 for missing backend tools / environment / IO failures.
    // Its usage rejection is still a compiler interface failure, not Vex CLI usage.
    if status.code() == Some(3) {
        Error::environment(message)
    } else {
        Error::compiler(message)
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

struct DryRunOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn emit_diagnostics(
    stderr: &[u8],
    sink: &mut dyn FnMut(serde_json::Value) -> Result<(), Error>,
) -> Result<(), Error> {
    for line in stderr
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(line) {
            if payload.is_object() {
                sink(
                    serde_json::json!({"event":"compiler-diagnostic", "origin":"vex", "compiler":payload}),
                )?;
            }
        }
    }
    Ok(())
}

fn run_wavec_dry_run(
    wavec: &Path,
    dry_run_args: &[String],
    report: &mut Option<&mut dyn FnMut(serde_json::Value) -> Result<(), Error>>,
) -> Result<DryRunOutput, Error> {
    let output = process::output(
        Command::new(wavec).args(dry_run_args),
        std::time::Duration::from_secs(60),
    )
    .map_err(|error| {
        Error::environment(format!(
            "failed to execute `{}`. Install wavec or set VEX_WAVEC=/path/to/wavec: {error}",
            wavec.display()
        ))
    })?;
    if let Some(sink) = report.as_mut() {
        emit_diagnostics(&output.stderr, &mut **sink)?;
    }
    if !output.status.success() {
        return Err(compiler_error(
            output.status,
            format!(
                "wavec dry-run failed using `{}` [{}]: {}",
                wavec.display(),
                classify_exit(output.status),
                combined_output(&output.stdout, &output.stderr)
            ),
        ));
    }
    Ok(DryRunOutput {
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stdout.trim(), stderr.trim()),
        (false, true) => stdout.trim().to_string(),
        (true, false) => stderr.trim().to_string(),
        (true, true) => "<no output>".to_string(),
    }
}

#[cfg(test)]
mod message_tests {
    #[test]
    fn tracking_failure_never_replaces_an_existing_error_category() {
        for category in [
            diagnostic::Category::Compiler,
            diagnostic::Category::Environment,
            diagnostic::Category::Timeout,
            diagnostic::Category::Cancelled,
        ] {
            let error = super::finish_compilation(
                Err(diagnostic::Error::new(category, "primary failure")),
                Err("registry failure".into()),
            )
            .unwrap_err();
            assert_eq!(error.category, category);
            assert_eq!(error.to_string(), "primary failure");
            assert_eq!(
                error.context_fields,
                vec![("artifact_ownership_error".into(), "registry failure".into())]
            );
        }
    }

    #[test]
    fn native_linker_arguments_cannot_alias_compiler_outputs() {
        let root = std::env::current_dir().unwrap();
        let plan = serde_json::json!({
            "target":"x86_64-unknown-linux-gnu", "link":null,
            "compile":[{"input":"src/main.wave","output":"target/native-input-alias.o"}],
            "inputs":[{"path":"src/main.wave"}]
        });
        let arguments = vec![format!(
            "-Clink-arg={}",
            root.join("target/native-input-alias.o").display()
        )];
        let error = super::validate_plan_outputs(&plan, &root, &arguments).unwrap_err();
        assert_eq!(error.category, diagnostic::Category::Compiler);
        assert!(error.to_string().contains("aliases native linker input"));
        super::validate_plan_outputs(
            &plan,
            &root,
            &[
                "-Clink-arg=-soname".into(),
                "-Clink-arg=libexample.so".into(),
            ],
        )
        .unwrap();
    }

    #[test]
    fn structured_diagnostic_preserves_nested_spans_and_related_locations() {
        let payload = serde_json::json!({"error":{"message":"fixture", "span":{"start":3,"end":8,"line":2}, "related":[{"span":{"start":0,"end":1}}]}});
        let bytes = serde_json::to_vec(&payload).unwrap();
        let mut events = Vec::new();
        super::emit_diagnostics(&bytes, &mut |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            events,
            vec![
                serde_json::json!({"event":"compiler-diagnostic", "origin":"vex", "compiler":payload})
            ]
        );
    }
}
