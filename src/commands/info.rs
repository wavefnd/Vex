// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;
use manifest::{DependencySource, Manifest, TargetKind};

pub fn info(args: &[String], selection: &crate::project::Selection) -> Result<(), Error> {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        diagnostic::outln!("usage: vex info");
        return Ok(());
    }
    if let Some(argument) = args.first() {
        return Err(Error::usage(format!(
            "unexpected argument `{argument}`\nusage: vex info"
        )));
    }
    let project = selection.enter()?;
    if project.workspace {
        diagnostic::outln!("workspace: {}", project.owner.source_path.display());
        diagnostic::outln!(
            "members: {}",
            project
                .members
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for manifest in &project.selected {
        run_info(manifest)?;
    }
    Ok(())
}

fn run_info(manifest: &Manifest) -> Result<(), Error> {
    diagnostic::outln!("Vex project info");
    diagnostic::outln!("name: {}", manifest.name);
    diagnostic::outln!("version: {}", manifest.version);
    if manifest.targets.len() == 1 {
        diagnostic::outln!("type: {}", kind_name(manifest.targets[0].kind));
    } else {
        diagnostic::outln!("type: multiple targets");
        for target in &manifest.targets {
            diagnostic::outln!(
                "  {} {} {}",
                kind_name(target.kind),
                target.name,
                target.path.display()
            );
        }
    }
    if let Some(default_run) = &manifest.default_run {
        diagnostic::outln!("default_run: {default_run}");
    }
    diagnostic::outln!("manifest: {}", manifest.source_path.display());
    if let Some(description) = manifest.description.as_ref() {
        diagnostic::outln!("description: {description}");
    }
    if let Some(author) = manifest.author.as_ref() {
        diagnostic::outln!("author: {author}");
    }
    if let Some(license) = manifest.license.as_ref() {
        diagnostic::outln!("license: {license}");
    }
    diagnostic::outln!("dependencies: {}", manifest.dependencies.len());

    for dep in manifest.dependencies.clone() {
        let alias = if dep.name != dep.package_name() {
            format!("{} => {}", dep.name, dep.package_name())
        } else {
            dep.name.clone()
        };
        let label = dep
            .version_req
            .as_ref()
            .map_or_else(|| alias.clone(), |req| format!("{alias} requires {req}"));
        match dep.source {
            DependencySource::Path { path } => match dep.version {
                Some(version) => diagnostic::outln!("  {} {} path {}", label, version, path),
                None => diagnostic::outln!("  {} path {}", label, path),
            },
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => {
                let reference = branch
                    .map(|value| format!(" branch {value}"))
                    .or_else(|| tag.map(|value| format!(" tag {value}")))
                    .or_else(|| rev.map(|value| format!(" rev {value}")))
                    .unwrap_or_default();
                match dep.version {
                    Some(version) => {
                        diagnostic::outln!(
                            "  {} {} git {}{}",
                            label,
                            version,
                            source::identity(&url),
                            reference
                        )
                    }
                    None => diagnostic::outln!(
                        "  {} git {}{}",
                        label,
                        source::identity(&url),
                        reference
                    ),
                }
            }
        }
    }
    Ok(())
}

fn kind_name(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Library => "library",
        TargetKind::Binary => "binary",
    }
}
