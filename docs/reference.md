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
| `format` | integer 1, 2, or 3 | 1 when absent; generated single-target manifests use 2 |
| `name` | string | Required package/import identity |
| `version` | version literal or string | `0.1.0`; dependency equality is exact |
| `lib` | boolean | Formats 1/2 only: false; true selects `src/lib.wave`, otherwise `src/main.wave` |
| `compiler` | string | Absent means no version requirement; exact canonical wavec version, including prerelease, without build metadata |
| `description`, `author`, `license` | string | Optional descriptive metadata |
| `dependencies` | array of objects | Empty when absent |
| `native` | object | Prebuilt library inputs and search paths; formats 1/2 also declare library output here; see [native interoperability](native-interop.md) |
| `targets` | object | Required in format 3; optional `lib` and array `bins` declare at least one target |
| `default_run` | string | Format 3: optional declared binary name |
| `profiles` | object | Format 3: dev/release overrides and custom profiles inheriting one builtin |
| `build` | object | Format 3: optional default compiler `target` triple |
| `target_config` | array of objects | Format 3: exact-triple sysroot/linker and binary platform overrides |

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

See [targets, profiles, and artifacts](targets-and-profiles.md) for all format-3
fields, selection, platform restrictions, and migration. Existing formats are
not automatically rewritten. Library dependencies may declare binaries in format
3, but consumers only use their library.

Format 1 preserves legacy literal backslashes. Formats 2 and 3 decode `\\`, `\"`,
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
program. A separate inherited OS lease protects each run generation until the
program and its children release their handles. There is no automatic
generation/backup garbage collection.

## Inspection and cleanup

`tree --invert <name>` shows why a package is needed, walking dependents back to
the project root. `--depth <n>` limits edges from the displayed root; zero prints
only that root. The default is the complete dependency tree. These options use
the usual resolution rules; use `--locked --offline` to prohibit lock changes
and network access.

`doctor` inspects Vex, Git, compiler selection/version/targets, project state,
and available plan/linker tools without fetching or repairing. It also works
outside a project. `--target <triple>` selects a target and `--format=json`
returns [doctor schema v1](doctor.md). Missing local graph data requires an
explicit `fetch` before project-plan inspection. Optional cross-linker gaps are
warnings; required tool or protocol failures produce a nonzero exit.

`clean` deletes only unchanged outputs whose creation Vex recorded; `--dry-run`
previews without creating coordination or message files. It preserves sources,
manifest/lockfiles, dependencies, legacy/untracked outputs, edited outputs,
symlinks/reparse points, and running generations. Empty owned directories can
be removed, but directories containing unknown files are retained. Ownership
begins with builds made by this version; existing files are never adopted merely
because a later compiler invocation writes the same path. Run lease files remain
as coordination identities, even after their generation is removed.
Builds now refuse to overwrite these unknown or edited files; move them aside
explicitly before rebuilding. A successful build verifies its declared outputs
exist before reporting artifact events.

`prune` previews unused Git checkouts; `--apply` performs deletion. A complete
current local graph and existing project coordination are required; run `fetch`
when they are missing or stale. Validation never fetches implicitly. Only newly
published checkouts with an intact ownership record are eligible. Dirty,
untracked, ignored, modified Git metadata, legacy/unrecorded checkouts, and
unknown files are preserved. Both cleanup commands refuse pending recovery;
use a normal `fetch` to recover first. Transaction backups and compiler
installations are outside cleanup scope. A verified shared Git cache may restore exact commits; otherwise
reusing a pruned dependency may require network access through `fetch`.

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

Help, version, info, tree, metadata, doctor, prune reports and requested dry-run
data use stdout. Status,
warnings and errors use stderr. User program stdin/stdout/stderr and TTY are
inherited. Raw wavec flags are not Vex flags; `run -- <args...>` passes native
arguments directly to the program.

Global `-q` / `--quiet` suppresses Vex progress only. Results, warnings, errors,
compiler/program streams and message-file events remain available. `-v` /
`--verbose` adds major selection decisions; `-vv` adds diagnostic context and
reasons. Verbosity is capped at two and conflicts with quiet. All Vex diagnostic
text is credential-redacted. Options after `run --` belong to the program.

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

Machine-readable schemas: [metadata v1](schemas/metadata-v1.json),
[metadata v2](schemas/metadata-v2.json), [build-plan v1](schemas/build-plan-v1.json),
[doctor](schemas/doctor-v1.json) and
[message events](schemas/messages-v1.json). Additive fields are permitted; changed
existing meanings require a new schema version. There is no registry or publish
command in this release.


## Build selection and outputs

`--lib` and repeatable `--bin <name>` select root package targets. Build/check
without selectors process all root targets; run uses `--bin`, `default_run`, or
the only binary. `--target <triple>` still selects a compiler platform.
`--profile <name>` defaults to dev; `--release` aliases the release profile and
conflicts with an explicit profile. The root's profile is used for imported code.

Every selected target is planned and preflighted before compilation. Targets then
compile in deterministic order, stopping at the first failure while preserving
already successful artifacts. General and native output paths are unified under
`target/<compiler-triple>/<profile>/{bin,lib}/`, with intermediate objects under
`.vex-obj/<kind>/<name>/<output-kind>/`. Old paths are left untouched. Run keeps an
independent generation, lease and lock-release boundary.

Dry-run stdout is one `vex.build-plan` schema-1 envelope for single or multiple
targets. It contains `target`, `profile: {name, opt_level}`, and ordered `plans`
entries with `package_target: {name, kind}` and the validated `compiler_plan`.
It replaces the previous direct compiler-plan output. Planning runs once per
selected target; capability queries are cached within the invocation. See the
[full build contract](targets-and-profiles.md).

## Verified build reuse

Freshness records live in owned `target/.vex-cache/<content-key>/` directories.
Inputs include package and transitive `.wave` sources and manifests, the exact
union lock, selected features, target capabilities/schema and profile arguments,
native prebuilt inputs, compiler executable/runtime libraries, standard-library
contents and relevant toolchain environment. File timestamps are not freshness.
Input contents are checked again before reuse and before recording successful
compilation. Failed compilation and program/test results are never recorded.

The initial implementation establishes compiler runtime closure on Linux ELF
hosts with `ldd`; other hosts, unavailable queries, symlinks, unknown native
source dependencies, SDK/custom linker settings and implicit hosted system links
compile normally without reuse. Check/object plans can be reused when complete
inputs are known. Wasm links additionally require an absolute linker with known
runtime and declared inputs. Correctness takes precedence over the hit rate.

Every command still validates its compiler plan and destination ownership. Hits
verify cached bytes and copy them into owned outputs, including a new inode for
each run generation. Missing, partial or corrupt entries are misses; unknown or
modified cache data is not silently overwritten. `clean` handles unmodified
owned cache files and preserves modified files. There is no automatic GC.
Verbose output explains eligibility, fresh copies, dirty/missing entries and
rebuilds; JSONL exposes a `freshness` event with `reused`.
