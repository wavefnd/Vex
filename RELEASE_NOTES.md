# Vex v0.0.1

Vex v0.0.1 is the first public release of the package manager and build tool
for the Wave programming language. It establishes the manifest, dependency,
lockfile, compiler, and release contracts that future versions will build on.

## Highlights

- Create and inspect `vex.ws` projects with `vex init` and `vex info`.
- Build, check, and run Wave packages while Vex manages the internal `wavec`
  invocation.
- Resolve recursive Git and path dependencies and record the complete graph in
  `vex.lock` schema v2.
- Reproduce exact Git commits with `--locked`, prohibit Git network access with
  `--offline`, or combine both modes for strict CI builds.
- Refresh the whole Git graph with `vex update`, or update direct and transitive
  packages selectively with `vex update <package>...` while preserving
  unrelated commits.
- Download reproducible archives with SHA-256 checksums and GitHub build
  provenance for Linux amd64/arm64/RISC-V, Windows x64, and macOS Intel/Apple
  Silicon.

## Compiler compatibility

Vex v0.0.1 is tested with `wavec 0.2.0-pre-beta`. A compatible compiler must
support `wavec build --dry-run --error-format=json` schema version 1. Vex finds
`wavec` on `PATH` by default; use `VEX_WAVEC=/path/to/wavec` to select another
binary.

## Verify before installing

Download the archive for your target together with `SHA256SUMS`, then run:

```sh
sha256sum --check SHA256SUMS
gh attestation verify <archive> --repo wavefnd/Vex
```

Linux GNU archives use Ubuntu 24.04 as their supported runtime baseline.
Windows x64 is validated on Windows Server 2025 and macOS archives on macOS 15.
The RISC-V archive is experimental and receives cross-build plus QEMU smoke
coverage rather than the complete native integration suite.

## Known scope

This release intentionally has no central registry, publishing command,
workspace support, `vex add`/`vex remove`, or global Git cache. Raw `wavec`
options are not accepted by Vex commands.

Development lockfiles containing absolute path dependency locations should be
rewritten once with `vex fetch` before using `--locked` with v0.0.1. See the
[changelog](https://github.com/wavefnd/Vex/blob/v0.0.1/CHANGELOG.md) and
[installation guide](https://github.com/wavefnd/Vex/blob/v0.0.1/README.md#install)
for full details.

Thank you to every contributor and tester who helped establish Vex's first
reproducible package-management and release baseline.
