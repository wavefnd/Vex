use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use serde_json::Value;

pub fn run_build_with_dry_run(args: &[String], user_requested_dry_run: bool) -> Result<(), String> {
    let mut dry_run_args = args.to_vec();

    if !contains_dry_run_flag(&dry_run_args) {
        insert_build_flag(&mut dry_run_args, "--dry-run");
    }
    insert_build_flag(&mut dry_run_args, "--error-format=json");

    let wavec = wavec_path();
    let validation_output = run_wavec_dry_run(&wavec, &dry_run_args)?;
    validate_dry_run_json_output(&validation_output.stdout, &validation_output.stderr).map_err(
        |err| {
            format!(
                "installed wavec is incompatible with Vex: {err}\nhelp: update wavec or set VEX_WAVEC=/path/to/wavec"
            )
        },
    )?;

    if !user_requested_dry_run {
        let status = Command::new(&wavec)
            .args(args)
            .status()
            .map_err(|e| format!("failed to execute `{}` build: {e}", wavec.display()))?;
        if !status.success() {
            return Err(format!("wavec build failed [{}]", classify_exit(status)));
        }
        return Ok(());
    }

    let status = Command::new(&wavec).args(args).status().map_err(|e| {
        format!(
            "failed to execute `{}` build --dry-run: {e}",
            wavec.display()
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

struct DryRunOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_wavec_dry_run(wavec: &Path, dry_run_args: &[String]) -> Result<DryRunOutput, String> {
    let output = Command::new(wavec)
        .args(dry_run_args)
        .output()
        .map_err(|e| {
            format!(
                "failed to execute `{}`. Install wavec or set VEX_WAVEC=/path/to/wavec: {e}",
                wavec.display()
            )
        })?;

    if !output.status.success() {
        return Err(format!(
            "wavec dry-run failed using `{}` [{}]: {}",
            wavec.display(),
            classify_exit(output.status),
            combined_output(&output.stdout, &output.stderr)
        ));
    }

    Ok(DryRunOutput {
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn wavec_path() -> PathBuf {
    env::var_os("VEX_WAVEC")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wavec"))
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
