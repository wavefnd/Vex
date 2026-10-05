# Unreleased: state coordination and format migration

This describes the development branch, not an already published Vex release.
No release version or tag is assigned by this change.

## Compatibility window

| Input | Normal command | `--locked` | `--offline` |
| --- | --- | --- | --- |
| Manifest without `format`, or format 1 | Literal backslashes | Same | Same |
| Manifest format 2 | JSON string escapes | Same | Same |
| Lockfile v1 | Resolve and migrate to v3 | Reject unchanged | Migrate only from locally available sources |
| Lockfile v2 | Read legacy strings; successful resolution may migrate to v3 | Preserve bytes if graph matches | Never fetch; migration still allowed without `--locked` |
| Lockfile v3 | Read/write escaped strings | Preserve bytes if graph matches | Never fetch |
| Unknown format or malformed graph | Error; preserve file | Error; preserve file | Error; preserve file |

Older Vex binaries do not understand manifest format 2 or lockfile v3. Upgrade
collaborators before committing a migrated lockfile. Existing manifests are never
automatically rewritten. To opt into escaped strings, add `format = 2` and encode
each existing string's actual value, rather than guessing what backslashes meant.
For example, legacy `"C:\tmp\new"` becomes `"C:\\tmp\\new"`; its value stays the same.
Ambiguous legacy quoting and literal multiline strings require an explicit edit.

The new shared WSON boundary recognizes comments and delimiters only outside
strings. It preserves URL fragments and former sentinel text, rejects duplicate
keys, and reports parser source locations. Unsupported versioned escapes fail
before graph publication. Package version strings retain their existing meaning;
format versions do not introduce SemVer dependency requirements or a registry.

## Dependency transaction

1. Validate CLI options and the root manifest without creating state.
2. Acquire `.vex/state.lock` before reading the lockfile; recover any interrupted
   publication first. Dry-run takes a shared lock and rejects pending recovery.
3. For targeted update, discover the current graph using only available local
   manifests and pinned Git checkouts. If that cannot be established, request an
   explicit `vex fetch`; do not query remotes just to validate a name.
4. Reuse verified, clean, already detached checkouts directly. Stage isolated
   candidates only for clone, migration, source changes or checkout updates.
   Resolve and validate the whole graph before moving live directories. Preserve
   declared origins, exact SHAs, dirty data, and unrelated remote-tracking refs.
5. Sync candidates and publish the journal. Move old checkouts into backups and
   rename complete candidates into the canonical `.vex/deps/pkg_<SHA-256 of package name>` paths.
6. Atomically replace the lockfile when its graph/format changes. This is the
   commit point. When lockfile bytes must stay unchanged, use the journal's
   committed marker after all checkout transitions instead.
7. Clear the active journal after successful publication or completed recovery.
   Keep backups and abandoned candidates for inspection; do not delete user data.

An interruption before the commit point restores the old checkouts. After the
commit point recovery retains the new graph. The current lockfile must match an
expected transaction state; inconsistent or dirty recovery data stops automatic
recovery with a diagnostic. Recovery is idempotent across interruptions.
Multiple directory renames are not one atomic filesystem operation. Readers see
consistent state because cooperating Vex commands hold the project lock.

The guard lives through dependency resolution and compiler planning/compilation.
Git/compiler children inherit its lifetime. On Unix this uses an inherited flock
descriptor; Windows uses inherited file-sharing restrictions, which persist with
the open handle even after the parent exits. Vex never deletes the coordination
file to recover ownership. These guarantees apply to cooperating Vex processes,
not arbitrary editor/Git modifications or a child deliberately closing its lease.

Implementation references: [Rust File locking](https://doc.rust-lang.org/std/fs/struct.File.html#method.lock)
and [Windows file-sharing lifetime](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew).

## Run generations

Each real run allocates `target/.vex-run/<unique generation>/`. Vex validates a
single JSON plan, compiles without `--run`, checks the produced artifact, drops
the dependency/build guard, and only then spawns the program or compiler-selected
runner with structured arguments. Node/WASM and QEMU runners are not converted to
shell command strings. Runtime cwd is the selected project root; environment and stdio remain inherited; the
program exit code is preserved (Unix signal exits use `128 + signal`).

Run generations have no automatic GC. This also avoids assuming that all runtime
children have exited when the original program returns. Disk usage increases
until the user performs deliberate cleanup when no process needs those outputs.
The internal generation paths are not the complete public artifact layout from
issue #47, and this change does not introduce multiple package targets or profiles.

Dry-run creates no generation and invokes the compiler planner once. It may create
the coordination directory/file. The printed plan uses a placeholder generation;
it is diagnostic output, not a stable Vex metadata interface.

## Release and merge policy

Rust 1.96.0 is the initial supported toolchain/MSRV, including rustfmt and Clippy.
The checked-in toolchain, Cargo declarations and CI/release setup agree. x.py
selects that toolchain even when the caller's rustup default differs.

Release versions permit normal versions and prereleases, without `+build.metadata`.
The workflow and x.py call the same validation policy. No official release is
created by local build/package verification.

Archives include the license texts and notices from the complete locked Cargo
graph in `THIRD_PARTY_LICENSES`, including build-time and platform-specific
packages. Regenerate and review these when Cargo.lock changes; CI checks their
lockfile fingerprint with CRLF normalized to LF. Known-vulnerability scans are time-specific release
evidence and must be repeated for the eventual release commit.

Master requires PRs, Quality and all nine platform acceptance checks and an up-to-date base. Force
pushes and branch deletion are prohibited. There are no bypass actors;
administrators must also meet the PR and check requirements.

Before release: run the new concurrency/recovery tests natively on Linux,
Windows and macOS, verify clean-environment packages, complete the dependency
audit, and validate/pin the next official Wave release. Cross compilation and a
local development wavec smoke do not substitute for those native release gates.

The subsequent [production hardening changes](production-hardening.md) add bounded
Git execution, cancellation, safe compiler artifact installation, dependency
scanning and release provenance. Their native CI acceptance remains required
before release; a local cross-compile is not that evidence.

## Wave 0.2.1 compatibility acceptance

Wave packaging was reviewed at `448370fe99e787645e7e61edccf7facf37fe8b4b`
([package.py](https://github.com/wavefnd/Wave/blob/448370fe99e787645e7e61edccf7facf37fe8b4b/tools/ci/package.py),
[release.py](https://github.com/wavefnd/Wave/blob/448370fe99e787645e7e61edccf7facf37fe8b4b/tools/ci/release.py)).
Its archive names and single-root layout remain compatible with Vex's extractor.
The additive `<archive>.metadata.json` has schema 1 and records compiler version,
source SHA, std compatibility revision, target/ABI, payload paths, external
prerequisites, archive name and SHA-256. Metadata target triples are compiler
triples; archive names normalize Linux `unknown` and RISC-V `gc` components.
Vex requires official `SHA256SUMS` or the GitHub release asset SHA-256 digest
and verifies available provenance. When both digests exist they must agree. It does not yet use this additive sidecar to authorize installation
or require it for older releases.

The #154 fixtures cover all nine host mappings, missing assets without GNU fallback,
both MSVC ZIPs and the LoongArch64 TAR, checksum failures, compiler discovery,
preserved std/runtime layout and preservation of the previous installation.
Foreign fixture files are data, not executable compiler validation. Actual
packaged std imports and native runtime prerequisites remain release acceptance.

The #71 package report records verified/unverified execution per archive; release
packaging rejects missing verifiers. The #91 pipe tests use already-closed OS
pipes, including JSON output and message files, and run on every native CI host.

For lockfile acceptance, `tests/lockfile_compatibility.rs` covers v1/v2/v3 and
future/malformed formats with normal/locked/offline policies and LF/CRLF inputs.
`tests/git_lock_reproducibility.rs` adds real SHA-256 v2/v3 reuse and migration with
the source remote unavailable, plus selected update and subsequent offline reuse.
Path relocation/diamond tests, transaction recovery tests, empty-lock tests and
credential rendering tests remain part of the full workspace suite. An issue's
completion requires its acceptance criteria and native CI evidence, not only
these implementation references.

The required compatible Wave release is `v0.2.1-pre-beta` on October 5, 2026.
Until its official artifact exists, a local development-compiler smoke is development
evidence only. It does not establish the artifact's compatibility or exact source
commit, and it does not replace native CI. No compiler source-build gate is added.

Use the release's verified official host artifact, record its archive SHA-256 and
provenance result, then run:

```sh
python3 tests/wave_compatibility.py --vex target/debug/vex \
  --wavec-bin /path/to/verified/compiler/bin --reexports \
  --expected-version 0.2.1-pre-beta
```

The smoke records the executable SHA-256; `--expected-sha256` can pin that
executable on subsequent runs. It is distinct from the archive SHA-256. The suite
checks PATH selection, compiler capabilities, one JSON dry-run plan, ancestor and
explicit project selection, Hello World, direct/transitive package imports,
public reexports, private-symbol rejection, locked/offline reuse and metadata.
The local Git remote is removed before offline checks. Missing backend/runtime
prerequisites must fail the smoke rather than silently skip it.

The required nine-platform CI now pins the expected official archive digests in
`tools/wave-release.json`. Each lane executes the full test suite, a packaged Vex,
verified compiler installation, and this real Wave smoke. RISC-V and LoongArch
execute target userspace through QEMU; FreeBSD uses a VM. Cross compilation alone
cannot produce acceptance. The public artifact checks and eventual release run
must pass before release; local candidate results do not satisfy those gates.
