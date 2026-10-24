// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::collections::HashSet;

use wson::{WsonMap, WsonValue};

use crate::parse::reject_unknown_fields;

/// Native link inputs are resolved relative to the declaring package.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeConfig {
    pub output: Option<NativeOutput>,
    pub exports: Vec<String>,
    pub libraries: Vec<NativeLibrary>,
    pub search_paths: Vec<NativeSearchPath>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeOutput {
    Object,
    Shared,
}

impl NativeOutput {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Shared => "shared",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLibrary {
    pub path: Option<String>,
    pub name: Option<String>,
    pub target: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeSearchPath {
    pub path: String,
    pub target: Option<String>,
}

pub(crate) fn parse(value: Option<&WsonValue>, lib: bool) -> Result<Option<NativeConfig>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let WsonValue::Object(object) = value else {
        return Err("manifest field `native` must be an object".into());
    };
    reject_unknown_fields(
        object,
        &["output", "exports", "libraries", "search_paths"],
        "native",
    )?;
    let output = match optional_string(object, "output", "native")? {
        None => None,
        Some(_) if !lib => return Err("native field `output` requires `lib = true`".into()),
        Some(value) => match value.as_str() {
            "object" => Some(NativeOutput::Object),
            "shared" => Some(NativeOutput::Shared),
            _ => return Err("native field `output` must be `object` or `shared`".into()),
        },
    };

    let mut exports = Vec::new();
    let mut export_names = HashSet::new();
    if object.contains_key("exports") && output != Some(NativeOutput::Shared) {
        return Err("native field `exports` requires `output = \"shared\"`".into());
    }
    for value in array(object, "exports")? {
        let WsonValue::String(symbol) = value else {
            return Err("native export must be a C symbol string".into());
        };
        let mut bytes = symbol.bytes();
        if !bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err("native export must be an ASCII C identifier".into());
        }
        if !export_names.insert(symbol.clone()) {
            return Err("native export is declared more than once".into());
        }
        exports.push(symbol.clone());
    }

    let mut libraries = Vec::new();
    let mut library_keys = HashSet::new();
    for value in array(object, "libraries")? {
        let WsonValue::Object(library) = value else {
            return Err("native library entry must be an object".into());
        };
        reject_unknown_fields(library, &["path", "name", "target"], "native library")?;
        let path = optional_string(library, "path", "native library")?;
        let name = optional_string(library, "name", "native library")?;
        if path.is_some() == name.is_some() {
            return Err("native library must specify exactly one of `path` or `name`".into());
        }
        if let Some(path) = &path {
            validate_path(path, "native library path")?;
            if ![".o", ".obj", ".a", ".lib"]
                .iter()
                .any(|extension| path.ends_with(extension))
            {
                return Err("native library `path` must name an object or static library (.o, .obj, .a, or .lib); use `name` and `search_paths` for shared libraries".into());
            }
        }
        if let Some(name) = &name {
            validate_name(name)?;
        }
        let target = target(library, "native library")?;
        if !library_keys.insert((path.clone(), name.clone(), target.clone())) {
            return Err("native library is declared more than once for the same target".into());
        }
        libraries.push(NativeLibrary { path, name, target });
    }

    let mut search_paths = Vec::new();
    let mut search_keys = HashSet::new();
    for value in array(object, "search_paths")? {
        let WsonValue::Object(search_path) = value else {
            return Err("native search path entry must be an object".into());
        };
        reject_unknown_fields(search_path, &["path", "target"], "native search path")?;
        let path = optional_string(search_path, "path", "native search path")?
            .ok_or_else(|| "native search path field `path` must be a string".to_string())?;
        validate_path(&path, "native search path")?;
        let target = target(search_path, "native search path")?;
        if !search_keys.insert((path.clone(), target.clone())) {
            return Err("native search path is declared more than once for the same target".into());
        }
        search_paths.push(NativeSearchPath { path, target });
    }
    Ok(Some(NativeConfig {
        output,
        exports,
        libraries,
        search_paths,
    }))
}

fn optional_string(object: &WsonMap, field: &str, context: &str) -> Result<Option<String>, String> {
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

fn array<'a>(object: &'a WsonMap, field: &str) -> Result<&'a [WsonValue], String> {
    match object.get(field) {
        None => Ok(&[]),
        Some(WsonValue::Array(values)) => Ok(values),
        Some(_) => Err(format!("native field `{field}` must be an array")),
    }
}

fn target(object: &WsonMap, context: &str) -> Result<Option<String>, String> {
    let target = optional_string(object, "target", context)?;
    if let Some(target) = &target {
        let components: Vec<_> = target.split('-').collect();
        if components.len() < 3
            || components.iter().any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
        {
            return Err(format!(
                "{context} field `target` must be an exact compiler target triple"
            ));
        }
    }
    Ok(target)
}

fn validate_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || !bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-'))
    {
        return Err("native library `name` must start with an ASCII letter, digit, or underscore and contain only ASCII letters, digits, `_`, `.`, `+`, or `-`".into());
    }
    Ok(())
}

pub(crate) fn validate_path(path: &str, context: &str) -> Result<(), String> {
    if path.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
        || path.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.starts_with('-')
                || part.ends_with(['.', ' '])
                || reserved_windows_component(part)
        })
    {
        return Err(format!("{context} must be a portable relative path with forward slashes, without parent, empty, dot, or option-like components"));
    }
    Ok(())
}

pub(crate) fn reserved_windows_component(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].into_iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode;

    fn manifest(native: &str, lib: bool) -> Result<crate::Manifest, String> {
        decode(
            &format!("{{format=2,name=\"app\",lib={lib},native={native}}}"),
            "vex.ws".into(),
        )
    }

    #[test]
    fn imports_prebuilt_libraries_and_preserves_order_and_target_variants() {
        let config = manifest(
            r#"{
            libraries=[{path="vendor/ffi/libsample.a",target="x86_64-unknown-linux-gnu"},
                       {name="sample++",target="aarch64-apple-darwin"},
                       {name="sample++",target="x86_64-apple-darwin"}],
            search_paths=[{path="vendor/native libs",target="aarch64-apple-darwin"},
                          {path="vendor/native libs",target="x86_64-apple-darwin"}]
        }"#,
            false,
        )
        .unwrap()
        .native
        .unwrap();
        assert_eq!(config.output, None);
        assert_eq!(config.libraries.len(), 3);
        assert_eq!(
            config.libraries[0].path.as_deref(),
            Some("vendor/ffi/libsample.a")
        );
        assert_eq!(config.libraries[1].name.as_deref(), Some("sample++"));
        assert_eq!(
            config.libraries[2].target.as_deref(),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(config.search_paths[0].path, "vendor/native libs");
    }

    #[test]
    fn exports_object_or_shared_library_only_for_library_packages() {
        for (value, output) in [
            ("object", NativeOutput::Object),
            ("shared", NativeOutput::Shared),
        ] {
            let raw = format!("{{output=\"{value}\"}}");
            assert_eq!(
                manifest(&raw, true).unwrap().native.unwrap().output,
                Some(output)
            );
            assert_eq!(output.as_str(), value);
            assert!(manifest(&raw, false)
                .unwrap_err()
                .contains("requires `lib = true`"));
        }
        for lib in [false, true] {
            assert_eq!(
                manifest("{}", lib).unwrap().native,
                Some(NativeConfig::default())
            );
            assert_eq!(
                decode(&format!("{{name=\"app\",lib={lib}}}"), "vex.ws".into())
                    .unwrap()
                    .native,
                None
            );
        }
    }

    #[test]
    fn shared_exports_accept_only_unique_c_symbols() {
        let config = manifest(r#"{output="shared",exports=["wave_add","_value2"]}"#, true)
            .unwrap()
            .native
            .unwrap();
        assert_eq!(config.exports, ["wave_add", "_value2"]);
        for raw in [
            r#"{exports=[]}"#,
            r#"{output="object",exports=["wave_add"]}"#,
            r#"{output="shared",exports="wave_add"}"#,
            r#"{output="shared",exports=[1]}"#,
            r#"{output="shared",exports=["wave_add","wave_add"]}"#,
            r#"{output="shared",exports=[],exports=[]}"#,
        ] {
            assert!(manifest(raw, true).is_err(), "accepted {raw}");
        }
        for symbol in [
            "",
            "1name",
            "name=other",
            "name,@1",
            "/EXPORT:name",
            "-flag",
            "x y",
            "x\n",
            "café",
        ] {
            let raw = format!("{{output=\"shared\",exports=[{}]}}", wson::quote(symbol));
            assert!(manifest(&raw, true).is_err(), "accepted {symbol:?}");
        }
    }

    #[test]
    fn rejects_unknown_duplicate_ambiguous_and_wrong_typed_fields() {
        for raw in [
            "[]",
            "false",
            r#"{output="static"}"#,
            "{output=1}",
            "{flags=[]}",
            "{libraries={}}",
            "{search_paths=false}",
            "{libraries=[1]}",
            "{libraries=[{}]}",
            r#"{libraries=[{name="m",path="libm.a"}]}"#,
            r#"{libraries=[{name="m",flags=[]}]}"#,
            "{libraries=[{name=false}]}",
            "{libraries=[{path=1}]}",
            r#"{libraries=[{name="m",target=1}]}"#,
            "{search_paths=[1]}",
            "{search_paths=[{}]}",
            "{search_paths=[{path=1}]}",
            r#"{search_paths=[{path="vendor",name="m"}]}"#,
            r#"{libraries=[{name="m"},{name="m"}]}"#,
            r#"{libraries=[{path="vendor/libm.a"},{path="vendor/libm.a"}]}"#,
            r#"{libraries=[{path="vendor/libm.so"}]}"#,
            r#"{libraries=[{path="vendor/libm.dylib"}]}"#,
            r#"{libraries=[{path="vendor/libm.dll"}]}"#,
            r#"{libraries=[{path="vendor/libm"}]}"#,
            r#"{search_paths=[{path="vendor"},{path="vendor"}]}"#,
            r#"{libraries=[{name="m",name="other"}]}"#,
            r#"{output="object",output="shared"}"#,
        ] {
            assert!(
                manifest(raw, true).is_err(),
                "accepted invalid native declaration: {raw}"
            );
        }
    }

    #[test]
    fn rejects_nonportable_paths_and_option_injection() {
        for path in [
            "",
            " ",
            "/lib.a",
            "//server/lib.a",
            "C:/lib.a",
            "C:lib.a",
            "lib\\ffi.a",
            "../lib.a",
            "vendor/../lib.a",
            ".",
            "./lib.a",
            "vendor/./lib.a",
            "vendor//lib.a",
            "vendor/",
            "-lfoo",
            "vendor/-lfoo",
            "lib?.a",
            "vendor./lib.a",
            "vendor /lib.a",
            "vendor\n/lib.a",
            "NUL.a",
            "con/lib.a",
            "LPT1.obj",
            "COM².lib",
        ] {
            for field in ["libraries", "search_paths"] {
                let raw = format!("{{{field}=[{{path={}}}]}}", wson::quote(path));
                assert!(
                    manifest(&raw, true).is_err(),
                    "accepted {field} path {path:?}"
                );
            }
        }
    }

    #[test]
    fn accepts_each_supported_prebuilt_library_extension() {
        for path in [
            "native/export.o",
            "native/export.obj",
            "native/export.a",
            "native/export.lib",
        ] {
            let raw = format!("{{libraries=[{{path={}}}]}}", wson::quote(path));
            assert_eq!(
                manifest(&raw, false).unwrap().native.unwrap().libraries[0]
                    .path
                    .as_deref(),
                Some(path)
            );
        }
    }

    #[test]
    fn rejects_invalid_names_and_target_patterns() {
        for name in [
            "", " ", ".", "..", "-lm", "m;echo", "m/path", "m\\path", "m\n", "café",
        ] {
            let raw = format!("{{libraries=[{{name={}}}]}}", wson::quote(name));
            assert!(manifest(&raw, false).is_err(), "accepted name {name:?}");
        }
        for target in [
            "",
            " ",
            "linux",
            "x86_64-linux",
            "*-unknown-linux-gnu",
            "x86_64--linux-gnu",
            "-x86_64-linux-gnu",
            "x86_64-unknown-linux-gnu ",
            "x86_64-unknown-linux-gnu\n",
        ] {
            for (field, declaration) in [
                ("libraries", "name=\"m\""),
                ("search_paths", "path=\"vendor\""),
            ] {
                let raw = format!(
                    "{{{field}=[{{{declaration},target={}}}]}}",
                    wson::quote(target)
                );
                assert!(manifest(&raw, true).is_err(), "accepted target {target:?}");
            }
        }
    }

    #[test]
    fn uses_existing_format_boundary_for_native_strings() {
        let raw = r#"{name="app",native={libraries=[{path="native\u002flib.a"}]}}"#;
        assert!(decode(raw, "vex.ws".into()).is_err());
        let escaped = raw.replacen('{', "{format=2,", 1);
        assert_eq!(
            decode(&escaped, "vex.ws".into())
                .unwrap()
                .native
                .unwrap()
                .libraries[0]
                .path
                .as_deref(),
            Some("native/lib.a")
        );
    }
}
