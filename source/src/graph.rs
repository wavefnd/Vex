// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Stable dependency activation concepts shared by manifests and locks.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    pub triple: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub mode: Option<String>,
}
impl Predicate {
    pub fn validate(&self) -> Result<(), String> {
        if self == &Self::default() {
            return Err("target predicate must contain a constraint".into());
        }
        if let Some(triple) = &self.triple {
            Context::new(triple)?;
        }
        for (label, value, allowed) in [
            (
                "os",
                &self.os,
                &[
                    "linux", "macos", "windows", "freebsd", "wasi", "none", "unknown",
                ][..],
            ),
            (
                "arch",
                &self.arch,
                &[
                    "x86_64",
                    "aarch64",
                    "riscv64",
                    "loongarch64",
                    "wasm32",
                    "wasm64",
                ][..],
            ),
            (
                "mode",
                &self.mode,
                &["hosted", "freestanding", "wasi", "bare_wasm"][..],
            ),
        ] {
            if value
                .as_ref()
                .is_some_and(|value| !allowed.contains(&value.as_str()))
            {
                return Err(format!("unsupported target predicate {label}"));
            }
        }
        if let Some(triple) = &self.triple {
            let context = Context::new(triple)?;
            if self.os.as_ref().is_some_and(|os| *os != context.os)
                || self.arch.as_ref().is_some_and(|arch| *arch != context.arch)
            {
                return Err("target predicate contradicts its exact triple".into());
            }
        }
        Ok(())
    }
    pub fn matches(&self, context: &Context) -> bool {
        [&self.triple, &self.os, &self.arch, &self.mode]
            .into_iter()
            .zip([&context.triple, &context.os, &context.arch, &context.mode])
            .all(|(required, found)| required.as_ref().is_none_or(|required| required == found))
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Context {
    pub triple: String,
    pub os: String,
    pub arch: String,
    pub mode: String,
}
impl Context {
    pub fn new(triple: &str) -> Result<Self, String> {
        let parts: Vec<_> = triple.split('-').collect();
        if parts.len() < 3
            || parts.iter().any(|p| p.is_empty())
            || !triple
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err("target must be a complete portable triple".into());
        }
        let os = if parts.contains(&"windows") {
            "windows"
        } else if parts.contains(&"darwin") {
            "macos"
        } else if parts.contains(&"linux") {
            "linux"
        } else if parts.contains(&"freebsd") {
            "freebsd"
        } else if parts.iter().any(|p| p.starts_with("wasi")) {
            "wasi"
        } else if parts.contains(&"none") {
            "none"
        } else {
            "unknown"
        };
        let mode = if os == "wasi" {
            "wasi"
        } else if parts[0].starts_with("wasm") {
            "bare_wasm"
        } else if os == "none" {
            "freestanding"
        } else {
            "hosted"
        };
        Ok(Self {
            triple: triple.into(),
            os: os.into(),
            arch: parts[0].into(),
            mode: mode.into(),
        })
    }
    pub fn host() -> Self {
        Self::new(&env!("VEX_HOST_TARGET").replacen("riscv64gc-", "riscv64-", 1))
            .expect("Rust target triple")
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub include_tests: bool,
    pub target: Option<Context>,
    pub features: BTreeSet<String>,
    pub no_default_features: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    #[serde(default, skip_serializing_if = "is_false")]
    pub development: bool,
    pub key: String,
    pub alias: String,
    pub optional: bool,
    pub default_features: bool,
    pub features: Vec<String>,
    pub target: Option<Predicate>,
}
impl Edge {
    pub fn unconditional(key: String, alias: String) -> Self {
        Self {
            development: false,
            key,
            alias,
            optional: false,
            default_features: true,
            features: Vec::new(),
            target: None,
        }
    }
}
pub type Features = BTreeMap<String, Vec<String>>;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub manifest: String,
    pub name: String,
    pub version: String,
    pub edges: Vec<Edge>,
    pub features: Features,
}

pub fn feature_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
/// Additive closure: local features, dep:alias, and alias/feature. Cyclic local
/// groups are finite unions; dependency cycles are rejected separately.
pub struct Expanded {
    pub enabled: BTreeSet<String>,
    pub forwarded: BTreeMap<String, BTreeSet<String>>,
    pub features: BTreeSet<String>,
}

pub fn expand(
    features: &Features,
    edges: &[Edge],
    requested: &BTreeSet<String>,
    defaults: bool,
) -> Result<Expanded, String> {
    let mut queue: Vec<_> = requested.iter().cloned().collect();
    if defaults && features.contains_key("default") {
        queue.push("default".into());
    }
    let mut visited = BTreeSet::new();
    let mut enabled = BTreeSet::new();
    let mut forwarded: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    while let Some(value) = queue.pop() {
        if !visited.insert(value.clone()) {
            continue;
        }
        if let Some(alias) = value.strip_prefix("dep:") {
            if !edges.iter().any(|e| e.alias == alias) {
                return Err(format!("unknown feature dependency `{alias}`"));
            }
            enabled.insert(alias.to_owned());
        } else if let Some((alias, feature)) = value.split_once('/') {
            if !feature_name(feature) || !edges.iter().any(|e| e.alias == alias) {
                return Err(format!("invalid dependency feature `{value}`"));
            }
            enabled.insert(alias.to_owned());
            forwarded
                .entry(alias.into())
                .or_default()
                .insert(feature.into());
        } else if let Some(members) = features.get(&value) {
            queue.extend(members.iter().cloned());
        } else if edges.iter().any(|e| e.optional && e.alias == value) {
            enabled.insert(value);
        } else {
            return Err(format!("unknown package feature `{value}`"));
        }
    }
    let active = visited
        .into_iter()
        .filter(|name| {
            features.contains_key(name)
                || edges
                    .iter()
                    .any(|edge| edge.optional && edge.alias == *name)
        })
        .collect();
    Ok(Expanded {
        enabled,
        forwarded,
        features: active,
    })
}

pub fn validate(features: &Features, edges: &[Edge]) -> Result<(), String> {
    for (name, values) in features {
        if !feature_name(name) {
            return Err(format!("invalid feature name `{name}`"));
        }
        if values.iter().collect::<BTreeSet<_>>().len() != values.len() {
            return Err(format!("duplicate member of feature `{name}`"));
        }
        for value in values {
            if let Some(alias) = value.strip_prefix("dep:") {
                if !edges.iter().any(|e| e.alias == alias) {
                    return Err(format!("unknown feature dependency `{alias}`"));
                }
            } else if let Some((alias, feature)) = value.split_once('/') {
                if !feature_name(feature) || !edges.iter().any(|e| e.alias == alias) {
                    return Err(format!("invalid dependency feature `{value}`"));
                }
            } else if !features.contains_key(value)
                && !edges.iter().any(|e| e.optional && e.alias == *value)
            {
                return Err(format!("unknown package feature `{value}`"));
            }
        }
    }
    for edge in edges {
        if let Some(target) = &edge.target {
            target.validate()?;
        }
        if !edge.features.iter().all(|feature| feature_name(feature))
            || edge.features.iter().collect::<BTreeSet<_>>().len() != edge.features.len()
        {
            return Err("invalid or duplicate dependency feature".into());
        }
    }
    Ok(())
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn predicates_are_conjunctions_and_reject_contradictory_triples() {
        let linux = Context::new("x86_64-unknown-linux-gnu").unwrap();
        let predicate = Predicate {
            os: Some("linux".into()),
            arch: Some("x86_64".into()),
            ..Default::default()
        };
        assert!(predicate.matches(&linux));
        assert!(!predicate.matches(&Context::new("aarch64-unknown-linux-gnu").unwrap()));
        assert!(Predicate {
            triple: Some(linux.triple),
            os: Some("windows".into()),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Predicate::default().validate().is_err());
    }
    #[test]
    fn additive_local_cycles_terminate_and_defaults_can_be_disabled() {
        let features = Features::from([
            ("default".into(), vec!["first".into()]),
            ("first".into(), vec!["second".into()]),
            ("second".into(), vec!["first".into()]),
        ]);
        validate(&features, &[]).unwrap();
        let result = expand(&features, &[], &BTreeSet::new(), true).unwrap();
        assert_eq!(result.features.len(), 3);
        assert!(expand(&features, &[], &BTreeSet::new(), false)
            .unwrap()
            .features
            .is_empty());
        for invalid in ["", "-bad", "bad/name", "bad name", "β"] {
            assert!(!feature_name(invalid));
        }
    }
}
