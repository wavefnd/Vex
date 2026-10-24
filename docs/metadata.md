# Project metadata schemas

`vex metadata [--format=json] [--format-version <1|2>] [--locked] [--offline]`
prints exactly one UTF-8 JSON document on stdout. Diagnostics and lock-wait status
use stderr. The default schema version remains 1. No compiler is required.
`--manifest-path` selects a project using the same rules as build.

The command only reads the existing local graph under a shared project lease.
It creates no coordination file, lockfile, checkout, transaction, recovery or
build output. A missing/inconsistent graph, missing coordination state, dirty or
mismatched Git checkout, unsupported lockfile or pending recovery is an error.
Run `vex fetch` explicitly to prepare or recover that state. Validation never
queries a remote. An explicitly requested `--message-file` remains a separate
user-requested JSONL output, with the usual no-overwrite contract.

Both versions contain:

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer 1 or 2; consumers must check it |
| `root` | Selected project's package record |
| `target_directory` | Absolute project `target` path, whether or not it exists |
| `packages` | Direct and transitive dependencies, sorted by package name |

Package records share `name`, `version`, `manifest_format` (1, 2, or 3), `compiler`
(null or an exact version requirement), `root` (physical absolute directory),
`manifest_path` (absolute `vex.ws` path), nullable `native` input declarations,
and `dependencies` (sorted direct package names). Package names are unique in the
graph. Root and dependency edges refer to `packages`.

Dependency records additionally contain `source`:

- Path: `kind: "path"` and `requested`, the declared path string.
- Git: `kind: "git"`, credential-free `url`, full exact `commit`, and nullable
  `branch`, `tag`, `rev` selectors. At most one selector is non-null.

## Schema 1

[JSON schema](schemas/metadata-v1.json). Each package contains exactly one `kind`
(`binary` or `library`) and absolute `entry_path`. For legacy manifests this is
`src/main.wave` or `src/lib.wave`; a format-3 single binary reports its declared
path. Metadata does not compile or search for a fallback input.

Schema 1 requires each root/dependency package to have exactly one target. A
mixed library/binary or multiple-binary package produces an error with instructions
to use `--format-version 2`; targets are never silently omitted or merged into a
misleading single package kind. Existing single-target field meanings and types
are preserved, and callers can explicitly request `--format-version 1`.

## Schema 2

[JSON schema](schemas/metadata-v2.json). Each root and dependency package replaces
`kind` and `entry_path` with `targets`, an array in deterministic manifest order.
Library precedes the declared binaries. Each target contains:

| Field | Meaning |
| --- | --- |
| `name` | Declared binary name, or package name for its library |
| `kind` | `library` or `binary` |
| `entry_path` | Absolute declared entry path |
| `output` | `object` or `shared` for library, null for binary |
| `exports` | Declared C export symbol list; empty when absent |
| `platform` | Nullable `mode`, `entry`, `startup`, and `linker_script` fields |

Each package also exposes `default_run` (nullable binary name), `profiles` (object
keyed by profile name with effective `opt_level` and nullable `inherits`),
`build_target` (nullable project default triple), and `target_config` (array of
exact-triple declarations). A target configuration has `triple`, nullable
`sysroot` and `linker`, and a `bins` array of `{name, platform}` overrides.

Platform/sysroot/linker-script paths retain their declaration spelling and are
not probed for SDK or compiler availability. These fields describe each package;
dependency tool settings and profiles do not override the root build. Metadata
does not apply target overrides or claim compiler support. It exposes no guessed
host triple, selected profile, or final artifact paths. Legacy packages are
normalized to one target and the default dev/release profiles in schema 2.

## Stability

Paths preserve Unicode and native separators. A path that cannot be expressed as
a JSON string is an explicit error, never a lossy replacement. Git URLs exclude
source authentication. The lockfile is preserved byte-for-byte, including v2,
when compatible; a v1 requiring migration is rejected until an explicit fetch.

The same project/graph on the same filesystem produces deterministic output.
Absolute paths intentionally differ when the project is moved. Object-key order
is not an API guarantee. Consumers should ignore additive fields in their chosen
schema; changes to existing field meanings or types require a new schema version.
Unsupported future manifest formats fail without migration.

Format 4 graphs require schema 2. See [dependency features](dependency-features.md) for binding keys, concrete IDs, union records and active selection.
