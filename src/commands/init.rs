use std::env;
use std::fs;
use std::path::Path;

use crate::lockfile::{write_lockfile, Lockfile};
use crate::manifest::{render_new_manifest, Manifest, MANIFEST_FILE};

pub fn init(args: &[String]) {
    let is_lib = match parse_options(args) {
        Ok(Some(is_lib)) => is_lib,
        Ok(None) => {
            println!("usage: vex init [--lib]");
            return;
        }
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("usage: vex init [--lib]");
            std::process::exit(2);
        }
    };
    if let Err(err) = run_init(is_lib) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn parse_options(args: &[String]) -> Result<Option<bool>, String> {
    let mut is_lib = false;
    for argument in args {
        match argument.as_str() {
            "--lib" if !is_lib => is_lib = true,
            "--lib" => return Err("`--lib` may only be specified once".to_string()),
            "-h" | "--help" if args.len() == 1 => return Ok(None),
            unknown => return Err(format!("unknown Vex option `{unknown}`")),
        }
    }
    Ok(Some(is_lib))
}

fn run_init(is_lib: bool) -> Result<(), String> {
    let manifest_path = Path::new(MANIFEST_FILE);
    let src_dir = Path::new("src");

    if manifest_path.exists() {
        return Err(format!("`{MANIFEST_FILE}` already exists."));
    }

    let project_name = env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "wave_project".to_string());

    let author = get_username().unwrap_or_else(|| "unknown".to_string());

    fs::create_dir_all(src_dir).map_err(|e| format!("failed to create src/: {e}"))?;

    let source_file = if is_lib { "lib.wave" } else { "main.wave" };
    let source_path = src_dir.join(source_file);
    if source_path.exists() {
        return Err(format!(
            "`{}` already exists.",
            source_path.to_string_lossy()
        ));
    }

    let source_template = if is_lib {
        "fun greet() {\n    println(\"Hello from library\");\n}\n"
    } else {
        "fun main() {\n    println(\"Hello World\");\n}\n"
    };

    fs::write(&source_path, source_template)
        .map_err(|e| format!("failed to write `{}`: {e}", source_path.to_string_lossy()))?;

    let manifest_text = render_new_manifest(&project_name, &author, is_lib);
    fs::write(manifest_path, manifest_text)
        .map_err(|e| format!("failed to write `{MANIFEST_FILE}`: {e}"))?;

    fs::create_dir_all(".vex/deps").map_err(|e| format!("failed to create .vex/deps: {e}"))?;

    let _manifest = Manifest::load()?;
    write_lockfile(&Lockfile::empty())?;

    println!("initialized Wave project");
    println!("created {MANIFEST_FILE}, vex.lock, and src/{source_file}");
    Ok(())
}

fn get_username() -> Option<String> {
    if cfg!(windows) {
        env::var("USERNAME").ok()
    } else {
        env::var("USER").ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn init_options_do_not_treat_help_or_unknown_flags_as_creation() {
        assert_eq!(parse_options(&strings(&["--help"])), Ok(None));
        assert!(parse_options(&strings(&["--unknown"])).is_err());
        assert!(parse_options(&strings(&["--lib", "--lib"])).is_err());
        assert_eq!(parse_options(&strings(&["--lib"])), Ok(Some(true)));
    }
}
