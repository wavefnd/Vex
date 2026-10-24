# User workspaces

A workspace is a group of Wave packages with one dependency lock, coordination lease and output directory. This is separate from Vex's implementation as a Rust workspace.

```wson
{
    format = 4,
    compiler = "0.2.1-pre-beta",
    workspace = {
        members = ["library", "application"],
        exclude = ["examples/independent"],
        default_members = ["application"]
    },
    profiles = { fast = { inherits = "release", opt_level = 3 } }
}
```

Omitting `name` creates a virtual workspace root. To include a package at the root, declare its normal name/version/targets alongside `workspace`; that package is included automatically. Member/default/exclude entries are explicit portable directory paths relative to the root; `.` denotes the root package. Globs, traversal, escaped paths, duplicate physical directories and case-conflicting package names are rejected. Members can use older manifest formats without rewriting them.

Without `default_members`, root commands select all members. Inside a member, nearest real-path discovery selects that member. An explicit `--manifest-path` has priority and still respects workspace ownership. Use repeated `-p <name>` / `--package <name>` to select packages, or `--workspace` to select all. These two selection forms cannot be combined. A nested independent workspace must be excluded by its ancestor; a package inside a workspace must be listed or explicitly excluded. Ambiguous ownership is an error.

Build/check process selected targets across the selected members. `--lib` and `--bin` filter matching targets across them. Run must resolve to one binary; use `-p` and `--bin` to remove ambiguity. Vex validates every source/profile, dependency/compiler requirement and compiler plan before executing the first compiler. Compilation is deterministic and stops at the first failure, retaining already successful outputs.

Root profiles, default build target and sysroot/linker settings govern member builds. Members cannot silently override these global settings. A member may keep its own target-specific binary mode/entry/startup/script overrides. An exact compiler requirement at the workspace root applies to every member and conflicting member requirements fail. Foreign inputs remain package-owned; dependency tool settings never propagate into a root build.

All workspace artifacts live under `target/<triple>/<profile>/packages/<package>/bin/` or `lib/`. This separates equal target names in different packages. Single-package projects retain their existing layout. Run uses a fresh generation under the workspace's `target/.vex-run/`, releases the workspace build lease before program spawn and executes in the selected member's directory. No automatic generation GC is added.

The root owns `vex.lock` v4 and `.vex/`. Fetch prepares the union of every member's declared dependency graph, including currently inactive target/feature sources. Existing member lockfiles and state are neither imported, moved nor deleted; commit a freshly reviewed root lock when adopting a workspace. Locked/offline selection requires that union to be prepared. Prune protects all union sources and clean operates on workspace-owned outputs; both commands are workspace-wide rather than accepting package filters.

Updates made with package selection refresh only bindings reachable from those selected members. A shared binding changes for every consumer of that binding. Unrelated locked bindings, even in the same Git repository, retain their commits. Targeted aliases are checked from the current local union before transport; an incomplete local graph requires fetch.

Feature requests for the same concrete package ID are additive across selected members and their active contexts, including a member also consumed as a dependency. With multiple selected members, name requested features as `package/feature`; ambiguous unqualified names fail. Compiler import maps remain per root, so independent member builds can use one import alias for different sources. Conflicting transitive aliases within one compiler invocation still require the future scoped Wave mapping contract.

`vex metadata --format-version 2` describes the workspace root, all member declarations, selected members, per-root `selections`, union bindings and exact sources. `selection` is null when more than one root/context is represented; the `selections` array is authoritative. Tree prints selected root views separately. Doctor diagnoses selected members. Metadata performs no compiler query or network access and does not create or repair state.
