use std::collections::HashSet;
use std::path::PathBuf;

use wson::{loads, WsonMap, WsonValue};

use crate::{Dependency, DependencySource, Manifest};

const ROOT_FIELDS: &[&str] = &[
    "format",
    "compiler",
    "name",
    "version",
    "lib",
    "description",
    "author",
    "license",
    "dependencies",
];
const DEPENDENCY_FIELDS: &[&str] = &["name", "version", "path", "git", "branch", "tag", "rev"];

pub(crate) fn parse_manifest(raw: &str, source_path: PathBuf) -> Result<Manifest, String> {
    let data = loads(raw, "format", 2).map_err(|e| format!("failed to parse manifest: {e}"))?;
    reject_unknown_fields(&data, ROOT_FIELDS, "manifest")?;
    if !matches!(data.get("format"), None | Some(WsonValue::Int(1 | 2))) {
        return Err(
            "unsupported manifest format; expected 1 (legacy) or 2 (escaped strings)".into(),
        );
    }

    let name = match data.get("name") {
        Some(WsonValue::String(value)) => restore_url_separators(value),
        _ => return Err("manifest field `name` must be a string".to_string()),
    };
    crate::validate_package_name(&name)?;
    let version = match data.get("version") {
        Some(value) => parse_version_string(value)
            .ok_or_else(|| "manifest field `version` must be a version or string".to_string())?,
        None => "0.1.0".to_string(),
    };
    let lib = match data.get("lib") {
        Some(WsonValue::Bool(value)) => *value,
        None => false,
        _ => return Err("manifest field `lib` must be a bool".to_string()),
    };
    let compiler = parse_optional_string(data.get("compiler"), "compiler")?;
    if let Some(required) = &compiler {
        let version = semver::Version::parse(required)
            .map_err(|_| "manifest compiler must be an exact version such as 0.2.1-pre-beta; ranges are unsupported")?;
        if !version.build.is_empty() || version.to_string() != *required {
            return Err(
                "manifest compiler must be a canonical exact version without build metadata".into(),
            );
        }
    }
    let description = parse_optional_string(data.get("description"), "description")?;
    let author = parse_optional_string(data.get("author"), "author")?;
    let license = parse_optional_string(data.get("license"), "license")?;
    let dependencies = parse_dependencies(data.get("dependencies"))?;

    if let Some(dependency) = dependencies
        .iter()
        .find(|dependency| dependency.name == name)
    {
        return Err(format!(
            "dependency `{name}` from `{}` must not reuse the root package name",
            dependency_source_label(&dependency.source)
        ));
    }

    Ok(Manifest {
        format: if matches!(data.get("format"), Some(WsonValue::Int(2))) {
            2
        } else {
            1
        },
        compiler,
        name,
        version,
        lib,
        description,
        author,
        license,
        dependencies,
        source_path,
    })
}

fn parse_optional_string(value: Option<&WsonValue>, field: &str) -> Result<Option<String>, String> {
    match value {
        Some(WsonValue::String(value)) => Ok(Some(restore_url_separators(value))),
        Some(_) => Err(format!("manifest field `{field}` must be a string")),
        None => Ok(None),
    }
}

fn parse_dependencies(value: Option<&WsonValue>) -> Result<Vec<Dependency>, String> {
    let mut dependencies = Vec::new();
    let mut names = HashSet::new();
    let Some(WsonValue::Array(items)) = value else {
        if value.is_some() {
            return Err("manifest field `dependencies` must be an array".to_string());
        }
        return Ok(dependencies);
    };

    for item in items {
        let WsonValue::Object(object) = item else {
            return Err("dependency entry must be an object".to_string());
        };
        let name = required_string(object.get("name"), "dependency field `name`")?;
        validate_dependency_name(&name)?;
        reject_unknown_fields(object, DEPENDENCY_FIELDS, &format!("dependency `{name}`"))?;
        if !names.insert(name.clone()) {
            return Err(format!("dependency `{name}` is declared more than once"));
        }

        let path = optional_string(object.get("path"), "dependency field `path`")?;
        let git = optional_string(object.get("git"), "dependency field `git`")?;
        let branch = optional_string(object.get("branch"), "dependency field `branch`")?;
        let tag = optional_string(object.get("tag"), "dependency field `tag`")?;
        let rev = optional_string(object.get("rev"), "dependency field `rev`")?;
        for (field, value) in [
            ("path", path.as_deref()),
            ("git", git.as_deref()),
            ("branch", branch.as_deref()),
            ("tag", tag.as_deref()),
            ("rev", rev.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(format!(
                    "dependency `{name}` field `{field}` must not be empty or whitespace-only"
                ));
            }
        }
        let version = match object.get("version") {
            Some(value) => Some(parse_version_string(value).ok_or_else(|| {
                format!("dependency `{name}` field `version` must be a version or string")
            })?),
            None => None,
        };

        let source = match (path, git) {
            (Some(path), None) => {
                if let Some(field) = [
                    ("branch", branch.as_ref()),
                    ("tag", tag.as_ref()),
                    ("rev", rev.as_ref()),
                ]
                .into_iter()
                .find_map(|(field, value)| value.map(|_| field))
                {
                    return Err(format!(
                        "dependency `{name}` field `{field}` applies only to Git dependencies"
                    ));
                }
                DependencySource::Path { path }
            }
            (None, Some(url)) => {
                let _ = source::identity(&url);
                let git_ref_count =
                    branch.is_some() as u8 + tag.is_some() as u8 + rev.is_some() as u8;
                if git_ref_count > 1 {
                    return Err(format!(
                        "dependency `{name}` can specify only one of `branch`, `tag`, or `rev`"
                    ));
                }
                DependencySource::Git {
                    url,
                    branch,
                    tag,
                    rev,
                }
            }
            (Some(_), Some(_)) => {
                return Err(format!(
                    "dependency `{name}` must specify only one of `path` or `git`"
                ));
            }
            (None, None) => {
                return Err(format!(
                    "dependency `{name}` must specify either `path` or `git`"
                ));
            }
        };
        dependencies.push(Dependency {
            name,
            version,
            source,
        });
    }
    Ok(dependencies)
}

fn reject_unknown_fields(object: &WsonMap, accepted: &[&str], context: &str) -> Result<(), String> {
    let Some(field) = object
        .keys()
        .find(|field| !accepted.contains(&field.as_str()))
    else {
        return Ok(());
    };
    let suggestion = closest_field(field, accepted)
        .map(|known| format!("; did you mean `{known}`?"))
        .unwrap_or_default();
    Err(format!(
        "{context} contains unknown field `{field}`{suggestion}"
    ))
}

fn closest_field<'a>(field: &str, accepted: &'a [&str]) -> Option<&'a str> {
    accepted
        .iter()
        .copied()
        .map(|candidate| (edit_distance(field, candidate), candidate))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, candidate)| (*distance, candidate.len()))
        .map(|(_, candidate)| candidate)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.chars().count()).collect::<Vec<_>>();
    let mut current = vec![0; previous.len()];
    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.chars().enumerate() {
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + usize::from(left_char != right_char));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.chars().count()]
}

fn dependency_source_label(source: &DependencySource) -> &str {
    match source {
        DependencySource::Path { path } => path,
        DependencySource::Git { url, .. } => url,
    }
}

fn required_string(value: Option<&WsonValue>, field: &str) -> Result<String, String> {
    match value {
        Some(WsonValue::String(value)) => Ok(restore_url_separators(value)),
        _ => Err(format!("{field} must be a string")),
    }
}

fn optional_string(value: Option<&WsonValue>, field: &str) -> Result<Option<String>, String> {
    match value {
        Some(WsonValue::String(value)) => Ok(Some(restore_url_separators(value))),
        Some(_) => Err(format!("{field} must be a string")),
        None => Ok(None),
    }
}

fn restore_url_separators(value: &str) -> String {
    value.to_owned()
}

fn parse_version_string(value: &WsonValue) -> Option<String> {
    match value {
        WsonValue::Version(version) => Some(
            version
                .iter()
                .map(|number| number.to_string())
                .collect::<Vec<_>>()
                .join("."),
        ),
        WsonValue::String(version) => Some(restore_url_separators(version)),
        _ => None,
    }
}

fn validate_dependency_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("dependency name cannot be empty".to_string());
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(format!(
            "invalid dependency name `{name}`: use [A-Za-z_][A-Za-z0-9_]*"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<Manifest, String> {
        parse_manifest(raw, PathBuf::from("vex.ws"))
    }

    #[test]
    fn parses_path_and_git_dependencies() {
        let manifest = parse(
            r#"{ name = "app", version = 0.1.0, dependencies = [
                { name = "local", path = "../local" },
                { name = "remote", git = "https://example.com/remote.git", tag = "v1" }
            ] }"#,
        )
        .expect("manifest must parse");
        assert_eq!(manifest.dependencies.len(), 2);
        assert!(matches!(
            manifest.dependencies[0].source,
            DependencySource::Path { .. }
        ));
        assert!(matches!(
            manifest.dependencies[1].source,
            DependencySource::Git { .. }
        ));
    }

    #[test]
    fn rejects_ambiguous_dependency_sources() {
        let error = parse(
            r#"{ name = "app", dependencies = [
                { name = "bad", path = "../bad", git = "https://example.com/bad.git" }
            ] }"#,
        )
        .expect_err("path and git cannot be combined");
        assert!(error.contains("path") && error.contains("git"), "{error}");
    }

    #[test]
    fn rejects_unknown_and_source_inapplicable_fields() {
        let error = parse(r#"{ name = "app", dependecies = [] }"#)
            .expect_err("unknown root fields must fail");
        assert!(error.contains("unknown field `dependecies`"), "{error}");
        assert!(error.contains("did you mean `dependencies`?"), "{error}");

        let error = parse(
            r#"{ name = "app", dependencies = [
                { name = "local", pth = "../local", path = "../local" }
            ] }"#,
        )
        .expect_err("unknown dependency fields must fail");
        assert!(
            error.contains("dependency `local` contains unknown field `pth`"),
            "{error}"
        );
        assert!(error.contains("did you mean `path`?"), "{error}");

        for selector in ["branch", "tag", "rev"] {
            let error = parse(&format!(
                r#"{{ name = "app", dependencies = [
                    {{ name = "local", path = "../local", {selector} = "main" }}
                ] }}"#
            ))
            .expect_err("Git selectors on path dependencies must fail");
            assert!(
                error.contains(&format!(
                    "field `{selector}` applies only to Git dependencies"
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn rejects_empty_dependency_sources_and_selectors() {
        for (field, dependency) in [
            ("path", r#"{ name = "dep", path = "  " }"#),
            ("git", r#"{ name = "dep", git = "" }"#),
            (
                "branch",
                r#"{ name = "dep", git = "https://example.com/dep.git", branch = " " }"#,
            ),
            (
                "tag",
                r#"{ name = "dep", git = "https://example.com/dep.git", tag = "   " }"#,
            ),
            (
                "rev",
                r#"{ name = "dep", git = "https://example.com/dep.git", rev = "   " }"#,
            ),
        ] {
            let error = parse(&format!(
                "{{ name = \"app\", dependencies = [{dependency}] }}"
            ))
            .expect_err("blank dependency values must fail");
            assert!(error.contains("dependency `dep`"), "{error}");
            assert!(
                error.contains(&format!(
                    "field `{field}` must not be empty or whitespace-only"
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn preserves_nonempty_dependency_values_verbatim() {
        let manifest = parse(
            r#"{ name = "app", dependencies = [
                { name = "local", path = "../local package" },
                { name = "remote", git = "https://example.com/remote.git", branch = "release candidate" }
            ] }"#,
        )
        .expect("nonempty values must parse");
        assert!(matches!(
            &manifest.dependencies[0].source,
            DependencySource::Path { path } if path == "../local package"
        ));
        assert!(matches!(
            &manifest.dependencies[1].source,
            DependencySource::Git { branch: Some(branch), .. } if branch == "release candidate"
        ));
    }

    #[test]
    fn rejects_duplicate_and_root_dependency_names() {
        let duplicate = parse(
            r#"{ name = "app", dependencies = [
                { name = "alpha", path = "../alpha" },
                { name = "beta", path = "../beta" },
                { name = "alpha", path = "../other-alpha" }
            ] }"#,
        )
        .expect_err("nonadjacent duplicate dependencies must fail");
        assert!(
            duplicate.contains("dependency `alpha` is declared more than once"),
            "{duplicate}"
        );

        let root_name = parse(
            r#"{ name = "app", dependencies = [
                { name = "app", path = "../shadow-app" }
            ] }"#,
        )
        .expect_err("a direct dependency must not reuse the root name");
        assert!(root_name.contains("dependency `app`"), "{root_name}");
        assert!(root_name.contains("../shadow-app"), "{root_name}");
        assert!(root_name.contains("root package name"), "{root_name}");
    }
}

#[cfg(test)]
mod format_contract_tests {
    use super::*;
    #[test]
    fn format_and_exact_compiler_requirements_are_explicit() {
        let parse = |text: &str| parse_manifest(text, PathBuf::from("vex.ws"));
        assert_eq!(parse("{name=\"app\"}").unwrap().format, 1);
        let manifest = parse("{format=2,name=\"app\",compiler=\"0.2.1-pre-beta\"}").unwrap();
        assert_eq!(manifest.format, 2);
        assert_eq!(manifest.compiler.as_deref(), Some("0.2.1-pre-beta"));
        for value in ["^0.2.1", ">=0.2.1", "v0.2.1", "0.2.1+build.1", "", "0.02.1"] {
            assert!(parse(&format!("{{name=\"app\",compiler=\"{value}\"}}")).is_err());
        }
    }
}
