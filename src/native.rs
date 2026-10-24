// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Manifest-owned native linking. Foreign sources and build scripts are never executed.
use compiler::Compiler;
use diagnostic::Error;
use manifest::{Manifest, NativeOutput, PackageTarget, PlatformMode, Startup, TargetKind};
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
    let mut values = BTreeMap::from([(
        root.name.clone(),
        (
            root.clone(),
            resolution.root_dependencies().to_vec(),
            format!("root:{}", root.source_path.display()),
        ),
    )]);
    for package in resolution.packages() {
        values.insert(
            package.name.clone(),
            (
                Manifest::load_from(package.directory().join(manifest::MANIFEST_FILE))?,
                package.dependencies.clone(),
                package.id.clone().unwrap_or_else(|| package.name.clone()),
            ),
        );
    }
    let mut incoming: BTreeMap<_, usize> = values.keys().map(|name| (name.clone(), 0)).collect();
    for (_, dependencies, _) in values.values() {
        for dependency in dependencies {
            *incoming.get_mut(dependency).ok_or_else(|| {
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
    let mut seen = BTreeSet::new();
    while let Some(name) = ready.pop_first() {
        let (manifest, dependencies, identity) = values.remove(&name).unwrap();
        for dependency in dependencies {
            let count = incoming.get_mut(&dependency).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.insert(dependency);
            }
        }
        if seen.insert(identity) {
            result.push(manifest);
        }
    }
    if !values.is_empty() {
        return Err(Error::resolution("cycle in native dependency graph"));
    }
    Ok(result)
}

/// Prepare one package-owned artifact without creating its output directories.
/// Dependency libraries remain source imports; only explicitly declared foreign
/// inputs are forwarded to the linker.
#[allow(clippy::too_many_arguments)]
pub fn prepare_target(
    root: &Manifest,
    package_target: &PackageTarget,
    resolution: &Resolution,
    compiler: &mut Compiler,
    triple: &str,
    profile: &str,
    check: bool,
    generation: Option<&Path>,
) -> Result<NativeBuild, Error> {
    let spec = compiler.target_spec(Some(triple))?;
    let manifests = manifests(root, resolution)?;
    let mut build = prepare_settings(root, package_target, &spec, profile, check, generation)?;
    let output = target_output(package_target);
    let format = spec["object_format"].as_str().unwrap();
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
            if !matches!(format, "elf" | "macho" | "coff") {
                return Err(Error::resolution(
                    "native C ABI inputs require an ELF, Mach-O, or COFF target",
                ));
            }
            let argument = if let Some(path) = &library.path {
                native_path(manifest, path, false)?
            } else {
                named_library(
                    manifest,
                    library.name.as_deref().unwrap(),
                    &search_paths,
                    format,
                )?
            };
            if !check && output != Some(NativeOutput::Object) {
                // Preserve package and declaration order: static archive order
                // and repeated libraries are meaningful to native linkers.
                build.global.push(format!("-Clink-arg={argument}"));
            }
        }
    }
    Ok(build)
}

fn target_output(target: &PackageTarget) -> Option<NativeOutput> {
    (target.kind == TargetKind::Library).then_some(target.output.unwrap_or(NativeOutput::Object))
}

fn prepare_settings(
    root: &Manifest,
    target: &PackageTarget,
    spec: &Value,
    profile: &str,
    check: bool,
    generation: Option<&Path>,
) -> Result<NativeBuild, Error> {
    let triple = spec["triple"].as_str().unwrap();
    let os = spec["os"].as_str().unwrap();
    let format = spec["object_format"].as_str().unwrap();
    let hosted = spec["hosted"]
        .as_bool()
        .ok_or_else(|| Error::compiler("target-spec is missing boolean hosted"))?;
    let output = target_output(target);
    if output.is_some() && !matches!(format, "elf" | "macho" | "coff" | "wasm")
        || output == Some(NativeOutput::Shared) && format == "wasm"
    {
        return Err(Error::resolution(
            "shared libraries require an ELF, Mach-O, or COFF target; object output additionally supports WebAssembly",
        ));
    }
    if output == Some(NativeOutput::Shared) && !hosted {
        return Err(Error::resolution(
            "shared libraries require a hosted target",
        ));
    }
    let filename = artifact_filename(target, os, format);
    if os == "windows" && windows_device_name(&filename) {
        return Err(Error::resolution(format!(
            "target `{}` produces reserved Windows output `{filename}`; choose a target name that is not a Windows device name",
            target.name
        )));
    }
    if output == Some(NativeOutput::Shared) && os == "windows" && target.exports.is_empty() {
        return Err(Error::resolution("Windows shared libraries require a nonempty `native.exports` (legacy) or `targets.lib.exports` list of C symbols declared with `export(c)`"));
    }
    let platform = root.platform_for(target, triple);
    let mode = platform.mode.unwrap_or_else(|| {
        if format == "wasm" {
            if os == "wasi" {
                PlatformMode::Wasi
            } else {
                PlatformMode::BareWasm
            }
        } else if hosted {
            PlatformMode::Hosted
        } else {
            PlatformMode::Freestanding
        }
    });
    match mode {
        PlatformMode::Hosted if !hosted || format == "wasm" => {
            return Err(Error::resolution(
                "hosted mode requires a hosted native target",
            ));
        }
        PlatformMode::Freestanding if format != "elf" => {
            return Err(Error::resolution(
                "freestanding mode requires an ELF target",
            ));
        }
        PlatformMode::Wasi if format != "wasm" || os != "wasi" || !hosted => {
            return Err(Error::resolution("wasi mode requires a hosted WASI target"));
        }
        PlatformMode::BareWasm if format != "wasm" || hosted || os == "wasi" => {
            return Err(Error::resolution(
                "bare_wasm mode requires a bare WebAssembly target",
            ));
        }
        _ => {}
    }
    let link_settings = platform.entry.is_some()
        || platform.linker_script.is_some()
        || platform.startup == Some(Startup::None);
    if target.kind == TargetKind::Library && (link_settings || platform.mode.is_some()) {
        return Err(Error::resolution(
            "binary platform settings cannot be applied to a library target",
        ));
    }
    if matches!(mode, PlatformMode::Hosted | PlatformMode::Wasi) && link_settings {
        return Err(Error::resolution(
            "entry, linker_script, and startup=none require freestanding or bare_wasm mode",
        ));
    }
    if mode == PlatformMode::BareWasm && platform.startup == Some(Startup::None) {
        return Err(Error::resolution("bare_wasm does not support startup=none; omit startup to use the compiler's bare WebAssembly entry behavior"));
    }
    if mode == PlatformMode::BareWasm && platform.linker_script.is_some() {
        return Err(Error::resolution(
            "bare WebAssembly does not support linker_script",
        ));
    }
    let script = platform
        .linker_script
        .as_ref()
        .map(|script| {
            native_path(
                root,
                script
                    .to_str()
                    .ok_or_else(|| Error::resolution("linker_script requires a UTF-8 path"))?,
                false,
            )
        })
        .transpose()?;
    let mut build = NativeBuild {
        global: vec![format!("--target={triple}")],
        inputs: Vec::new(),
        output: Vec::new(),
    };
    if let Some(config) = root
        .target_config
        .iter()
        .find(|config| config.triple == triple)
    {
        let linker = config
            .linker
            .as_deref()
            .map(|value| linker_path(root, value))
            .transpose()?;
        let sysroot = config
            .sysroot
            .as_ref()
            .map(|path| sysroot_path(root, path))
            .transpose()?;
        if sysroot.is_some() && !matches!(format, "elf" | "macho") {
            return Err(Error::resolution("selected compiler cannot apply one sysroot to compilation and linking for this target"));
        }
        if sysroot.is_some() && linker.is_some() && format == "macho" {
            return Err(Error::resolution("selected compiler cannot forward a Mach-O sysroot to a custom linker; use the compiler's default linker"));
        }
        if let Some(sysroot) = sysroot {
            build.global.push(format!("--sysroot={sysroot}"));
            // Bundled ELF and Darwin linkers consume the compiler sysroot
            // directly. An explicit ELF linker needs explicit forwarding.
            if !check && output != Some(NativeOutput::Object) && linker.is_some() {
                build.global.push(format!("-Clink-sysroot={sysroot}"));
            }
        }
        if !check {
            if let Some(linker) = linker {
                build.global.push(format!("-Clinker={linker}"));
            }
        }
    }
    if mode == PlatformMode::Freestanding {
        // This controls source/codegen semantics as well as default libraries,
        // and is meaningful even when only checking sources.
        build.output.push("--freestanding".into());
    }
    if check {
        return Ok(build);
    }
    if target.kind == TargetKind::Binary {
        if let Some(entry) = &platform.entry {
            build.output.push(format!("--entry={entry}"));
        }
        if let Some(script) = script {
            build.output.push(format!("--linker-script={script}"));
        }
        if mode == PlatformMode::Freestanding && platform.startup == Some(Startup::None) {
            build.output.push("--no-start-files".into());
        }
    }
    let directory = generation
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("target"))
        .join(triple)
        .join(profile);
    let directory = if root.scope.is_some() {
        directory.join("packages").join(&root.name)
    } else {
        directory
    };
    let output_directory = directory.join(if target.kind == TargetKind::Library {
        "lib"
    } else {
        "bin"
    });
    build
        .output
        .extend(["-o".into(), path_text(&output_directory.join(filename))?]);
    match output {
        Some(NativeOutput::Object) => build.output.push("--emit=obj".into()),
        Some(NativeOutput::Shared) => {
            build.output.push("--shared".into());
            build
                .global
                .extend(["-C".into(), "relocation-model=pic".into()]);
            if os == "windows" {
                build.global.extend(
                    target
                        .exports
                        .iter()
                        .map(|name| format!("-Clink-arg=/EXPORT:{name}")),
                );
            } else if os == "macos" {
                build.global.extend([
                    "-Clink-arg=-install_name".into(),
                    format!("-Clink-arg=@rpath/lib{}.dylib", target.name),
                ]);
            } else if format == "elf" {
                build.global.extend([
                    "-Clink-arg=-soname".into(),
                    format!("-Clink-arg=lib{}.so", target.name),
                ]);
            }
        }
        None => {}
    }
    if output != Some(NativeOutput::Object) {
        build.output.push(format!(
            "--target-dir={}",
            path_text(
                &directory
                    .join(".vex-obj")
                    .join(if target.kind == TargetKind::Library {
                        "lib"
                    } else {
                        "bin"
                    })
                    .join(&target.name)
                    .join(output.map(NativeOutput::as_str).unwrap_or("binary"))
            )?
        ));
    }
    Ok(build)
}

fn artifact_filename(target: &PackageTarget, os: &str, format: &str) -> String {
    match target_output(target) {
        Some(NativeOutput::Object) => format!(
            "{}.{}",
            target.name,
            if format == "coff" { "obj" } else { "o" }
        ),
        Some(NativeOutput::Shared) => match os {
            "windows" => format!("{}.dll", target.name),
            "macos" => format!("lib{}.dylib", target.name),
            _ => format!("lib{}.so", target.name),
        },
        None => format!(
            "{}{}",
            target.name,
            if format == "wasm" {
                ".wasm"
            } else if os == "windows" {
                ".exe"
            } else {
                ""
            }
        ),
    }
}

fn project_root(manifest: &Manifest) -> Result<PathBuf, Error> {
    manifest
        .source_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(Error::environment)
}

fn sysroot_path(manifest: &Manifest, path: &Path) -> Result<String, Error> {
    let path = project_root(manifest)?
        .join(path)
        .canonicalize()
        .map_err(|error| {
            Error::environment(format!(
                "sysroot `{}` is unavailable: {error}",
                path.display()
            ))
        })?;
    if !path.is_dir() {
        return Err(Error::environment(format!(
            "sysroot `{}` must be a directory",
            path.display()
        )));
    }
    path_text(&path)
}

fn linker_path(manifest: &Manifest, value: &str) -> Result<String, Error> {
    if value.is_empty() || value.starts_with('-') || value.contains(['\0', '\r', '\n']) {
        return Err(Error::resolution(
            "linker must be one executable name or path, without arguments",
        ));
    }
    let path = Path::new(value);
    let root = project_root(manifest)?;
    if path.components().count() > 1 || path.is_absolute() || value.contains('\\') {
        let path = root.join(path);
        if !linker_executable(&path) {
            return Err(Error::environment(format!(
                "linker `{value}` must be an executable regular file"
            )));
        }
        return path_text(&path);
    }
    if value.chars().any(char::is_whitespace) {
        // Relative filenames with spaces must be explicit paths, such as
        // ./my linker, so an argument string cannot become an executable name.
        return Err(Error::resolution(
            "linker must be one executable name or path, without arguments",
        ));
    }
    let filenames = {
        #[cfg(windows)]
        {
            let mut names = vec![PathBuf::from(value)];
            if path.extension().is_none() {
                let extensions =
                    std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
                names.extend(
                    extensions
                        .split(';')
                        .filter(|s| !s.is_empty())
                        .map(|suffix| PathBuf::from(format!("{value}{suffix}"))),
                );
            }
            names
        }
        #[cfg(not(windows))]
        vec![PathBuf::from(value)]
    };
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            for filename in &filenames {
                let candidate = root.join(&directory).join(filename);
                if linker_executable(&candidate) {
                    // Keep the selected executable basename. Linker and rustup-style
                    // multicall tools use argv[0], and Wave classifies ld.lld
                    // by that name rather than its canonical symlink target.
                    return path_text(&candidate);
                }
            }
        }
    }
    Err(Error::environment(format!(
        "could not find executable linker `{value}` in PATH"
    )))
}

fn linker_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
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

#[cfg(test)]
mod tests {
    use super::*;
    use manifest::{PlatformSettings, TargetConfig};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn package(lib: bool) -> Manifest {
        manifest::decode(
            &format!("{{name=\"demo\",version=\"0.1.0\",lib={lib}}}"),
            PathBuf::from("vex.ws"),
        )
        .unwrap()
    }

    fn spec(triple: &str, os: &str, format: &str, hosted: bool) -> Value {
        json!({"triple":triple,"os":os,"object_format":format,"hosted":hosted})
    }

    fn output(build: &NativeBuild) -> &str {
        build
            .output
            .windows(2)
            .find(|args| args[0] == "-o")
            .unwrap()[1]
            .as_str()
    }

    #[test]
    fn binaries_libraries_and_shared_companions_have_separate_paths() {
        let root = package(false);
        let target = &root.targets[0];
        let windows = spec("x86_64-pc-windows-msvc", "windows", "coff", true);
        let binary = prepare_settings(&root, target, &windows, "release", false, None).unwrap();
        assert_eq!(
            output(&binary),
            "target/x86_64-pc-windows-msvc/release/bin/demo.exe"
        );
        assert!(binary.output.iter().any(|arg| arg
            == "--target-dir=target/x86_64-pc-windows-msvc/release/.vex-obj/bin/demo/binary"));
        let mut library = package(true);
        library.targets[0].output = Some(NativeOutput::Shared);
        library.targets[0].exports = vec!["demo_add".into()];
        let shared = prepare_settings(
            &library,
            &library.targets[0],
            &windows,
            "release",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            output(&shared),
            "target/x86_64-pc-windows-msvc/release/lib/demo.dll"
        );
        assert!(shared
            .global
            .contains(&"-Clink-arg=/EXPORT:demo_add".into()));
        assert!(shared
            .output
            .iter()
            .any(|arg| arg.ends_with(".vex-obj/lib/demo/shared")));
        library.targets[0].output = Some(NativeOutput::Object);
        let object =
            prepare_settings(&library, &library.targets[0], &windows, "dev", false, None).unwrap();
        assert_eq!(
            output(&object),
            "target/x86_64-pc-windows-msvc/dev/lib/demo.obj"
        );
        assert!(!object
            .output
            .iter()
            .any(|arg| arg.starts_with("--target-dir=")));
    }

    #[test]
    fn generation_contains_target_profile_and_artifact_identity() {
        let root = package(false);
        let build = prepare_settings(
            &root,
            &root.targets[0],
            &spec("x86_64-unknown-linux-gnu", "linux", "elf", true),
            "fast",
            false,
            Some(Path::new("target/.vex-run/123-4")),
        )
        .unwrap();
        assert_eq!(
            output(&build),
            "target/.vex-run/123-4/x86_64-unknown-linux-gnu/fast/bin/demo"
        );
    }

    #[test]
    fn wasm_modes_are_target_checked_and_use_wasm_extension() {
        let mut root = package(false);
        root.targets[0].platform = PlatformSettings {
            mode: Some(PlatformMode::BareWasm),
            entry: Some("start".into()),
            startup: None,
            linker_script: None,
        };
        let bare = spec("wasm32-unknown-unknown", "unknown", "wasm", false);
        let build = prepare_settings(&root, &root.targets[0], &bare, "dev", false, None).unwrap();
        assert_eq!(
            output(&build),
            "target/wasm32-unknown-unknown/dev/bin/demo.wasm"
        );
        assert!(build.output.contains(&"--entry=start".into()));
        assert!(!build.output.contains(&"--no-start-files".into()));
        root.targets[0].platform.startup = Some(Startup::None);
        let invalid = prepare_settings(&root, &root.targets[0], &bare, "dev", true, None)
            .err()
            .unwrap();
        assert_eq!(invalid.category, diagnostic::Category::Resolution);
        assert!(invalid.contains("startup=none"));
        root.targets[0].platform.startup = None;
        let wasi = spec("wasm32-wasip1", "wasi", "wasm", true);
        assert!(prepare_settings(&root, &root.targets[0], &wasi, "dev", false, None).is_err());
        root.targets[0].platform = PlatformSettings::default();
        assert!(prepare_settings(&root, &root.targets[0], &wasi, "dev", false, None).is_ok());
        root.targets[0].platform.mode = Some(PlatformMode::Hosted);
        assert!(prepare_settings(&root, &root.targets[0], &wasi, "dev", false, None).is_err());
        let library = package(true);
        for wasm in [&wasi, &bare] {
            let object =
                prepare_settings(&library, &library.targets[0], wasm, "dev", false, None).unwrap();
            assert!(output(&object).ends_with("/lib/demo.o"));
            assert!(object.output.contains(&"--emit=obj".into()));
            let mut shared = library.clone();
            shared.targets[0].output = Some(NativeOutput::Shared);
            assert!(
                prepare_settings(&shared, &shared.targets[0], wasm, "dev", false, None).is_err()
            );
        }
    }

    #[test]
    fn check_validates_export_policy_but_does_not_request_artifacts() {
        let mut root = package(true);
        root.targets[0].output = Some(NativeOutput::Shared);
        let windows = spec("x86_64-pc-windows-msvc", "windows", "coff", true);
        assert!(prepare_settings(&root, &root.targets[0], &windows, "dev", true, None).is_err());
        root.targets[0].exports.push("demo_add".into());
        let build = prepare_settings(&root, &root.targets[0], &windows, "dev", true, None).unwrap();
        assert!(build.output.is_empty());
        assert!(!build.global.iter().any(|arg| arg.starts_with("-Clink")));
    }

    #[test]
    fn freestanding_check_omits_link_only_arguments() {
        let mut root = package(false);
        root.targets[0].platform = PlatformSettings {
            mode: Some(PlatformMode::Freestanding),
            entry: Some("_start".into()),
            startup: Some(Startup::None),
            linker_script: None,
        };
        let bare = spec("x86_64-unknown-none-elf", "none", "elf", false);
        let check = prepare_settings(&root, &root.targets[0], &bare, "dev", true, None).unwrap();
        assert_eq!(check.output, vec!["--freestanding"]);
        let build = prepare_settings(&root, &root.targets[0], &bare, "dev", false, None).unwrap();
        assert!(build.output.contains(&"--freestanding".into()));
        assert!(build.output.contains(&"--entry=_start".into()));
        assert!(build.output.contains(&"--no-start-files".into()));
        assert!(prepare_settings(
            &root,
            &root.targets[0],
            &spec("x86_64-pc-windows-msvc", "windows", "coff", true),
            "dev",
            false,
            None
        )
        .is_err());
    }

    #[test]
    fn root_tool_settings_apply_same_external_sdk_to_compile_and_link() {
        static ID: AtomicU64 = AtomicU64::new(0);
        let temporary = std::env::temp_dir().join(format!(
            "vex-native-platform-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        let project = temporary.join("project");
        let sdk = temporary.join("sdk");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&sdk).unwrap();
        let mut root = package(false);
        root.source_path = project.join("vex.ws");
        root.target_config.push(TargetConfig {
            triple: "aarch64-unknown-linux-gnu".into(),
            sysroot: Some(PathBuf::from("../sdk")),
            linker: Some(path_text(&std::env::current_exe().unwrap()).unwrap()),
            bins: vec![],
        });
        let target = spec("aarch64-unknown-linux-gnu", "linux", "elf", true);
        let build = prepare_settings(&root, &root.targets[0], &target, "dev", false, None).unwrap();
        let sdk_text = path_text(&sdk.canonicalize().unwrap()).unwrap();
        assert!(build.global.contains(&format!("--sysroot={sdk_text}")));
        assert!(build.global.contains(&format!("-Clink-sysroot={sdk_text}")));
        assert!(build.global.contains(&format!(
            "-Clinker={}",
            path_text(&std::env::current_exe().unwrap().canonicalize().unwrap()).unwrap()
        )));
        let check = prepare_settings(&root, &root.targets[0], &target, "dev", true, None).unwrap();
        assert!(check.global.contains(&format!("--sysroot={sdk_text}")));
        assert!(!check.global.iter().any(|arg| arg.starts_with("-Clink")));
        let other = prepare_settings(
            &root,
            &root.targets[0],
            &spec("x86_64-unknown-linux-gnu", "linux", "elf", true),
            "dev",
            false,
            None,
        )
        .unwrap();
        assert_eq!(other.global, vec!["--target=x86_64-unknown-linux-gnu"]);
        assert_eq!(
            linker_path(&root, "cc --malicious").unwrap_err().category,
            diagnostic::Category::Resolution
        );
        assert_eq!(
            linker_path(&root, "./missing-linker").unwrap_err().category,
            diagnostic::Category::Environment
        );
        assert_eq!(
            linker_path(&root, "vex-absent-test-linker")
                .unwrap_err()
                .category,
            diagnostic::Category::Environment
        );
        assert_eq!(
            linker_path(&root, ".").unwrap_err().category,
            diagnostic::Category::Environment
        );
        assert_eq!(
            sysroot_path(&root, Path::new("missing-sdk"))
                .unwrap_err()
                .category,
            diagnostic::Category::Environment
        );
        // Git is available in target execution guests as well as build hosts.
        assert!(Path::new(&linker_path(&root, "git").unwrap()).is_absolute());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = project.join("disabled-linker");
            std::fs::write(&executable, "not executable").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                linker_path(&root, "./disabled-linker")
                    .unwrap_err()
                    .category,
                diagnostic::Category::Environment
            );
            let actual = std::env::current_exe().unwrap();
            let named = project.join("ld.lld");
            std::os::unix::fs::symlink(actual, &named).unwrap();
            assert!(linker_path(&root, "./ld.lld")
                .unwrap()
                .ends_with("/./ld.lld"));
        }
        std::fs::remove_dir_all(temporary).unwrap();
    }
}
