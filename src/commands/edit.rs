// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use crate::{project::Selection, ui};
use diagnostic::Error;
use std::collections::BTreeMap;

pub fn edit(remove: bool, args: &[String], selection: &Selection) -> Result<(), Error> {
    let name = args
        .first()
        .filter(|s| !s.starts_with('-'))
        .ok_or_else(|| Error::usage("expected a direct dependency name"))?;
    manifest::validate_package_name(name).map_err(Error::usage)?;
    let mut options = BTreeMap::new();
    let mut offline = false;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--offline" && !offline {
            offline = true;
            i += 1;
            continue;
        }
        let key = args[i]
            .strip_prefix("--")
            .filter(|k| {
                matches!(
                    *k,
                    "git"
                        | "path"
                        | "branch"
                        | "tag"
                        | "rev"
                        | "version"
                        | "version-req"
                        | "package-name"
                        | "subdir"
                )
            })
            .ok_or_else(|| Error::usage(format!("unknown dependency option `{}`", args[i])))?;
        let value = args
            .get(i + 1)
            .filter(|s| !s.is_empty() && !s.starts_with('-'))
            .ok_or_else(|| Error::usage(format!("missing value for --{key}")))?;
        if options.insert(key, value).is_some() {
            return Err(Error::usage(format!("duplicate --{key}")));
        }
        i += 2;
    }
    if remove && !options.is_empty() {
        return Err(Error::usage(
            "remove accepts only a direct dependency name and --offline",
        ));
    }
    if !remove && (options.contains_key("git") == options.contains_key("path")) {
        return Err(Error::usage("add requires exactly one of --git or --path"));
    }
    let project = selection.enter()?;
    if project.selected.len() != 1 {
        return Err(Error::usage(
            "select one package with -p for dependency editing",
        ));
    }
    resolver::edit_manifest(
        &project.primary(),
        offline,
        |m, raw| {
            let addition = if remove {
                None
            } else {
                let quote = |value: &str| -> Result<String, Error> {
                    if m.format >= 2 {
                        Ok(wson::quote(value))
                    } else if value.contains(['"', '\n', '\r']) || value.ends_with('\\') {
                        Err(Error::usage("value cannot be represented without changing legacy string semantics; migrate the manifest explicitly"))
                    } else {
                        Ok(format!("\"{value}\""))
                    }
                };
                let mut fields = vec![format!("name = {}", quote(name)?)];
                for (key, value) in &options {
                    fields.push(format!(
                        "{} = {}",
                        if *key == "package-name" {
                            "package".to_owned()
                        } else {
                            key.replace('-', "_")
                        },
                        quote(value)?
                    ));
                }
                Some(format!("{{ {} }}", fields.join(", ")))
            };
            wson::edit::dependency(raw, name, addition.as_deref()).map_err(Error::resolution)
        },
        ui::status,
    )?;
    ui::status(if remove { "Removed" } else { "Added" }, name);
    Ok(())
}
