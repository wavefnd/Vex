use diagnostic::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::plan;

/// Compilation ends before this value is returned. No project guard is owned
/// here: the caller must drop its resolution/guard before calling execute().
pub struct Execution {
    program: String,
    args: Vec<OsString>,
}
impl Execution {
    pub fn execute(self) -> Result<ExitStatus, Error> {
        process::status(Command::new(&self.program).args(&self.args), None, true)
            .map_err(|e| Error::environment(format!("failed to run `{}`: {e}", self.program)))
    }
}

pub fn run_build_with_dry_run(
    compiler: &crate::Compiler,
    args: &[String],
    runtime: &[OsString],
    user_requested_dry_run: bool,
    generation: Option<&Path>,
    mut report: Option<&mut dyn FnMut(serde_json::Value) -> Result<(), Error>>,
) -> Result<Option<Execution>, Error> {
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
        let mut execution_args: Vec<OsString> = run_args.into_iter().map(OsString::from).collect();
        execution_args.extend_from_slice(runtime);
        Some((
            Execution {
                program,
                args: execution_args,
            },
            PathBuf::from(output),
        ))
    } else {
        None
    };
    if user_requested_dry_run {
        if !runtime.is_empty() {
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
        diagnostic::outln!(
            "{}",
            serde_json::to_string_pretty(&plan).map_err(Error::internal)?
        );
        return Ok(None);
    }
    state::ensure_dir(Path::new("target")).map_err(Error::environment)?;
    let mut compile_args: Vec<_> = args[..separator]
        .iter()
        .filter(|a| a.as_str() != "--run")
        .cloned()
        .collect();
    let status = if let Some(sink) = report.as_mut() {
        insert_build_flag(&mut compile_args, "--error-format=json");
        let output = process::output_without_deadline(Command::new(wavec).args(&compile_args))
            .map_err(Error::environment)?;
        diagnostic::output::stdout(format_args!("{}", String::from_utf8_lossy(&output.stdout)))?;
        // Compiler diagnostics retain their original structured payload and spans.
        // User-program streams never pass through this compiler-only capture.
        diagnostic::output::stderr(format_args!("{}", String::from_utf8_lossy(&output.stderr)))?;
        emit_diagnostics(&output.stderr, &mut **sink)?;
        output.status
    } else {
        process::status(Command::new(wavec).args(&compile_args), None, false).map_err(|e| {
            Error::environment(format!(
                "failed to execute `{}` build: {e}",
                wavec.display()
            ))
        })?
    };
    if !status.success() {
        return Err(compiler_error(
            status,
            format!("wavec build failed [{}]", classify_exit(status)),
        ));
    }
    if let Some(sink) = report.as_mut() {
        let outputs: Vec<_> = if let Some(output) = plan["link"]["output"].as_str() {
            vec![output.to_owned()]
        } else {
            plan["compile"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|job| job["output"].as_str().map(str::to_owned))
                .collect()
        };
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
