use colorex::Colorize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn version() -> &'static str {
    let version = VERSION;
    version
}

pub fn version_vex() {
    println!("{} {}", "vex".color("2,161,47"), version().color("2,161,47"));
}