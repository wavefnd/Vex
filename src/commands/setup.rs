use std::process::{Command, Stdio};
use crate::spinner::show_spinner;

pub fn install_wavec(version: Option<&str>) {
    let version_arg = match version {
        Some(ver) => format!("--version {}", ver),
        None => "latest".to_string(),
    };

    println!("⬇️ Installing wavec ({})...", version_arg);

    let full_command = format!(
        "curl -fsSL https://wave-lang.dev/install.sh | bash -s -- {}",
        version_arg
    );

    let child = Command::new("bash")
        .arg("-c")
        .arg(&full_command)
        .stdin(Stdio::null())
        .stdout(Stdio::null()) // 로그 출력 안 함
        .stderr(Stdio::null())
        .spawn()
        .expect("❌ Failed to start wavec installer");

    let status = show_spinner("Installing wavec...", child);

    if status.success() {
        println!("✅ wavec installed successfully.");
    } else {
        eprintln!("❌ wavec installation failed.");
    }
}