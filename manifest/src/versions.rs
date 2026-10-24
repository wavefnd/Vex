// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Package compatibility is SemVer; compiler releases retain exact identity.
#[cfg(test)]
mod tests {
    use crate::decode;
    use std::path::PathBuf;
    fn dependency(requirement: &str) -> crate::Dependency {
        decode(&format!("{{format=4,name=\"app\",targets={{lib={{}}}},dependencies=[{{name=\"renamed\",package=\"actual\",path=\"../actual\",version_req=\"{requirement}\"}}]}}"), PathBuf::from("vex.ws")).unwrap().dependencies.remove(0)
    }
    #[test]
    fn requirement_boundaries_and_prereleases_follow_semver() {
        for (req, version, matches) in [
            ("^1.2.3", "1.9.9", true),
            ("^1.2.3", "2.0.0", false),
            ("^0.2.3", "0.2.9", true),
            ("^0.2.3", "0.3.0", false),
            ("^0.0.3", "0.0.4", false),
            ("~1.2.3", "1.3.0", false),
            (">=1.2.0, <2.0.0", "1.5.0", true),
            ("=1.2.3", "1.2.4", false),
            ("1.*", "1.7.2", true),
            ("*", "1.0.0-alpha", false),
            ("^1.0.0-alpha", "1.0.0-beta", true),
            ("^1.0.0-alpha", "1.1.0-beta", false),
            ("=1.0.0", "1.0.0+build.1", true),
        ] {
            let dep = dependency(req);
            assert_eq!(dep.package_name(), "actual");
            assert_eq!(
                dep.validate_version(version).is_ok(),
                matches,
                "{req} / {version}"
            );
        }
    }
    #[test]
    fn format_boundary_preserves_legacy_version_meaning() {
        assert!(decode("{name=\"old\",version=\"not semver\"}", "vex.ws".into()).is_ok());
        assert!(decode(
            "{format=4,name=\"new\",version=\"not semver\",targets={lib={}}}",
            "vex.ws".into()
        )
        .is_err());
        for fragment in [
            "version_req=\"bad range\"",
            "version=\"1.0.0\",version_req=\"^1\"",
            "package=\"bad-name\"",
        ] {
            assert!(decode(&format!("{{format=4,name=\"app\",targets={{lib={{}}}},dependencies=[{{name=\"dep\",path=\"../dep\",{fragment}}}]}}"), "vex.ws".into()).is_err());
        }
        for version in ["01.0.0", "1.0", "v1.0.0", "1.0.0-01"] {
            assert!(decode(
                &format!("{{format=4,name=\"app\",version=\"{version}\",targets={{lib={{}}}}}}"),
                "vex.ws".into()
            )
            .is_err());
        }
    }
}
