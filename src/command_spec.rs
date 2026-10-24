// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! One command model drives help and installable offline CLI tooling.
use diagnostic::Error;
use serde_json::Value;
fn spec() -> Value {
    serde_json::from_str(include_str!("../tools/command-spec.json"))
        .expect("validated embedded command specification")
}
pub fn help(command: Option<&str>) -> Result<bool, Error> {
    let spec = spec();
    if let Some(name) = command {
        if let Some(entry) = spec["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
        {
            diagnostic::outln!(
                "usage: vex {}\n\n{}",
                entry["usage"].as_str().unwrap(),
                entry["description"].as_str().unwrap()
            );
            return Ok(true);
        }
        return Ok(false);
    }
    diagnostic::outln!(
        "Vex - Wave package manager\n\nUsage:\n  vex {} <command> [options]",
        spec["globals"].as_str().unwrap()
    );
    for entry in spec["commands"].as_array().unwrap() {
        diagnostic::outln!("  vex {}", entry["usage"].as_str().unwrap());
    }
    diagnostic::outln!("  vex --version\n\nOutput:\n  -q, --quiet    Hide Vex progress; keep results, warnings, errors, and program output\n  -v, --verbose  Show decisions; repeat (-vv) for redacted diagnostic detail");
    Ok(true)
}
pub fn completions(args: &[String]) -> Result<(), Error> {
    let [shell] = args else {
        return Err(Error::usage(
            "usage: vex completions <bash|zsh|fish|powershell>",
        ));
    };
    let spec = spec();
    let commands = spec["commands"].as_array().unwrap();
    let mut words = std::collections::BTreeSet::new();
    for text in commands
        .iter()
        .filter_map(|c| c["usage"].as_str())
        .chain(spec["globals"].as_str())
    {
        for word in text.split(|c: char| c.is_whitespace() || "[]()|".contains(c)) {
            if word.starts_with('-') || commands.iter().any(|c| c["name"] == word) {
                words.insert(word);
            }
        }
    }
    let list = words.iter().copied().collect::<Vec<_>>().join(" ");
    match shell.as_str() {
        "bash"=>diagnostic::outln!("# Generated from command-spec.json\n_vex() {{ COMPREPLY=( $(compgen -W '{list}' -- \"${{COMP_WORDS[COMP_CWORD]}}\") ); }}\ncomplete -F _vex vex"),
        "zsh"=>diagnostic::outln!("#compdef vex\n# Generated from command-spec.json\n_arguments '*:command or option:({list})'"),
        "fish"=> {diagnostic::outln!("# Generated from command-spec.json"); for word in words { if let Some(long)=word.strip_prefix("--").filter(|long| !long.is_empty()) {diagnostic::outln!("complete -c vex -l {long}");} else if !word.starts_with('-') {diagnostic::outln!("complete -c vex -f -a {word}");} }},
        "powershell"=>diagnostic::outln!("# Generated from command-spec.json\nRegister-ArgumentCompleter -Native -CommandName vex -ScriptBlock {{ param($wordToComplete, $commandAst, $cursorPosition)\n  '{list}'.Split(' ') | Where-Object {{ $_.StartsWith($wordToComplete) }} | ForEach-Object {{ [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }}\n}}"),
        _=>return Err(Error::usage("unsupported completion shell; choose bash, zsh, fish, or powershell")),
    }
    Ok(())
}
pub fn manpage(args: &[String]) -> Result<(), Error> {
    if !args.is_empty() {
        return Err(Error::usage("usage: vex manpage"));
    }
    let spec = spec();
    diagnostic::outln!(".TH VEX 1\n.SH NAME\nvex \\- Wave package manager\n.SH SYNOPSIS\n.B vex\n{} <command> [options]\n.SH COMMANDS",spec["globals"].as_str().unwrap());
    for entry in spec["commands"].as_array().unwrap() {
        diagnostic::outln!(
            ".TP\n.B vex {}\n{}",
            entry["usage"].as_str().unwrap().replace('-', "\\-"),
            entry["description"].as_str().unwrap()
        );
    }
    diagnostic::outln!(".SH FILES\nvex.ws, vex.lock\n.SH ENVIRONMENT\nVEX_WAVEC, VEX_CACHE_HOME\n.SH EXIT STATUS\n0 success; 1 internal error; 2 usage; 3 resolution; 4 compiler; 5 environment; 124 timeout; 130 cancellation. Program exit codes are preserved.");
    Ok(())
}
