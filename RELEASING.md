# Releasing Vex

This document is the maintainer procedure for producing an official Vex
release. The release workflow builds reproducible archives from an existing
annotated version tag, creates provenance attestations, and prepares a draft
GitHub Release. It never publishes a release automatically.

## Release contract

- `Cargo.toml` is the single source of truth for the Vex version.
- The release tag must be the exact `v<version>` tag, point at the release
  commit, and be annotated. A signed annotated tag is preferred.
- The release commit and committed `Cargo.lock` must be used without changes.
- Every configured target must build and package successfully before a draft
  release is created. Partial releases are not supported.
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

Do not create a release tag from a feature branch, a dirty checkout, or a
commit that has not passed the required checks.

## 2. Create and push the version tag

Fetch the authoritative repository and verify the commit before tagging:

```sh
git remote add upstream https://github.com/wavefnd/Vex.git  # if not already configured
git fetch upstream
git switch master
git merge --ff-only upstream/master
git status --short --branch
git log -1 --oneline
python3 x.py check
git tag -s v0.0.1 -m "Vex v0.0.1"
python3 x.py verify-release
git push upstream v0.0.1
```

If signed tags are not available in the maintainer environment, `git tag -a`
meets the automation's minimum annotated-tag requirement. Record that exception
in the release notes. Never replace or move a published release tag.

Pushing `v*` to `wavefnd/Vex` starts `.github/workflows/release.yml`. Pushing a
tag only to a personal fork does not create the official release. A maintainer
can rerun the same workflow manually with an existing annotated tag; the
workflow still checks that the tag matches `Cargo.toml` and the checked-out
commit.

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

- A failed matrix does not create a GitHub Release. Fix the problem in a new
  commit and use a new pre-release version or tag; do not move a public tag.
- If the workflow fails before the draft is created, inspect the failed target,
  correct the release commit, and restart the release process with an
  appropriate new tag.
- If review finds a problem in an unpublished draft, delete the draft and its
  unadvertised tag only after confirming no user depends on it, then prepare a
  corrected release commit and tag.
- Never publish a partial set of target archives or hand-edit generated
  archives and checksums.
