mod cli;
mod commands;
mod messages;
mod native;
mod outcome;
mod project;
mod ui;

fn main() {
    std::process::exit(cli::run());
}
