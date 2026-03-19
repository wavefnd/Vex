use std::{thread, time::Duration, io::{self, Write}};

pub fn show_spinner(message: &str, mut child: std::process::Child) -> std::process::ExitStatus {
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut i = 0;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                print!("\r✓ {}{}\n", message, " ".repeat(20));
                io::stdout().flush().unwrap();
                return status;
            }
            Ok(None) => {
                print!("\r{} {}", frames[i % frames.len()], message);
                io::stdout().flush().unwrap();
                thread::sleep(Duration::from_millis(100));
                i += 1;
            }
            Err(e) => {
                eprintln!("❌ Failed to wait on child process: {}", e);
                std::process::exit(1);
            }
        }
    }
}