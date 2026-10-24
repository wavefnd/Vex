// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use wson::{WsonMap, WsonValue};

use crate::{native, parse::reject_unknown_fields, NativeConfig, NativeOutput};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    Library,
    Binary,
}
impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Binary => "binary",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageTarget {
    pub name: String,
    pub kind: TargetKind,
    pub path: PathBuf,
    pub output: Option<NativeOutput>,
    pub exports: Vec<String>,
    pub platform: PlatformSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformMode {
    Hosted,
    Freestanding,
    Wasi,
    BareWasm,
}
impl PlatformMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hosted => "hosted",
            Self::Freestanding => "freestanding",
            Self::Wasi => "wasi",
            Self::BareWasm => "bare_wasm",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Startup {
    Default,
    None,
}
impl Startup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::None => "none",
        }
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlatformSettings {
    pub mode: Option<PlatformMode>,
    pub entry: Option<String>,
    pub startup: Option<Startup>,
    pub linker_script: Option<PathBuf>,
}
impl PlatformSettings {
    pub(crate) fn overlay(&mut self, other: &Self) {
        if other.mode.is_some() {
            self.mode = other.mode;
        }
        if other.entry.is_some() {
            self.entry.clone_from(&other.entry);
        }
        if other.startup.is_some() {
            self.startup = other.startup;
        }
        if other.linker_script.is_some() {
            self.linker_script.clone_from(&other.linker_script);
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildProfile {
    pub opt_level: u8,
    pub inherits: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetConfig {
    pub triple: String,
    pub sysroot: Option<PathBuf>,
    pub linker: Option<String>,
    pub bins: Vec<BinaryOverride>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BinaryOverride {
    pub name: String,
    pub platform: PlatformSettings,
}

pub(crate) struct Model {
    pub targets: Vec<PackageTarget>,
    pub default_run: Option<String>,
    pub profiles: BTreeMap<String, BuildProfile>,
    pub build_target: Option<String>,
    pub target_config: Vec<TargetConfig>,
}

pub(crate) fn legacy(name: &str, lib: bool, native: Option<&NativeConfig>) -> Model {
    Model {
        targets: vec![PackageTarget {
            name: name.to_owned(),
            kind: if lib {
                TargetKind::Library
            } else {
                TargetKind::Binary
            },
            path: if lib { "src/lib.wave" } else { "src/main.wave" }.into(),
            output: lib.then(|| {
                native
                    .and_then(|n| n.output)
                    .unwrap_or(NativeOutput::Object)
            }),
            exports: native.map(|n| n.exports.clone()).unwrap_or_default(),
            platform: PlatformSettings::default(),
        }],
        default_run: None,
        profiles: default_profiles(),
        build_target: None,
        target_config: vec![],
    }
}

pub(crate) fn parse(data: &WsonMap, package_name: &str) -> Result<Model, String> {
    let target_object = object(data.get("targets"), "manifest field `targets`")?;
    reject_unknown_fields(target_object, &["lib", "bins", "tests"], "targets")?;
    let mut targets = Vec::new();
    if let Some(value) = target_object.get("lib") {
        let library = object(Some(value), "targets.lib")?;
        reject_unknown_fields(library, &["output", "exports"], "targets.lib")?;
        let output = match string(library, "output", "targets.lib")?.as_deref() {
            None | Some("object") => NativeOutput::Object,
            Some("shared") => NativeOutput::Shared,
            Some(_) => return Err("targets.lib output must be `object` or `shared`".into()),
        };
        // The native export parser enforces one portable C symbol per entry.
        let mut export_config = library.clone();
        export_config.insert("output".into(), WsonValue::String(output.as_str().into()));
        let exports = native::parse(Some(&WsonValue::Object(export_config)), true)?
            .expect("library config exists")
            .exports;
        targets.push(PackageTarget {
            name: package_name.into(),
            kind: TargetKind::Library,
            path: "src/lib.wave".into(),
            output: Some(output),
            exports,
            platform: PlatformSettings::default(),
        });
    }
    let mut bin_names = HashSet::new();
    for value in array(target_object, "bins", "targets")? {
        let bin = object(Some(value), "binary target")?;
        reject_unknown_fields(
            bin,
            &["name", "path", "mode", "entry", "startup", "linker_script"],
            "binary target",
        )?;
        let name = required_string(bin, "name", "binary target")?;
        validate_component(&name, "binary target name")?;
        if !bin_names.insert(name.to_ascii_lowercase()) {
            return Err(format!("binary target `{name}` is declared more than once (names are ASCII-case-insensitive)"));
        }
        let path = required_string(bin, "path", "binary target")?;
        native::validate_path(&path, "binary target path")?;
        if !path.ends_with(".wave") {
            return Err("binary target path must end in `.wave`".into());
        }
        let platform = platform(bin, "binary target")?;
        validate_platform(&platform, "binary target")?;
        targets.push(PackageTarget {
            name,
            kind: TargetKind::Binary,
            path: path.into(),
            output: None,
            exports: vec![],
            platform,
        });
    }
    if targets.is_empty() && array(target_object, "tests", "targets")?.is_empty() {
        return Err("format 3 requires at least one explicitly declared target".into());
    }
    let default_run = string(data, "default_run", "manifest")?;
    if let Some(name) = &default_run {
        if !targets
            .iter()
            .any(|t| t.kind == TargetKind::Binary && t.name == *name)
        {
            return Err(format!(
                "default_run `{name}` must name a declared binary target"
            ));
        }
    }
    let profiles = profiles(data.get("profiles"))?;
    let build_target = match data.get("build") {
        None => None,
        Some(value) => {
            let build = object(Some(value), "build")?;
            reject_unknown_fields(build, &["target"], "build")?;
            let target = string(build, "target", "build")?;
            if let Some(triple) = &target {
                validate_triple(triple)?;
            }
            target
        }
    };
    let mut configs = Vec::new();
    let mut triples = HashSet::new();
    for value in array(data, "target_config", "manifest")? {
        let config = object(Some(value), "target_config entry")?;
        reject_unknown_fields(
            config,
            &["triple", "sysroot", "linker", "bins"],
            "target_config",
        )?;
        let triple = required_string(config, "triple", "target_config")?;
        validate_triple(&triple)?;
        if !triples.insert(triple.clone()) {
            return Err(format!(
                "target_config `{triple}` is declared more than once"
            ));
        }
        let sysroot = string(config, "sysroot", "target_config")?.map(PathBuf::from);
        let linker = string(config, "linker", "target_config")?;
        if linker.as_ref().is_some_and(|value| value.starts_with('-')) {
            return Err(
                "target_config linker must be one executable name or path without arguments".into(),
            );
        }
        let mut bins = Vec::new();
        let mut names = HashSet::new();
        for value in array(config, "bins", "target_config")? {
            let bin = object(Some(value), "target_config binary override")?;
            reject_unknown_fields(
                bin,
                &["name", "mode", "entry", "startup", "linker_script"],
                "target_config binary override",
            )?;
            let name = required_string(bin, "name", "target_config binary override")?;
            let target = targets
                .iter()
                .find(|t| t.kind == TargetKind::Binary && t.name == name)
                .ok_or_else(|| {
                    format!("target_config binary `{name}` does not name a declared binary target")
                })?;
            if !names.insert(name.clone()) {
                return Err(format!(
                    "target_config binary `{name}` is declared more than once"
                ));
            }
            let platform = platform(bin, "target_config binary override")?;
            let mut combined = target.platform.clone();
            combined.overlay(&platform);
            validate_platform(&combined, "target_config binary override")?;
            bins.push(BinaryOverride { name, platform });
        }
        configs.push(TargetConfig {
            triple,
            sysroot,
            linker,
            bins,
        });
    }
    Ok(Model {
        targets,
        default_run,
        profiles,
        build_target,
        target_config: configs,
    })
}

fn default_profiles() -> BTreeMap<String, BuildProfile> {
    [
        (
            "dev".into(),
            BuildProfile {
                opt_level: 0,
                inherits: None,
            },
        ),
        (
            "release".into(),
            BuildProfile {
                opt_level: 2,
                inherits: None,
            },
        ),
    ]
    .into()
}
fn profiles(value: Option<&WsonValue>) -> Result<BTreeMap<String, BuildProfile>, String> {
    let mut result = default_profiles();
    let Some(value) = value else {
        return Ok(result);
    };
    let declarations = object(Some(value), "profiles")?;
    // Resolve builtins first so a custom profile inherits its effective value.
    for builtin in ["dev", "release"] {
        if let Some(value) = declarations.get(builtin) {
            let profile = object(Some(value), "profile")?;
            reject_unknown_fields(profile, &["opt_level"], &format!("profile `{builtin}`"))?;
            if let Some(level) = opt_level(profile)? {
                result.get_mut(builtin).unwrap().opt_level = level;
            }
        }
    }
    for (name, value) in declarations {
        validate_component(name, "profile name")?;
        if matches!(name.as_str(), "dev" | "release") {
            continue;
        }
        let profile = object(Some(value), "profile")?;
        reject_unknown_fields(
            profile,
            &["inherits", "opt_level"],
            &format!("profile `{name}`"),
        )?;
        let inherits = required_string(profile, "inherits", "custom profile")?;
        if !matches!(inherits.as_str(), "dev" | "release") {
            return Err(format!(
                "custom profile `{name}` must inherit only `dev` or `release`"
            ));
        }
        let opt_level = opt_level(profile)?.unwrap_or(result[&inherits].opt_level);
        result.insert(
            name.clone(),
            BuildProfile {
                opt_level,
                inherits: Some(inherits),
            },
        );
    }
    let mut names = HashSet::new();
    if result
        .keys()
        .any(|name| !names.insert(name.to_ascii_lowercase()))
    {
        return Err("profile names must not differ only in ASCII case".into());
    }
    Ok(result)
}
fn opt_level(profile: &WsonMap) -> Result<Option<u8>, String> {
    match profile.get("opt_level") {
        None => Ok(None),
        Some(WsonValue::Int(value)) if (0..=3).contains(value) => Ok(Some(*value as u8)),
        Some(_) => Err("profile opt_level must be an integer from 0 to 3".into()),
    }
}
fn platform(object: &WsonMap, context: &str) -> Result<PlatformSettings, String> {
    let mode = match string(object, "mode", context)?.as_deref() {
        None => None,
        Some("hosted") => Some(PlatformMode::Hosted),
        Some("freestanding") => Some(PlatformMode::Freestanding),
        Some("wasi") => Some(PlatformMode::Wasi),
        Some("bare_wasm") => Some(PlatformMode::BareWasm),
        Some(_) => {
            return Err(format!(
                "{context} mode must be hosted, freestanding, wasi, or bare_wasm"
            ))
        }
    };
    let entry = string(object, "entry", context)?;
    if entry.as_ref().is_some_and(|entry| {
        let mut bytes = entry.bytes();
        !bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) {
        return Err(format!(
            "{context} entry must be an ASCII symbol identifier"
        ));
    }
    let startup = match string(object, "startup", context)?.as_deref() {
        None => None,
        Some("default") => Some(Startup::Default),
        Some("none") => Some(Startup::None),
        Some(_) => return Err(format!("{context} startup must be `default` or `none`")),
    };
    let linker_script = string(object, "linker_script", context)?
        .map(|path| {
            native::validate_path(&path, "linker_script")?;
            Ok::<_, String>(PathBuf::from(path))
        })
        .transpose()?;
    Ok(PlatformSettings {
        mode,
        entry,
        startup,
        linker_script,
    })
}
fn validate_platform(platform: &PlatformSettings, context: &str) -> Result<(), String> {
    match platform.mode.unwrap_or(PlatformMode::Hosted) {
        PlatformMode::Hosted | PlatformMode::Wasi if platform.entry.is_some() || platform.startup == Some(Startup::None) || platform.linker_script.is_some() =>
            Err(format!("{context} entry, startup=none, and linker_script require freestanding or bare_wasm mode")),
        PlatformMode::BareWasm if platform.linker_script.is_some() => Err(format!("{context} bare_wasm does not support linker_script")),
        _ => Ok(()),
    }
}
pub(crate) fn validate_component(value: &str, context: &str) -> Result<(), String> {
    let mut bytes = value.bytes();
    if !bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        || native::reserved_windows_component(value)
    {
        return Err(format!("{context} must be a portable [A-Za-z_][A-Za-z0-9_-]* name and must not be a Windows device name"));
    }
    Ok(())
}
fn validate_triple(triple: &str) -> Result<(), String> {
    let components: Vec<_> = triple.split('-').collect();
    // Compiler target identifiers include two-component WASI names such as
    // `wasm32-wasip1`; availability is checked against the selected compiler.
    if components.len() < 2
        || components.iter().any(|part| {
            part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    {
        return Err("build target must be an exact compiler target triple".into());
    }
    Ok(())
}
fn object<'a>(value: Option<&'a WsonValue>, context: &str) -> Result<&'a WsonMap, String> {
    match value {
        Some(WsonValue::Object(value)) => Ok(value),
        _ => Err(format!("{context} must be an object")),
    }
}
fn array<'a>(object: &'a WsonMap, field: &str, context: &str) -> Result<&'a [WsonValue], String> {
    match object.get(field) {
        None => Ok(&[]),
        Some(WsonValue::Array(value)) => Ok(value),
        Some(_) => Err(format!("{context} field `{field}` must be an array")),
    }
}
fn required_string(object: &WsonMap, field: &str, context: &str) -> Result<String, String> {
    string(object, field, context)?
        .ok_or_else(|| format!("{context} field `{field}` must be a string"))
}
fn string(object: &WsonMap, field: &str, context: &str) -> Result<Option<String>, String> {
    match object.get(field) {
        None => Ok(None),
        Some(WsonValue::String(value))
            if !value.trim().is_empty() && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.clone()))
        }
        Some(WsonValue::String(_)) => Err(format!(
            "{context} field `{field}` must not be empty or contain control characters"
        )),
        Some(_) => Err(format!("{context} field `{field}` must be a string")),
    }
}

pub(crate) fn parse_tests(data: &WsonMap, name: &str) -> Result<Vec<PackageTarget>, String> {
    let Some(WsonValue::Object(targets)) = data.get("targets") else {
        return Ok(vec![]);
    };
    let tests = array(targets, "tests", "targets")?;
    if tests.is_empty() {
        return Ok(vec![]);
    }
    let mut model = data.clone();
    model.remove("default_run");
    model.remove("target_config");
    model.insert(
        "targets".into(),
        WsonValue::Object(WsonMap::from([(
            "bins".into(),
            WsonValue::Array(tests.to_vec()),
        )])),
    );
    parse(&model, name).map(|model| model.targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, render_new_manifest};

    fn manifest(fields: &str) -> Result<crate::Manifest, String> {
        decode(
            &format!("{{format=3,name=\"app\",{fields}}}"),
            "vex.ws".into(),
        )
    }

    #[test]
    fn explicit_targets_allow_library_and_same_named_binary_and_preserve_order() {
        let m = manifest(r#"targets={lib={},bins=[{name="app",path="src/main.wave"},{name="admin",path="src/bin/admin.wave"}]},default_run="app""#).unwrap();
        assert!(m.lib);
        assert_eq!(m.targets.len(), 3);
        assert_eq!(m.targets[0].kind, TargetKind::Library);
        assert_eq!(m.targets[0].output, Some(NativeOutput::Object));
        assert_eq!(m.targets[0].name, "app");
        assert_eq!(m.targets[0].path, PathBuf::from("src/lib.wave"));
        assert_eq!(m.targets[1].kind, TargetKind::Binary);
        assert_eq!(m.targets[1].name, "app");
        assert_eq!(m.targets[2].name, "admin");
        assert_eq!(m.default_entry_path(), PathBuf::from("src/lib.wave"));
        assert_eq!(m.default_run.as_deref(), Some("app"));
        assert_eq!(m.profile("dev").unwrap().opt_level, 0);
        assert_eq!(m.profile("release").unwrap().opt_level, 2);
        assert!(m.profile("missing").is_err());
    }

    #[test]
    fn library_shared_exports_reuse_native_symbol_contract() {
        let m = manifest(r#"targets={lib={output="shared",exports=["wave_add"]}},native={libraries=[{name="m"}]}"#).unwrap();
        assert_eq!(m.targets[0].output, Some(NativeOutput::Shared));
        assert_eq!(m.targets[0].exports, ["wave_add"]);
        assert_eq!(m.native.unwrap().libraries[0].name.as_deref(), Some("m"));
        for fields in [
            r#"targets={lib={output="static"}}"#,
            r#"targets={lib={exports=["value"]}}"#,
            r#"targets={lib={output="shared",exports=["value","value"]}}"#,
            r#"targets={lib={path="elsewhere.wave"}}"#,
            r#"targets={lib={}},native={output="object"}"#,
            r#"targets={lib={}},native={exports=[]}"#,
            r#"targets={lib={}},lib=false"#,
        ] {
            assert!(manifest(fields).is_err(), "accepted {fields}");
        }
    }

    #[test]
    fn old_formats_normalize_targets_without_changing_escape_or_native_meanings() {
        for format in [1, 2] {
            let m = decode(&format!("{{format={format},name=\"libapp\",lib=true,native={{output=\"shared\",exports=[\"value\"]}}}}"), "vex.ws".into()).unwrap();
            assert_eq!(m.targets[0].kind, TargetKind::Library);
            assert_eq!(m.targets[0].output, Some(NativeOutput::Shared));
            assert_eq!(m.targets[0].exports, ["value"]);
            assert_eq!(m.native.unwrap().exports, ["value"]);
            assert_eq!(m.format, format);
        }
        let m = decode(r#"{name="app"}"#, "vex.ws".into()).unwrap();
        assert!(!m.lib);
        assert_eq!(m.targets[0].kind, TargetKind::Binary);
        assert_eq!(m.targets[0].path, PathBuf::from("src/main.wave"));
        for field in [
            "targets={lib={}}",
            "profiles={}",
            "default_run=\"app\"",
            "build={}",
            "target_config=[]",
        ] {
            for format in [1, 2] {
                assert!(decode(
                    &format!("{{format={format},name=\"app\",{field}}}"),
                    "vex.ws".into()
                )
                .unwrap_err()
                .contains("requires `format = 3`"));
            }
        }
        for format in [2, 3] {
            let targets = if format == 3 { ",targets={lib={}}" } else { "" };
            let m = decode(
                &format!(r#"{{format={format},name="app",description="C:\\tmp\nnext"{targets}}}"#),
                "vex.ws".into(),
            )
            .unwrap();
            assert_eq!(m.description.as_deref(), Some("C:\\tmp\nnext"));
        }
        let m = decode(
            r#"{name="app",description="C:\tmp\nnext"}"#,
            "vex.ws".into(),
        )
        .unwrap();
        assert_eq!(m.description.as_deref(), Some(r"C:\tmp\nnext"));
    }

    #[test]
    fn binary_target_names_and_paths_are_portable_and_nonempty() {
        for name in [
            "", " ", "1app", "-app", "a.b", "café", "CON", "nul", "COM1", "Lpt9", "app/new",
        ] {
            let fields = format!(
                "targets={{bins=[{{name={},path=\"src/main.wave\"}}]}}",
                wson::quote(name)
            );
            assert!(manifest(&fields).is_err(), "accepted name {name:?}");
        }
        for path in [
            "",
            "../main.wave",
            "/main.wave",
            "src/../main.wave",
            "./main.wave",
            "src//main.wave",
            "src\\main.wave",
            "C:/main.wave",
            "src/main.wv",
            "src/-main.wave",
            "src/CON.wave",
            "src/main\n.wave",
        ] {
            let fields = format!(
                "targets={{bins=[{{name=\"app\",path={}}}]}}",
                wson::quote(path)
            );
            assert!(manifest(&fields).is_err(), "accepted path {path:?}");
        }
        let m = manifest(r#"targets={bins=[{name="_app-2",path="src/한글 app.wave"}]}"#).unwrap();
        assert_eq!(m.default_entry_path(), PathBuf::from("src/한글 app.wave"));
        for fields in [
            "targets={}",
            "targets={bins=[]}",
            "targets={lib=false}",
            "targets=[]",
            r#"targets={bins=[{name="app",path="src/a.wave"},{name="APP",path="src/b.wave"}]}"#,
            r#"targets={bins=[{name="app",path="src/a.wave",typo=true}]}"#,
            r#"targets={lib={}},default_run="app""#,
            r#"targets={bins=[{name="app",path="src/a.wave"}]},default_run="APP""#,
        ] {
            assert!(manifest(fields).is_err(), "accepted {fields}");
        }
    }

    #[test]
    fn profiles_inherit_only_effective_builtins_and_reject_unsupported_fields() {
        let m = manifest(r#"targets={lib={}},profiles={fast={inherits="release"},dev={opt_level=1},release={opt_level=3},small={inherits="dev",opt_level=0}}"#).unwrap();
        assert_eq!(m.profile("dev").unwrap().opt_level, 1);
        assert_eq!(m.profile("release").unwrap().opt_level, 3);
        assert_eq!(m.profile("fast").unwrap().opt_level, 3);
        assert_eq!(m.profile("small").unwrap().opt_level, 0);
        assert_eq!(
            m.profile("fast").unwrap().inherits.as_deref(),
            Some("release")
        );
        for profiles in [
            "[]",
            "{fast={}}",
            r#"{fast={inherits="fast"}}"#,
            r#"{fast={inherits="other"},other={inherits="dev"}}"#,
            r#"{dev={inherits="release"}}"#,
            "{dev={opt_level=4}}",
            "{dev={opt_level=-1}}",
            r#"{dev={opt_level="s"}}"#,
            "{release={lto=true}}",
            "{dev={debug=true}}",
            r#"{fast={inherits="release",overflow_checks=true}}"#,
            r#"{DEV={inherits="dev"}}"#,
            r#"{CON={inherits="dev"}}"#,
            r#"{fast={inherits="release",opt_level=2,opt_level=3}}"#,
        ] {
            assert!(
                manifest(&format!("targets={{lib={{}}}},profiles={profiles}")).is_err(),
                "accepted profiles {profiles}"
            );
        }
    }

    #[test]
    fn exact_target_overrides_overlay_only_matching_binary_and_triple() {
        let m = manifest(r#"targets={lib={},bins=[{name="app",path="src/main.wave",mode="freestanding",entry="_start",startup="default",linker_script="ld/default.ld"}]},build={target="x86_64-unknown-linux-gnu"},target_config=[{triple="x86_64-unknown-linux-gnu",sysroot="/opt/sdk",linker="/opt/SDK tools/clang",bins=[{name="app",startup="none",linker_script="ld/custom.ld"}]}]"#).unwrap();
        assert_eq!(m.build_target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(m.target_config[0].sysroot, Some(PathBuf::from("/opt/sdk")));
        let base = &m.targets[1];
        let platform = m.platform_for(base, "x86_64-unknown-linux-gnu");
        assert_eq!(platform.mode, Some(PlatformMode::Freestanding));
        assert_eq!(platform.entry.as_deref(), Some("_start"));
        assert_eq!(platform.startup, Some(Startup::None));
        assert_eq!(platform.linker_script, Some(PathBuf::from("ld/custom.ld")));
        assert_eq!(
            m.platform_for(base, "aarch64-unknown-linux-gnu"),
            base.platform
        );
        assert_eq!(
            m.platform_for(&m.targets[0], "x86_64-unknown-linux-gnu"),
            PlatformSettings::default()
        );
        let wasi = manifest(r#"targets={bins=[{name="app",path="src/main.wave",mode="wasi"}]},build={target="wasm32-wasip1"},target_config=[{triple="wasm32-wasip1"}]"#).unwrap();
        assert_eq!(wasi.build_target.as_deref(), Some("wasm32-wasip1"));
    }

    #[test]
    fn rejects_unknown_duplicate_and_incompatible_platform_configuration() {
        let bin = r#"targets={bins=[{name="app",path="src/main.wave"}]}"#;
        for config in [
            "{}",
            "[{triple=1}]",
            r#"[{triple="linux"}]"#,
            r#"[{triple="*-unknown-linux-gnu"}]"#,
            r#"[{triple="x86_64-unknown-linux-gnu",sysroot=""}]"#,
            r#"[{triple="x86_64-unknown-linux-gnu",linker="-flag"}]"#,
            r#"[{triple="x86_64-unknown-linux-gnu",linker_args=[]}]"#,
            r#"[{triple="x86_64-unknown-linux-gnu",bins=[{name="other"}]}]"#,
            r#"[{triple="x86_64-unknown-linux-gnu",bins=[{name="app"},{name="app"}]}]"#,
            r#"[{triple="x86_64-unknown-linux-gnu"},{triple="x86_64-unknown-linux-gnu"}]"#,
        ] {
            assert!(
                manifest(&format!("{bin},target_config={config}")).is_err(),
                "accepted {config}"
            );
        }
        for fields in [
            r#"mode="other""#,
            r#"entry="-start""#,
            r#"entry="_start""#,
            r#"mode="hosted",startup="none""#,
            r#"mode="wasi",entry="_start""#,
            r#"mode="freestanding",startup="other""#,
            r#"mode="bare_wasm",linker_script="link.ld""#,
            r#"mode="freestanding",linker_script="../link.ld""#,
        ] {
            assert!(
                manifest(&format!(
                    "targets={{bins=[{{name=\"app\",path=\"src/main.wave\",{fields}}}]}}"
                ))
                .is_err(),
                "accepted {fields}"
            );
        }
        for mode in ["hosted", "freestanding", "wasi", "bare_wasm"] {
            let m = manifest(&format!(
                "targets={{bins=[{{name=\"app\",path=\"src/main.wave\",mode=\"{mode}\"}}]}}"
            ))
            .unwrap();
            assert_eq!(m.targets[0].platform.mode.unwrap().as_str(), mode);
        }
    }

    #[test]
    fn new_manifest_round_trips_explicit_format_three_targets_and_escaped_author() {
        for is_lib in [false, true] {
            let author = "Author \"quoted\" \\ path\nnext";
            let raw = render_new_manifest("app", author, is_lib);
            let m = decode(&raw, "vex.ws".into()).unwrap();
            assert_eq!(m.format, 3);
            assert_eq!(m.lib, is_lib);
            assert_eq!(m.author.as_deref(), Some(author));
            assert_eq!(m.targets.len(), 1);
            assert_eq!(
                m.targets[0].kind,
                if is_lib {
                    TargetKind::Library
                } else {
                    TargetKind::Binary
                }
            );
        }
    }
}
