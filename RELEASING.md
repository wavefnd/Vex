# Releasing Vex

This document is the maintainer procedure for producing an official Vex
release. The release workflow builds reproducible archives from an existing
authoritative `master` commit, creates the version tag in `wavefnd/Vex` only
after every target packages successfully, creates provenance attestations, and
prepares a draft GitHub Release. It never publishes a release automatically.

## Release contract

- `Cargo.toml` is the single source of truth for the Vex version.
- The release tag must be the exact `v<version>` tag, point at the release
  commit, and be annotated. The upstream Release workflow creates it using the
  GitHub Actions identity; maintainers do not push the official tag from a
  local checkout or personal fork.
- The release commit and committed `Cargo.lock` must be used without changes.
- Every configured target must build and package successfully, and the complete
  archive set must be verified, before the tag or draft release is created.
  Partial releases are not supported.
- Release archives and `SHA256SUMS` receive GitHub build-provenance
  attestations.
- Publishing the reviewed draft is a separate, intentional maintainer action.

## 1. Prepare the release commit

Start from the current `wavefnd/Vex:master`. Complete the release-candidate
checklist before tagging:

1. Update `CHANGELOG.md` and the reviewed `RELEASE_NOTES.md` used by the release
   workflow.
2. Confirm the supported platform table and compatible `wavec` contract.
3. Confirm that `Cargo.toml` contains the intended version and that
   `Cargo.lock` is committed.
4. Audit the locked Rust dependency graph for known vulnerabilities and review
   every dependency license. Record the scanner, advisory database date, and
   result in the pull request. For example, OSV-Scanner v2 can inspect the
   committed lockfile with:

   ```sh
   osv-scanner scan source --lockfile Cargo.lock
   cargo metadata --locked --format-version 1
   ```

5. Run the complete local validation suite:

   ```sh
   python3 x.py check
   ```

6. Run a real product smoke with a compatible `wavec`: initialize a temporary
   project, run Hello World through `PATH`, repeat a locked/offline build, and
   confirm a raw compiler option such as `vex build --emit=obj` is rejected.
   The integration suite must also cover path-lock relocation, Git lock
   reproducibility, full and targeted updates, and compiler schema rejection.
7. Merge the release-candidate pull request and wait for every required CI
   check on `master` to pass.

Do not create or push the release tag locally. The workflow guard ensures the
official tag is created only in `wavefnd/Vex`, from its current `master`.

## 2. Dispatch the upstream Release workflow

After the release-candidate pull request is merged and the required `master`
checks pass, dispatch `.github/workflows/release.yml` in the authoritative
repository:

```sh
gh workflow run release.yml --repo wavefnd/Vex --ref master
gh run list --repo wavefnd/Vex --workflow release.yml --limit 1
```

The workflow has no user-supplied tag input. It derives `v<version>` from the
checked-in `Cargo.toml` and records the exact current `wavefnd/Vex:master`
commit. It fails before building if it is dispatched in a fork, from another
branch, or from a stale commit.

The platform matrix builds and smoke-tests that exact commit without a tag.
After all targets succeed and the complete archive set passes checksum
verification, the final job creates an annotated tag through the
`wavefnd/Vex` workflow token, verifies it with `python3 x.py verify-release`,
attests the artifacts, and creates the draft release. A retry may reuse only an
existing annotated tag that points to the same release commit; it never moves
or replaces a tag. An already published release also cannot be overwritten.

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
single `SHA256SUMS`, generates provenance attestations, and creates a draft
GitHub Release.

Download the draft assets into an empty directory and verify them:

```sh
gh release download v0.0.1 --repo wavefnd/Vex --dir vex-v0.0.1
cd vex-v0.0.1
sha256sum --check SHA256SUMS
gh attestation verify vex-v0.0.1-x86_64-unknown-linux-gnu.tar.gz \
  --repo wavefnd/Vex
```

Repeat attestation verification for every archive and `SHA256SUMS`. Extract at
least one native archive in a clean environment and run `vex --version` and
`vex --help`. Complete the documented Wave project smoke test with a compatible
`wavec` before publication. Confirm that each archive also contains
`README.md`, `CHANGELOG.md`, `LICENSE`, `NOTICE`, `COPYRIGHT`, and
`THIRD_PARTY_LICENSES.md`.

## 4. Publish deliberately

Review the generated notes, platform status, compatibility requirements,
checksums, and attached assets in the draft. Only then publish it through the
GitHub Releases interface or with:

```sh
gh release edit v0.0.1 --repo wavefnd/Vex --draft=false
```

After publication, repeat the checksum, attestation, version, help, and Wave
project smoke tests using assets downloaded from the public release.

## Failure and recovery

- A failed validation, build, package, or checksum step creates neither a tag
  nor a GitHub Release. Fix the problem in a new pull request, merge it, and
  dispatch the workflow again.
- If the workflow fails after creating the tag but before creating the draft,
  rerun it from the unchanged release commit. It safely reuses only the same
  annotated tag. If source changes are required, increment the version; never
  move an existing official tag.
- If review finds a problem in an unpublished draft, do not publish it. Remove
  the draft and unadvertised tag only after confirming no user depends on them,
  then prepare a corrected release commit with a new version and dispatch the
  upstream workflow again.
- Never publish a partial set of target archives or hand-edit generated
  archives and checksums.
