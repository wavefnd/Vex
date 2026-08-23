use crate::manifest::{DependencySource, Manifest};

pub fn info(args: &[String]) {
    if matches!(args, [help] if help == "-h" || help == "--help") {
        println!("usage: vex info");
        return;
    }
    if let Some(argument) = args.first() {
        eprintln!("error: unexpected argument `{argument}`\nusage: vex info");
        std::process::exit(2);
    }
    if let Err(err) = run_info() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_info() -> Result<(), String> {
    let manifest = Manifest::load()?;
    println!("Vex project info");
    println!("name: {}", manifest.name);
    println!("version: {}", manifest.version);
    println!("type: {}", if manifest.lib { "library" } else { "binary" });
    println!("manifest: {}", manifest.source_path.to_string_lossy());
    if let Some(description) = manifest.description.as_ref() {
        println!("description: {description}");
    }
    if let Some(author) = manifest.author.as_ref() {
        println!("author: {author}");
    }
    if let Some(license) = manifest.license.as_ref() {
        println!("license: {license}");
    }
    println!("dependencies: {}", manifest.dependencies.len());

    for dep in manifest.dependencies {
        match dep.source {
            DependencySource::Path { path } => match dep.version {
                Some(version) => println!("  {} {} path {}", dep.name, version, path),
                None => println!("  {} path {}", dep.name, path),
            },
            DependencySource::Git {
                url,
                branch,
                tag,
                rev,
            } => {
                let reference = branch
                    .map(|value| format!(" branch {value}"))
                    .or_else(|| tag.map(|value| format!(" tag {value}")))
                    .or_else(|| rev.map(|value| format!(" rev {value}")))
                    .unwrap_or_default();
                match dep.version {
                    Some(version) => {
                        println!("  {} {} git {}{}", dep.name, version, url, reference)
                    }
                    None => println!("  {} git {}{}", dep.name, url, reference),
                }
            }
        }
    }
    Ok(())
}
