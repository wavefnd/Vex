# CLI outcome and message contract (#90)

Status: approved and merged in #149. Project metadata was merged separately in
#153. Each subsequent change still requires its own native platform CI evidence.

## Exit outcomes

| Code | Category | Meaning |
| --- | --- | --- |
| 0 | success | Completed command, including help/version |
| 1 | internal | Unexpected Vex failure; not a fallback for known operational errors |
| 2 | usage | Unknown command/option, malformed option value, incompatible options |
| 3 | resolution | Invalid project/lock data, invalid package name, unresolved dependency, invalid update selection, locked/offline constraints |
| 4 | compiler | Compiler rejection of a build/check or invalid/incompatible compiler plan |
| 5 | environment | Missing project/compiler, filesystem permissions, unavailable transport/tool, authentication/network failure, failed installation verification |
| 124 | timeout | Vex-supervised tool exceeded its deadline |
| 130 | cancelled | Vex observed cancellation and supervised shutdown |

An executed user program retains its own exit code, including values used by
Vex. Unix signal termination maps to 128 + signal. A runtime code of 3 is not a
resolution error: machine-readable outcomes identify their origin as `program`.
Cancellation observed by Vex takes precedence over a racing ordinary child
completion; ordinary program signal termination is still a program outcome.
The runtime has no implicit timeout.

Vex-owned stdout uses fallible writes on Unix and Windows. A broken pipe stops
further stdout output and keeps the command outcome; other stdout failures are
environment errors (5). Diagnostic writes are best effort if stderr is closed,
and cannot replace the original failure or program exit. Compiler-plan stderr
forwarding failures stop before compilation with an environment error. Inherited
compiler/program stdio is unchanged. A message file remains a separate channel.

Classification follows the error's origin, not a search of its text or only
the command phase. For example, an invalid manifest is resolution, a missing
manifest is environment, and permission denied while reading either is
environment. A Git authentication failure is environment; a verified missing
requested revision is resolution. A failed compiler spawn is environment;
compiler-reported build failure or an incompatible plan is compiler. The wavec
environment exit code 3 becomes Vex environment (5), not resolution (3). An invalid
setup version argument is usage; a checksum mismatch is environment. A pending
recovery journal that dry-run cannot recover is environment. Refusal to overwrite
existing user files during init is environment.

Introduce typed errors at their origin and propagate them through crate
boundaries. Preserve useful contextual messages and credential redaction. Do not
infer dependency failure versus network failure from localized Git stderr; use
an explicit verification where possible, and classify an ambiguous transport
failure as environment.

## Machine-readable messages

Global syntax: `vex --message-file <new-path> <command> ...`.
No implicit JSON stream on stdout or stderr. Human status and compiler/program
streams keep their existing destinations. The file is UTF-8 JSON Lines: one
complete compact JSON object followed by a newline for each event. It contains
Vex events, not arbitrary compiler or user-program output. Consumers must not
parse human messages to identify outcomes.

The option applies to every command. It precedes the subcommand and is never
interpreted from runtime arguments following `--`. Duplicate, missing, or invalid
global options are usage errors. If an unambiguous message destination has not
been established, argument errors can only be reported on stderr.

Common fields: `schema_version` (1), `sequence` (monotonically increasing from
1), `event`, and `command`. Initial event `started`; intermediate `status` (with
a `phase` such as `compiler` or `running`) and
`diagnostic`; final event `finished`. Diagnostic fields: `severity`, `category`,
and redacted `message`. Finished fields: `success`, `origin` (`vex` or `program`),
`category`, `exit_code`, and nullable `signal`. Categories include the exit-table
names plus `program`. A successful program outcome uses category `program`,
success true, and code 0. A cancelled runtime uses origin `vex` and category
`cancelled`. There is no raw argv/environment dump or embedded compiler plan.
Diagnostic text includes explanatory context with source authentication redacted. Optional new fields may be added within schema 1; incompatible
semantics require a new schema version. Consumers ignore unknown fields/events.

Example, user program exits 42:

```json
{"schema_version":1,"sequence":1,"event":"started","command":"run"}
{"schema_version":1,"sequence":2,"event":"finished","command":"run","success":false,"origin":"program","category":"program","exit_code":42,"signal":null}
```

A missing final event means an incomplete report, never success. Ordinary
handled outcomes emit exactly one final event when the sink remains writable.
Forced termination, crashes, and storage failure can leave an incomplete final
line. Readers accept complete lines and detect absence of completion. Flush each
event; this is an observation stream, not a durable transaction journal.

## File and dry-run behavior

Create the report exclusively, before project mutation, in an existing parent
directory. Refuse an existing destination (including symlinks); do not truncate,
append, follow a destination symlink, create parent directories, or silently
fall back to a different output. Use owner-only permissions on Unix; normal
inherited access controls apply on Windows. Report paths are user-selected.

`--message-file` combined with `--dry-run` validates the destination but creates
no report. An existing destination (including a dangling symlink) or missing parent
is an environment error, just as for an ordinary report. Dry-run still permits
coordination files only, with no recovery or migration. Existing dry-run plan
output on stdout remains unchanged. Flags after the run argument separator `--`
are program arguments and do not change Vex reporting behavior.

If opening/writing the report fails before runtime starts, stop further work
and return environment (5). Already committed project changes are not rolled
back solely because logging failed. Do not start the user program if the sink
has already failed. If reporting fails after runtime starts, report that failure
on stderr and preserve the program's outcome (or observed Vex cancellation);
logging failure must not terminate an otherwise valid running program or replace
its exit code. An incomplete file does not count as a successful machine report.

## Implementation and acceptance

1. Centralize typed outcomes and command completion, replacing direct command
   exits where needed. Apply the same taxonomy to every command.
2. Add the global report option, credential-safe event writer, and final outcome
   emission, while retaining existing human and runtime streams.
3. Add process tests for every exit family, origin collisions, help/version,
   malformed options, compiler/tool failures, timeout/cancellation, report-path
   conflicts, sink failures, dry-run no-write behavior, and incomplete reports.
4. Verify runtime stdin/stdout/stderr, terminal behavior, project lock release
   before spawn, and exact program exit preservation remain unchanged.
5. Run the full local suite and cross checks. Native Windows/macOS execution
   remains a PR CI acceptance requirement; cross checks do not satisfy it.

Cross-platform process fixtures cover category/origin reporting and report write
failures. Unix tests additionally distinguish direct program signal termination
from Vex cancellation. Forced aborts and operating-system termination cannot be
guaranteed to emit a final event. Missing final events always mean incomplete.

## Structured compiler and artifact events

With `--message-file`, compiler diagnostics are recorded as `compiler-diagnostic`
events containing the original JSON payload, including source spans. Compiler
stdout/stderr is bounded during this reporting mode; user-program streams and TTY
remain inherited. `artifact` events record target, emit kind, output paths and the
run executable before the program starts. Vex diagnostic events include structured
context fields and cause chains. Neither human messages nor localized compiler text
is reparsed to assign an exit category. Report writes still stop work before runtime
starts and never replace an already executed program's exit status.
