use std::time::Instant;

use crate::manifest::Manifest;
use crate::resolver::{resolve, ResolveOptions};
use crate::ui;

#[derive(Debug, Default)]
struct DependencyOptions {
    locked: bool,
    offline: bool,
}

pub fn fetch(update: bool, args: &[String]) {
    if let Err(err) = run_fetch(update, args) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_fetch(update: bool, args: &[String]) -> Result<(), String> {
    let options = parse_options(update, args)?;
    if update && options.locked {
        return Err(
            "`--locked` cannot be used with `vex update` because update rewrites vex.lock"
                .to_string(),
        );
    }
    if update && options.offline {
        return Err(
            "`--offline` cannot be used with `vex update` because update refreshes Git refs"
                .to_string(),
        );
    }
    let started = Instant::now();
    let manifest = Manifest::load()?;
    let resolution = resolve(
        &manifest,
        ResolveOptions {
            dry_run: false,
            update,
            locked: options.locked,
            offline: options.offline,
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

fn parse_options(update: bool, args: &[String]) -> Result<DependencyOptions, String> {
    let mut options = DependencyOptions::default();
    for argument in args {
        match argument.as_str() {
            "--locked" => options.locked = true,
            "--offline" => options.offline = true,
            "-h" | "--help" => {
                return Err(if update {
                    "usage: vex update".to_string()
                } else {
                    "usage: vex fetch [--locked] [--offline]".to_string()
                });
            }
            _ => {
                return Err(format!(
                    "unknown Vex option `{argument}`\nusage: vex {}",
                    if update {
                        "update"
                    } else {
                        "fetch [--locked] [--offline]"
                    }
                ));
            }
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_fetch_policy_options() {
        let options = parse_options(false, &strings(&["--locked", "--offline"]))
            .expect("fetch policy options must parse");
        assert!(options.locked);
        assert!(options.offline);
    }
}
