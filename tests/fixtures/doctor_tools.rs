// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::{env, fs::OpenOptions, io::Write};
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    let kind = env::current_exe().unwrap().file_stem().unwrap().to_string_lossy().into_owned();
    if let Ok(path) = env::var("DOCTOR_LOG") {
        writeln!(OpenOptions::new().create(true).append(true).open(path).unwrap(), "{kind}:{}", args.join(" ")).unwrap();
    }
    if kind == "git" {
        assert_eq!(args, ["--version"], "doctor must never fetch or access a remote");
        println!("git version 2.48.0"); return;
    }
    if args == ["--version"] {
        assert!(env::var_os("NO_COLOR").is_some());
        println!("wavec 0.2.1-pre-beta"); return;
    }
    if args == ["print", "supported-targets", "--format=json"] {
        println!("{}", env::var("DOCTOR_TARGETS").unwrap()); return;
    }
    let target = args.iter().find_map(|s| s.strip_prefix("--target=")).map(str::to_owned)
        .unwrap_or_else(|| env::var("DOCTOR_HOST_TARGET").unwrap());
    if args.first().map(String::as_str) == Some("print") && args.get(1).map(String::as_str) == Some("target-spec") {
        println!("{{\"triple\":{target:?},\"supported\":true,\"hosted\":true,\"object_format\":\"elf\",\"os\":\"linux\"}}"); return;
    }
    assert!(args.iter().any(|s| s == "--dry-run"), "doctor must not compile");
    assert!(!args.iter().any(|s| s == "--run"), "doctor must not run");
    if let Ok(error) = env::var("DOCTOR_ERROR") { eprintln!("{error}"); std::process::exit(env::var("DOCTOR_EXIT").ok().and_then(|s| s.parse().ok()).unwrap_or(1)); }
    let schema = env::var("DOCTOR_SCHEMA").unwrap_or_else(|_| "1".into());
    let requested_output = args.windows(2).find(|pair| pair[0] == "-o").map(|pair| pair[1].clone()).unwrap_or_else(|| "target/main".into());
    let link_output = env::var("DOCTOR_LINK_OUTPUT").unwrap_or(requested_output.clone());
    let object = args.iter().any(|arg| arg == "--emit=obj");
    let emit = if object { "obj" } else { "bin" };
    let program = env::var("DOCTOR_LINK").unwrap_or_else(|_| env::current_exe().unwrap().to_string_lossy().into_owned());
    let link = if object { "null".into() } else {
        format!("{{\"output\":{link_output:?},\"inputs\":[],\"program\":{program:?},\"args\":[]}}")
    };
    let compile = if object {
        format!("[{{\"input\":\"src/lib.wave\",\"kind\":\"wave\",\"output\":{requested_output:?},\"command\":\"wavec\"}}]")
    } else { "[]".into() };
    println!("{{\"schema_version\":{schema},\"mode\":\"build\",\"target\":{target:?},\"emit\":{emit:?},\"emit_kinds\":[],\"control_mode\":null,\"forced_input_type\":null,\"inputs\":[],\"emit_jobs\":[],\"compile\":{compile},\"link\":{link},\"execute\":null}}");
}
