# Conditional dependencies and additive features

Format 4 adds dependency activation without raw compiler flags or scripts:

```wson
{
    format = 4,
    name = "app",
    targets = { bins = [{ name = "app", path = "src/main.wave" }] },
    dependencies = [
        { name = "platform", package = "platform", path = "../unix_impl", target = { os = "linux" } },
        { name = "platform", package = "platform", path = "../windows_impl", target = { os = "windows" } },
        { name = "extras", path = "../extras", optional = true, default_features = false }
    ],
    features = { default = ["extended"], extended = ["dep:extras", "extras/fast"] }
}
```

A `target` object combines its supplied constraints with AND: exact `triple`, `os`, `arch`, and `mode`. OS values are `linux`, `macos`, `windows`, `freebsd`, `wasi`, `none`, `unknown`; architecture values are `x86_64`, `aarch64`, `riscv64`, `loongarch64`, `wasm32`, `wasm64`. Modes are `hosted`, `freestanding`, `wasi`, `bare_wasm`. Empty predicates, unknown fields/values, and OS/architecture constraints contradicting an exact triple are errors. One alias can have several distinct target predicates. Simultaneously active mappings to different concrete packages fail explicitly under Wave's global import map.

Features are additive. Names use `[A-Za-z_][A-Za-z0-9_-]*`. A group can contain another local group, `dep:alias`, or `alias/feature`; an optional dependency also has an implicit feature with its alias unless a declared group has that name. `alias/feature` enables the dependency and requests that child's feature. Local group cycles are finite unions; package dependency cycles remain errors. Unknown feature references fail rather than being ignored.

`default` is an ordinary group enabled initially. `--no-default-features` disables only the selected root's defaults. `--features group,other` adds root requests. Each dependency enables its own defaults unless that edge sets `default_features = false`. An edge's `features` list requests child feature groups. All active incoming requests for the same concrete package ID are united, including requests through different aliases. Defaults requested by any incoming edge remain enabled.

The version 4 lock stores the union of all declared target and optional sources, exact commits, root declarations, activation edges and feature groups. Fetch prepares the complete union, including currently inactive packages. It does not scan a registry. After preparation, target/feature selection changes need neither network nor a lockfile rewrite. Missing union sources still require `vex fetch`; locked/offline modes never invent source choices. `prune` preserves every union source, including inactive packages.

Build/check/run accept feature selection and use their selected compiler triple plus each binary's manifest-owned mode. Fetch/update/tree/metadata accept `--target <triple>` without querying the compiler; their default is `build.target` or the Vex host triple with its usual mode. A target query describes graph activation and does not certify that a selected compiler supports that target.

Tree shows the active graph. Metadata schema 2 adds the effective `selection`, `selected_features`, and `union_bindings`; each selected package has a `binding` key used by dependency edges, its import `alias`, and concrete package `id`. Inverse queries accept an alias or binding key and report ambiguity when several bindings match. Metadata remains read-only and requires the local union graph to be prepared.

These features select dependency graphs. They do not add Wave language conditional-compilation syntax or synthesize compiler flags. Source imports must be valid for the chosen graph.
