use std::fs;
use std::process::{Command, Stdio};

use crate::{create_installer_file, remove_installer};

const INSTALLER_URL: &str = "https://wave-lang.dev/install.ps1";

pub(crate) fn install(args: &[String]) -> Result<(), String> {
    let (script, output) = create_installer_file("ps1")?;
    drop(output);
    let status = process::status(
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Invoke-WebRequest -UseBasicParsing -Uri $args[0] -OutFile $args[1]",
                INSTALLER_URL,
            ])
            .arg(&script)
            .stdin(Stdio::null()),
        Some(std::time::Duration::from_secs(300)),
        false,
    );
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

    let result = process::status(
        Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .args(args)
            .stdin(Stdio::null()),
        Some(std::time::Duration::from_secs(900)),
        false,
    );
    let cleanup = remove_installer(&script);
    crate::finish_installer(
        result.map(|status| (status.success(), status.to_string())),
        cleanup,
    )
}
