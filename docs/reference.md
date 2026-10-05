# Manifest and operational reference

[Command help](commands.md) is generated from the executable and verified on each
platform. Project commands select `--manifest-path` first, otherwise search real
ancestors from the invocation directory. `init` uses the current directory.
Builds and user programs execute from the selected project root.

## Manifest

`vex.ws` is a WSON object. Unknown and duplicate fields are errors. Names match
`[A-Za-z_][A-Za-z0-9_]*`; initialization rejects invalid names without conversion.

| Field | Type | Default / meaning |
| --- | --- | --- |
| `format` | integer 1 or 2 | 1 when absent; generated manifests use 2 |
| `name` | string | Required package/import identity |
| `version` | version literal or string | `0.1.0`; dependency equality is exact |
| `lib` | boolean | false; true selects `src/lib.wave`, otherwise `src/main.wave` |
| `compiler` | string | Absent means no version requirement; exact canonical wavec version, including prerelease, without build metadata |
| `description`, `author`, `license` | string | Optional descriptive metadata |
| `dependencies` | array of objects | Empty when absent |

A dependency requires a unique `name` and exactly one nonempty `path` or `git`
string. Optional `version` is an exact package version assertion, not a range.
Git alone accepts at most one nonempty `branch`, `tag` or `rev` string. Without a
selector, initial fetch and explicit update follow the remote default branch;
locked reuse preserves the exact commit. Path values resolve relative to their
own declaring manifest. Dependencies must declare libraries. One package name
cannot identify conflicting sources or versions anywhere in the graph.

```wson
{
    format = 2,
    name = "application",
    compiler = "0.2.1-pre-beta",
    dependencies = [{ name = "greeting", path = "../greeting" }]
}
```

Format 1 preserves legacy literal backslashes. Format 2 decodes `\\`, `\"`,
`\/`, `\b`, `\f`, `\n`, `\r`, `\t`, and JSON `\uXXXX` sequences;
unsupported escapes are rejected. A format change is
explicit: files are never silently reinterpreted. Additive fields may retain a
format version; incompatible string or field semantics require a new version.
Future formats fail without rewriting the manifest. Metadata exposes the format
and compiler requirement. See [compatibility policy](release-readiness.md).

## Dependency state

Commit generated `vex.lock`. New writes use v3; compatible v2 stays byte-identical
under `--locked`. Legacy v1 needs an explicit normal fetch to migrate. Unsupported,
malformed, conflicting or cyclic locks fail before publication. Git commits are
exact; path content is not pinned and must be versioned separately.

`fetch` prepares/reuses the graph. `update` refreshes all Git dependencies;
`update <name>...` validates names entirely from the local graph and requires
`fetch` when that graph is incomplete. Typo validation never contacts a remote.
`--locked` prohibits a lockfile change; `--offline` prohibits Git network access.
Both still validate manifests and checkout integrity. Git credentials may be
provided for transport but are redacted from reports and never stored in the
lockfile. Git submodules are rejected. See [source policies](../README.md).

A project lease covers lockfile reads, resolution and compilation. Staged checkout
and lockfile publication uses a recovery journal. Normal commands recover pending
publication; dry-run fails without recovering. Dry-run permits coordination files
only, with no message file, checkout, lockfile migration or build output. Each run
has its own output generation, and releases its build lease before spawning the
program. There is no automatic generation/backup garbage collection.

## Environment

| Variable | Contract |
| --- | --- |
| `VEX_WAVEC` | Explicit selected compiler; empty is an error; relative paths use invocation directory |
| `PATH` | Compiler and required system-tool lookup; precedes the managed compiler |
| `VEX_TOOLCHAIN_HOME` | Managed compiler installation root; default documented in [installation](../README.md#install) |
| `VEX_WAVEC_ARCHIVE_SHA256` | Optional independent exact SHA-256 pin for compiler installation |
| `VEX_GIT_TIMEOUT` | Integer seconds 1–86400 per Git command; default 300 |
| `NO_COLOR` | Presence disables color; redirected output never contains automatic ANSI color |

Git keeps authentication, SSH, proxy and ordinary configuration while removing
repository-selection environment overrides. Transport is noninteractive; timeout
and cancellation terminate supervised child trees. Release infrastructure options
such as `VEX_RELEASE_HOST` belong to `x.py`, not normal Vex commands.

## Streams and outcomes

Help, version, info, tree, metadata and requested dry-run data use stdout. Status,
warnings and errors use stderr. User program stdin/stdout/stderr and TTY are
inherited. Raw wavec flags are not Vex flags; `run -- <args...>` passes native
arguments directly to the program.

| Outcome | Exit code |
| --- | --- |
| Success | 0 |
| Vex internal failure | 1 |
| CLI usage | 2 |
| Dependency/project resolution | 3 |
| Compiler failure | 4 |
| Execution environment | 5 |
| Timeout | 124 |
| Vex cancellation | 130 |
| User program | Its original code; Unix signal uses 128 + signal |

A program can return the same number as Vex. Use the structured `origin` field
to distinguish them. [`--message-file`](cli-contract-design.md) writes schema-1
JSONL exclusively to a new file, never mixed into stdout. Failure before program
start stops Vex; reporting failure after program start warns and preserves the
program exit. Missing final events mean an incomplete report.

Machine-readable schemas: [metadata](schemas/metadata-v1.json) and
[message events](schemas/messages-v1.json). Additive fields are permitted; changed
existing meanings require a new schema version. There is no registry or publish
command in this release.
