// Standalone process fixture: rustc compiles this without Cargo dependencies.
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

fn checkpoint(name: &str) {
    if let Ok(address) = env::var(format!("VEX_TEST_{name}")) {
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
        socket.write_all(&[1]).unwrap();
        socket.read_exact(&mut [0]).unwrap();
    }
}

fn main() {
    if env::current_exe().unwrap().file_stem().unwrap() == "git" {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|a| a == "--version") {
        if env::var_os("NO_COLOR").is_some() {
            println!("wavec 0.2.1-pre-beta");
        } else {
            println!("\x1b[32mwavec\x1b[0m \x1b[32m0.2.1-pre-beta\x1b[0m");
        }
        return;
    }
    if env::current_exe().unwrap().file_stem().unwrap() == "runner" {
        let status = std::process::Command::new(&args[0]).args(&args[1..]).status().unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }
    if env::current_exe().unwrap().file_stem().unwrap() == "program" {
        if let Ok(vex) = env::var("VEX_TEST_NESTED") {
            assert!(std::process::Command::new(vex).args(["fetch", "--locked", "--offline"]).status().unwrap().success());
        }
        if env::var_os("VEX_TEST_RUN").is_some() { fs::write("program.pid", std::process::id().to_string()).unwrap(); }
        checkpoint("RUN");
        if env::var_os("VEX_TEST_ECHO_STDIN").is_some() {
            let mut line = String::new(); std::io::stdin().read_line(&mut line).unwrap();
            print!("PROGRAM_STDIN:{line}"); eprintln!("PROGRAM_STDERR");
        }
        println!("PROGRAM_CWD:{:?}", env::current_dir().unwrap());
        #[cfg(unix)]
        if env::var_os("VEX_TEST_ARGUMENT_BYTES").is_some() {
            use std::os::unix::ffi::OsStrExt;
            for arg in &args { println!("ARG_BYTES:{:?}", arg.as_bytes()); }
        }
        println!("FAKE_WAVEC_EXECUTED {:?}", args);
        if let Ok(code) = env::var("VEX_TEST_RUN_EXIT") { std::process::exit(code.parse().unwrap()); }
        return;
    }
    let args: Vec<_> = args.into_iter().map(|s| s.into_string().unwrap()).collect();
    if let Ok(path) = env::var("FAKE_WAVEC_LOG") {
        let mut log = OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(log, "{}", args.join(" ")).unwrap();
    }
    if args.first().is_some_and(|s| s == "print") {
        assert_eq!(args, ["print", "supported-targets", "--format=json"]);
        println!("{}", env::var("FAKE_TARGETS").unwrap_or_else(|_| "[\"x86_64-unknown-linux-gnu\",\"aarch64-unknown-linux-gnu\"]".into()));
        return;
    }
    let generation = args.iter().find_map(|a| a.strip_prefix("--target-dir=")).map(PathBuf::from);
    if args.iter().any(|a| a == "--dry-run") {
        checkpoint("PLAN");
        let mode = if args.iter().any(|a| a == "--run") { "build+run" } else { "build" };
        let schema = env::var("FAKE_SCHEMA").unwrap_or_else(|_| "1".into());
        let output = generation.map(|p| p.join(if cfg!(windows) { "program.exe" } else { "program" }));
        let (link, execute) = if let Some(output) = output {
            let output = env::var("FAKE_OUTPUT").unwrap_or_else(|_| output.to_string_lossy().into_owned());
            let link = format!("{{\"output\":{output:?},\"inputs\":[],\"program\":\"linker\",\"args\":[]}}");
            let runtime = args.iter().position(|a| a == "--").map(|i| &args[i+1..]).unwrap_or(&[]);
            let execute = if let Ok(runner) = env::var("FAKE_RUNNER") {
                format!("{{\"program\":{runner:?},\"args\":[{output:?}]}}")
            } else { format!("{{\"program\":{output:?},\"args\":{runtime:?}}}") };
            (link, execute)
        } else { ("null".into(), "null".into()) };
        println!("{{\"schema_version\":{schema},\"mode\":\"{mode}\",\"target\":\"test-target\",\"emit\":\"bin\",\"emit_kinds\":[],\"control_mode\":null,\"forced_input_type\":null,\"inputs\":[],\"emit_jobs\":[],\"compile\":[],\"link\":{link},\"execute\":{execute}}}");
    } else {
        assert!(!args.iter().any(|a| a == "--run" || a == "--"));
        checkpoint("COMPILE");
        if let Ok(code) = env::var("VEX_TEST_COMPILE_EXIT") { std::process::exit(code.parse().unwrap()); }
        if let Some(generation) = generation {
            fs::create_dir_all(&generation).unwrap();
            fs::copy(env::current_exe().unwrap(), generation.join(if cfg!(windows) { "program.exe" } else { "program" })).unwrap();
        } else { println!("FAKE_WAVEC_EXECUTED"); }
    }
}
