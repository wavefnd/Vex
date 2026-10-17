# Native C ABI interoperability

Vex links prebuilt native code into Wave packages and emits Wave objects or shared
libraries for native callers. The ABI is the C ABI implemented by the selected
Wave compiler. C++ and Rust libraries must expose C ABI symbols (`extern "C"`),
not their language-specific ABI. Vex never builds C/C++/Rust source trees or runs
foreign build scripts. Prepare native inputs with the appropriate toolchain first.

The supported compiler baseline is the public Wave `0.2.1-pre-beta` release.
Object and shared output are supported; static-library creation is not part of
this interface. An object can be archived by an external build system. Wave
`pub` controls module visibility; `export(c)` controls exported C symbols.

## Wave calls native libraries

```wson
{
    format = 2,
    name = "app",
    version = "0.1.0",
    native = {
        libraries = [
            { path = "native/libarithmetic.a", target = "x86_64-unknown-linux-gnu" },
            { name = "support" }
        ],
        search_paths = [{ path = "native" }]
    }
}
```

```wave
extern(c, "native_add") fun native_add(a: i32, b: i32) -> i32;
fun main() {
    var result: i32 = native_add(20, 22);
    println("{}", result);
}
```

A library entry has exactly one `path` or `name`. A path names a prebuilt `.o`,
`.obj`, `.a`, or `.lib`; a shared library uses `name` plus `search_paths` (on
Windows the linker consumes its import `.lib`, not a `.dll`). `name` uses the
selected target's linker convention. Do not include flags in names or paths.

With public Wave `0.2.1-pre-beta`, declarations inside imported packages must
use the explicit linker-symbol form `extern(c, "native_add")`. The shorter
`extern(c)` form works at the root but can incorrectly receive a module prefix
inside imported packages. The explicit symbol form is used in the transitive
interoperability acceptance fixture.

Paths are UTF-8, relative to the **declaring package**, and use `/` on every OS.
Absolute paths, parent traversal, empty/dot components, Windows-reserved path
components and symlink escapes outside that package are rejected. Selected files
and search directories must already exist, including during `check` and dry-run.
Native declarations in transitive Git/path packages are included in the final
link. Consuming packages precede their dependencies, and each package's library
declaration order is preserved, including names repeated across packages. A name is resolved
first within its declaring package's selected search paths (shared before static
on Unix); if no local file matches, the compiler's default system library search
is used. One package's search paths do not select another package's library.
No package graph or lockfile format change is introduced.

An optional `target` is an exact compiler triple. Only matching entries are used;
there is no predicate language or target-dependent Wave dependency graph here.
For example, use separate `.a` and MSVC `.lib` entries for different targets.
`--target` still selects the compiler target and must be supported by that
compiler. Vex does not install native SDKs or cross toolchains automatically.

Shared inputs need a runtime loader search configuration supplied by the caller
or deployment environment. Vex does not modify `PATH`, `LD_LIBRARY_PATH`, or
`DYLD_LIBRARY_PATH`, copy DLLs, or insert library search directories into runtime
loader paths. Compile-time search paths alone do not ensure runtime availability.

## Native callers use Wave

```wson
{
    format = 2,
    name = "arithmetic",
    version = "0.1.0",
    lib = true,
    native = { output = "object" }
}
```

```wave
export(c) fun wave_add(a: i32, b: i32) -> i32 { return a + b; }
```

Use `output = "shared"` for a shared library. Both values require `lib = true`.
A library without an explicit output setting now builds an object instead of
attempting to link an executable without `main`. A binary package can declare
native inputs but cannot set a library output kind. `vex run` still rejects
library packages. `vex check` performs Wave checking without linking native
inputs or producing native artifacts; it still validates their selected paths.

Windows DLLs also require an explicit list of exported C symbols:

```wson
native = { output = "shared", exports = ["wave_add"] }
```

Public Wave `0.2.1-pre-beta` emits C ABI symbols for `export(c)` but does not
automatically add them to the DLL export table. Vex passes the root package's
`exports` to the Windows linker, which rejects undefined symbols. Entries must
be unique ASCII C identifiers; aliases, ordinals, and linker options are not
accepted. The source still needs `export(c)` (or its explicit symbol form).
`exports` is allowed only with `output = "shared"` and must be nonempty for
Windows shared builds. On ELF/Mach-O it is retained in metadata but does not
restrict symbol visibility. Dependency export lists are not merged into the
root DLL; list every intended public C symbol in the root package.

On Linux, a C caller can declare `int32_t wave_add(int32_t, int32_t);` and link the
produced object with its C compiler. C++ callers use `extern "C"`; Rust callers
use an `extern "C"` declaration and appropriate native linking. Keep the C
header/declarations synchronized with Wave yourself: Vex does not generate
bindings or headers. Integer widths, layout, calling convention, ownership,
allocation/free pairing and pointer lifetimes remain the caller/library contract.
Exceptions or Rust panics must not unwind through an incompatible ABI boundary.

## Artifacts and reproducibility

Library packages and binary packages using native declarations (including
transitive declarations) use:

- `target/<compiler-triple>/dev/` or `target/<compiler-triple>/release/`
- `<package>.o` (`.obj` on Windows) for objects
- `lib<package>.so`, `lib<package>.dylib`, or `<package>.dll` for shared libraries
- `<package>` (`.exe` on Windows) for native binaries
- `.vex-obj/<package>/` below that directory for intermediate objects

Run outputs remain in a per-run generation; its lease protects them while the
program executes. Binary projects with no native declarations retain their
existing layout. This is the native slice of the broader targets/layout design;
multiple named package targets and custom profiles are not introduced.

ELF shared libraries receive a stable SONAME; Mach-O shared libraries receive an
`@rpath` install name. Windows import libraries and other compiler-generated
companions are preserved and tracked when produced. `--message-file` artifact
events report paths only after successful output verification. Metadata schema 1
adds a nullable `native` declaration object for each package without querying a
compiler. Consumers must continue to tolerate additive fields.

Builds refuse overwriting unowned or edited artifact files, including implicit
Windows companions. Move such files aside explicitly before rebuilding. `clean`
only removes unchanged owned artifacts and skips leased generations. If compiler
execution fails, its error remains primary even if ownership bookkeeping fails.

`--locked` preserves the selected Wave graph; `--offline` prevents dependency
network access. Neither option locks an external SDK, a system library selected
by name, or mutable path-package contents. Version and distribute prebuilt native
inputs with the package, select the correct target entry and verify the resulting
binary with the intended runtime environment. Native inputs committed in Git
packages follow that package's exact locked commit.

The nine-platform acceptance pipeline runs `tests/native_interop.py` using the
pinned public compiler. It builds C/C++/Rust fixtures outside Vex, links Wave in
both directions, and checks object/shared artifacts, transitive native inputs,
locked/offline behavior and conservative cleanup. A local cross-plan check is
not a substitute for executing those host acceptance jobs.
