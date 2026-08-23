# Changelog

All notable changes to Vex are documented in this file. Vex follows Semantic
Versioning while the public command and lockfile contracts are still maturing.

## [0.0.1] - 2026-08-23

### Added

- Manifest-based `init`, `build`, `run`, `check`, `fetch`, `update`, `info`, and
  `setup wavec` commands for Wave projects.
- Recursive Git and path dependency resolution with cycle, package-name,
  source, and version conflict detection.
- `vex.lock` schema v2, which records exact Git object IDs and dependency graph
  edges so a manifest and lockfile reproduce the same dependency graph.
- Full and package-targeted Git updates. `vex update <package>...` preserves
  unrelated locked commits and accepts directly or transitively referenced
  package names.
- `--locked` and `--offline` dependency modes, including their combined use.
- Cargo-style progress reporting and actionable dependency errors.
- `wavec` discovery through `PATH`, an explicit `VEX_WAVEC` override, and
  validation of the compiler dry-run JSON schema v1 contract.
- Reproducible release archives for Linux amd64/arm64/RISC-V, Windows x64, and
  macOS Intel/Apple Silicon, with SHA-256 checksums and GitHub provenance
  attestations.

### Fixed and hardened

- Path dependency locations are stored relative to the project when possible,
  so the same relative package tree and lockfile can move together.
- Help is read-only and succeeds without a manifest; invalid `init`, `info`,
  target, and global command arguments now fail consistently.
- Git object IDs loaded from a lockfile must be complete hexadecimal IDs.
- Git clone and revision commands terminate option parsing, Git's external
  transport protocol is disabled for managed operations, and symbolic links
  cannot redirect the managed `.vex/deps` area outside the project.
- The downloaded `wavec` installer uses exclusive, uniquely named temporary
  files and removes them after execution.
- Release archives carry a lockfile-checked inventory of third-party licenses
  and copyright notices.

### Compatibility notes

- Lockfiles produced by development snapshots may contain absolute path
  dependency locations. Run `vex fetch` once to rewrite those entries before
  using `--locked` with v0.0.1.
- Vex has been tested with `wavec 0.2.0-pre-beta`. The authoritative compiler
  compatibility check is support for `build --dry-run --error-format=json`
  schema version 1.
- A central registry, publishing, workspaces, `vex add`/`vex remove`, and a
  global Git cache are not part of v0.0.1.

[0.0.1]: https://github.com/wavefnd/Vex/releases/tag/v0.0.1
