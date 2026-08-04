#[derive(Debug)]
pub struct BuildValidationRequest<'a> {
    pub inputs: &'a [String],
    pub build_args: &'a [String],
    pub run_args: &'a [String],
    pub run_separator_seen: bool,
    pub global_args: &'a [String],
}

pub fn validate_build_invocation(request: BuildValidationRequest<'_>) -> Result<(), String> {
    if request.inputs.is_empty() {
        return Err("build requires at least one input".to_string());
    }

    let mut emit_kinds: Vec<String> = Vec::new();
    let mut forced_input_type: Option<String> = None;
    let mut link_only = false;
    let mut run = false;
    let mut has_output = false;
    let mut has_out_dir = false;
    let mut shared = false;
    let mut static_link = false;
    let mut pie = false;
    let mut no_pie = false;
    let mut compile_only = false;
    let mut has_entry = false;
    let mut has_linker_script = false;
    let mut has_no_start_files = false;

    let mut i = 0;
    while i < request.build_args.len() {
        let token = request.build_args[i].as_str();
        match token {
            "-c" => compile_only = true,
            "--link-only" => link_only = true,
            "--run" => run = true,
            "--shared" => shared = true,
            "--static" => static_link = true,
            "--pie" => pie = true,
            "--no-pie" => no_pie = true,
            "--no-start-files" => has_no_start_files = true,
            "-o" | "--output" => {
                has_output = true;
                i += 1;
            }
            "--out-dir" => {
                has_out_dir = true;
                i += 1;
            }
            "--target-dir" | "--error-format" => i += 1,
            "--emit" => {
                i += 1;
                if i >= request.build_args.len() {
                    return Err("missing value for `--emit`".to_string());
                }
                emit_kinds.extend(split_csv(&request.build_args[i]));
            }
            "--input-type" => {
                i += 1;
                if i >= request.build_args.len() {
                    return Err("missing value for `--input-type`".to_string());
                }
                forced_input_type = Some(request.build_args[i].clone());
            }
            "--entry" => {
                has_entry = true;
                i += 1;
            }
            "--linker-script" => {
                has_linker_script = true;
                i += 1;
            }
            _ => {
                if let Some(value) = token.strip_prefix("--emit=") {
                    emit_kinds.extend(split_csv(value));
                } else if let Some(value) = token.strip_prefix("--input-type=") {
                    forced_input_type = Some(value.to_string());
                } else if token.starts_with("--output=") {
                    has_output = true;
                } else if token.starts_with("--out-dir=") {
                    has_out_dir = true;
                } else if token.starts_with("--entry=") {
                    has_entry = true;
                } else if token.starts_with("--linker-script=") {
                    has_linker_script = true;
                }
            }
        }

        i += 1;
    }

    if emit_kinds.is_empty() {
        emit_kinds.push("bin".to_string());
    }

    for kind in &emit_kinds {
        if !matches!(
            kind.as_str(),
            "check" | "ast" | "ir" | "bc" | "asm" | "obj" | "bin"
        ) {
            return Err(format!("unsupported emit kind: `{kind}`"));
        }
    }

    let input_types = resolve_input_types(request.inputs, forced_input_type.as_deref())?;

    if emit_kinds.iter().any(|emit| emit == "check") {
        if emit_kinds.len() != 1 {
            return Err("`check` emit must be used alone".to_string());
        }
        if run {
            return Err("`--emit=check` cannot be combined with `--run`".to_string());
        }
        if link_only {
            return Err("`--emit=check` cannot be combined with `--link-only`".to_string());
        }
        if has_output || has_out_dir {
            return Err(
                "`--emit=check` cannot be combined with `-o/--output/--out-dir`".to_string(),
            );
        }
        if input_types.iter().any(|kind| kind != "wave") {
            return Err("`--emit=check` accepts only `.wave` inputs".to_string());
        }
    }

    if link_only {
        if emit_kinds.len() != 1 || emit_kinds[0] != "bin" {
            return Err("`--link-only` requires `--emit=bin`".to_string());
        }
        if input_types.iter().any(|kind| !is_link_ready_input(kind)) {
            return Err(
                "`--link-only` accepts only link-ready inputs (`.o`, `.obj`, `.a`)".to_string(),
            );
        }
    }

    if emit_kinds.len() == 1
        && emit_kinds[0] == "obj"
        && input_types.iter().all(|kind| is_link_ready_input(kind))
    {
        return Err("`--emit=obj` requires at least one compilable input (`.wave`, `.ll/.ir`, `.bc`, `.s/.asm`)".to_string());
    }

    if run && !emit_kinds.iter().any(|kind| kind == "bin") {
        return Err("`--run` requires `bin` emit".to_string());
    }

    if request.run_separator_seen && !run {
        return Err("run arguments after `--` require `--run`".to_string());
    }

    if shared && run {
        return Err("`--run` cannot be used with `--shared`".to_string());
    }

    if shared && static_link {
        return Err("`--shared` and `--static` cannot be used together".to_string());
    }

    if pie && no_pie {
        return Err("`--pie` and `--no-pie` cannot be used together".to_string());
    }

    if shared && (pie || no_pie) {
        return Err("`--shared` cannot be used with `--pie` or `--no-pie`".to_string());
    }

    let relocation_model = detect_relocation_model(request.global_args, request.build_args)?;
    if pie {
        if let Some(model) = relocation_model.as_deref() {
            if model != "pie" {
                return Err("`--pie` allows only `-C relocation-model=pie`".to_string());
            }
        }
    }

    if no_pie && relocation_model.as_deref() == Some("pie") {
        return Err("`--no-pie` cannot be used with `-C relocation-model=pie`".to_string());
    }

    if shared {
        if let Some(model) = relocation_model.as_deref() {
            if model != "pic" && model != "dynamic-no-pic" {
                return Err(
                    "`--shared` allows `-C relocation-model=pic|dynamic-no-pic` only".to_string(),
                );
            }
        }
    }

    let has_link_step = link_only || (emit_kinds.iter().any(|kind| kind == "bin") && !compile_only);
    if (has_entry || has_linker_script || has_no_start_files) && !has_link_step {
        return Err(
            "`--entry`, `--linker-script`, `--no-start-files` require a link stage".to_string(),
        );
    }

    for emit in &emit_kinds {
        if !has_compatible_input(emit, &input_types) {
            return Err(format!(
                "no compatible input for emit `{emit}` with current input set"
            ));
        }
    }

    if !request.run_args.is_empty() && !request.run_separator_seen {
        return Err("internal error: run args provided without separator marker".to_string());
    }

    Ok(())
}

pub fn collect_inputs(build_args: &[String]) -> Vec<String> {
    let mut inputs = Vec::new();
    let mut i = 0;

    while i < build_args.len() {
        let token = build_args[i].as_str();

        if token == "--" {
            break;
        }

        if needs_value(token) {
            i += 2;
            continue;
        }

        if is_option_with_inline_value(token) {
            i += 1;
            continue;
        }

        if token.starts_with('-') {
            i += 1;
            continue;
        }

        inputs.push(build_args[i].clone());
        i += 1;
    }

    inputs
}

fn resolve_input_types(
    inputs: &[String],
    forced_input_type: Option<&str>,
) -> Result<Vec<String>, String> {
    let forced_input_type = forced_input_type.map(normalize_input_type).transpose()?;
    let mut input_types = Vec::new();

    for input in inputs {
        let inferred = infer_input_type(input);
        let resolved = match (forced_input_type.as_deref(), inferred) {
            (Some(forced), Some(found)) if forced != found => {
                return Err(format!(
                    "input `{input}` inferred type `{found}` conflicts with `--input-type={forced}`"
                ));
            }
            (Some(forced), _) => forced.to_string(),
            (None, Some(found)) => found.to_string(),
            (None, None) => {
                return Err(format!(
                    "cannot infer input type for `{input}`. use `--input-type=<wave,ir,bc,asm,obj,archive>`"
                ));
            }
        };

        input_types.push(resolved);
    }

    Ok(input_types)
}

fn normalize_input_type(kind: &str) -> Result<String, String> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "wave" => Ok("wave".to_string()),
        "ir" => Ok("ir".to_string()),
        "bc" => Ok("bc".to_string()),
        "asm" => Ok("asm".to_string()),
        "obj" => Ok("obj".to_string()),
        "archive" | "lib" => Ok("archive".to_string()),
        _ => Err(format!(
            "unsupported input type: `{kind}`. expected wave, ir, bc, asm, obj, archive"
        )),
    }
}

fn infer_input_type(input: &str) -> Option<&'static str> {
    let lower = input.to_ascii_lowercase();
    if lower.ends_with(".wave") {
        Some("wave")
    } else if lower.ends_with(".ll") || lower.ends_with(".ir") {
        Some("ir")
    } else if lower.ends_with(".bc") {
        Some("bc")
    } else if lower.ends_with(".s") || lower.ends_with(".asm") {
        Some("asm")
    } else if lower.ends_with(".o") || lower.ends_with(".obj") {
        Some("obj")
    } else if lower.ends_with(".a") {
        Some("archive")
    } else {
        None
    }
}

fn has_compatible_input(emit: &str, input_types: &[String]) -> bool {
    match emit {
        "check" => input_types.iter().any(|kind| kind == "wave"),
        "ast" => input_types.iter().any(|kind| kind == "wave"),
        "ir" => input_types
            .iter()
            .any(|kind| kind == "wave" || kind == "ir"),
        "bc" => input_types
            .iter()
            .any(|kind| kind == "wave" || kind == "ir" || kind == "bc"),
        "asm" => input_types
            .iter()
            .any(|kind| kind == "wave" || kind == "ir" || kind == "bc" || kind == "asm"),
        "obj" => input_types
            .iter()
            .any(|kind| matches!(kind.as_str(), "wave" | "ir" | "bc" | "asm")),
        "bin" => input_types.iter().any(|kind| {
            matches!(
                kind.as_str(),
                "wave" | "ir" | "bc" | "asm" | "obj" | "archive"
            )
        }),
        _ => false,
    }
}

fn is_link_ready_input(kind: &str) -> bool {
    matches!(kind, "obj" | "archive")
}

fn detect_relocation_model(
    global_args: &[String],
    build_args: &[String],
) -> Result<Option<String>, String> {
    let mut model: Option<String> = None;
    parse_codegen_args(global_args, &mut model)?;
    parse_codegen_args(build_args, &mut model)?;
    Ok(model)
}

fn parse_codegen_args(
    args: &[String],
    relocation_model: &mut Option<String>,
) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();

        if token == "-C" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for `-C`".to_string());
            }
            let value = args[i].as_str();
            maybe_set_relocation_model(value, relocation_model);
        } else if let Some(value) = token.strip_prefix("-C") {
            maybe_set_relocation_model(value, relocation_model);
        }

        i += 1;
    }

    Ok(())
}

fn maybe_set_relocation_model(value: &str, relocation_model: &mut Option<String>) {
    if let Some(model) = value.strip_prefix("relocation-model=") {
        *relocation_model = Some(model.to_string());
    }
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn needs_value(token: &str) -> bool {
    matches!(
        token,
        "-o" | "--output"
            | "--out-dir"
            | "--target-dir"
            | "--emit"
            | "--input-type"
            | "--error-format"
            | "--entry"
            | "--linker-script"
    )
}

fn is_option_with_inline_value(token: &str) -> bool {
    token.starts_with("--output=")
        || token.starts_with("--out-dir=")
        || token.starts_with("--target-dir=")
        || token.starts_with("--emit=")
        || token.starts_with("--input-type=")
        || token.starts_with("--error-format=")
        || token.starts_with("--entry=")
        || token.starts_with("--linker-script=")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn validate(build_args: &[&str]) -> Result<(), String> {
        let build_args = strings(build_args);
        let inputs = collect_inputs(&build_args);
        validate_build_invocation(BuildValidationRequest {
            inputs: &inputs,
            build_args: &build_args,
            run_args: &[],
            run_separator_seen: false,
            global_args: &[],
        })
    }

    #[test]
    fn archive_inputs_follow_wavec_contract() {
        validate(&["src/main.wave", "native.o", "libnative.a", "--emit=obj,bin"])
            .expect("mixed Wave/object/archive input must be accepted");
        validate(&["libnative.a", "--link-only", "--emit=bin"])
            .expect("archive input must be link-ready");
        validate(&[
            "libnative.a",
            "--input-type=archive",
            "--link-only",
            "--emit=bin",
        ])
        .expect("forced archive input type must be accepted");
        validate(&[
            "libnative.a",
            "--input-type=lib",
            "--link-only",
            "--emit=bin",
        ])
        .expect("lib must be accepted as an archive alias");
    }

    #[test]
    fn obj_emit_rejects_only_link_ready_inputs() {
        let err = validate(&["native.o", "libnative.a", "--emit=obj"])
            .expect_err("obj emit cannot transform link-ready inputs into another object");
        assert!(
            err.contains("requires at least one compilable input"),
            "{err}"
        );
    }

    #[test]
    fn check_and_run_conflicts_are_preflighted() {
        let err = validate(&["src/main.wave", "--emit=check,ast"])
            .expect_err("check must be a standalone control mode");
        assert!(err.contains("check"), "{err}");

        let build_args = strings(&["src/main.wave", "--emit=obj"]);
        let inputs = collect_inputs(&build_args);
        let run_args = strings(&["arg"]);
        let err = validate_build_invocation(BuildValidationRequest {
            inputs: &inputs,
            build_args: &build_args,
            run_args: &run_args,
            run_separator_seen: true,
            global_args: &[],
        })
        .expect_err("run args require --run");
        assert!(err.contains("--run"), "{err}");
    }

    #[test]
    fn relocation_conflicts_match_wavec_rules() {
        let build_args = strings(&["src/main.wave", "--no-pie"]);
        let inputs = collect_inputs(&build_args);
        let global_args = strings(&["-C", "relocation-model=pie"]);
        let err = validate_build_invocation(BuildValidationRequest {
            inputs: &inputs,
            build_args: &build_args,
            run_args: &[],
            run_separator_seen: false,
            global_args: &global_args,
        })
        .expect_err("--no-pie conflicts with pie relocation model");
        assert!(err.contains("relocation-model=pie"), "{err}");
    }
}
