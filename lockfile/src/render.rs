// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use crate::{LockedPackage, LockedSource, Lockfile};

pub fn render_lockfile(lockfile: Lockfile) -> String {
    // Writing uses escaped strings. Read-only locked operations never call this.
    let version = lockfile.version.max(3);
    let roots = if lockfile.roots.is_empty() {
        String::new()
    } else {
        format!("    roots = {},\n", graph_json(&lockfile.roots))
    };
    let blocks = lockfile
        .packages
        .iter()
        .map(render_package)
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        return format!("{{\n    version = {version},\n{roots}    package = []\n}}\n");
    }
    format!(
        "{{\n    version = {version},\n{roots}    package = [\n{}\n    ]\n}}\n",
        blocks.join(",\n")
    )
}

fn render_package(package: &LockedPackage) -> String {
    let mut fields = vec![
        render_field("name", &package.name),
        render_field("version", &package.version),
    ];
    if let Some(name) = &package.package {
        fields.push(render_field("package", name));
    }
    if let Some(id) = &package.id {
        fields.push(render_field("id", id));
    }
    if let Some(alias) = &package.import_name {
        fields.push(render_field("import_name", alias));
    }
    if !package.edges.is_empty() {
        fields.push(format!(
            "            edges = {}",
            graph_json(&package.edges)
        ));
    }
    if !package.features.is_empty() {
        fields.push(format!(
            "            features = {}",
            graph_json(&package.features)
        ));
    }
    match &package.source {
        LockedSource::Path {
            requested,
            resolved,
        } => {
            fields.push(render_field("source", "path"));
            fields.push(render_field("path", requested));
            fields.push(render_field("resolved", &portable_path(resolved)));
        }
        LockedSource::Git {
            url,
            branch,
            tag,
            rev,
            commit,
            resolved,
        } => {
            fields.push(render_field("source", "git"));
            fields.push(render_field("git", &source::identity(url)));
            if let Some(branch) = branch {
                fields.push(render_field("branch", branch));
            }
            if let Some(tag) = tag {
                fields.push(render_field("tag", tag));
            }
            if let Some(rev) = rev {
                fields.push(render_field("rev", rev));
            }
            fields.push(render_field("commit", commit));
            fields.push(render_field("resolved", &portable_path(resolved)));
        }
    }
    if let Some(subdir) = &package.subdir {
        fields.push(render_field("subdir", subdir));
    }
    let dependencies = package
        .dependencies
        .iter()
        .map(|name| format!("\"{}\"", escape(name)))
        .collect::<Vec<_>>()
        .join(", ");
    fields.push(format!("            dependencies = [{dependencies}]"));
    format!("        {{\n{}\n        }}", fields.join(",\n"))
}

fn render_field(key: &str, value: &str) -> String {
    format!("            {key} = \"{}\"", escape(value))
}

fn escape(value: &str) -> String {
    let quoted = wson::quote(value);
    quoted[1..quoted.len() - 1].to_owned()
}

fn portable_path(path: &std::path::Path) -> String {
    #[cfg(windows)]
    {
        path.to_string_lossy().replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

fn graph_json(value: &impl serde::Serialize) -> String {
    wson::render_json(&serde_json::to_value(value).expect("graph is serializable"))
        .expect("validated graph field names")
}
