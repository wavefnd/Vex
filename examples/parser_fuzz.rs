// SPDX-License-Identifier: MPL-2.0
//! Bounded mutation fuzzer. A failing seed and minimized input are reproducible.
use std::{fs, panic, path::PathBuf};
fn crashes(kind: usize, bytes: &[u8]) -> bool {
    panic::catch_unwind(|| match kind {
        0 => {
            if let Ok(text) = std::str::from_utf8(bytes) {
                let _ = manifest::decode(text, PathBuf::from("fuzz.ws"));
            }
        }
        1 => {
            if let Ok(text) = std::str::from_utf8(bytes) {
                let _ = lockfile::decode(text);
            }
        }
        _ => {
            let _ = compiler::validate_plan(bytes);
        }
    })
    .is_err()
}
fn main() {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args
        .next()
        .unwrap_or_else(|| "20261005".into())
        .parse()
        .unwrap();
    let count: usize = args
        .next()
        .unwrap_or_else(|| "20000".into())
        .parse()
        .unwrap();
    let output = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "target/parser-fuzz-crash".into()),
    );
    let seeds: [&[u8]; 3] = [
        br#"{format=2,name="app",compiler="0.2.1-pre-beta",dependencies=[]}"#,
        br#"{version=3,package=[]}"#,
        br#"{"schema_version":1,"compile":[],"link":null,"execute":null}"#,
    ];
    let mut state = seed;
    for index in 0..count {
        let kind = index % 3;
        let mut bytes = seeds[kind].to_vec();
        let mut random = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        for _ in 0..(random() % 64 + 1) {
            let at = random() % (bytes.len() + 1);
            match random() % 4 {
                0 => bytes.insert(at, random() as u8),
                1 if at < bytes.len() => {
                    bytes.remove(at);
                }
                2 if at < bytes.len() => bytes[at] = random() as u8,
                _ if bytes.len() < 16384 => {
                    let copy = bytes.clone();
                    bytes.extend(copy);
                }
                _ => {}
            }
        }
        if crashes(kind, &bytes) {
            let mut size = bytes.len() / 2;
            while size > 0 {
                let mut at = 0;
                while at + size <= bytes.len() {
                    let mut candidate = bytes.clone();
                    candidate.drain(at..at + size);
                    if crashes(kind, &candidate) {
                        bytes = candidate;
                    } else {
                        at += size;
                    }
                }
                size /= 2;
            }
            fs::create_dir_all(&output).unwrap();
            fs::write(
                output.join(format!("kind-{kind}-seed-{seed}-case-{index}.bin")),
                &bytes,
            )
            .unwrap();
            panic!(
                "parser {kind} crashed: seed={seed}, case={index}, minimized bytes={}",
                bytes.len()
            );
        }
    }
    println!("passed {count} parser mutations, seed={seed}");
}
