use std::env;
use std::fs;
use std::path::Path;

use crate::lockfile::write_lockfile;
use crate::manifest::{render_new_manifest, Manifest, MANIFEST_FILE};

pub fn init(is_lib: bool) {
    if let Err(err) = run_init(is_lib) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
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

    let manifest = Manifest::load()?;
    write_lockfile(&manifest)?;

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
