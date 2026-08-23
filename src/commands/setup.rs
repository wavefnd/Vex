use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const UNIX_INSTALLER_URL: &str = "https://wave-lang.dev/install.sh";
const WINDOWS_INSTALLER_URL: &str = "https://wave-lang.dev/install.ps1";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub fn install_wavec(version: Option<&str>) {
    if let Err(err) = run_install_wavec(version) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_install_wavec(version: Option<&str>) -> Result<(), String> {
    let installer_args = installer_args(version);
    let display_version = if installer_args.len() == 2 {
        installer_args[1].as_str()
    } else {
        "latest"
    };

    println!("installing wavec {display_version}");

    let installer = if cfg!(windows) {
        download_windows_installer()?
    } else {
        download_unix_installer()?
    };
    let child = if cfg!(windows) {
        spawn_windows_installer(&installer, &installer_args)
    } else {
        spawn_unix_installer(&installer, &installer_args)
    };
    let mut child = match child {
        Ok(child) => child,
        Err(err) => {
            let _ = fs::remove_file(&installer);
            return Err(err);
        }
    };

    let status = child.wait();
    fs::remove_file(&installer).map_err(|e| {
        format!(
            "failed to remove temporary wavec installer `{}`: {e}",
            installer.display()
        )
    })?;
    let status = status.map_err(|e| format!("failed to wait for wavec installer: {e}"))?;

    if status.success() {
        println!("wavec installed successfully");
        Ok(())
    } else {
        Err(format!("wavec installation failed with status: {status}"))
    }
}

fn installer_args(version: Option<&str>) -> Vec<String> {
    match version {
        Some(ver) => vec!["--version".to_string(), ver.to_string()],
        None => vec!["latest".to_string()],
    }
}

fn download_unix_installer() -> Result<PathBuf, String> {
    let (script, output) = create_installer_file("sh")?;
    let status = Command::new("curl")
        .args(["-fsSL", UNIX_INSTALLER_URL])
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .status();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            let _ = fs::remove_file(&script);
            return Err(format!("failed to start curl for wavec installer: {error}"));
        }
    };

    if !status.success() {
        let _ = fs::remove_file(&script);
        return Err(format!("failed to download wavec installer: {status}"));
    }

    Ok(script)
}

fn spawn_unix_installer(script: &PathBuf, args: &[String]) -> Result<std::process::Child, String> {
    Command::new("bash")
        .arg(script)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start wavec installer: {e}"))
}

fn download_windows_installer() -> Result<PathBuf, String> {
    let (script, output) = create_installer_file("ps1")?;
    drop(output);
    let shell = windows_shell();
    let status = Command::new(shell)
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "Invoke-WebRequest -UseBasicParsing -Uri $args[0] -OutFile $args[1]",
            WINDOWS_INSTALLER_URL,
        ])
        .arg(&script)
        .stdin(Stdio::null())
        .status();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            let _ = fs::remove_file(&script);
            return Err(format!(
                "failed to start PowerShell for wavec installer download: {error}"
            ));
        }
    };

    if !status.success() {
        let _ = fs::remove_file(&script);
        return Err(format!("failed to download wavec installer: {status}"));
    }

    Ok(script)
}

fn create_installer_file(extension: &str) -> Result<(PathBuf, File), String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for _ in 0..100 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "vex-wave-install-{}-{timestamp}-{id}.{extension}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create temporary wavec installer `{}`: {error}",
                    path.display()
                ))
            }
        }
    }
    Err("failed to allocate a unique temporary path for the wavec installer".to_string())
}

fn spawn_windows_installer(
    script: &PathBuf,
    args: &[String],
) -> Result<std::process::Child, String> {
    Command::new(windows_shell())
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start wavec PowerShell installer: {e}"))
}

fn windows_shell() -> &'static str {
    if cfg!(windows) {
        "powershell.exe"
    } else {
        "pwsh"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_arguments_preserve_an_explicit_version() {
        assert_eq!(installer_args(None), ["latest"]);
        assert_eq!(
            installer_args(Some("0.2.0-pre-beta")),
            ["--version", "0.2.0-pre-beta"]
        );
    }

    #[test]
    fn installer_temporary_files_are_unique_and_exclusive() {
        let (first_path, first_file) =
            create_installer_file("test").expect("first temporary installer must be created");
        let (second_path, second_file) =
            create_installer_file("test").expect("second temporary installer must be created");
        assert_ne!(first_path, second_path);
        drop(first_file);
        drop(second_file);
        fs::remove_file(first_path).expect("first temporary installer must be removed");
        fs::remove_file(second_path).expect("second temporary installer must be removed");
    }
}
