use diagnostic::Error;
use lockfile::LockedSource;
use manifest::Manifest;
use serde_json::{json, Value};
use std::path::Path;

pub fn metadata(args: &[String], selection: &crate::project::Selection) -> Result<(), Error> {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        diagnostic::outln!("usage: vex metadata [--format=json] [--locked] [--offline]");
        return Ok(());
    }
    let mut locked = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--locked" => locked = true,
            "--offline" | "--format=json" => {}
            "--format" if args.next().is_some_and(|s| s == "json") => {}
            _ => {
                return Err(Error::usage(format!(
                    "unsupported metadata option `{arg}`; only JSON format is supported"
                )))
            }
        }
    }
    let project = selection.enter()?;
    let root = project.manifest_path.parent().unwrap();
    let root_text = json_path(root)?;
    let manifest = Manifest::load()?;
    let resolution = resolver::inspect(&manifest, locked, crate::ui::status)?;
    let mut root_dependencies: Vec<_> = manifest
        .dependencies
        .iter()
        .map(|d| d.name.clone())
        .collect();
    root_dependencies.sort();
    let mut packages = Vec::new();
    for package in resolution.packages() {
        let (resolved, source) = match &package.source {
            LockedSource::Path {
                requested,
                resolved,
            } => (resolved, json!({"kind":"path", "requested":requested})),
            LockedSource::Git {
                url,
                branch,
                tag,
                rev,
                commit,
                resolved,
            } => (
                resolved,
                json!({"kind":"git", "url":source::identity(url), "branch":branch,"tag":tag,"rev":rev,"commit":commit}),
            ),
        };
        let directory = resolved.canonicalize().map_err(Error::environment)?;
        let package_manifest_path = directory.join(manifest::MANIFEST_FILE);
        let package_manifest = Manifest::load_from(&package_manifest_path)?;
        let mut dependencies = package.dependencies.clone();
        dependencies.sort();
        packages.push(json!({"name":package.name, "version":package.version, "manifest_format":package_manifest.format, "compiler":package_manifest.compiler,
            "source":source, "root":json_path(&directory)?, "manifest_path":json_path(&package_manifest_path)?,
            "entry_path":json_path(&directory.join(package_manifest.default_entry_path()))?,
            "kind":if package_manifest.lib {"library"} else {"binary"}, "dependencies":dependencies}));
    }
    packages.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let value: Value = json!({"schema_version":1,
        "root":{"name":manifest.name,"version":manifest.version,"manifest_format":manifest.format,"compiler":manifest.compiler,"kind":if manifest.lib {"library"} else {"binary"},
            "root":root_text,"manifest_path":json_path(&project.manifest_path)?,
            "entry_path":json_path(&root.join(manifest.default_entry_path()))?,"dependencies":root_dependencies},
        "target_directory":json_path(&root.join("target"))?, "packages":packages});
    diagnostic::outln!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(Error::internal)?
    );
    Ok(())
}

fn json_path(path: &Path) -> Result<&str, Error> {
    path.to_str()
        .ok_or_else(|| Error::resolution("metadata JSON cannot represent a non-UTF-8 path"))
}
