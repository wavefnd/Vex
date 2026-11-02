// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use std::path::PathBuf;
pub fn toolchain(args: &[String], selection: &crate::project::Selection) -> Result<(), Error> {
    const USAGE: &str = "usage: vex toolchain capture [--wavec <path>] [--std <path>] [--replace]";
    if matches!(args,[arg]if arg=="--help"||arg=="-h") {
        diagnostic::outln!("{USAGE}");
        return Ok(());
    }
    if args.first().is_none_or(|a| a != "capture") {
        return Err(Error::usage(USAGE));
    }
    let invocation = std::env::current_dir().map_err(Error::environment)?;
    let mut wavec = None;
    let mut standard = None;
    let mut replace = false;
    let mut at = 1;
    while at < args.len() {
        match args[at].as_str() {
            "--wavec" | "--std" => {
                let slot = if args[at] == "--wavec" {
                    &mut wavec
                } else {
                    &mut standard
                };
                if slot.is_some() {
                    return Err(Error::usage("duplicate toolchain path"));
                }
                at += 1;
                let value = args
                    .get(at)
                    .filter(|v| !v.is_empty() && !v.starts_with('-'))
                    .ok_or_else(|| Error::usage("missing toolchain path"))?;
                *slot = Some(invocation.join(PathBuf::from(value)));
            }
            "--replace" if !replace => replace = true,
            _ => return Err(Error::usage(USAGE)),
        }
        at += 1;
    }
    let mut compiler = compiler::Compiler::select_explicit(wavec.as_deref())?;
    if let Some(root) = standard {
        compiler.set_standard(&root)?;
    }
    let project = selection.enter()?;
    compiler.validate_version(project.owner.compiler.as_deref())?;
    let root = project.owner.source_path.parent().unwrap();
    compiler.capture_identity(root, replace)?;
    crate::ui::status(
        "Pinned",
        format!("compiler/std identity for {}", compiler::identity::host()),
    );
    Ok(())
}
