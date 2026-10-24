# Package versions and import identities

Manifest format 4 uses canonical SemVer package versions (`1.2.3`, `0.2.0-beta.1`). Compiler releases are a separate exact identity: `compiler = "0.2.1-pre-beta"` remains an equality check, not a SemVer compatibility promise. Existing manifest formats retain their original version strings and exact dependency comparisons. No manifest is automatically converted.

A dependency's `version` remains exact string equality. Format 4 adds `version_req`, mutually exclusive with `version`, using SemVer requirements: `^1.2.3`, `~1.2`, `=1.2.3`, `>=1.2, <2`, `1.*`. An unprefixed requirement has caret semantics. Prereleases must be explicitly admitted for the same release tuple; `*` does not include prereleases. Build metadata does not affect requirement matching. Vex release versions still prohibit build metadata under the separate release policy.

Requirements validate the concrete package in the selected Git/path source. They do not search tags, solve registry versions, or change the exact commit selected by a lockfile. Compatible requirements on a shared source can coexist. An incompatible request fails with its dependency path and leaves prior published state intact.

```wson
{
    format = 4,
    name = "application",
    version = "1.0.0",
    targets = { bins = [{ name = "application", path = "src/main.wave" }] },
    dependencies = [
        { name = "old_math", package = "math", path = "../math_v1", version_req = "^1.2" },
        { name = "new_math", package = "math", path = "../math_v2", version_req = "^2" }
    ]
}
```

`name` is the Wave import alias; `package` is the source manifest's declared name and defaults to `name`. Vex validates that declaration rather than renaming source files. Version 4 lock records distinguish import bindings from concrete package IDs. A package ID hashes the declared name, exact version, concrete source (Git URL/commit or project-relative path), and subdirectory. Selectors and aliases do not change concrete identity. Tree output shows `alias => package`; metadata schema 2 (`--format-version 2`) adds `alias` and `id` while retaining the declared `name`.

Current Wave accepts one global `--dep=<alias>=<path>` map. Distinct aliases can import two versions; conflicting transitive uses of one alias for different sources cannot be represented and fail explicitly. Full scoped dependency mapping remains an outstanding compiler contract under #58. Vex never rewrites user source to emulate it.

Version 1/2/3 lockfiles remain readable. Commands using old manifest models preserve their existing `--locked` files. Format 4 requires version 4 lock state; `--locked` reports a required migration and preserves the old bytes. Run `vex fetch` explicitly before committing a migrated lockfile. IDs in a lockfile are checked against their concrete source before graph use.
