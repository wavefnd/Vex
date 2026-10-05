# Command reference

Generated from Vex command help. Regenerate with `python3 tools/cli_reference.py --write --vex target/debug/vex`.

See [manifest and operational contracts](reference.md) for field types, environment, streams and exit codes.

## Global options

```text
Vex - Wave package manager

Usage:
  vex [--message-file <new-path>] [--manifest-path <vex.ws>] <command> [options]
  vex init [--lib]
  vex build [--target <triple>] [--release] [--dry-run] [--locked] [--offline]
  vex run [--target <triple>] [--release] [--dry-run] [--locked] [--offline] [-- <args...>]
  vex check [--target <triple>] [--release] [--dry-run] [--locked] [--offline]
  vex fetch [--locked] [--offline]
  vex update [<package>...]
  vex info
  vex metadata [--format=json] [--locked] [--offline]
  vex tree [--locked] [--offline]
  vex setup wavec [--version <version>] [--script-fallback]
  vex --version
```

## init

```text
usage: vex init [--lib]
```

## build

```text
usage: vex build [--target <triple>] [--release] [--dry-run] [--locked] [--offline]
```

## check

```text
usage: vex check [--target <triple>] [--release] [--dry-run] [--locked] [--offline]
```

## run

```text
usage: vex run [--target <triple>] [--release] [--dry-run] [--locked] [--offline] [-- <args...>]
```

## fetch

```text
usage: vex fetch [--locked] [--offline]
```

## update

```text
usage: vex update [<package>...]
```

## info

```text
usage: vex info
```

## tree

```text
usage: vex tree [--locked] [--offline]
```

## metadata

```text
usage: vex metadata [--format=json] [--locked] [--offline]
```

## setup

```text
usage: vex setup wavec [--version <version>] [--script-fallback]
```
