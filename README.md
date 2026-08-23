# Vex

Vex is the package manager and build tool for the Wave programming language. It manages Wave project manifests, dependency resolution, lockfiles, and stable invocation of `wavec`.

Vex is designed to sit above `wavec` in the same way Cargo sits above `rustc`: Vex owns project structure and dependency orchestration, while `wavec` remains the compiler with detailed build flags.

## Requirements

- `wavec` compatible with the `build --dry-run --error-format=json` schema v1 contract
- `git` when using Git dependencies
- Rust toolchain only when building Vex from source
- Python 3.11 or newer when using the release tooling

Vex runs `wavec` from `PATH` by default. Set `VEX_WAVEC=/path/to/wavec` to use a specific compiler binary.
Vex v0.0.1 is tested with `wavec 0.2.0-pre-beta`; support for schema v1 is the
authoritative compatibility requirement. Vex reports a schema mismatch before
the real build and suggests selecting another compiler with `VEX_WAVEC`.

## Platform validation

The v0.0.1 release candidate targets below are validated on every pull request.
“Candidate” means the platform is intended to receive a release archive after
the release workflow also passes package and clean-environment smoke tests.

| Platform | Rust target | CI validation | v0.0.1 status |
| --- | --- | --- | --- |
| Linux amd64 | `x86_64-unknown-linux-gnu` | native tests, build, package smoke | Candidate |
| Linux arm64 | `aarch64-unknown-linux-gnu` | native tests and build | Candidate |
| Windows x64 | `x86_64-pc-windows-msvc` | native tests and build | Candidate |
| macOS Intel | `x86_64-apple-darwin` | native tests and build | Candidate |
| macOS Apple Silicon | `aarch64-apple-darwin` | native tests and build | Candidate |
| Linux RISC-V | `riscv64gc-unknown-linux-gnu` | cross-build and QEMU version smoke | Experimental |

Windows release artifacts use the MSVC target. A Windows GNU artifact is not
part of the v0.0.1 scope. RISC-V remains experimental because its test coverage
is limited to cross-build and QEMU smoke rather than the complete integration
suite.

The v0.0.1 Linux GNU archives are built on Ubuntu 24.04 and require a glibc-based
system; Ubuntu 24.04 is the supported runtime baseline. Windows artifacts are
validated on the GitHub Windows Server 2025 runner, and macOS artifacts on
macOS 15. Older operating systems and other distributions are best effort for
this first release.

## Install

Download the archive and `SHA256SUMS` for your platform from the
[GitHub release](https://github.com/wavefnd/Vex/releases/tag/v0.0.1). Verify the
download before extracting it:

```sh
sha256sum --check SHA256SUMS
tar -xzf vex-v0.0.1-x86_64-unknown-linux-gnu.tar.gz
install -m 0755 vex-v0.0.1-x86_64-unknown-linux-gnu/vex ~/.local/bin/vex
vex --version
```

On Windows, compare `Get-FileHash <archive> -Algorithm SHA256` with the matching
line in `SHA256SUMS`, extract the zip, and place `vex.exe` in a directory on
`PATH`.

To build from source instead:

```sh
git clone https://github.com/wavefnd/Vex.git
cd Vex
cargo build --locked --release
install -m 0755 target/release/vex ~/.local/bin/vex
```

Install `wavec` separately and make it available on `PATH`, or set
`VEX_WAVEC` to its full path. `vex setup wavec` is an explicit convenience
command that downloads and executes the official installer from
`wave-lang.dev`; review that trust and network boundary before using it. A
specific compiler can be requested with `vex setup wavec --version
0.2.0-pre-beta`.

## Commands

```sh
vex init [--lib]
vex build [--target <triple>] [--release] [--dry-run] [--locked] [--offline]
vex run [--target <triple>] [--release] [--dry-run] [--locked] [--offline] [-- <args...>]
vex check [--target <triple>] [--release] [--dry-run] [--locked] [--offline]
vex fetch [--locked] [--offline]
vex update [<package>...]
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

Every dependency is a library package: its manifest must set `lib = true` and
its canonical entry is `src/lib.wave`. Wave source imports the package name,
not the entry filename:

```wave
import("local_math");
import("local_math::vector");
import("local_math")::{sum, Point};
```

The first form resolves `local_math/src/lib.wave`; the second resolves
`local_math/src/vector.wave`. Vex passes exact mappings for every direct and
transitive dependency to `wavec`, and only `pub` declarations are visible to
consumers.

On the first `vex fetch`, build, run, or check, Vex resolves each Git selector to an exact commit and records the complete transitive graph in `vex.lock`. Later commands reuse those commits without updating branches or tags. Run `vex update` explicitly to refresh every Git dependency and rewrite the lockfile.

Pass one or more package names to update only those packages, including transitive dependencies. Unrelated packages keep their exact locked commits and are not fetched. If an updated package changes its dependencies, Vex recalculates that part of the graph while preserving unrelated locked packages.

```sh
# Refresh the complete Git dependency graph.
vex update

# Refresh only alpha and the transitive package shared_core.
vex update alpha shared_core
```

Commit `vex.lock` so the same manifest and lockfile select the same dependency graph. A dry run never fetches or rewrites dependencies; use `vex fetch` first when the locked checkout is not available locally.

### Reproducible and Offline Modes

Use `--locked` when Vex must not create or modify `vex.lock`. The command fails if the lockfile is missing, uses an older schema, or does not match the manifest graph. It may still download a commit already pinned by the lockfile when the managed checkout is missing.

Use `--offline` to prohibit all Git network operations. Vex may switch an existing managed checkout to a locally available locked commit, but it fails with instructions to run `vex fetch` when a checkout or commit is missing.

Combine both options for the strictest CI build:

```sh
vex fetch --locked
vex build --locked --offline
```

`vex update` intentionally accepts neither option because it refreshes Git refs and rewrites the lockfile.

## Build Model

Vex uses `wavec` internally and validates the compiler dry-run plan before executing a real build. Vex commands stay manifest-based; raw compiler flags belong to `wavec`, not to Vex.

Build progress is written to stderr with Cargo-style stages such as `Resolving`, `Fetching`, `Compiling`, `Checking`, `Running`, and `Finished`. Program output remains on stdout.

Examples:

```sh
vex build --target x86_64-unknown-linux-gnu
vex build --locked --offline
vex run -- arg1 arg2
vex check
VEX_WAVEC=/opt/wave/bin/wavec vex build --dry-run
```

## Development and release tooling

The repository-level `x.py` script is the supported entry point for release
builds and packages. It reads the version from `Cargo.toml`, always builds with
the committed `Cargo.lock`, and writes archives plus `SHA256SUMS` to `dist/`.
Run it with Python 3.11 or newer:

```sh
# Show the host and every supported release target.
python3 x.py list-targets

# Run formatting, release-tool tests, Rust tests, Clippy, and a debug build.
python3 x.py check

# Build and package the native target.
python3 x.py build
python3 x.py package

# Build or package one or more explicit targets.
python3 x.py build x86_64-unknown-linux-gnu
python3 x.py package x86_64-unknown-linux-gnu

# Verify an assembled target set and regenerate its checksums.
python3 x.py checksum x86_64-unknown-linux-gnu
```

Archives contain the Vex executable together with `README.md`, `LICENSE`,
`NOTICE`, and `COPYRIGHT`. Their file order, permissions, owners, and timestamps
are normalized. Set
`SOURCE_DATE_EPOCH` to an explicit non-negative Unix timestamp when reproducing
an artifact outside the tagged source revision.

`python3 x.py release [<target>...]` is intentionally stricter than separate
build and package commands. It runs the complete validation suite and succeeds
only when the working tree is clean and `HEAD` has the exact `v<version>` tag.
Cross-target builds still require the corresponding Rust target and native
linker to be installed. `VEX_RELEASE_HOST` exists for release infrastructure
that must override host-target detection; normal development should not set it.

`python3 x.py verify-release` checks that the source tree is clean and `HEAD`
has the annotated `v<version>` tag for local release reproduction. For an
official release, a maintainer dispatches the Release workflow from
`wavefnd/Vex:master`; the workflow refuses forks and non-`master` refs. It
validates and packages the exact upstream commit and verifies the complete
archive set. Its final
`gh release create --target ... --generate-notes` call creates the tag in
`wavefnd/Vex` and generates the GitHub release notes from merged changes.
Publishing a reviewed draft remains a separate maintainer action. See
[RELEASING.md](RELEASING.md) for the complete procedure.

Verify downloaded archives from the directory containing `SHA256SUMS`:

```sh
sha256sum --check SHA256SUMS
```

## License

[MPL 2.0 LICENSE](LICENSE)

## Community and Project Policies

- [Contributing](CONTRIBUTING.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Maintainers](MAINTAINERS)
- [Security Policy](SECURITY.md)
- [Release Process](RELEASING.md)
- [Copyright](COPYRIGHT)
- [Notice](NOTICE)
- [AI Usage Policy](ai.txt)
