# Unreleased production hardening

These contracts are included in the `0.0.2-beta` release preparation. Rust
1.96.0 is the supported toolchain. Quality and all nine platform acceptance
checks must pass before release. See [RELEASING](../RELEASING.md).

## Changes and compatibility

- **Initialization (#21, #33):** names use Wave import identifier syntax. Invalid
  directory names are rejected before state creation. Existing source, manifest
  and lockfile destinations are never replaced. Complete staged files are
  published with exclusive hard links; `vex.ws` is the final commit point. A
  durable init journal supports restart recovery. Recovery checks file identity
  and contents before removing owned files and stops for user edits or replacement.
  Filesystems without hard-link support fail safely without falling back to
  overwriting writes.
- **Checkout layout (#24, #79):** new Git checkouts use `pkg_` followed by the full
  SHA-256 of the case-sensitive UTF-8 package name. This bounds component length
  and avoids device names and filesystem case folding. Locked/dry-run commands
  keep historical paths. A successful ordinary fetch stages the new layout and
  updates the resolved path; the historical checkout is retained. Invalid
  lockfile destinations are rejected. Source changes clone a fresh candidate.
  Windows Git invocations enable `core.longpaths` for the command and its Git
  children so staging and pack paths can exceed MAX_PATH without changing user
  or repository Git configuration. Windows fetches retain packs (`fetch.unpackLimit=1`)
  because loose-object unpacking has a separate long-path limitation.
  Deep candidates use object-format discovery
  followed by private initialization without Git templates, fetch and checkout,
  avoiding clone's separate
  absolute `GIT_DIR` limit. Git operations select `.git` relative to the checkout;
  SHA-1/SHA-256 and fresh locked restoration are covered by the long-path fixture.
  Windows checkout directories themselves (including staging) are limited to
  240 UTF-16 code units because process startup and early Git discovery retain
  directory-length limits. Vex rejects longer directories before transport,
  preserves existing checkout/lockfile state, and asks for a shorter project path.
  This bound does not restrict the length of files inside supported checkouts.
  Git for Windows can cache an explicit `core.longpaths=false` before applying
  command overrides and may report inaccessible long paths as local changes.
  If it reports a long-path error or unexpected changes, Vex preserves state and
  advises checking `git config --show-origin --get-all core.longpaths`: remove
  the explicit disabling value or enable long paths in the reported config.
  Vex does not rewrite that user configuration.
- **Reuse (#146):** a matching source, SHA, clean worktree and detached HEAD can
  be reused without copying, publishing or retaining a new backup. The project
  lease still spans compiler use. Real transitions keep transactional publication.
  Tests compare transaction counts and Git index modification times across
  repeated normal, locked and offline fetches.
- **Lock meaning (#110, #147):** path identity uses the resolved location, while
  the historical request spelling remains an annotation. Reordering a diamond
  graph does not rewrite a locked file. Missing empty lockfiles are created by
  normal/offline fetch; locked mode still rejects absence. New resolved paths use
  `/` on every OS. Historical native Windows separators need migration on Windows;
  literal Unix backslashes are not silently reinterpreted.
- **Processes (#86, #92, #90):** Git has null stdin, prompt suppression, a default
  300-second per-command deadline, configurable `VEX_GIT_TIMEOUT=1..86400`, and
  bounded captured output. Unix process groups and Windows Job Objects supervise
  cancellation. Windows children are suspended until assigned to their job.
  Runtime stdin/stdout/stderr and terminal interaction remain inherited, with no
  runtime deadline. Program exit codes are preserved; Unix signals translate to
  `128 + signal`. Vex returns 1 for internal errors, 2 for CLI usage, 3 for
  project/dependency resolution, 4 for compiler failures, 5 for environment,
  124 for timeout, and 130 for cancellation. `--message-file <new-path>` records
  schema-1 JSONL events separately from stdio and identifies `vex` versus `program`
  outcomes. It never overwrites existing files. Dry-run validates the destination
  without creating the report. Reporting failure stops work before runtime starts;
  after execution it warns without replacing the program exit code. See
  [the CLI contract](cli-contract-design.md). Native CI remains acceptance work.
- **Credentials (#148):** transport input remains usable with user Git rewrites.
  HTTP userinfo, SSH passwords, and recognized authentication query fields are
  removed from rendered sources and new lockfiles; SSH account routing and other
  query fields are preserved. Diagnostics also suppress known credential values,
  including percent-decoded values. New checkout origins are credential-free.
  A historical credential-bearing lockfile requires unlocked migration; locked
  mode errors without rewriting it. New journal v2 records the previous lock's
  SHA-256 instead of duplicating its contents. Journal v1 recovery remains readable.
  Remove historical credentials from version-control history separately.
- **Structured inputs (#80):** WSON reads and in-memory documents are bounded at
  8 MiB, nesting at 128, and scalar length at 1024. A deterministic mutation corpus
  exercises both version boundaries. Captured compiler plans are bounded at 8 MiB.
  This is bounded regression coverage, not a claim of exhaustive fuzzing.

Changing the checkout layout does not change the meaning of the v3 `resolved`
field: it still records a project-relative path. New readers retain v2/v3 support.
Do not downgrade Vex while a recovery journal is pending. Dry-run never performs
migration or recovery. Backup, run-generation and installed compiler directories
have no automatic garbage collector.

## Compiler installation (#96)

`vex setup wavec` resolves an official release and exact host asset before writing
an installation. HTTPS-only downloads use published SHA256SUMS. Available GitHub
provenance must verify for the Wave repository and release source commit; this
requires `gh` when provenance exists. An explicit 404/no attestations is reported
as checksum-only verification. Network, permission and verification failures are
not treated as absence. This does not claim a signature when none is published.

Archive extraction rejects traversal, absolute paths, Windows special paths,
links, special files, duplicate/case-colliding entries and oversized archives.
It limits compressed size to 512 MiB, expanded size to 2 GiB, entry count to
100,000 and depth to 64. The binary version is checked before syncing and publishing
an immutable generation. A small `current` pointer is replaced atomically;
previous generations remain usable if download, verification or switching fails.
`VEX_TOOLCHAIN_HOME` overrides the user-local prefix. Compiler selection remains
VEX_WAVEC, then PATH, then the managed current installation.

`--script-fallback` is the only path to the downloaded official installer script.
It is explicitly opt-in and uses the external script's installation behavior.
The artifact path and fallback report their own failures without a cleanup error
hiding the original failure. Fake archive and pointer-publication failures are
covered locally alongside fake-release metadata selection without network access.

## Audit and provenance (#68, #69, #70)

`python3 tools/dependency_audit.py` queries exact registry versions from Cargo.lock
against OSV. Exit 0 means completed with no unsuppressed findings, 1 means findings,
and 2 means the audit failed or was incomplete. The JSON report includes the UTC
observation time and lockfile hash. Empty/malformed batches, pagination failures,
unsupported sources, network failures and expired exceptions cannot produce a
clean result. Exceptions require an exact package/version/advisory, owner, reason,
and expiry within 90 days; none are pre-approved.

Required Quality CI and release validation run the audit; an additional weekly
workflow detects advisories after merge. Action uses are pinned to verified full
commit SHAs. Release build jobs
attest their archives; publication verifies repository, signer workflow, master
ref and exact source/signer commit before creating the release. No release
workflow has been dispatched to test this unpublished branch.

## Release acceptance

Local Linux tests, fault injection, cross-target cargo checks, an OSV observation,
and an isolated official Wave artifact install are development evidence. They do
not substitute for native Windows/macOS CI on this PR or an actual release run.
Do not mark all referenced issues closed from this document alone.

Wave v0.2.0-pre-beta remains incompatible with canonical package imports despite
passing version/Hello World checks. #66/#131 require public `v0.2.1-pre-beta` artifacts on all nine hosts.
The required checks reject draft assets and do not substitute a local development
compiler. Until those checks pass, this remains release preparation evidence.
