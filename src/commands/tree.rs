// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use diagnostic::Error;

use std::collections::BTreeMap;

use crate::ui;
use lockfile::{LockedPackage, LockedSource};
use manifest::Manifest;
use resolver::{resolve, ResolveOptions, UpdatePolicy};

const USAGE: &str = "usage: vex tree [--invert <name>] [--depth <n>] [--locked] [--offline]";

#[derive(Debug, Default, Eq, PartialEq)]
struct TreeOptions {
    locked: bool,
    offline: bool,
    invert: Option<String>,
    depth: Option<usize>,
}

pub fn tree(args: &[String], selection: &crate::project::Selection) -> Result<(), Error> {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        diagnostic::outln!("{USAGE}");
        diagnostic::outln!("  --invert <name>  Show the packages that depend on this package");
        diagnostic::outln!(
            "  --depth <n>      Show at most n edges from the displayed root (0: root only)"
        );
        return Ok(());
    }
    let options = parse_options(args).map_err(Error::usage)?;
    let _project = selection.enter()?;
    run_tree(options, selection.graph.clone(), _project.primary())
}

fn run_tree(
    options: TreeOptions,
    graph: source::graph::Selection,
    manifest: Manifest,
) -> Result<(), Error> {
    let resolution = resolve(
        &manifest,
        ResolveOptions {
            requests: Vec::new(),
            update_bindings: None,
            force_bindings: false,
            additional_contexts: Vec::new(),
            selection: graph,
            dry_run: false,
            update: UpdatePolicy::ReuseLocked,
            locked: options.locked,
            offline: options.offline,
        },
        ui::status,
    )?;
    let mut found = options.invert.is_none();
    for view in resolution.views() {
        let invert = if let Some(requested) = options.invert.as_deref() {
            let matching: Vec<_> = view
                .packages
                .iter()
                .filter(|p| {
                    p.name == requested
                        || p.import_name() == requested
                        || p.id.as_deref() == Some(requested)
                })
                .collect();
            match matching.as_slice() {
                [package] => { found = true; Some(package.name.as_str()) },
                [] if requested == view.root.name => { found = true; Some(requested) },
                [] => continue,
                _ => return Err(Error::resolution("inverse query matches multiple bindings; select a binding key from metadata schema 2")),
            }
        } else {
            None
        };
        diagnostic::out!(
            "{}",
            render_tree(
                &view.root.name,
                &view.root.version,
                &view.dependencies,
                &view.packages,
                invert,
                options.depth
            )
            .map_err(Error::resolution)?
        );
    }
    if !found {
        return Err(Error::resolution(format!(
            "package `{}` is not present in the selected dependency graph",
            options.invert.as_deref().unwrap()
        )));
    }
    Ok(())
}

fn parse_options(args: &[String]) -> Result<TreeOptions, String> {
    let mut options = TreeOptions::default();
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--locked" => options.locked = true,
            "--offline" => options.offline = true,
            "--invert" => {
                if options.invert.is_some() {
                    return Err(format!("--invert may only be specified once\n{USAGE}"));
                }
                let name = arguments
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with('-'))
                    .ok_or_else(|| format!("--invert requires a package name\n{USAGE}"))?;
                options.invert = Some(name.clone());
            }
            "--depth" => {
                if options.depth.is_some() {
                    return Err(format!("--depth may only be specified once\n{USAGE}"));
                }
                let depth = arguments
                    .next()
                    .filter(|value| !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()))
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| format!("--depth requires a non-negative integer\n{USAGE}"))?;
                options.depth = Some(depth);
            }
            _ => {
                return Err(format!("unknown Vex option `{argument}`\n{USAGE}"));
            }
        }
    }
    Ok(options)
}

struct TreeNode {
    label: String,
    edges: Vec<String>,
}

fn render_tree(
    root_name: &str,
    root_version: &str,
    root_dependencies: &[String],
    packages: &[LockedPackage],
    invert: Option<&str>,
    depth: Option<usize>,
) -> Result<String, String> {
    let mut graph = BTreeMap::new();
    graph.insert(
        root_name.to_owned(),
        TreeNode {
            label: format!("{root_name} v{root_version}"),
            edges: root_dependencies.to_vec(),
        },
    );
    for package in packages {
        if graph
            .insert(
                package.name.clone(),
                TreeNode {
                    label: package_label(package),
                    edges: package.dependencies.clone(),
                },
            )
            .is_some()
        {
            return Err(format!(
                "dependency graph contains duplicate package `{}`",
                package.name
            ));
        }
    }
    for node in graph.values() {
        for edge in &node.edges {
            if !graph.contains_key(edge) {
                return Err(format!(
                    "dependency graph references missing package `{edge}` in vex.lock"
                ));
            }
        }
    }
    let selected = invert.unwrap_or(root_name);
    if !graph.contains_key(selected) {
        return Err(format!(
            "package `{selected}` is not present in the dependency graph"
        ));
    }
    if invert.is_some() {
        let mut reversed = graph
            .keys()
            .map(|name| (name.clone(), Vec::new()))
            .collect::<BTreeMap<_, _>>();
        for (name, node) in &graph {
            for dependency in &node.edges {
                reversed.get_mut(dependency).unwrap().push(name.clone());
            }
        }
        for (name, node) in &mut graph {
            node.edges = reversed.remove(name).unwrap();
        }
    }
    for node in graph.values_mut() {
        node.edges.sort();
        node.edges.dedup();
    }
    let mut renderer = TreeRenderer {
        graph: &graph,
        expanded: BTreeMap::new(),
        repeated: false,
        output: String::new(),
    };
    renderer.render_node(selected, "", None, depth.unwrap_or(usize::MAX));
    if renderer.repeated {
        renderer.output.push_str(if invert.is_some() {
            "\n(*) package dependents already shown\n"
        } else {
            "\n(*) package dependencies already shown\n"
        });
    }
    Ok(renderer.output)
}

struct TreeRenderer<'a> {
    graph: &'a BTreeMap<String, TreeNode>,
    // A node first reached at the depth limit must still expand if a later path
    // reaches it with more depth available.
    expanded: BTreeMap<String, usize>,
    repeated: bool,
    output: String,
}

impl TreeRenderer<'_> {
    fn render_node(&mut self, name: &str, prefix: &str, last: Option<bool>, remaining: usize) {
        let graph = self.graph;
        let node = &graph[name];
        let expandable = remaining > 0 && !node.edges.is_empty();
        let already_expanded = expandable
            && self
                .expanded
                .get(name)
                .is_some_and(|previous| *previous >= remaining);
        self.output.push_str(prefix);
        if let Some(last) = last {
            self.output.push_str(if last { "└── " } else { "├── " });
        }
        self.output.push_str(&node.label);
        if already_expanded {
            self.output.push_str(" (*)");
            self.repeated = true;
        }
        self.output.push('\n');
        if !expandable || already_expanded {
            return;
        }
        self.expanded.insert(name.to_owned(), remaining);
        let child_prefix = match last {
            Some(true) => format!("{prefix}    "),
            Some(false) => format!("{prefix}│   "),
            None => String::new(),
        };
        for (index, child) in node.edges.iter().enumerate() {
            self.render_node(
                child,
                &child_prefix,
                Some(index + 1 == node.edges.len()),
                if remaining == usize::MAX {
                    remaining
                } else {
                    remaining - 1
                },
            );
        }
    }
}

fn package_label(package: &LockedPackage) -> String {
    let name = if package.import_name() != package.package_name() {
        format!("{} => {}", package.import_name(), package.package_name())
    } else {
        package.import_name().to_owned()
    };
    match &package.source {
        LockedSource::Path { requested, .. } => {
            format!("{} v{} (path {requested})", name, package.version)
        }
        LockedSource::Git {
            url,
            branch,
            tag,
            rev,
            commit,
            ..
        } => {
            let reference = branch
                .as_ref()
                .map(|value| format!(" branch {value}"))
                .or_else(|| tag.as_ref().map(|value| format!(" tag {value}")))
                .or_else(|| rev.as_ref().map(|value| format!(" rev {value}")))
                .unwrap_or_default();
            let short_commit = &commit[..commit.len().min(7)];
            let subdir = package
                .subdir
                .as_ref()
                .map(|p| format!(" subdir {p}"))
                .unwrap_or_default();
            format!(
                "{} v{} (git {}{reference}{subdir} @ {short_commit})",
                name,
                package.version,
                source::identity(url)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn path_package(name: &str, version: &str, dependencies: &[&str]) -> LockedPackage {
        LockedPackage {
            package: None,
            id: None,
            import_name: None,
            edges: Vec::new(),
            features: Default::default(),
            subdir: None,
            name: name.to_string(),
            version: version.to_string(),
            source: LockedSource::Path {
                requested: format!("../{name}"),
                resolved: PathBuf::from(format!("../{name}")),
            },
            dependencies: dependencies
                .iter()
                .map(|dependency| dependency.to_string())
                .collect(),
        }
    }

    #[test]
    fn renders_sources_commits_and_shared_dependencies() {
        let packages = vec![
            LockedPackage {
                package: None,
                id: None,
                import_name: None,
                edges: Vec::new(),
                features: Default::default(),
                subdir: None,
                name: "alpha".to_string(),
                version: "1.0.0".to_string(),
                source: LockedSource::Git {
                    url: "https://example.com/alpha.git".to_string(),
                    branch: Some("main".to_string()),
                    tag: None,
                    rev: None,
                    commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                    resolved: PathBuf::from(".vex/deps/alpha"),
                },
                dependencies: vec!["shared".to_string()],
            },
            path_package("beta", "2.0.0", &["shared"]),
            path_package("shared", "0.2.0", &["leaf"]),
            path_package("leaf", "0.1.0", &[]),
        ];

        let rendered = render_tree(
            "app",
            "0.1.0",
            &["beta".to_string(), "alpha".to_string()],
            &packages,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            rendered,
            concat!(
                "app v0.1.0\n",
                "├── alpha v1.0.0 (git https://example.com/alpha.git branch main @ 0123456)\n",
                "│   └── shared v0.2.0 (path ../shared)\n",
                "│       └── leaf v0.1.0 (path ../leaf)\n",
                "└── beta v2.0.0 (path ../beta)\n",
                "    └── shared v0.2.0 (path ../shared) (*)\n",
                "\n",
                "(*) package dependencies already shown\n",
            )
        );
    }

    #[test]
    fn rejects_missing_graph_edges() {
        let error =
            render_tree("app", "0.1.0", &["missing".to_string()], &[], None, None).unwrap_err();
        assert!(error.contains("missing package `missing`"), "{error}");
    }

    #[test]
    fn parses_locked_and_offline_options() {
        assert_eq!(
            parse_options(&["--locked".to_string(), "--offline".to_string()]).unwrap(),
            TreeOptions {
                locked: true,
                offline: true,
                ..TreeOptions::default()
            }
        );
    }

    fn diamond() -> Vec<LockedPackage> {
        vec![
            path_package("alpha", "0.1.0", &["shared"]),
            path_package("beta", "0.1.0", &["shared"]),
            path_package("shared", "0.1.0", &["leaf"]),
            path_package("leaf", "0.1.0", &[]),
        ]
    }

    #[test]
    fn inverse_queries_include_all_parents_and_the_project() {
        let packages = diamond();
        let roots = vec!["beta".into(), "alpha".into()];
        let rendered = render_tree("app", "0.1.0", &roots, &packages, Some("leaf"), None).unwrap();
        assert_eq!(
            rendered,
            concat!(
                "leaf v0.1.0 (path ../leaf)\n",
                "└── shared v0.1.0 (path ../shared)\n",
                "    ├── alpha v0.1.0 (path ../alpha)\n",
                "    │   └── app v0.1.0\n",
                "    └── beta v0.1.0 (path ../beta)\n",
                "        └── app v0.1.0\n",
            )
        );
        assert_eq!(
            render_tree("app", "0.1.0", &roots, &packages, Some("app"), None).unwrap(),
            "app v0.1.0\n"
        );
    }

    #[test]
    fn inverse_shared_dependents_are_only_expanded_once() {
        let mut packages = diamond();
        packages.push(path_package("top", "0.1.0", &["beta", "alpha"]));
        let rendered = render_tree(
            "app",
            "0.1.0",
            &["top".into()],
            &packages,
            Some("shared"),
            None,
        )
        .unwrap();
        assert_eq!(
            rendered,
            concat!(
                "shared v0.1.0 (path ../shared)\n",
                "├── alpha v0.1.0 (path ../alpha)\n",
                "│   └── top v0.1.0 (path ../top)\n",
                "│       └── app v0.1.0\n",
                "└── beta v0.1.0 (path ../beta)\n",
                "    └── top v0.1.0 (path ../top) (*)\n",
                "\n(*) package dependents already shown\n",
            )
        );
    }

    #[test]
    fn depth_limits_apply_in_both_directions_and_include_zero() {
        let packages = diamond();
        let roots = vec!["alpha".into(), "beta".into()];
        assert_eq!(
            render_tree("app", "0.1.0", &roots, &packages, None, Some(0)).unwrap(),
            "app v0.1.0\n"
        );
        assert_eq!(
            render_tree("app", "0.1.0", &roots, &packages, None, Some(1)).unwrap(),
            concat!(
                "app v0.1.0\n",
                "├── alpha v0.1.0 (path ../alpha)\n",
                "└── beta v0.1.0 (path ../beta)\n",
            )
        );
        assert_eq!(
            render_tree("app", "0.1.0", &roots, &packages, Some("leaf"), Some(0)).unwrap(),
            "leaf v0.1.0 (path ../leaf)\n"
        );
        assert_eq!(
            render_tree("app", "0.1.0", &roots, &packages, Some("shared"), Some(1)).unwrap(),
            concat!(
                "shared v0.1.0 (path ../shared)\n",
                "├── alpha v0.1.0 (path ../alpha)\n",
                "└── beta v0.1.0 (path ../beta)\n",
            )
        );
    }

    #[test]
    fn shallower_paths_expand_nodes_previously_cut_off_by_depth() {
        let packages = diamond();
        let rendered = render_tree(
            "app",
            "0.1.0",
            &["alpha".into(), "shared".into()],
            &packages,
            None,
            Some(2),
        )
        .unwrap();
        assert_eq!(
            rendered,
            concat!(
                "app v0.1.0\n",
                "├── alpha v0.1.0 (path ../alpha)\n",
                "│   └── shared v0.1.0 (path ../shared)\n",
                "└── shared v0.1.0 (path ../shared)\n",
                "    └── leaf v0.1.0 (path ../leaf)\n",
            )
        );
    }

    #[test]
    fn rendering_is_independent_of_package_and_edge_order() {
        let mut packages = diamond();
        let roots = vec!["beta".into(), "alpha".into(), "alpha".into()];
        let rendered = render_tree("app", "0.1.0", &roots, &packages, None, None).unwrap();
        let inverse = render_tree("app", "0.1.0", &roots, &packages, Some("leaf"), None).unwrap();
        packages.reverse();
        for package in &mut packages {
            package.dependencies.reverse();
        }
        assert_eq!(
            render_tree(
                "app",
                "0.1.0",
                &["alpha".into(), "beta".into()],
                &packages,
                None,
                None
            )
            .unwrap(),
            rendered
        );
        assert_eq!(
            render_tree(
                "app",
                "0.1.0",
                &["alpha".into(), "beta".into()],
                &packages,
                Some("leaf"),
                None
            )
            .unwrap(),
            inverse
        );
    }

    #[test]
    fn unknown_inverse_names_fail_even_at_zero_depth() {
        let error = render_tree("app", "0.1.0", &[], &[], Some("typo"), Some(0)).unwrap_err();
        assert_eq!(
            error,
            "package `typo` is not present in the dependency graph"
        );
    }

    #[test]
    fn invalid_and_repeated_query_options_are_usage_errors() {
        for args in [
            vec!["--depth"],
            vec!["--depth", "-1"],
            vec!["--depth", "+1"],
            vec!["--depth", "1.5"],
            vec!["--depth", "184467440737095516160"],
            vec!["--depth", "0", "--depth", "1"],
            vec!["--invert"],
            vec!["--invert", "--offline"],
            vec!["--invert", "alpha", "--invert", "beta"],
        ] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            assert!(parse_options(&args).is_err(), "{args:?}");
        }
        let args: Vec<_> = ["--depth", "0", "--offline", "--invert", "alpha", "--locked"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            parse_options(&args).unwrap(),
            TreeOptions {
                locked: true,
                offline: true,
                invert: Some("alpha".into()),
                depth: Some(0)
            }
        );
    }
}
