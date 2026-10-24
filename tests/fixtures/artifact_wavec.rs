// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::{env, fs, path::Path, process};
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
fn main() {
    let args: Vec<_> = env::args().collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("wavec 0.2.1-pre-beta");
        return;
    }
    if args.get(1).is_some_and(|arg| arg == "print") {
        if args.get(2).is_some_and(|arg| arg == "supported-targets") {
            println!("[\"x86_64-unknown-linux-gnu\",\"x86_64-pc-windows-msvc\"]");
        } else {
            let triple = args.iter().find_map(|arg| arg.strip_prefix("--target="))
                .map(str::to_owned).unwrap_or_else(|| env::var("PLAN_TARGET").unwrap());
            let (os, format) = if triple.contains("windows") { ("windows", "coff") } else { ("linux", "elf") };
            println!("{{\"triple\":{},\"os\":{},\"object_format\":{},\"hosted\":true,\"supported\":true}}", quote(&triple), quote(os), quote(format));
        }
        return;
    }
    if args.iter().any(|arg| arg == "--dry-run") {
        println!("{}", env::var("PLAN").unwrap());
        return;
    }
    fs::write("compiled", "compiler started").unwrap();
    for path in env::var("OUTPUTS").unwrap_or_default().lines() {
        let path = Path::new(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if env::var_os("DIRECTORY_OUTPUT").is_some() {
            fs::create_dir(path).unwrap();
        } else {
            fs::write(path, b"compiled artifact").unwrap();
        }
    }
    if env::var_os("FAIL_OWNERSHIP").is_some() {
        fs::create_dir(".vex/artifacts.json").unwrap();
    }
    let code = env::var("COMPILE_EXIT").unwrap_or_else(|_| "0".into()).parse().unwrap();
    if code != 0 { eprintln!("fixture compiler diagnostic"); }
    process::exit(code);
}
