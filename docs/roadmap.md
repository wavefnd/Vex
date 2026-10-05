# Vex 0.0.2-beta release acceptance

This release is one integrated production-readiness change: all nine supported
hosts, transactional dependency recovery, explicit CLI/compiler contracts,
verified installation and provenance, and end-to-end public Wave 0.2.1 testing.
See [platforms and acceptance](../README.md#platform-validation) and
[the release procedure](../RELEASING.md).

Completed earlier work remains complete: project discovery, local metadata,
lockfile v3, source credential handling, project leases, isolated run generations,
and atomic initialization are extended rather than reimplemented.

Release evidence must cover every platform and exact source SHA. Build-only or
version-only cross smoke cannot satisfy the platform gate. Missing public Wave
artifacts and unsuccessful platform jobs block publication. Issue closure requires
checking the full acceptance criteria against implementation and real test results.

User workspaces (#30), shared caches (#29), multiple package targets and profiles
(#46/#47/#48), package version-range policy (#132/#51), optional dependencies,
registry and publish remain separate product designs. No automatic deletion of
run generations, previous installations, or dependency backups is introduced.
