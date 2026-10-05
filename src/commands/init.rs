use diagnostic::Error;
use std::env;
use std::fs;
use std::path::Path;

use lockfile::Lockfile;
use manifest::{render_new_manifest, validate_package_name, MANIFEST_FILE};

pub fn init(args: &[String]) -> Result<(), Error> {
    let is_lib = match parse_options(args).map_err(Error::usage)? {
        Some(value) => value,
        None => {
            diagnostic::outln!("usage: vex init [--lib]");
            return Ok(());
        }
    };
    run_init(is_lib)
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

fn run_init(is_lib: bool) -> Result<(), Error> {
    let directory = env::current_dir()?;
    let project_name = directory
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::resolution("project directory must have a UTF-8 package name"))?;
    validate_package_name(project_name).map_err(Error::resolution)?;
    // Validate the name before creating even the coordination directory.
    let guard = state::Guard::acquire(false, crate::ui::status).map_err(Error::environment)?;
    resolver::recover_project(&guard, false)?;
    let author = get_username().unwrap_or_else(|| "unknown".to_string());
    let source_file = if is_lib { "lib.wave" } else { "main.wave" };
    let source_name = format!("src/{source_file}");
    let source_template = if is_lib {
        "pub fun greet() {\n    println(\"Hello from library\");\n}\n"
    } else {
        "fun main() {\n    println(\"Hello World\");\n}\n"
    };
    let mut files = vec![
        (source_name.as_str(), source_template.to_string()),
        ("vex.lock", lockfile::encode(Lockfile::empty())),
    ];
    let created_ignore = fs::symlink_metadata(Path::new(".gitignore")).is_err();
    if created_ignore {
        files.push((".gitignore", "/target/\n/.vex/\n".into()));
    }
    files.push((
        MANIFEST_FILE,
        render_new_manifest(project_name, &author, is_lib),
    ));
    state::initialize(&guard, &files).map_err(Error::environment)?;

    diagnostic::outln!("initialized Wave project");
    diagnostic::outln!("created {MANIFEST_FILE}, vex.lock, and src/{source_file}");
    if created_ignore {
        diagnostic::outln!("created .gitignore");
    }
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
