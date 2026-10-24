// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use serde_json::Value;

pub(crate) fn validate_dry_run_json_output(stdout: &[u8], _stderr: &[u8]) -> Result<Value, String> {
    if stdout.len() > 8 * 1024 * 1024 {
        return Err("compiler plan exceeds 8 MiB".into());
    }
    let text = std::str::from_utf8(stdout).map_err(|_| "wavec plan stdout is not UTF-8")?;
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("expected one JSON plan document on stdout: {e}"))?;
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
    require_link_or_null(object.get("link")).map_err(|e| format!("link: {e}"))?;
    require_execute_or_null(object.get("execute")).map_err(|e| format!("execute: {e}"))?;
    for key in ["emit_kinds", "emit_jobs"] {
        string_array(&value[key], key)?;
    }
    for (key, fields) in [
        ("inputs", &["path", "kind"][..]),
        ("compile", &["input", "kind", "output", "command"][..]),
    ] {
        for (i, item) in value[key].as_array().unwrap().iter().enumerate() {
            let item = item
                .as_object()
                .ok_or_else(|| format!("{key}[{i}] must be an object"))?;
            for field in fields {
                require_string(item, field).map_err(|e| format!("{key}[{i}].{field}: {e}"))?;
            }
        }
    }
    if !value["link"].is_null() {
        string_array(&value["link"]["inputs"], "link.inputs")?;
        string_array(&value["link"]["args"], "link.args")?;
    }
    if !value["execute"].is_null() {
        string_array(&value["execute"]["args"], "execute.args")?;
    }
    Ok(value)
}

pub(crate) fn string_array(value: &Value, path: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?
        .iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{path}[{i}] must be a string"))
        })
        .collect()
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
        let error = validate_dry_run_json_output(missing_execute.as_bytes(), b"")
            .expect_err("execute is required by the v1 contract");
        assert!(error.contains("execute"), "{error}");
        let schema_two = valid_plan().replace("\"schema_version\": 1", "\"schema_version\": 2");
        let error = validate_dry_run_json_output(schema_two.as_bytes(), b"")
            .expect_err("unknown schema version must be rejected");
        assert!(error.contains("schema_version"), "{error}");
    }

    #[test]
    fn dry_run_json_rejects_noisy_or_multiple_documents() {
        let output = format!("debug line\n{}\n", valid_plan());
        validate_dry_run_json_output(output.as_bytes(), b"")
            .expect_err("stdout must contain exactly one plan");
        validate_dry_run_json_output(format!("{}{}", valid_plan(), valid_plan()).as_bytes(), b"")
            .expect_err("multiple plans are ambiguous");
    }
    #[test]
    fn nested_contract_rejects_wrong_types_with_field_locations() {
        let mut valid: Value = serde_json::from_str(valid_plan()).unwrap();
        valid["execute"] = serde_json::json!({"program":"target/main","args":[]});
        for (pointer, expected) in [
            ("/inputs/0/path", "inputs[0].path"),
            ("/inputs/0/kind", "inputs[0].kind"),
            ("/compile/0/input", "compile[0].input"),
            ("/compile/0/kind", "compile[0].kind"),
            ("/compile/0/output", "compile[0].output"),
            ("/compile/0/command", "compile[0].command"),
            ("/link/output", "link"),
            ("/link/program", "link"),
            ("/link/inputs/0", "link.inputs[0]"),
            ("/link/args/0", "link.args[0]"),
            ("/execute/program", "execute"),
            ("/execute/args", "execute"),
            ("/emit_kinds/0", "emit_kinds[0]"),
        ] {
            let mut invalid = valid.clone();
            *invalid.pointer_mut(pointer).unwrap() = serde_json::json!(123);
            let error = validate_dry_run_json_output(&serde_json::to_vec(&invalid).unwrap(), b"")
                .unwrap_err();
            assert!(error.contains(expected), "{pointer}: {error}");
        }
        for (key, value) in [
            ("inputs", serde_json::json!([false])),
            ("compile", serde_json::json!([[]])),
            ("emit_jobs", serde_json::json!([null])),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            let error = validate_dry_run_json_output(&serde_json::to_vec(&invalid).unwrap(), b"")
                .unwrap_err();
            assert!(error.contains(key), "{error}");
        }
    }

    #[test]
    fn contract_accepts_bom_whitespace_and_additive_fields_but_not_non_utf8() {
        let mut valid: Value = serde_json::from_str(valid_plan()).unwrap();
        valid["future_optional"] = serde_json::json!({"value":true});
        valid["link"]["future_optional"] = serde_json::json!(true);
        let text = format!("\u{feff} \n{valid}\n\t");
        validate_dry_run_json_output(text.as_bytes(), b"warning on stderr").unwrap();
        assert!(validate_dry_run_json_output(&[0xff], b"")
            .unwrap_err()
            .contains("UTF-8"));
        assert!(
            validate_dry_run_json_output(&vec![b' '; 8 * 1024 * 1024 + 1], b"")
                .unwrap_err()
                .contains("8 MiB")
        );
    }
}
