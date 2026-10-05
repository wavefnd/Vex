# Metadata schema v1

`vex metadata [--format=json] [--locked] [--offline]` prints exactly one UTF-8 JSON
document on stdout. Diagnostics and lock-wait status use stderr. No compiler is
required. `--manifest-path` selects a project using the same rules as build.

The command only reads the existing local graph under a shared project lease.
It creates no coordination file, lockfile, checkout, transaction, recovery or
build output. A missing/inconsistent graph, missing coordination state, dirty or
mismatched Git checkout, unsupported lockfile or pending recovery is an error.
Run `vex fetch` explicitly to prepare or recover that state. Validation never
queries a remote. An explicitly requested `--message-file` remains a separate
user-requested JSONL output, with the usual no-overwrite contract.

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer `1`; consumers must check it |
| `root` | Selected project's package record |
| `target_directory` | Absolute project `target` path, whether or not it exists |
| `packages` | All direct and transitive dependencies, sorted by package name |

Root and dependency package records contain `name`, `version`, `kind` (`binary`
or `library`), `root` (physical absolute directory), `manifest_path` (absolute
`vex.ws` path), `entry_path` (absolute canonical entry layout, `src/main.wave` or
`src/lib.wave`), and `dependencies` (sorted direct package names). `entry_path`
describes the manifest's conventional entry; metadata does not compile or search
for a fallback input. Package names are unique in the graph. Root edges refer to
`packages`, and dependency edges refer to the same list.

Dependency records also contain `source`:

- Path: `kind: "path"` and `requested`, the declared path string.
- Git: `kind: "git"`, credential-free `url`, full exact `commit`, and nullable
  `branch`, `tag`, `rev` selectors. At most one selector is non-null.

Paths use native separators and preserve Unicode. A path that cannot be expressed
as a JSON string is an explicit error, never a lossy replacement. Git URLs exclude
source authentication. The lockfile is preserved byte-for-byte, including v2,
when compatible; v1 requiring migration is rejected until an explicit fetch.

The same project/graph on the same filesystem produces deterministic output.
Absolute paths intentionally differ when the project is moved. Object-key order
is not an API guarantee. Consumers should ignore additive fields in schema v1;
changes to existing field meanings or types require a new schema version.

Every root/package object exposes `manifest_format` (legacy default 1, escaped
strings 2) and `compiler` (null or an exact version requirement). These are additive
schema-1 fields. Metadata still does not invoke a compiler, fetch, recover, or write
project state. Unsupported future manifest formats fail without migration.
