use crate::commands::build::{build, BuildMode};
use crate::commands::check::check;
use crate::commands::deps::fetch;
use crate::commands::info::info;
use crate::commands::init::init;
use crate::commands::run::run;
use crate::commands::setup::install_wavec;
use crate::version::version_vex;

mod commands;
mod lockfile;
mod manifest;
mod resolver;
mod ui;
mod validate;
mod version;
mod wavec;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print_help();
        return;
    }

    match args[0].as_str() {
        "init" => init(&args[1..]),
        "build" => build(BuildMode::Build, &args[1..]),
        "run" => run(&args[1..]),
        "check" => check(&args[1..]),
        "fetch" => fetch(false, &args[1..]),
        "update" => fetch(true, &args[1..]),
        "info" => info(&args[1..]),
        "setup" => setup(&args[1..]),
        "--version" | "-V" | "version" if args.len() == 1 => version_vex(),
        "--help" | "-h" | "help" if args.len() == 1 => print_help(),
        "--version" | "-V" | "version" | "--help" | "-h" | "help" => {
            eprintln!("error: unexpected argument `{}`", args[1]);
            std::process::exit(2);
        }
        unknown => {
            eprintln!("error: unknown command `{unknown}`");
            print_help();
            std::process::exit(2);
        }
    }
}

fn setup(args: &[String]) {
    if matches!(args, [help] if help == "-h" || help == "--help")
        || matches!(args, [wavec, help] if wavec == "wavec" && (help == "-h" || help == "--help"))
    {
        println!("usage: vex setup wavec [--version <version>]");
        return;
    }
    if args.first().map(String::as_str) != Some("wavec") {
        eprintln!("error: usage: vex setup wavec [--version <version>]");
        std::process::exit(2);
    }

    let mut version: Option<&str> = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                if version.is_some() {
                    eprintln!("error: `--version` may only be specified once");
                    std::process::exit(2);
                }
                if i + 1 >= args.len() {
                    eprintln!("error: missing value for --version");
                    std::process::exit(2);
                }
                if args[i + 1].is_empty() || args[i + 1].starts_with('-') {
                    eprintln!("error: `--version` must be a version value, not an option");
                    std::process::exit(2);
                }
                version = Some(args[i + 1].as_str());
                i += 2;
            }
            unknown => {
                eprintln!("error: unknown setup option `{unknown}`");
                eprintln!("usage: vex setup wavec [--version <version>]");
                std::process::exit(2);
            }
        }
    }

    install_wavec(version);
}

fn print_help() {
    println!("Vex - Wave package manager");
    println!();
    println!("Usage:");
    println!("  vex init [--lib]");
    println!("  vex build [--target <triple>] [--release] [--dry-run] [--locked] [--offline]");
    println!(
        "  vex run [--target <triple>] [--release] [--dry-run] [--locked] [--offline] [-- <args...>]"
    );
    println!("  vex check [--target <triple>] [--release] [--dry-run] [--locked] [--offline]");
    println!("  vex fetch [--locked] [--offline]");
    println!("  vex update [<package>...]");
    println!("  vex info");
    println!("  vex setup wavec [--version <version>]");
    println!("  vex --version");
}
