use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::{Dependency, DependencySource, Manifest};

pub const LOCKFILE_NAME: &str = "vex.lock";

pub fn write_lockfile(manifest: &Manifest) -> Result<(), String> {
    let mut dependencies = manifest.dependencies.clone();
    dependencies.sort_by(|a, b| a.name.cmp(&b.name));

    let package_blocks = dependencies
        .iter()
        .map(render_dependency_entry)
        .collect::<Vec<_>>();

    let content = if package_blocks.is_empty() {
        "{\n    version = 1,\n    package = []\n}\n".to_string()
    } else {
        format!(
            "{{\n    version = 1,\n    package = [\n{}\n    ]\n}}\n",
            package_blocks.join(",\n")
        )
    };

    fs::write(LOCKFILE_NAME, content).map_err(|e| format!("failed to write `{LOCKFILE_NAME}`: {e}"))
}

fn render_dependency_entry(dependency: &Dependency) -> String {
    let mut fields = vec![format!(
        "            name = \"{}\"",
        escape_wson_string(&dependency.name)
    )];

    match &dependency.source {
        DependencySource::Path { path } => {
            let resolved = resolve_path_dependency(path);
            fields.push("            source = \"path\"".to_string());
            fields.push(format!(
                "            path = \"{}\"",
                escape_wson_string(path)
            ));
            fields.push(format!(
                "            resolved = \"{}\"",
                escape_wson_string(&resolved.to_string_lossy())
            ));
        }
        DependencySource::Git {
            url,
            branch,
            tag,
            rev,
        } => {
            let resolved = Path::new(".vex/deps").join(&dependency.name);
            fields.push("            source = \"git\"".to_string());
            fields.push(format!("            git = \"{}\"", escape_wson_string(url)));
            if let Some(branch) = branch {
                fields.push(format!(
                    "            branch = \"{}\"",
                    escape_wson_string(branch)
                ));
            }
            if let Some(tag) = tag {
                fields.push(format!("            tag = \"{}\"", escape_wson_string(tag)));
            }
            if let Some(rev) = rev {
                fields.push(format!("            rev = \"{}\"", escape_wson_string(rev)));
            }
            fields.push(format!(
                "            resolved = \"{}\"",
                escape_wson_string(&resolved.to_string_lossy())
            ));
        }
    }

    if let Some(version) = dependency.version.as_ref() {
        fields.push(format!(
            "            version = \"{}\"",
            escape_wson_string(version)
        ));
    }

    format!("        {{\n{}\n        }}", fields.join(",\n"))
}

fn resolve_path_dependency(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    let absolute_candidate = if candidate.is_absolute() {
        candidate
    } else {
        Path::new(".").join(candidate)
    };

    absolute_candidate
        .canonicalize()
        .unwrap_or(absolute_candidate)
}

fn escape_wson_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
