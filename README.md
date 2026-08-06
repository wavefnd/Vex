# Vex

Vex is the package manager and build tool for the Wave programming language. It manages Wave project manifests, dependency resolution, lockfiles, and stable invocation of `wavec`.

Vex is designed to sit above `wavec` in the same way Cargo sits above `rustc`: Vex owns project structure and dependency orchestration, while `wavec` remains the compiler with detailed build flags.

## Requirements

- Rust toolchain for building Vex from source
- `wavec` compatible with the `build --dry-run --error-format=json` schema v1 contract
- `git` when using Git dependencies

Vex runs `wavec` from `PATH` by default. Set `VEX_WAVEC=/path/to/wavec` to use a specific compiler binary.

## Commands

```sh
vex init [--lib]
vex build [--target <triple>] [--release] [--dry-run]
vex run [--target <triple>] [--release] [--dry-run] [-- <args...>]
vex check [--target <triple>] [--release] [--dry-run]
vex fetch
vex update
vex info
vex setup wavec [--version <version>]
vex --version
```

## Project Layout

```text
my_project/
├── src/
│   └── main.wave
├── vex.ws
├── vex.lock
└── .vex/
    └── deps/
```

## Manifest

Vex uses `vex.ws` as the project manifest. The extension is `.ws`.

```wson
{
    name = "my_project",
    version = 0.1.0,
    lib = false,
    description = "my_project Project",
    author = "unknown",
    license = "Unknown",
    dependencies = []
}
```

## Dependencies

Vex currently uses a Git-first package model and does not require a central package registry. Local path dependencies are also supported.

Path dependency:

```wson
{
    name = "my_project",
    version = 0.1.0,
    dependencies = [
        { name = "local_math", path = "../local_math" }
    ]
}
```

Git dependency:

```wson
{
    name = "my_project",
    version = 0.1.0,
    dependencies = [
        { name = "math", git = "https://github.com/example/wave-math.git", tag = "v0.1.0" }
    ]
}
```

A dependency entry must use exactly one of `path` or `git`. Git dependencies may specify at most one of `branch`, `tag`, or `rev`.

Fetched Git dependencies are stored under `.vex/deps/<name>`. Every fetched dependency must contain a `vex.ws` file at its root. Dependency manifests are resolved recursively, and a package name must identify one source and version requirement across the graph.

On the first `vex fetch`, build, run, or check, Vex resolves each Git selector to an exact commit and records the complete transitive graph in `vex.lock`. Later commands reuse those commits without updating branches or tags. Run `vex update` explicitly to refresh Git refs and rewrite the lockfile.

Commit `vex.lock` so the same manifest and lockfile select the same dependency graph. A dry run never fetches or rewrites dependencies; use `vex fetch` first when the locked checkout is not available locally.

## Build Model

Vex uses `wavec` internally and validates the compiler dry-run plan before executing a real build. Vex commands stay manifest-based; raw compiler flags belong to `wavec`, not to Vex.

Build progress is written to stderr with Cargo-style stages such as `Resolving`, `Fetching`, `Compiling`, `Checking`, `Running`, and `Finished`. Program output remains on stdout.

Examples:

```sh
vex build --target x86_64-unknown-linux-gnu
vex run -- arg1 arg2
vex check
VEX_WAVEC=/opt/wave/bin/wavec vex build --dry-run
```

## License

[MPL 2.0 LICENSE](LICENSE)
