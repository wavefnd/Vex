use commands::check::check;
use commands::info::info;
use commands::init::init;
use commands::run::run;
use crate::commands::setup::install_wavec;
use crate::version::version_vex;

mod commands;
mod version;
mod spinner;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let is_lib = args.iter().any(|arg| arg == "--lib");

    if args.len() >= 2 && args[1] == "init" {
        init(is_lib);
    } else if args.len() >= 2 && args[1] == "info" {
        info();
    } else if args.len() >= 2 && args[1] == "run" {
        run();
    } else if args.len() >= 2 && args[1] == "check" {
        check();
    } else if args.len() >= 2 && (args[1] == "--version" || args[1] == "-V") {
        version_vex();
    } else if args.len() > 3 && args[1] == "setup" && args[2] == "wavec" {
        let version = if args.len() > 5 && args[3] == "--version" {
            Some(args[4].as_str())
        } else {
            None
        };

        install_wavec(version);
    } else {
        println!("❌ Unknown command.");
    }
}
