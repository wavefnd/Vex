// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::{env, fs, io::Write, path::PathBuf};
fn quote(value: &str) -> String {
    let mut text = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => text.push_str("\\\\"), '"' => text.push_str("\\\""),
            '\n' => text.push_str("\\n"), '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            c if c.is_control() => text.push_str(&format!("\\u{:04x}", c as u32)),
            c => text.push(c),
        }
    }
    text.push('"'); text
}
fn list(values: &[String]) -> String {
    format!("[{}]", values.iter().map(|s| quote(s)).collect::<Vec<_>>().join(","))
}
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if !env::current_exe().unwrap().file_stem().unwrap().to_string_lossy().starts_with("fake-native-wavec") {
        fs::write("program-started", list(&args)).unwrap();
        println!("native fixture program");
        std::process::exit(17);
    }
    if let Some(path) = env::var_os("NATIVE_COMPILER_LOG") {
        let mut log = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(log, "{}", list(&args)).unwrap();
    }
    if args.iter().any(|s| s == "--version") { println!("wavec 0.2.1-pre-beta"); return; }
    let value = |flag: &str| args.iter().find_map(|s| s.strip_prefix(&format!("{flag}=")).map(str::to_owned))
        .or_else(|| args.iter().position(|s| s == flag).and_then(|i| args.get(i+1).cloned()));
    let target = value("--target").unwrap_or_else(|| env::var("NATIVE_DEFAULT_TARGET").unwrap_or_else(|_| "x86_64-unknown-linux-gnu".into()));
    if args.first().is_some_and(|s| s == "print") {
        if args.get(1).is_some_and(|s| s == "supported-targets") {
            println!("[\"x86_64-unknown-linux-gnu\",\"aarch64-unknown-linux-gnu\",\"x86_64-pc-windows-msvc\",\"aarch64-pc-windows-msvc\",\"x86_64-apple-darwin\",\"aarch64-apple-darwin\"]");
        } else if let Ok(bad) = env::var("NATIVE_BAD_SPEC") { println!("{bad}"); }
        else {
            let (os, format) = if target.contains("windows") { ("windows", "coff") }
                else if target.contains("apple") { ("macos", "macho") } else { ("linux", "elf") };
            println!("{{\"triple\":{},\"supported\":true,\"hosted\":true,\"os\":{},\"object_format\":{}}}", quote(&target), quote(os), quote(format));
        }
        return;
    }
    let check = args.iter().any(|s| s == "--emit=check");
    let object = args.iter().any(|s| s == "--emit=obj");
    let run = args.iter().any(|s| s == "--run");
    let input = args.iter().find(|s| s.ends_with(".wave")).unwrap();
    let output = value("-o").unwrap_or_else(|| "target/default-program".into());
    let intermediate = if object { output.clone() }
        else { PathBuf::from(value("--target-dir").unwrap_or_else(|| "target".into())).join("native.o").to_string_lossy().into_owned() };
    if args.iter().any(|s| s == "--dry-run") {
        let emit = if check { "check" } else if object { "obj" } else { "bin" };
        let compile = if check { "[]".into() } else { format!("[{{\"input\":{},\"kind\":\"wave\",\"output\":{},\"command\":\"fixture compile\"}}]", quote(input), quote(&intermediate)) };
        let native_args: Vec<_> = args.iter().filter_map(|arg| arg.strip_prefix("-Clink-arg=").map(str::to_owned)).collect();
        let link = if check || object { "null".into() } else { format!("{{\"output\":{},\"inputs\":[{}],\"program\":\"fixture linker\",\"args\":{}}}", quote(&output), quote(&intermediate), list(&native_args)) };
        let execute = if run { format!("{{\"program\":{},\"args\":[]}}", quote(&output)) } else { "null".into() };
        println!("{{\"schema_version\":1,\"mode\":{},\"target\":{},\"emit\":{},\"emit_kinds\":[],\"control_mode\":null,\"forced_input_type\":null,\"inputs\":[{{\"path\":{},\"kind\":\"wave\"}}],\"emit_jobs\":[],\"compile\":{},\"link\":{},\"execute\":{}}}", quote(if run { "build+run" } else { "build" }), quote(&target), quote(emit), quote(input), compile, link, execute);
        return;
    }
    fs::write("compiler-started", list(&args)).unwrap();
    if !check {
        for path in if object { vec![intermediate] } else { vec![intermediate, output.clone()] } {
            let path = PathBuf::from(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"native artifact").unwrap();
        }
        if output.contains(".vex-run") { fs::copy(env::current_exe().unwrap(), output).unwrap(); }
    }
}
