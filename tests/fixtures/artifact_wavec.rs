// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::{env, fs, path::Path, process};
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
fn output_prefix(actual: &str, suffix: &Path) -> Option<std::path::PathBuf> {
    let path = Path::new(actual);
    if !path.ends_with(suffix) { return None; }
    let mut prefix = path.to_path_buf();
    for _ in suffix.components() { prefix.pop(); }
    Some(prefix)
}
fn main() {
    let args: Vec<_> = env::args().collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("wavec 0.2.1-pre-beta");
        return;
    }
    if let Some(at)=args.iter().position(|arg|arg=="print") {
        if args.get(at+1).is_some_and(|arg|arg=="std-path") {
            if let Some(root)=args.iter().find_map(|a|a.strip_prefix("--std-root=")) {println!("{}",quote(root));}else{println!("null");}
        } else if args.get(at+1).is_some_and(|arg|arg=="vex-capabilities") {
            if env::var_os("MODERN_CONTRACT").is_some(){println!("{{\"schema_version\":1,\"package_graph\":1,\"path_remap\":1,\"source_inputs\":1}}");}else{process::exit(2);}
        } else if args.get(at+1).is_some_and(|arg| arg == "supported-targets") {
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
    let outputs=env::var("OUTPUTS").unwrap_or_default();
    let actual_output=args.windows(2).find(|pair|pair[0]=="-o").map(|pair|pair[1].as_str());
    let actual_directory=args.iter().find_map(|arg|arg.strip_prefix("--target-dir="));
    let mut prefix=None;
    for output in outputs.lines() {
        if let Some(actual)=actual_output.and_then(|path|output_prefix(path, Path::new(output))) {prefix=Some(actual);break;}
        if let Some(parent)=Path::new(output).parent() {
            if let Some(actual)=actual_directory.and_then(|path|output_prefix(path,parent)) {prefix=Some(actual);break;}
        }
    }
    for original in outputs.lines() {
        let mapped=prefix.as_ref().map(|prefix|prefix.join(original)).unwrap_or_else(||Path::new(original).to_path_buf());
        let path = mapped.as_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if env::var_os("DIRECTORY_OUTPUT").is_some() {
            fs::create_dir(path).unwrap();
        } else {
            fs::write(path, env::var("ARTIFACT_BYTES").unwrap_or_else(|_|"compiled artifact".into())).unwrap();
        }
    }
    if let Some(marker) = env::var_os("PAUSE_AFTER_OUTPUT") {
        fs::write(marker, "outputs staged").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    if env::var_os("FAIL_OWNERSHIP").is_some() {
        let _=fs::remove_file(".vex/artifacts.json");
        fs::create_dir(".vex/artifacts.json").unwrap();
    }
    let code = env::var("COMPILE_EXIT").unwrap_or_else(|_| "0".into()).parse().unwrap();
    if code != 0 { eprintln!("fixture compiler diagnostic"); }
    process::exit(code);
}
