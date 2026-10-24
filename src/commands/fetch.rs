// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use std::collections::BTreeSet;
use std::time::Instant;

use crate::ui;
use manifest::Manifest;
use resolver::{resolve, ResolveOptions, UpdatePolicy};

#[derive(Debug, Default)]
struct DependencyOptions {
    locked: bool,
    offline: bool,
    packages: BTreeSet<String>,
}

pub fn fetch(args: &[String], selection: &crate::project::Selection) -> Result<(), Error> {
    dependency_command(false, args, selection)
}
pub(super) fn dependency_command(
    update: bool,
    args: &[String],
    selection: &crate::project::Selection,
) -> Result<(), Error> {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        diagnostic::outln!(
            "usage: vex {}",
            if update {
                "update [<package>...]"
            } else {
                "fetch [--locked] [--offline]"
            }
        );
        return Ok(());
    }
    let options = parse_options(update, args).map_err(Error::usage)?;
    if update && (options.locked || options.offline) {
        let option = if options.locked {
            "--locked"
        } else {
            "--offline"
        };
        return Err(Error::usage(format!("`{option}` cannot be used with `vex update` because update refreshes Git refs and rewrites vex.lock")));
    }
    let _project = selection.enter()?;
    run_fetch(update, options, selection.graph.clone(), _project.primary())
}

fn run_fetch(
    update: bool,
    options: DependencyOptions,
    graph: source::graph::Selection,
    manifest: Manifest,
) -> Result<(), Error> {
    let started = Instant::now();
    let update_policy = if update {
        if options.packages.is_empty() {
            ui::status("Updating", "all Git dependencies");
            UpdatePolicy::UpdateAll
        } else {
            ui::status(
                "Updating",
                format!(
                    "Git package{} {}",
                    if options.packages.len() == 1 { "" } else { "s" },
                    options
                        .packages
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            UpdatePolicy::UpdateSelected(options.packages)
        }
    } else {
        UpdatePolicy::ReuseLocked
    };
    let resolution = resolve(
        &manifest,
        ResolveOptions {
            requests: Vec::new(),
            update_bindings: None,
            force_bindings: false,
            additional_contexts: Vec::new(),
            selection: graph,
            dry_run: false,
            update: update_policy,
            locked: options.locked,
            offline: options.offline,
        },
        ui::status,
    )?;
    ui::status(
        "Finished",
        format!(
            "resolved {} package{} in {:.2}s",
            resolution.package_count(),
            if resolution.package_count() == 1 {
                ""
            } else {
                "s"
            },
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}

fn parse_options(update: bool, args: &[String]) -> Result<DependencyOptions, String> {
    let mut options = DependencyOptions::default();
    for argument in args {
        match argument.as_str() {
            "--locked" => options.locked = true,
            "--offline" => options.offline = true,
            "-h" | "--help" => {
                return Err(if update {
                    "usage: vex update [<package>...]".to_string()
                } else {
                    "usage: vex fetch [--locked] [--offline]".to_string()
                });
            }
            _ if update && !argument.starts_with('-') => {
                options.packages.insert(argument.clone());
            }
            _ => {
                return Err(format!(
                    "unknown Vex option `{argument}`\nusage: vex {}",
                    if update {
                        "update [<package>...]"
                    } else {
                        "fetch [--locked] [--offline]"
                    }
                ));
            }
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_fetch_policy_options() {
        let options = parse_options(false, &strings(&["--locked", "--offline"]))
            .expect("fetch policy options must parse");
        assert!(options.locked);
        assert!(options.offline);
    }

    #[test]
    fn parses_and_deduplicates_targeted_update_packages() {
        let options = parse_options(true, &strings(&["beta", "alpha", "beta"]))
            .expect("targeted update packages must parse");
        assert_eq!(
            options.packages.into_iter().collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }
}
