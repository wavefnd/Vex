# Contributing to Vex

Thank you for helping improve Vex, the package manager and build tool for the
Wave programming language. Vex accepts focused contributions through GitHub
pull requests and email patches.

## Project direction

Vex is not a raw command-line wrapper around `wavec`. It owns the Wave project
manifest, dependency resolution, the lockfile, and compiler orchestration.
Changes should preserve these rules:

- `vex.ws` is the supported project manifest.
- `wavec` is an internal compiler dependency, not a source of Vex CLI flags.
- Git and path dependencies are the current package sources; a central registry
  and publishing are outside the current scope.
- The same manifest and lockfile must produce the same dependency graph.
- `--locked` must prevent lockfile changes.
- `--offline` must prevent Git network access.
- Errors and progress should explain what Vex is doing and how users can recover.

Please open an issue before beginning a large change that alters these product
boundaries or a persistent file format.

## Development setup

You need:

- a stable Rust toolchain with `rustfmt` and `clippy`
- Python 3.11 or newer for `x.py` release tooling
- Git for dependency integration tests
- a compatible `wavec` in `PATH` for end-to-end build and run tests

Set `VEX_WAVEC=/path/to/wavec` when testing a specific compiler binary.

Clone your fork and run the baseline checks:

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

The same baseline is available through the repository release tool:

```sh
python3 x.py check
```

## Making a change

Create a branch from the current `wavefnd/Vex:master`. Use `feat/<topic>` for
features and `patch/<topic>` for fixes. Keep each branch focused on one logical
change.

```sh
git switch -c feat/example
```

Do not overwrite unrelated working-tree changes. Do not commit build output,
managed dependencies under `.vex/`, local release notes under `.tmp/`, secrets,
or editor state.

## Tests

Add tests at the same level as the behavior being changed:

- parser and policy details belong in unit tests
- dependency graph and Git behavior belong in integration tests
- compiler invocation changes require dry-run schema and end-to-end smoke tests
- release-tool changes require `python3 -m unittest discover -s tests/xpy -v`
- package changes require archive-content, checksum, and executable smoke tests
- platform changes must preserve native integration coverage where a hosted
  runner exists; cross-build-only targets must be documented as experimental

Git integration tests must use local fixture repositories and must not require
external network access. Dependency changes should cover direct and transitive
graphs, exact locked commits, cycles, source/version/name conflicts, and relevant
`--locked` and `--offline` behavior.

When changing selective update behavior, prove that unrelated locked commits and
remote-tracking refs remain unchanged.

Release packages are created with `python3 x.py build` followed by
`python3 x.py package`. Do not hand-edit `dist/` artifacts. The stricter
`python3 x.py release` command is reserved for a clean commit carrying the exact
`v<version>` tag.

## Pull requests

Push your branch to a fork and open a pull request against
`wavefnd/Vex:master`. A pull request should contain:

- a concise Summary
- Why the change is needed
- observable Behavior and compatibility impact
- Validation commands and results
- documentation updates for user-facing behavior

Draft pull requests are welcome for early review. Keep commits understandable
and avoid mixing cleanup with functional changes.

## Email patches

Email patches are accepted when GitHub is not suitable. Send them to
`luna@lunastev.org` with a subject beginning `[Vex PATCH]`.

```sh
git commit -s
git format-patch --cover-letter -1
git send-email --to luna@lunastev.org *.patch
```

Use `[Vex PATCH 0/N]` for a multi-patch series. Each patch should build on the
previous one, explain its purpose in the commit message, and address one logical
change. Do not send security vulnerabilities through a public mailing list or
public issue; follow [SECURITY.md](SECURITY.md).

## Developer Certificate of Origin

New contributions must include a `Signed-off-by:` line certifying the
[Developer Certificate of Origin 1.1](https://developercertificate.org/).
Create it with:

```sh
git commit -s
```

By signing off, you certify that you have the right to submit the contribution
under the project's license.

## Documentation and compatibility

Public command syntax, lockfile behavior, environment variables, supported
platforms, and recovery instructions must be documented. A lockfile format
change needs an explicit compatibility and migration plan; silently reinterpreting
an existing lockfile is not acceptable.

Use clear English for code, public identifiers, commit messages, and canonical
project documentation. Translations may be added alongside the canonical text.

## Conduct and licensing

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).
Security reports follow [SECURITY.md](SECURITY.md).

Unless a file says otherwise, contributions are licensed under the
[Mozilla Public License 2.0](LICENSE).
