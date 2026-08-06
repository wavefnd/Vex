use std::time::Instant;

use crate::manifest::Manifest;
use crate::resolver::{resolve, ResolveOptions};
use crate::ui;

pub fn fetch(update: bool, args: &[String]) {
    if let Err(err) = run_fetch(update, args) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_fetch(update: bool, args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(format!(
            "unexpected argument `{}`\nusage: vex {}",
            args[0],
            if update { "update" } else { "fetch" }
        ));
    }
    let started = Instant::now();
    let manifest = Manifest::load()?;
    let resolution = resolve(
        &manifest,
        ResolveOptions {
            dry_run: false,
            update,
        },
    )?;
    ui::status(
        "Finished",
        format!(
            "resolved {} package{} in {:.2}s",
            resolution.package_count(),
            if resolution.package_count() == 1 {
                ""
            } else {
                "s"
            },
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}
