// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
// Independent compiler process fixture; compiled by rustc without dependencies.
use std::{env, fs, io::Write, path::PathBuf};

fn quote(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch.is_control() => result.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}
fn list(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| quote(value))
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn host() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "x86_64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if env::current_exe().unwrap().file_stem().unwrap() != "target-wavec" {
        if let Some(directory) = env::var_os("TARGET_PROGRAM_EXPECT_CWD") {
            assert_eq!(env::current_dir().unwrap().canonicalize().unwrap(), PathBuf::from(directory).canonicalize().unwrap());
        }
        let name=env::current_exe().unwrap().file_stem().unwrap().to_string_lossy().into_owned();
        if let Some(log)=env::var_os("TARGET_PROGRAM_LOG") {
            let mut file=fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
            writeln!(file,"{name}").unwrap();
        }
        if let Ok(codes)=env::var("TARGET_PROGRAM_EXIT_MAP") {
            if let Some(code)=codes.split(',').find_map(|part|part.split_once('=').filter(|(key,_)|*key==name).map(|(_,value)|value.parse::<i32>().unwrap())) { env::set_var("TARGET_PROGRAM_EXIT",code.to_string()); }
        }
        println!("TARGET_PROGRAM:{}", list(&args));
        if let Ok(vex) = env::var("TARGET_PROGRAM_CHECK_LOCK_RELEASE") {
            let status = std::process::Command::new(vex)
                .args(["fetch", "--locked", "--offline"])
                .status()
                .unwrap();
            assert!(
                status.success(),
                "Vex must release its build lock before program spawn"
            );
        }
        std::process::exit(
            env::var("TARGET_PROGRAM_EXIT")
                .unwrap_or_else(|_| "17".into())
                .parse()
                .unwrap(),
        );
    }
    if let Some(path) = env::var_os("TARGET_COMPILER_LOG") {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{}", list(&args)).unwrap();
    }
    if args.iter().any(|arg| arg == "--version") {
        println!("wavec 0.2.1-pre-beta");
        return;
    }
    let value = |flag: &str| {
        args.iter()
            .find_map(|arg| arg.strip_prefix(&format!("{flag}=")).map(str::to_owned))
            .or_else(|| {
                args.iter()
                    .position(|arg| arg == flag)
                    .and_then(|i| args.get(i + 1).cloned())
            })
    };
    let triple = value("--target").unwrap_or_else(|| host().into());
    if args.first().is_some_and(|arg| arg == "print") {
        if args.get(1).is_some_and(|arg|arg=="std-path") {
            if let Ok(path)=env::var("TARGET_STD_PATH") { println!("{}",quote(&path)); return; }
        }
        if args.get(1).is_some_and(|arg| arg == "supported-targets") {
            println!("[\"x86_64-unknown-linux-gnu\",\"aarch64-unknown-linux-gnu\",\"x86_64-pc-windows-msvc\",\"aarch64-pc-windows-msvc\",\"x86_64-apple-darwin\",\"aarch64-apple-darwin\",\"wasm32-unknown-unknown\",\"wasm32-wasip1\"]");
        } else {
            let (os, format) = if triple == "wasm32-wasip1" { ("wasi", "wasm") }
            else if triple == "wasm32-unknown-unknown" { ("unknown", "wasm") }
            else if triple.contains("windows") {
                ("windows", "coff")
            } else if triple.contains("darwin") {
                ("macos", "macho")
            } else {
                ("linux", "elf")
            };
            println!("{{\"triple\":{},\"supported\":true,\"hosted\":{},\"os\":{},\"object_format\":{}}}", quote(&triple), os != "unknown", quote(os), quote(format));
        }
        return;
    }
    let input = args.iter().find(|arg| arg.ends_with(".wave")).unwrap();
    let dry = args.iter().any(|arg| arg == "--dry-run");
    let failure = if dry {
        "TARGET_PLAN_FAIL_INPUT"
    } else {
        "TARGET_COMPILE_FAIL_INPUT"
    };
    if env::var(failure).is_ok_and(|pattern| input.ends_with(&pattern)) {
        eprintln!("fixture rejected {input}");
        std::process::exit(1);
    }
    let check = args.iter().any(|arg| arg == "--emit=check");
    let object = args.iter().any(|arg| arg == "--emit=obj");
    let run = args.iter().any(|arg| arg == "--run");
    let output = value("-o").unwrap_or_else(|| "target/default".into());
    let intermediate = if object {
        output.clone()
    } else {
        PathBuf::from(value("--target-dir").unwrap_or_else(|| "target".into()))
            .join("package.o")
            .to_string_lossy()
            .into_owned()
    };
    if dry {
        let emit = if check {
            "check"
        } else if object {
            "obj"
        } else {
            "bin"
        };
        let compile = if check {
            "[]".into()
        } else {
            format!("[{{\"input\":{},\"kind\":\"wave\",\"output\":{},\"command\":\"target fixture compile\"}}]", quote(input), quote(&intermediate))
        };
        let link = if check || object {
            "null".into()
        } else {
            format!("{{\"output\":{},\"inputs\":[{}],\"program\":{},\"args\":[]}}", quote(&output), quote(&intermediate), quote(&env::var("TARGET_LINK_PROGRAM").unwrap_or_else(|_|"target fixture linker".into())))
        };
        let execute = if run {
            format!("{{\"program\":{},\"args\":[]}}", quote(&output))
        } else {
            "null".into()
        };
        println!("{{\"schema_version\":1,\"mode\":{},\"target\":{},\"emit\":{},\"emit_kinds\":[],\"control_mode\":null,\"forced_input_type\":null,\"inputs\":[{{\"path\":{},\"kind\":\"wave\"}}],\"emit_jobs\":[],\"compile\":{},\"link\":{},\"execute\":{}}}", quote(if run { "build+run" } else { "build" }), quote(&triple), quote(emit), quote(input), compile, link, execute);
        return;
    }
    assert!(!run, "Vex must separate compilation from execution");
    if !check {
        let outputs = if object {
            vec![intermediate]
        } else {
            vec![intermediate, output.clone()]
        };
        for output in outputs {
            let path = PathBuf::from(output);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"target fixture artifact").unwrap();
        }
        if output.contains(".vex-run") {
            fs::copy(env::current_exe().unwrap(), output).unwrap();
        }
    }
}
