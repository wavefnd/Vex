# Package targets, profiles, and artifacts

Manifest format 3 declares a package's build targets. Formats 1 and 2 retain their
single-target meaning and string rules; Vex reads them without rewriting the
manifest. Their outputs use the same new layout as format 3. Lockfile v3 is
unchanged: target declarations do not introduce package self-edges or alter the
meaning of a dependency source, commit, or edge.

## Declare targets

```wson
{
    format = 3,
    name = "demo",
    version = "0.1.0",
    targets = {
        lib = { output = "object" },
        bins = [
            { name = "demo", path = "src/main.wave" },
            { name = "admin", path = "src/bin/admin.wave" }
        ]
    },
    default_run = "demo",
    profiles = {
        dev = { opt_level = 0 },
        release = { opt_level = 2 },
        fast = { inherits = "release", opt_level = 3 }
    }
}
```

A package declares at least one target. `targets.lib` is optional, names the
package's sole library, and uses `src/lib.wave`. Its `output` is `object` (the
default) or `shared`; `exports` is a list of C symbols for shared output, following
[the native ABI contract](native-interop.md). Format 3 does not accept root `lib`
or `native.output`/`native.exports`; declare those choices in `targets.lib`.
`native.libraries` and `native.search_paths` still describe package-owned inputs.

Each `targets.bins` entry requires `name` and `path`. Paths are portable relative
`.wave` paths inside the package, without symlink escapes. Vex does not discover
additional binaries by scanning `src/bin`. Binary/profile names use portable
`[A-Za-z_][A-Za-z0-9_-]*` components and exclude Windows device names; names must
not differ only in ASCII case. A binary and library may share a name because their
artifacts use separate directories.

A binary imports its own package library through the ordinary Wave package import
contract. Vex does not automatically link a separately emitted copy of that
library object into the binary. A dependency package must provide a library;
its declared binaries are not built as part of consuming that dependency.

## Select targets and profiles

`build` and `check` select every root target by default. `--lib` selects the
library, and repeatable `--bin <name>` selects declared binaries. Selection errors
are reported before compilation. All selected plans and output destinations are
validated before the first compiler execution. Vex then processes targets in a
deterministic order and stops at the first failure. Previously completed target
artifacts remain available; the command is not a transaction across targets.

`run` selects `--bin <name>`, then `default_run`, then the only binary. A package
with no binary or several binaries without a default produces an actionable
error. A library is never an executable run target. Bare WebAssembly has no process
argument ABI; nonempty program arguments require a WASI target.

```sh
vex build                         # All root targets, dev profile
vex check --lib --bin demo         # Selected library and binary
vex build --bin admin --profile fast
vex build --release                # Alias for --profile release
vex run --bin demo -- argument
```

`--profile <name>` defaults to `dev`. `--release` and `--profile` cannot be used
together. Profiles are selected from the root manifest and apply to the entire
build, including imported dependency code. The default optimization levels are
`dev = 0` and `release = 2`. Either builtin can set `opt_level`; a custom profile
must inherit exactly `dev` or `release`, and may override its level. Custom-profile
chains are not supported. `opt_level` accepts integers 0, 1, 2, or 3 only.

Debug information, overflow/assertion behavior, LTO, and `s`/`z` optimizations are
not declared because the supported compiler contract does not guarantee them.
Unknown settings fail instead of being silently ignored. Dependency profiles do
not override the root's chosen build settings.

## Configure a compiler target

`--target <triple>` selects a compiler platform, separately from a package target
such as `--bin admin`. When absent, `build.target` supplies a project default;
otherwise the selected compiler supplies its default. Vex validates the selected
triple and applicable settings against compiler capabilities before building.
Settings match an exact compiler triple, with no target predicate language or
change to the Wave dependency graph.

```wson
{
    format = 3,
    name = "firmware",
    targets = {
        bins = [{
            name = "firmware",
            path = "src/main.wave",
            mode = "freestanding",
            entry = "start",
            startup = "none",
            linker_script = "link/firmware.ld"
        }]
    },
    build = { target = "aarch64-unknown-linux-gnu" },
    target_config = [{
        triple = "aarch64-unknown-linux-gnu",
        sysroot = "/opt/sdk/aarch64",
        linker = "aarch64-linux-gnu-gcc",
        bins = [{ name = "firmware", entry = "platform_start" }]
    }]
}
```

The example describes settings; availability of a matching compiler target,
SDK, linker, startup environment and entry point must be verified separately.
Vex does not install them automatically.

| Field | Contract |
| --- | --- |
| Binary `mode` | `hosted`, `freestanding`, `wasi`, or `bare_wasm`; absent uses the selected target's native/Wasm hostedness |
| Binary `entry` | ASCII symbol identifier; freestanding/bare Wasm modes only |
| Binary `startup` | `default` or `none`; `none` requires freestanding/bare Wasm |
| Binary `linker_script` | Package-relative linker-script path; freestanding only |
| `build.target` | Optional default exact compiler triple |
| `target_config[].triple` | Unique exact triple selecting these settings |
| `target_config[].sysroot` | SDK directory, relative to project root or absolute; applied to compile and link |
| `target_config[].linker` | One executable name or path, with no embedded arguments |
| `target_config[].bins` | Named binary overrides of mode/entry/startup/linker_script |

An override changes only explicitly present fields. The resulting combination
must be valid. A sysroot can point outside the project; package source and linker
script paths remain constrained to their declaring project. Only root project
tool settings choose the linker/sysroot for a build; dependency manifests do not
replace them. Unsupported compiler/target combinations fail before compilation.
For the supported compiler, a sysroot is available on ELF and bundled Mach-O
linkers; a custom Mach-O linker with sysroot, COFF sysroots, and Wasm sysroots
are rejected because one consistent compile/link SDK cannot be guaranteed.
Raw compiler/linker flag lists are not part of this manifest interface.

## Output layout and cleanup

Every build, including legacy manifests and builds without native inputs, uses:

```text
target/<compiler-triple>/<profile>/
├── bin/<binary-name>[.exe or .wasm]
├── lib/<library artifact and companion files>
└── .vex-obj/<kind>/<name>/<output-kind>/
```

Object libraries are `<package>.o` (`.obj` on Windows). Shared libraries use
`lib<package>.so`, `lib<package>.dylib`, or `<package>.dll`; import libraries and
other compiler companions remain alongside their library output. Bin and lib
namespaces prevent collisions when a Windows binary and DLL have the same name.

Old output files are not moved, adopted, or deleted automatically. Vex continues
to refuse overwriting unknown or edited artifacts. `clean` removes only unchanged
recorded outputs. `run` retains a unique `target/.vex-run/<generation>/` prefix,
releases the build lock before spawning the program, and protects live outputs
with a run lease. There is no automatic generation garbage collection.

## Dry-run and metadata

`build --dry-run`, `check --dry-run`, and `run --dry-run` always print one
Vex-owned JSON document,
even when selecting a single target:

```json
{
  "schema": "vex.build-plan",
  "schema_version": 2,
  "target": "x86_64-unknown-linux-gnu",
  "profile": { "name": "dev", "opt_level": 0 },
  "plans": [
    {
      "package_target": { "name": "demo", "kind": "binary" },
      "compiler_plan": { "schema_version": 1 }
    }
  ]
}
```

`compiler_plan` contains the complete validated compiler plan; the abbreviated
object above illustrates the envelope only. There is one planner invocation per
selected package target. Compiler capability queries are cached for that Vex
process. Dry-run may create the existing coordination directory/file only, and
never outputs, dependency checkouts, recovery, or a lockfile rewrite. The former
direct compiler-plan stdout shape is replaced by this envelope.

[Metadata schema 2](metadata.md) exposes the declared targets/profiles/platform
configuration without querying a compiler. It does not guess an actual default
compiler triple or an artifact path. `vex metadata` defaults to schema 1 for
single-target consumers; use `--format-version 2` for mixed or multiple targets.

Format 5 extends the library declarations, external storage, completed results
and dry-run schema 2. See [external builds](external-builds.md). Formats 1–4
retain their meanings; new initialization uses format 5 with a ready v4 lock.
