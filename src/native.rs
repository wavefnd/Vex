//! Manifest-owned native linking. Foreign sources and build scripts are never executed.
use compiler::Compiler;
use diagnostic::Error;
use manifest::{Manifest, NativeOutput};
use resolver::Resolution;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct NativeBuild {
    pub global: Vec<String>,
    pub inputs: Vec<String>,
    pub output: Vec<String>,
}

pub fn configuration(manifest: &Manifest) -> Value {
    match &manifest.native {
        None => Value::Null,
        Some(config) => json!({
            "output":config.output.map(NativeOutput::as_str),
            "exports":config.exports,
            "libraries":config.libraries.iter().map(|lib| json!({"path":lib.path,"name":lib.name,"target":lib.target})).collect::<Vec<_>>(),
            "search_paths":config.search_paths.iter().map(|path| json!({"path":path.path,"target":path.target})).collect::<Vec<_>>()
        }),
    }
}

/// Validate selected native files while the dependency/project guard is held.
fn native_path(manifest: &Manifest, value: &str, directory: bool) -> Result<String, Error> {
    let base = manifest
        .source_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let root = base.canonicalize().map_err(Error::environment)?;
    let requested = root.join(value);
    let path = requested.canonicalize().map_err(|e| {
        Error::resolution(format!(
            "native input `{value}` in package `{}` is unavailable: {e}",
            manifest.name
        ))
    })?;
    if !path.starts_with(&root) || (directory && !path.is_dir()) || (!directory && !path.is_file())
    {
        return Err(Error::resolution(format!(
            "native input `{value}` must be a {} inside package `{}`",
            if directory {
                "directory"
            } else {
                "regular file"
            },
            manifest.name
        )));
    }
    path_text(&path)
}

fn path_text(path: &Path) -> Result<String, Error> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::resolution("native compiler inputs require UTF-8 paths"))?;
    #[cfg(windows)]
    {
        if let Some(tail) = path.strip_prefix(r"\\?\UNC\") {
            return Ok(format!("//{}", tail.replace('\\', "/")));
        }
        Ok(path
            .strip_prefix(r"\\?\")
            .unwrap_or(path)
            .replace('\\', "/"))
    }
    #[cfg(not(windows))]
    Ok(path.into())
}

fn named_library(
    manifest: &Manifest,
    name: &str,
    search_paths: &[PathBuf],
    object_format: &str,
) -> Result<String, Error> {
    let (candidates, fallback) = match object_format {
        "coff" => {
            let library = if name.to_ascii_lowercase().ends_with(".lib") {
                name.to_owned()
            } else {
                format!("{name}.lib")
            };
            (vec![library.clone()], library)
        }
        "macho" => (
            vec![format!("lib{name}.dylib"), format!("lib{name}.a")],
            format!("-l{name}"),
        ),
        _ => (
            vec![format!("lib{name}.so"), format!("lib{name}.a")],
            format!("-l{name}"),
        ),
    };
    for directory in search_paths {
        for candidate in &candidates {
            let path = directory.join(candidate);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => return native_path(manifest, &path_text(&path)?, false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Error::environment(format!(
                        "cannot inspect native library `{}`: {error}",
                        path.display()
                    )))
                }
            }
        }
    }
    // A package's search paths never influence another package's fallback.
    // The compiler and linker retain their platform system-library discovery.
    Ok(fallback)
}

/// Dependent libraries precede their dependencies, including diamonds. Preserve
/// declaration order within a package because static archive order is significant.
fn manifests(root: &Manifest, resolution: &Resolution) -> Result<Vec<Manifest>, Error> {
    let mut values = BTreeMap::from([(root.name.clone(), root.clone())]);
    for package in resolution.packages() {
        let directory = match &package.source {
            lockfile::LockedSource::Git { resolved, .. }
            | lockfile::LockedSource::Path { resolved, .. } => resolved,
        };
        values.insert(
            package.name.clone(),
            Manifest::load_from(directory.join(manifest::MANIFEST_FILE))?,
        );
    }
    let mut incoming: BTreeMap<_, usize> = values.keys().map(|name| (name.clone(), 0)).collect();
    for manifest in values.values() {
        for dependency in &manifest.dependencies {
            *incoming.get_mut(&dependency.name).ok_or_else(|| {
                Error::resolution("incomplete native dependency graph; run vex fetch")
            })? += 1;
        }
    }
    let mut ready: BTreeSet<_> = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| name.clone())
        .collect();
    let mut result = Vec::new();
    while let Some(name) = ready.pop_first() {
        let manifest = values.remove(&name).unwrap();
        for dependency in &manifest.dependencies {
            let count = incoming.get_mut(&dependency.name).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.insert(dependency.name.clone());
            }
        }
        result.push(manifest);
    }
    if !values.is_empty() {
        return Err(Error::resolution("cycle in native dependency graph"));
    }
    Ok(result)
}

pub fn prepare(
    root: &Manifest,
    resolution: &Resolution,
    compiler: &mut Compiler,
    target: Option<&str>,
    release: bool,
    check: bool,
    generation: Option<&Path>,
) -> Result<Option<NativeBuild>, Error> {
    let manifests = manifests(root, resolution)?;
    if !root.lib && !manifests.iter().any(|m| m.native.is_some()) {
        return Ok(None);
    }
    let spec = compiler.target_spec(target)?;
    let triple = spec["triple"].as_str().unwrap();
    let os = spec["os"].as_str().unwrap();
    if !matches!(
        spec["object_format"].as_str(),
        Some("elf" | "macho" | "coff")
    ) {
        return Err(Error::resolution(
            "native C ABI artifacts require an ELF, Mach-O, or COFF target",
        ));
    }
    let kind = if root.lib {
        Some(
            root.native
                .as_ref()
                .and_then(|n| n.output)
                .unwrap_or(NativeOutput::Object),
        )
    } else {
        None
    };
    if kind == Some(NativeOutput::Shared) && spec["hosted"] != true {
        return Err(Error::resolution(
            "shared libraries require a hosted target",
        ));
    }
    let mut build = NativeBuild {
        global: vec![format!("--target={triple}")],
        inputs: Vec::new(),
        output: Vec::new(),
    };
    for manifest in &manifests {
        let Some(native) = &manifest.native else {
            continue;
        };
        let mut search_paths = Vec::new();
        for search in &native.search_paths {
            if search.target.as_deref().is_some_and(|t| t != triple) {
                continue;
            }
            search_paths.push(PathBuf::from(native_path(manifest, &search.path, true)?));
        }
        for library in &native.libraries {
            if library.target.as_deref().is_some_and(|t| t != triple) {
                continue;
            }
            let argument = if let Some(path) = &library.path {
                native_path(manifest, path, false)?
            } else {
                named_library(
                    manifest,
                    library.name.as_deref().unwrap(),
                    &search_paths,
                    spec["object_format"].as_str().unwrap(),
                )?
            };
            if !check && kind != Some(NativeOutput::Object) {
                // Keep paths and names in one sequence after Wave's objects.
                // Repeated archives can be necessary, including across packages.
                build.global.push(format!("-Clink-arg={argument}"));
            }
        }
    }
    if check {
        return Ok(Some(build));
    }
    let directory = generation.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from("target")
            .join(triple)
            .join(if release { "release" } else { "dev" })
    });
    let filename = match kind {
        Some(NativeOutput::Object) => format!(
            "{}.{}",
            root.name,
            if os == "windows" { "obj" } else { "o" }
        ),
        Some(NativeOutput::Shared) => match os {
            "windows" => format!("{}.dll", root.name),
            "macos" => format!("lib{}.dylib", root.name),
            _ => format!("lib{}.so", root.name),
        },
        None => format!("{}{}", root.name, if os == "windows" { ".exe" } else { "" }),
    };
    if os == "windows" && windows_device_name(&filename) {
        return Err(Error::resolution(format!(
            "package `{}` produces reserved Windows output `{filename}`; choose a package name that is not a Windows device name",
            root.name
        )));
    }
    build
        .output
        .extend(["-o".into(), path_text(&directory.join(filename))?]);
    match kind {
        Some(NativeOutput::Object) => build.output.push("--emit=obj".into()),
        Some(NativeOutput::Shared) => {
            build.output.push("--shared".into());
            build
                .global
                .extend(["-C".into(), "relocation-model=pic".into()]);
            // wavec links through temporary files. Give the final library a
            // stable loader identity instead of embedding that temporary path.
            if os == "windows" {
                let exports = &root.native.as_ref().unwrap().exports;
                if exports.is_empty() {
                    return Err(Error::resolution(
                        "Windows shared libraries require a nonempty `native.exports` list of C symbols declared with `export(c)`",
                    ));
                }
                build.global.extend(
                    exports
                        .iter()
                        .map(|name| format!("-Clink-arg=/EXPORT:{name}")),
                );
            } else if os == "macos" {
                build.global.extend([
                    "-Clink-arg=-install_name".into(),
                    format!("-Clink-arg=@rpath/lib{}.dylib", root.name),
                ]);
            } else if spec["object_format"] == "elf" {
                build.global.extend([
                    "-Clink-arg=-soname".into(),
                    format!("-Clink-arg=lib{}.so", root.name),
                ]);
            }
        }
        None => {}
    }
    if kind != Some(NativeOutput::Object) {
        build.output.push(format!(
            "--target-dir={}",
            path_text(&directory.join(".vex-obj").join(&root.name))?
        ));
    }
    Ok(Some(build))
}

fn windows_device_name(filename: &str) -> bool {
    let stem = filename.split('.').next().unwrap().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].into_iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
        })
}
