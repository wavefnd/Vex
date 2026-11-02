# External builds and completed artifacts

These contracts are introduced for Vex `0.0.3-beta`. Manifest formats 1–4 keep
their meanings and are never automatically rewritten. New projects use format
5. Formats 4–5 use dependency lock v4; formats 1–3 retain v3 writing and
legacy read compatibility. Toolchain identity has a separate file.

## Source, build and dependency storage

`--build-dir <B>` selects a build root, overriding `VEX_BUILD_DIR`. Relative CLI
and environment paths resolve from the invocation directory, before project
selection. Without an override the project/workspace root is the build root.
Outputs, intermediates, caches and generations live under `B/target/`; ownership,
build publication and generation leases live under `B/.vex/`. Vex claims its own
subdirectories, not other files in the surrounding Meson build directory.

`--state-dir <S>` overrides `VEX_STATE_DIR` and chooses dependency state, including
checkouts and their recovery journal. The default is the source `.vex/`, or
`B/.vex/dependencies/` when a build root is selected. Reviewed `vex.ws`,
`vex.lock` and `vex.toolchain.lock` remain source inputs. No existing state is
silently moved. A store/build ownership marker binds storage to one source root.
Protected source/managed-output overlaps and another project's stores are errors.

All commands coordinate the source identity through a persistent OS state file
before reading its dependency lock, then acquire the selected dependency-store
lease. The OS location is `LOCALAPPDATA/Vex/state` on Windows,
`XDG_STATE_HOME/vex` or `~/.local/state/vex` on Unix (temporary-directory fallback
only when no home/state directory is available). A separate `B/.vex/build.lock`
serializes publication into one build root. Two roots may compile concurrently
from prepared source while update remains excluded until compilation completes.
These are OS leases, not PID/existence locks. Never delete a coordination inode
as a stale-lock workaround.

Prepare each selected store explicitly while source is writable:

```sh
vex --manifest-path /src/project/vex.ws --build-dir /build/one fetch --offline
vex --manifest-path /src/project/vex.ws --build-dir /build/one build --locked --offline
```

The second command works with read-only source. Preparation must already have
created the common project and dependency leases. Metadata and clean honor the
same storage selectors. Cleaning one root preserves other roots and source.
Pending dependency/build/report transactions stop clean and dry-run; a normal
fetch/build performs the appropriate validated recovery.

## Library settings and archives

```wson
{
    format = 5,
    name = "module",
    version = "1.0.0",
    targets = { lib = { output = "static", mode = "freestanding", relocation = "pic" } },
    build = { reproducible = true, archiver = "llvm-ar" },
    target_config = [{ triple = "aarch64-pc-windows-msvc", lib = { relocation = "default" }, archiver = "llvm-lib" }]
}
```

A library output is `object` (default), `shared`, or format-5 `static`. Mode is
`hosted` or `freestanding`; relocation is `default`, `pic`, or `static`. Exact
triple library overrides take precedence over the library declaration. Profiles
continue to control optimization only. Entry/startup/linker scripts belong to
binaries. Explicit relocation is supported for ELF/Mach-O; COFF/Wasm reject
unverified explicit models. Freestanding native libraries support ELF, Mach-O
and COFF; freestanding binaries retain the ELF restriction. Wasm archives are
not supported. Shared libraries require PIC where an explicit model is selected.

Static output is `lib<package>.a` for ELF/Mach-O and `<package>.lib` for MSVC,
under `target/<triple>/<profile>/lib/` (workspace package subdirectories apply).
Vex queries the selected archiver, requests a deterministic target format and
symbol index, and verifies all members, object machines, timestamps and hashes.
It archives the objects from this root Wave compilation, which includes imported
Wave code once. Foreign objects/archives and system libraries are **not bundled**;
`metadata.native_requirements` reports selected transitive declarations in link
order, resolved paths when known and owning package IDs. Consumers own those
additional links. Repeated native libraries retain their order and repetition.

`build.archiver` and a triple-specific `archiver` select one executable name/path;
Vex does not split shell words or accept raw argument strings. Selection otherwise
checks compiler-adjacent LLVM tools, PATH `llvm-ar`/`llvm-lib`, and GNU `ar` for
ELF. Install suitable tools explicitly; Vex does not download them. Unknown
archiver/runtime closure disables artifact reuse rather than guessing freshness.

## Toolchain identity and reproducibility

`VEX_WAVEC` → PATH → managed installation remains the compiler precedence.
`VEX_WAVE_STD` selects an explicit prepared standard-library directory. Paths
resolve before entering the project. Exact manifest `compiler` version checks,
content pins, capabilities and package-language compatibility remain independent.

```sh
vex toolchain capture --wavec /prepared/wavec --std /prepared/std
vex toolchain capture --replace
```

Explicit capture writes `vex.toolchain.lock` schema 1 with host-specific compiler
and std SHA-256 identities, and the compiler version. Other hosts are preserved.
An existing host requires `--replace`. Source revision is null unless reliably
known; no revision is inferred from a path/version. A compiler reporting no std
records a null std identity, distinct from a prepared tree. Tree hashing rejects
links/special files and sorts relative UTF-8 names. All build/check/run/test
paths, including dry-run, reject a mismatched or missing host pin before dependency
mutation; they never install or fall back to another compiler. Doctor reports
expected and observed identities without repair/network access.

`build.reproducible = true` requests strict directory-independent object/static
bytes and portable provenance. It requires Wave's versioned package graph and
logical path remapping. Package IDs and `/vex/...` logical roots replace physical
paths in compiler symbol identities. Diagnostics and depfiles retain real local
paths. Reproducibility currently rejects executable/shared links, SDK/sysroot and
other unverified external link plans. Explicit remapping prefixes containing `=`
are not representable by the current compiler protocol and are rejected. This is
a bounded guarantee for supported compiler/object/archive formats, not a claim
that arbitrary external SDKs or native linkers are reproducible.

## Result publication and depfiles

```sh
vex build --locked --offline --artifact-manifest /build/artifacts.json --depfile /build/inputs.d
```

Paths are invocation-relative. Existing files must be unchanged reports owned by
this build root. Unowned/edited files are errors, even in dry-run. Check emits no
artifacts and rejects these options. Run/test publish before executing programs;
program stdio/exit codes and exclusive JSONL `--message-file` behavior stay intact.

Compilation targets private leased generations. Required outputs and optional
companions are verified; captured inputs and observed compiler/std identities are
checked again before publication. Compatibility paths are mirrors. The **atomic
result-pointer replacement** is the per-target commit point; several mirror
renames are not one atomic filesystem operation. Cooperating Vex readers hold the
build lock. External readers use the immutable paths in a completed result and
hold its generation lease while copying/consuming. Before commit recovery restores
old mirrors; after commit it retains the complete new generation. User edits or
invalid journals stop recovery without discarding evidence. Earlier successful
targets remain when a later target fails. No automatic generation GC is added.

The requested [`vex.artifacts` schema 1](schemas/artifacts-v1.json) aggregates
selected package-target results in deterministic order. Each contains roles,
immutable and compatibility paths, SHA-256, target/profile/features/effective
settings, compiler/std identities, source/config fingerprints and a distinct
path-free `provenance` object. Intermediate, debug and import-library companions
are identified separately. Compiler/archiver/input-verification failure leaves
the previous complete generation and requested reports intact. Report replacement
has its own journal/ownership commit; an interrupted report transaction is
recovered before the next normal build. Dry-run creates neither reports nor
artifacts and performs no recovery. Unchanged verified report bytes keep mtimes.

Depfiles use Make/Ninja escaping and list selected sources/imports, manifests,
lock/config files, compiler/std files, declared resolved native inputs and source
listing directories. Unknown SDK/system-name closures prevent unsafe cache reuse;
a depfile does not pin them. Always run the consumer freshness scan so newly
added/deleted imports, tool selections and environment changes cannot hide behind
an old scheduler file. `--locked` pins dependency resolution, not mutable path or
SDK contents. No execution/test results are cached.

Metadata keeps schema 1 as its default; format-5 declarations require explicit
[`--format-version 3`](schemas/metadata-v3.json), without querying a compiler.
Dry-run always emits [`vex.build-plan` schema 2](schemas/build-plan-v2.json),
including source/build roots, selected reports and observed toolchain identity.

## Meson consumer and acceptance

The [official example](../examples/meson-consumer/meson.build) links a multi-file
Wave library into C, C++ and Rust programs. Prepare its Wave store first, then:

```sh
meson setup /build/consumer examples/meson-consumer -Dvex=/absolute/path/to/vex
meson compile -C /build/consumer
meson test -C /build/consumer --no-rebuild
```

Select a prepared compatible compiler/std through the environment. The
[adapter](../tools/meson_consumer.py) always invokes Vex locked/offline, reads
verified machine results, leases a generation while copying, and atomically
updates Meson-owned bridge bytes only when changed. Meson compiles/links foreign
sources. For Meson 1.8 Rust, a generated entry includes the original caller and
verified archive digest so both scheduling and re-linking are compiler-input
dependencies. Build-machine Vex/wavec can produce another output target; Meson's
host configuration must match `-Dwave_target`. Running cross programs requires a
matching Meson execution wrapper and runtime.

`tests/external_builds.py --vex ... --wavec ... --std ... --cross-formats` exercises
real Meson restat, imports, readonly source, separate roots, profile/features,
failure preservation, portable provenance and indexed O0/O2 cross-format archives.
The checked fixture digests identify a small Whale-shaped C ABI package; it is
not Whale compiler source. Native release acceptance separately links actual
Wave archives into C/C++/Rust on all nine existing hosts.

New scoped-import/result/reproducibility acceptance requires a public Wave
artifact advertising `vex-capabilities` schema 1 with `package_graph`, `path_remap`
and `source_inputs` at version 1. [Wave #1071](https://github.com/wavefnd/Wave/pull/1071) supplies the
#1066/#1067 compiler contract in a separate PR. Until a compatible public release is pinned and actually executed,
local development-compiler results are supplemental and do not establish new
nine-host release readiness. Existing pinned public Wave 0.2.1 gates remain.
