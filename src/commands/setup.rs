// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;

pub fn setup(args: &[String]) -> Result<(), Error> {
    const USAGE: &str = "usage: vex setup wavec [--version <version>] [--script-fallback]";
    if matches!(args, [help] if help == "-h" || help == "--help")
        || matches!(args, [wavec, help] if wavec == "wavec" && (help == "-h" || help == "--help"))
    {
        diagnostic::outln!("{USAGE}");
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("wavec") {
        return Err(Error::usage(USAGE));
    }
    let mut version: Option<&str> = None;
    let mut script_fallback = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--script-fallback" if !script_fallback => {
                script_fallback = true;
                index += 1;
            }
            "--version" => {
                if version.is_some() {
                    return Err(Error::usage("`--version` may only be specified once"));
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| Error::usage("missing value for --version"))?;
                if value.is_empty() || value.starts_with('-') {
                    return Err(Error::usage(
                        "`--version` must be a version value, not an option",
                    ));
                }
                toolchain::validate_version(value).map_err(Error::usage)?;
                version = Some(value.as_str());
                index += 2;
            }
            unknown => {
                return Err(Error::usage(format!(
                    "unknown setup option `{unknown}`\n{USAGE}"
                )))
            }
        }
    }
    diagnostic::outln!("installing wavec {}", toolchain::requested_version(version));
    match toolchain::install_wavec(version) {
        Ok(binary) => diagnostic::outln!("wavec installed at {}", binary.display()),
        Err(error) if script_fallback && !process::cancelled() => {
            crate::ui::error(format!("artifact installation failed: {error}\nusing explicitly requested official script fallback"));
            toolchain::install_wavec_script(version).map_err(Error::environment)?;
        }
        Err(error) => return Err(Error::environment(error)),
    }
    Ok(())
}
