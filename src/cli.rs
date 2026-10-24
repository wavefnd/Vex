// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use colorex::Colorize;
use std::io::IsTerminal;

use crate::commands::build::{build, BuildMode};
use crate::commands::check::check;
use crate::commands::fetch::fetch;
use crate::commands::info::info;
use crate::commands::init::init;
use crate::commands::run::run as run_package;
use crate::commands::setup::setup;
use crate::commands::tree::tree;
use crate::commands::update::update;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run() -> i32 {
    match run_reported() {
        Ok(code) => code,
        Err(error) => {
            crate::ui::diagnostic(&error);
            error.category.code()
        }
    }
}

fn run_reported() -> Result<i32, diagnostic::Error> {
    use crate::{
        messages::Messages,
        outcome::{self, Outcome},
    };
    use diagnostic::Error;
    use serde_json::json;
    let mut raw = std::env::args_os().skip(1);
    let mut path = None;
    let mut selection = crate::project::Selection::default();
    let mut args = Vec::new();
    let mut runtime = Vec::new();
    let mut quiet = false;
    let mut verbosity = 0_u8;
    while let Some(arg) = raw.next() {
        if arg == "--" {
            args.push("--".to_owned());
            runtime.extend(raw);
            break;
        }
        if arg == "-p"
            || arg == "--package"
            || arg.to_str().is_some_and(|s| s.starts_with("--package="))
        {
            let inline = arg
                .to_str()
                .and_then(|s| s.strip_prefix("--package="))
                .map(str::to_owned);
            let value = inline
                .or_else(|| raw.next().and_then(|s| s.into_string().ok()))
                .filter(|s| !s.is_empty() && !s.starts_with('-'))
                .ok_or_else(|| Error::usage("--package requires a package name"))?;
            selection.packages.push(value);
        } else if arg == "--workspace" {
            if selection.all_packages {
                return Err(Error::usage("--workspace may only be specified once"));
            }
            selection.all_packages = true;
        } else if arg == "--features" || arg.to_str().is_some_and(|s| s.starts_with("--features="))
        {
            let inline = arg
                .to_str()
                .and_then(|s| s.strip_prefix("--features="))
                .map(str::to_owned);
            let value = inline
                .or_else(|| raw.next().and_then(|s| s.into_string().ok()))
                .ok_or_else(|| Error::usage("--features requires a comma-separated list"))?;
            if value.starts_with('-') || value.split(',').any(str::is_empty) {
                return Err(Error::usage("empty feature name"));
            }
            selection
                .graph
                .features
                .extend(value.split(',').map(str::to_owned));
        } else if arg == "--include-tests" {
            selection.graph.include_tests = true;
        } else if arg == "--no-default-features" {
            if selection.graph.no_default_features {
                return Err(Error::usage(
                    "--no-default-features may only be specified once",
                ));
            }
            selection.graph.no_default_features = true;
        } else if arg == "--quiet" || arg == "-q" {
            quiet = true;
        } else if arg == "--verbose" || arg == "-v" || arg == "-vv" {
            verbosity = (verbosity + if arg == "-vv" { 2 } else { 1 }).min(2);
        } else if arg == "--message-file" || arg == "--manifest-path" {
            let slot = if arg == "--message-file" {
                &mut path
            } else {
                &mut selection.manifest_path
            };
            if slot.is_some() {
                return Err(Error::usage(format!(
                    "{} may only be specified once",
                    arg.to_string_lossy()
                )));
            }
            let value = raw
                .next()
                .filter(|v| !v.is_empty() && !v.to_string_lossy().starts_with('-'))
                .ok_or_else(|| {
                    Error::usage(format!("missing path for {}", arg.to_string_lossy()))
                })?;
            *slot = Some(std::path::PathBuf::from(value));
        } else {
            args.push(arg.into_string().map_err(|_| {
                Error::usage(
                    "Vex options must be UTF-8; program arguments after -- may use OS strings",
                )
            })?);
        }
    }
    if quiet && verbosity > 0 {
        return Err(Error::usage(
            "--quiet and --verbose cannot be used together",
        ));
    }
    crate::ui::configure(quiet, verbosity);
    if selection.all_packages && !selection.packages.is_empty() {
        return Err(Error::usage(
            "--workspace cannot be combined with --package",
        ));
    }
    if (selection.all_packages || !selection.packages.is_empty())
        && !args.first().is_some_and(|s| {
            matches!(
                s.as_str(),
                "test"
                    | "add"
                    | "remove"
                    | "build"
                    | "check"
                    | "run"
                    | "fetch"
                    | "update"
                    | "tree"
                    | "metadata"
                    | "info"
                    | "doctor"
            )
        })
    {
        return Err(Error::usage(
            "package selection requires a workspace project command",
        ));
    }
    if selection.graph.include_tests
        && !args
            .first()
            .is_some_and(|s| matches!(s.as_str(), "fetch" | "update" | "tree" | "metadata"))
    {
        return Err(Error::usage("--include-tests selects a dependency query graph; vex test includes tests automatically"));
    }
    let graph_command = args.first().is_some_and(|s| {
        matches!(
            s.as_str(),
            "test" | "build" | "check" | "run" | "fetch" | "update" | "tree" | "metadata"
        )
    });
    if !graph_command
        && (!selection.graph.features.is_empty() || selection.graph.no_default_features)
    {
        return Err(Error::usage(
            "feature selection requires a build or dependency graph command",
        ));
    }
    if args
        .first()
        .is_some_and(|s| matches!(s.as_str(), "fetch" | "update" | "tree" | "metadata"))
    {
        let mut position = 1;
        while position < args.len() {
            let value = if args[position] == "--target" {
                if position + 1 >= args.len() {
                    return Err(Error::usage("--target requires a triple"));
                }
                let value = args.remove(position + 1);
                args.remove(position);
                Some(value)
            } else if let Some(value) = args[position].strip_prefix("--target=") {
                let value = value.to_owned();
                args.remove(position);
                Some(value)
            } else {
                None
            };
            if let Some(value) = value {
                if selection.graph.target.is_some() {
                    return Err(Error::usage("--target may only be specified once"));
                }
                selection.graph.target =
                    Some(source::graph::Context::new(&value).map_err(Error::usage)?);
            } else {
                position += 1;
            }
        }
    }
    let command = args.first().map(String::as_str).unwrap_or("help");
    let dry_run = matches!(
        command,
        "test" | "build" | "check" | "run" | "clean" | "prune" | "cache"
    ) && args
        .iter()
        .skip(1)
        .take_while(|s| s.as_str() != "--")
        .any(|s| s == "--dry-run");
    let mut messages = Messages::open(path.as_deref(), command, dry_run)?;
    messages.emit(json!({"event":"started"}))?;
    let result = process::install_handlers()
        .map_err(Error::environment)
        .and_then(|()| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                dispatch(&args, &runtime, &selection, &mut messages)
            }))
            .unwrap_or_else(|_| Err(Error::internal("unexpected Vex internal failure")))
        });
    let result = result.map_err(outcome::supervised);
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            crate::ui::diagnostic(&error);
            if let Err(log_error) = messages.diagnostic(&error) {
                crate::ui::diagnostic(&log_error);
                return Ok(if error.category == diagnostic::Category::Cancelled {
                    error.category.code()
                } else {
                    log_error.category.code()
                });
            }
            Outcome::error(&error)
        }
    };
    if let Err(error) = messages.emit(json!({"event":"finished", "success":outcome.code == 0,
        "origin":outcome.origin, "category":outcome.category, "exit_code":outcome.code, "signal":outcome.signal})) {
        if outcome.origin == "program" { crate::ui::warning(&error); } else { crate::ui::diagnostic(&error); }
        // Once the user program has executed, logging cannot replace its outcome.
        if outcome.origin != "program" && outcome.category != "cancelled" { return Ok(error.category.code()); }
    }
    Ok(outcome.code)
}

fn dispatch(
    args: &[String],
    runtime: &[std::ffi::OsString],
    selection: &crate::project::Selection,
    messages: &mut crate::messages::Messages,
) -> Result<crate::outcome::Outcome, diagnostic::Error> {
    use crate::outcome::Outcome;
    use diagnostic::Error;
    #[cfg(debug_assertions)]
    if std::env::var_os("VEX_TEST_INTERNAL_FAILURE").is_some() {
        return Err(Error::internal("injected internal failure"));
    }
    if args.is_empty() {
        print_help()?;
        return Ok(Outcome::success());
    }
    let help_args;
    let args = if args
        .iter()
        .skip(1)
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
        && matches!(
            args[0].as_str(),
            "init"
                | "build"
                | "run"
                | "check"
                | "fetch"
                | "update"
                | "info"
                | "tree"
                | "metadata"
                | "setup"
                | "clean"
                | "prune"
                | "doctor"
        ) {
        help_args = vec![args[0].clone(), "--help".into()];
        help_args.as_slice()
    } else {
        args
    };
    if selection.manifest_path.is_some()
        && !matches!(
            args[0].as_str(),
            "test"
                | "add"
                | "remove"
                | "build"
                | "run"
                | "check"
                | "fetch"
                | "update"
                | "info"
                | "tree"
                | "metadata"
                | "clean"
                | "prune"
                | "doctor"
        )
    {
        return Err(Error::usage(
            "--manifest-path is only valid for project commands",
        ));
    }
    if args
        .iter()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
        && crate::command_spec::help(Some(&args[0]))?
    {
        return Ok(Outcome::success());
    }
    match args[0].as_str() {
        "new" => crate::commands::new::new(&args[1..]).map(|()| Outcome::success()),
        "add" | "remove" => crate::commands::edit::edit(args[0] == "remove", &args[1..], selection)
            .map(|()| Outcome::success()),
        "completions" => crate::command_spec::completions(&args[1..]).map(|()| Outcome::success()),
        "manpage" => crate::command_spec::manpage(&args[1..]).map(|()| Outcome::success()),
        "cache" => crate::commands::cache::cache(&args[1..]).map(|()| Outcome::success()),
        "init" => init(&args[1..]).map(|()| Outcome::success()),
        "test" => build(BuildMode::Test, &args[1..], runtime, selection, messages),
        "build" => build(BuildMode::Build, &args[1..], runtime, selection, messages),
        "run" => run_package(&args[1..], runtime, selection, messages),
        "check" => check(&args[1..], runtime, selection, messages),
        "fetch" => fetch(&args[1..], selection).map(|()| Outcome::success()),
        "update" => update(&args[1..], selection).map(|()| Outcome::success()),
        "info" => info(&args[1..], selection).map(|()| Outcome::success()),
        "tree" => tree(&args[1..], selection).map(|()| Outcome::success()),
        "metadata" => {
            crate::commands::metadata::metadata(&args[1..], selection).map(|()| Outcome::success())
        }
        "setup" => setup(&args[1..]).map(|()| Outcome::success()),
        "clean" => {
            crate::commands::clean::clean(&args[1..], selection).map(|()| Outcome::success())
        }
        "prune" => {
            crate::commands::prune::prune(&args[1..], selection).map(|()| Outcome::success())
        }
        "doctor" => {
            crate::commands::doctor::doctor(&args[1..], selection).map(|()| Outcome::success())
        }
        "--version" | "-V" | "version" if args.len() == 1 => {
            print_version()?;
            Ok(Outcome::success())
        }
        "--help" | "-h" | "help" if args.len() == 1 => {
            print_help()?;
            Ok(Outcome::success())
        }
        "--version" | "-V" | "version" | "--help" | "-h" | "help" => {
            Err(Error::usage(format!("unexpected argument `{}`", args[1])))
        }
        unknown => Err(Error::usage(format!(
            "unknown command `{unknown}`\nhelp: run vex --help"
        ))),
    }
}

fn print_version() -> Result<(), diagnostic::Error> {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        diagnostic::outln!("{} {}", "vex".color("2,161,47"), VERSION.color("2,161,47"));
    } else {
        diagnostic::outln!("vex {VERSION}");
    }
    Ok(())
}

fn print_help() -> Result<(), diagnostic::Error> {
    crate::command_spec::help(None).map(|_| ())
}
