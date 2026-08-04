use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const UNIX_INSTALLER_URL: &str = "https://wave-lang.dev/install.sh";
const WINDOWS_INSTALLER_URL: &str = "https://wave-lang.dev/install.ps1";

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

    let cleanup_path;
    let mut child = if cfg!(windows) {
        cleanup_path = Some(download_windows_installer()?);
        spawn_windows_installer(cleanup_path.as_ref().unwrap(), &installer_args)?
    } else {
        cleanup_path = Some(download_unix_installer()?);
        spawn_unix_installer(cleanup_path.as_ref().unwrap(), &installer_args)?
    };

    let status = child
        .wait()
        .map_err(|e| format!("failed to wait for wavec installer: {e}"))?;

    if let Some(path) = cleanup_path {
        let _ = fs::remove_file(path);
    }

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
    let script = env::temp_dir().join(format!("wave-install-{}.sh", std::process::id()));
    let status = Command::new("curl")
        .args(["-fsSL", UNIX_INSTALLER_URL, "-o"])
        .arg(&script)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("failed to start curl for wavec installer: {e}"))?;

    if !status.success() {
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
    let script = env::temp_dir().join(format!("wave-install-{}.ps1", std::process::id()));
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
        .status()
        .map_err(|e| format!("failed to start PowerShell for wavec installer download: {e}"))?;

    if !status.success() {
        return Err(format!("failed to download wavec installer: {status}"));
    }

    Ok(script)
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
