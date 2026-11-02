// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use crate::{project::Selection, ui};
use diagnostic::Error;

pub fn clean(args: &[String], selection: &Selection) -> Result<(), Error> {
    const USAGE: &str = "usage: vex clean [--dry-run]";
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        diagnostic::outln!("{USAGE}");
        return Ok(());
    }
    let dry_run = match args {
        [] => false,
        [arg] if arg == "--dry-run" => true,
        _ => return Err(Error::usage(USAGE)),
    };
    let _project = selection.enter()?;
    let _manifest = _project.primary();
    let source_root = std::env::current_dir().map_err(Error::environment)?;
    let root = state::layout::build_root(&source_root);
    let dependencies = state::layout::dependency(&source_root, ".vex");
    for path in [
        dependencies.join("init.json"),
        dependencies.join("transaction.json"),
        root.join(".vex/build-publication.json"),
        root.join(".vex/report-publication.json"),
    ] {
        state::reject_link(&path).map_err(Error::environment)?;
        if path.exists() {
            return Err(Error::resolution("recovery pending; clean preserves transaction files; run a normal fetch/build first"));
        }
    }
    if !state::artifacts::has_records(&root).map_err(Error::environment)? {
        ui::status(
            "Finished",
            "no tracked build artifacts; existing untracked files were preserved",
        );
        return Ok(());
    }
    let guard = if dry_run {
        state::Guard::acquire_existing(ui::status)
    } else {
        state::Guard::acquire_existing_exclusive(ui::status)
    }
    .map_err(Error::environment)?;
    let _build_guard = if !dry_run {
        Some(
            state::FileGuard::exclusive(&root.join(".vex/build.lock"), || {
                ui::status("Waiting", "build directory")
            })
            .map_err(Error::environment)?,
        )
    } else {
        None
    };
    let mut output_error = None;
    state::artifacts::clean_at(&guard, &root, !dry_run, |action, path| {
        // Preview is command output, not progress, and remains visible with -q.
        if dry_run {
            if let Err(error) =
                diagnostic::output::stdout(format_args!("{action}: {}\n", source::redact(&path)))
            {
                output_error.get_or_insert(error);
            }
        } else {
            ui::status(action, path);
        }
    })
    .map_err(Error::environment)?;
    if let Some(error) = output_error {
        return Err(error);
    }
    ui::status(
        "Finished",
        if dry_run {
            "clean preview; no files changed"
        } else {
            "tracked build artifacts cleaned; untracked files preserved; use build/run to regenerate"
        },
    );
    Ok(())
}
