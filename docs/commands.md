# Command reference

Generated from Vex command help. Regenerate with `python3 tools/cli_reference.py --write --vex target/debug/vex`.

See [manifest and operational contracts](reference.md) for field types, environment, streams and exit codes.

## Global options

```text
Vex - Wave package manager

Usage:
  vex [-q|--quiet] [-v|--verbose] [--message-file <new-path>] [--manifest-path <vex.ws>] [-p|--package <name>|--workspace] [--features <features>] [--no-default-features] [--include-tests] <command> [options]
  vex init [--lib]
  vex build [--lib] [--bin <name>] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline]
  vex run [--bin <name>] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline] [-- <args...>]
  vex check [--lib] [--bin <name>] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline]
  vex test [filter] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline] [-- <args...>]
  vex fetch [--locked] [--offline] [--target <triple>]
  vex update [<package>...] [--target <triple>]
  vex info
  vex metadata [--format=json] [--format-version <1|2>] [--locked] [--offline] [--target <triple>]
  vex tree [--invert <name>] [--depth <n>] [--locked] [--offline] [--target <triple>]
  vex clean [--dry-run]
  vex prune [--apply] [--dry-run]
  vex doctor [--format=json] [--target <triple>]
  vex setup wavec [--version <version>] [--script-fallback]
  vex new <path> [--name <name>] [--lib] [--git]
  vex add <name> (--git <url>|--path <path>) [--branch <ref>|--tag <ref>|--rev <rev>] [--version <version>] [--version-req <requirement>] [--package-name <name>] [--subdir <path>] [--offline]
  vex remove <name> [--offline]
  vex cache prune [--apply] [--dry-run]
  vex completions <bash|zsh|fish|powershell>
  vex manpage
  vex --version

Output:
  -q, --quiet    Hide Vex progress; keep results, warnings, errors, and program output
  -v, --verbose  Show decisions; repeat (-vv) for redacted diagnostic detail
```

## init

```text
usage: vex init [--lib]

Manage init
```

## build

```text
usage: vex build [--lib] [--bin <name>] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline]

Compile selected package targets
```

## run

```text
usage: vex run [--bin <name>] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline] [-- <args...>]

Compile and execute one binary
```

## check

```text
usage: vex check [--lib] [--bin <name>] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline]

Check selected package targets
```

## test

```text
usage: vex test [filter] [--target <triple>] [--profile <name>|--release] [--dry-run] [--locked] [--offline] [-- <args...>]

Compile and execute explicit main-based tests; run every selected test and return the first failure
```

## fetch

```text
usage: vex fetch [--locked] [--offline] [--target <triple>]

Manage fetch
```

## update

```text
usage: vex update [<package>...] [--target <triple>]

Manage update
```

## info

```text
usage: vex info

Manage info
```

## metadata

```text
usage: vex metadata [--format=json] [--format-version <1|2>] [--locked] [--offline] [--target <triple>]

Manage metadata
```

## tree

```text
usage: vex tree [--invert <name>] [--depth <n>] [--locked] [--offline] [--target <triple>]

Manage tree
```

## clean

```text
usage: vex clean [--dry-run]

Manage clean
```

## prune

```text
usage: vex prune [--apply] [--dry-run]

Manage prune
```

## doctor

```text
usage: vex doctor [--format=json] [--target <triple>]

Manage doctor
```

## setup

```text
usage: vex setup wavec [--version <version>] [--script-fallback]

Manage setup
```

## new

```text
usage: vex new <path> [--name <name>] [--lib] [--git]

Create a new project without overwriting data
```

## add

```text
usage: vex add <name> (--git <url>|--path <path>) [--branch <ref>|--tag <ref>|--rev <rev>] [--version <version>] [--version-req <requirement>] [--package-name <name>] [--subdir <path>] [--offline]

Add a direct dependency preserving comments and formatting
```

## remove

```text
usage: vex remove <name> [--offline]

Remove a direct dependency preserving comments and formatting
```

## cache

```text
usage: vex cache prune [--apply] [--dry-run]

Explicitly prune shared Git objects
```

## completions

```text
usage: vex completions <bash|zsh|fish|powershell>

Print shell completion script
```

## manpage

```text
usage: vex manpage

Print the vex(1) manual
```
