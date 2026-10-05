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
            crate::ui::error(&error);
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
    while let Some(arg) = raw.next() {
        if arg == "--" {
            args.push("--".to_owned());
            runtime.extend(raw);
            break;
        }
        if arg == "--message-file" || arg == "--manifest-path" {
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
    let command = args.first().map(String::as_str).unwrap_or("help");
    let dry_run = matches!(command, "build" | "check" | "run")
        && args
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
            crate::ui::error(&error);
            if let Err(log_error) = messages.diagnostic(&error) {
                crate::ui::error(&log_error);
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
        if outcome.origin == "program" { crate::ui::warning(&error); } else { crate::ui::error(&error); }
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
        ) {
        help_args = vec![args[0].clone(), "--help".into()];
        help_args.as_slice()
    } else {
        args
    };
    if selection.manifest_path.is_some()
        && !matches!(
            args[0].as_str(),
            "build" | "run" | "check" | "fetch" | "update" | "info" | "tree" | "metadata"
        )
    {
        return Err(Error::usage(
            "--manifest-path is only valid for project commands",
        ));
    }
    match args[0].as_str() {
        "init" => init(&args[1..]).map(|()| Outcome::success()),
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
    diagnostic::outln!("Vex - Wave package manager");
    diagnostic::outln!();
    diagnostic::outln!("Usage:");
    diagnostic::outln!(
        "  vex [--message-file <new-path>] [--manifest-path <vex.ws>] <command> [options]"
    );
    diagnostic::outln!("  vex init [--lib]");
    diagnostic::outln!(
        "  vex build [--target <triple>] [--release] [--dry-run] [--locked] [--offline]"
    );
    diagnostic::outln!(
        "  vex run [--target <triple>] [--release] [--dry-run] [--locked] [--offline] [-- <args...>]"
    );
    diagnostic::outln!(
        "  vex check [--target <triple>] [--release] [--dry-run] [--locked] [--offline]"
    );
    diagnostic::outln!("  vex fetch [--locked] [--offline]");
    diagnostic::outln!("  vex update [<package>...]");
    diagnostic::outln!("  vex info");
    diagnostic::outln!("  vex metadata [--format=json] [--locked] [--offline]");
    diagnostic::outln!("  vex tree [--locked] [--offline]");
    diagnostic::outln!("  vex setup wavec [--version <version>] [--script-fallback]");
    diagnostic::outln!("  vex --version");
    Ok(())
}
