# Vex development and release acceptance

The public release is `v0.0.2-beta`. The current integration targets
`v0.0.3-beta`; implementation on a branch is not evidence of a published release.

The existing base includes user workspaces, content-verified build reuse, Git
object sharing/subdirectories, conditional dependency/feature union locks,
SemVer/aliases, new/add/remove transactions, main-based executable tests,
cleanup/prune, doctor, tree queries and Meson repository automation. These are
maintained through their regression suites.

The v0.0.3 integration covers external build/dependency roots (#177), complete
depfiles (#178), reusable completed results (#179), library mode/relocation
(#180), compiler/std content pins (#181), bounded reproducibility (#182),
last-good artifact generations (#183), a real Meson C/C++/Rust consumer (#184)
and deterministic static archives (#185). It also addresses #162–174, conditional
alias removal #186, issue forms #76 and subsystem ownership #83. See
[the implementation contracts and validation limits](external-builds.md).

Full package-scoped multiple-version imports (#58) additionally require the
separate Wave #1066 contract; logical remapping requires Wave #1067. Existing
public Wave 0.2.1 acceptance stays required on all nine hosts. New capability
acceptance must use a verified compatible public Wave release before it becomes
a mandatory release gate. Local development-compiler or cross-format object
inspection does not establish native execution support for those new features.

The approved test workflow (#60) uses explicit main-based test targets, separate
test dependencies, deterministic filters, JSONL results and unchanged program
exit codes. Compiler-native unit-test syntax/harness is not claimed. No registry,
publish, foreign-source build-script system or automatic garbage collector is
introduced by this integration.

Issue completion follows implemented acceptance and test evidence. A release
requires all nine host reports for its exact commit, including native ABI
consumers and RISC-V/LoongArch cross-build plus QEMU execution. Historical CI
results do not validate a later source commit.
