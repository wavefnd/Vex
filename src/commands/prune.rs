// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;

const USAGE: &str = "usage: vex prune [--apply] [--dry-run]";

pub fn prune(args: &[String], selection: &crate::project::Selection) -> Result<(), Error> {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        diagnostic::outln!("{USAGE}");
        diagnostic::outln!("  Preview unused, unchanged checkouts created by Vex");
        diagnostic::outln!("  --apply    Delete only the verified unused checkouts");
        diagnostic::outln!(
            "  --dry-run  Preview without changing project state (also overrides --apply)"
        );
        return Ok(());
    }
    let mut apply = false;
    let mut dry_run = false;
    for argument in args {
        match argument.as_str() {
            "--apply" => apply = true,
            "--dry-run" => dry_run = true,
            _ => {
                return Err(Error::usage(format!(
                    "unknown Vex option `{argument}`\n{USAGE}"
                )))
            }
        }
    }
    let _project = selection.enter()?;
    let manifest = _project.primary();
    let applied = apply && !dry_run;
    let report = resolver::prune(&manifest, applied, crate::ui::status)?;
    for path in &report.removable {
        diagnostic::outln!(
            "{} {}",
            if applied { "Removed" } else { "Would remove" },
            source::redact(&path.display().to_string())
        );
    }
    for entry in &report.preserved {
        diagnostic::outln!(
            "Preserved {}: {}",
            source::redact(&entry.path.display().to_string()),
            source::redact(&entry.reason)
        );
    }
    diagnostic::outln!(
        "{} {} unused checkout(s); preserved {}",
        if applied { "Removed" } else { "Would remove" },
        if applied {
            report.removed
        } else {
            report.removable.len()
        },
        report.preserved.len()
    );
    if !applied && !report.removable.is_empty() {
        diagnostic::outln!("Run `vex prune --apply` to remove these checkouts.");
    }
    if report.removed > 0 {
        diagnostic::outln!(
            "Restoring a removed dependency requires `vex fetch`; network access may be needed if its exact commit is not cached."
        );
    }
    Ok(())
}
