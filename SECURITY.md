# Security Policy

## Supported versions

Vex has not published a stable release yet. Until v0.0.1 is released, security
fixes are made on the latest `master` branch. This table will be updated when a
public release is available.

| Version | Supported |
|---|---|
| `master` | Best effort |
| Unreleased snapshots and older commits | No |

## Reporting a vulnerability

Do not disclose a suspected vulnerability in a public issue, pull request,
discussion, or email list.

Use GitHub's private vulnerability reporting for
[`wavefnd/Vex`](https://github.com/wavefnd/Vex/security/advisories/new) when the
report form is available. Otherwise email `luna@lunastev.org` with the subject
`[Vex SECURITY]` and ask to establish a private reporting channel. Do not place
exploit details in an initial unencrypted email if that would put users at risk.

Include, when possible:

- affected Vex version or commit
- affected operating system and architecture
- impact and attack prerequisites
- minimal reproduction steps or proof of concept
- known mitigations
- whether the issue has been disclosed elsewhere

Maintainers will acknowledge reports on a best-effort basis, coordinate a fix
and disclosure timeline with the reporter, and credit reporters who request it
when publishing an advisory.

## Scope

Security-sensitive areas include dependency source validation, Git checkout
handling, lockfile integrity, path traversal, command execution, archive or
release integrity, and the boundary between Vex and `wavec`.

Dependency confusion against a registry is currently out of scope because Vex
does not implement a central registry. Vulnerabilities in `wavec` or a third-party
Wave package should be reported to that project's maintainers unless Vex creates
or amplifies the issue.

## Responsible disclosure

Please allow maintainers a reasonable opportunity to investigate and release a
fix before public disclosure. Maintainers will avoid requesting unnecessary
personal information and will keep the report private until coordinated
disclosure.
