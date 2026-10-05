# Releasing Vex

This document is the maintainer procedure for producing an official Vex
release. The release workflow builds reproducible archives from an existing
authoritative `master` commit and then asks GitHub to create the version tag and
release in `wavefnd/Vex`. It defaults to a draft and never publishes without
the dispatcher's explicit choice.

## Release contract

- `Cargo.toml` is the single source of truth for the Vex version.
- Official versions are normal `x.y.z` or SemVer prereleases. Build metadata
  (`+...`) is not supported. `tools/release_version.py` is shared by x.py and the
  workflow; leading zero numeric identifiers and empty prerelease parts are errors.
- Rust 1.96.0 is the pinned release toolchain and initial supported MSRV. Keep
  `rust-toolchain.toml`, all workspace rust-version declarations, and CI/release
  setup aligned; run rustfmt and Clippy from that same toolchain.
- Include the format-2 manifest and lockfile-v3 compatibility window and
  migration notes in the next release (see `docs/release-readiness.md`).
- The release tag must be the exact `v<version>` tag and point at the release
  commit. The upstream Release workflow creates it as part of
  `gh release create --target`; maintainers do not push the official tag from
  a local checkout or personal fork.
- The release commit and committed `Cargo.lock` must be used without changes.
- Every configured target must build and package successfully, and the complete
  archive set must be verified, before the tag or draft release is created.
  Partial releases are not supported.
- Every package must execute its version/help smoke. Missing native/emulator
  verification is an error. Never use the development-only `package
  --allow-unverified` opt-in in release jobs. Retain the per-archive JSON output
  with its digest and `verification: verified` as validation evidence.
- Publishing the reviewed draft is a separate, intentional maintainer action.

## 1. Prepare the release commit

Start from the current `wavefnd/Vex:master`. Complete the release-candidate
checklist before tagging:

1. Review the merged pull requests that GitHub will use to generate the release
   notes.
2. Update the README platform table and SECURITY support policy for the planned
   release. Confirm published versus experimental targets, installation links,
   and the compatible `wavec` contract before publication.
3. Confirm that `Cargo.toml` contains the intended version and that
   `Cargo.lock` is committed.
4. Audit the locked Rust dependency graph for known vulnerabilities and review
   every dependency license. Record the scanner/API, UTC observation time, lock hash, and
   result in the pull request. The committed OSV client inspects the locked graph with:

   ```sh
   python3 tools/dependency_audit.py
   cargo metadata --locked --format-version 1
   ```

   Audit exit code 1 means unsuppressed findings; 2 means the audit could not
   complete. Neither is a clean audit. Exceptions in audit-exceptions.json
   require an exact advisory/package/version, owner, reason and expiry within
   90 days. Do not renew an exception without reviewing the underlying finding.

   When Cargo.lock changes, regenerate and review the bundled third-party
   notices from all locked package sources (including platform-specific crates):

   ```sh
   cargo metadata --locked --format-version 1 > /tmp/vex-cargo-metadata.json
   python3 tools/dependency_notices.py /tmp/vex-cargo-metadata.json
   ```

   CI checks that THIRD_PARTY_LICENSES matches Cargo.lock, normalizing CRLF to LF
   for its fingerprint so Windows checkouts produce the same result. Review newly added
   license terms and notices; generating this file does not replace that review.

5. Run the complete local validation suite:

   ```sh
   python3 x.py check
   ```

6. Run a real product smoke with a compatible `wavec`: initialize a temporary
   project, run Hello World through `PATH`, repeat a locked/offline build, and
   confirm a raw compiler option such as `vex build --emit=obj` is rejected.
   The integration suite must also cover path-lock relocation, Git lock
   reproducibility, full and targeted updates, and compiler schema rejection.
   For Wave v0.2.1-pre-beta, wait for the official artifact, verify its archive
   checksum and available provenance, and run the procedure in
   [release readiness](docs/release-readiness.md#wave-021-compatibility-acceptance).
   Packaging source or a local development compiler is not release-artifact
   evidence. Record the archive and executable SHA-256 separately.
7. Merge the release-candidate pull request and wait for every required CI
   check on `master` to pass.

Do not create or push the release tag locally. The workflow guard ensures the
official tag is created only in `wavefnd/Vex`, from its current `master`.

## 2. Dispatch the upstream Release workflow

After the release-candidate pull request is merged and the required `master`
checks pass, dispatch `.github/workflows/release.yml` in the authoritative
repository:

```sh
release_version='REPLACE_WITH_CARGO_VERSION' # Without the v prefix.
gh workflow run release.yml --repo wavefnd/Vex --ref master \
  -f version="$release_version" \
  -f draft=true \
  -f prerelease=false
gh run list --repo wavefnd/Vex --workflow release.yml --limit 1
```

Enter the version without a `v` prefix. The workflow requires it to match the
version checked into `Cargo.toml`, derives the `v<version>` tag, and records the
exact current `wavefnd/Vex:master` commit. It fails before building if it is
dispatched in a fork, from another branch, from a stale commit, with a version
mismatch, or when that tag already exists upstream.

The platform matrix builds and smoke-tests that exact commit without a tag.
After all targets succeed and the complete archive set passes checksum
verification, the final job verifies the exact commit's latest master CI run:
quality, package validation, all five native test jobs, and the RISC-V build
must all succeed. Missing, pending, cancelled, skipped, and failed jobs block
publication. `tools/release_gate.py` logs the checked run, attempt, and job set;
keep its required names synchronized with `.github/workflows/ci.yml` and the
repository's merge rules. The job then rechecks upstream master immediately
before running `gh release create --target <commit>
--generate-notes`. GitHub creates the tag in `wavefnd/Vex`, generates notes
from merged changes, attaches the complete asset set, and applies the selected
draft and prerelease settings.

Release dispatches share one concurrency group across versions. If master moves
while packages are built, dispatch again from the new commit; no stale tag is
created. The final API check is not an atomic GitHub branch-and-tag transaction,
so maintainers should avoid merging during the publication step.

## 3. Review the draft release

The workflow packages these targets:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `riscv64gc-unknown-linux-gnu`

Each target is built and smoke-tested on its native runner, except RISC-V,
which is cross-built and executed with QEMU. The final job runs only after the
complete matrix succeeds. It rejects missing or unexpected archives, writes a
single `SHA256SUMS`, verifies each build attestation against this repository,
the release workflow and exact source/signer commit, and creates a GitHub Release.
Build jobs generate provenance before uploading the archives.

Download the draft assets into an empty directory and verify them:

```sh
gh release download "v$release_version" --repo wavefnd/Vex --dir "vex-v$release_version"
cd "vex-v$release_version"
sha256sum --check SHA256SUMS
# Set release_commit to the reviewed master commit used for this release.
for archive in vex-*.tar.gz vex-*.zip; do
  gh attestation verify "$archive" --repo wavefnd/Vex \
    --signer-workflow wavefnd/Vex/.github/workflows/release.yml \
    --source-ref refs/heads/master --source-digest "$release_commit" \
    --signer-digest "$release_commit" --deny-self-hosted-runners
done
```

Extract at least one native archive in a clean environment and run
`vex --version` and `vex --help`. Complete the documented Wave project smoke
test with a compatible `wavec` before publication. Confirm that each archive
also contains `README.md`, `LICENSE`, `NOTICE`, `COPYRIGHT`, and
`THIRD_PARTY_LICENSES`.

## 4. Publish deliberately

Review the GitHub-generated notes, platform status, compatibility requirements,
checksums, and attached assets in the draft. Only then publish it through the
GitHub Releases interface or with:

```sh
gh release edit "v$release_version" --repo wavefnd/Vex --draft=false
```

After publication, repeat the checksum, version, help, and Wave project smoke
tests using assets downloaded from the public release.

## Failure and recovery

- A failed validation, build, package, or checksum step creates neither a tag
  nor a GitHub Release. Fix the problem in a new pull request, merge it, and
  dispatch the workflow again.
- The tag and GitHub release are created together in the final step. If that
  step reports that the tag already exists, inspect the upstream release and
  tag before taking any action; the workflow never moves or replaces a tag.
- If review finds a problem in an unpublished draft, do not publish it. Remove
  the draft and unadvertised tag only after confirming no user depends on them,
  then prepare a corrected release commit with a new version and dispatch the
  upstream workflow again.
- Never publish a partial set of target archives or hand-edit generated
  archives and checksums.

## Nine-platform release gate

The release source version is `0.0.2-beta`; publish it as a prerelease. All nine
platform acceptance reports must identify the exact release commit, a verified
Wave 0.2.1 archive, and a successful real compiler/package execution. Draft Wave
assets cannot satisfy acceptance. Review `tools/wave-release.json` against the
final official assets before merging; changed digests require a reviewed update.

Archives, acceptance reports, and SHA256SUMS are attested. Publication verifies
repository, signer workflow, master ref, and exact source/signer commit. Missing or
invalid attestations fail closed. Check downloaded release files with `gh attestation
verify` using the same constraints. If a workflow/token compromise is suspected,
stop publication, revoke the affected credentials, investigate the exact signed
source and workflow, and publish a reviewed replacement release with an advisory.
Do not silently replace already published release archives. GitHub OIDC supplies
the signing identity; there is no repository-held long-lived signing key to rotate.

The minimum supported runtime is the tested OS/libc recorded in each acceptance
report. Do not infer a lower baseline from Rust target support or successful linking.
