// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use std::path::Path;

mod support;
use support::git_url;

#[cfg(not(windows))]
#[test]
fn local_git_url_encodes_reserved_characters() {
    assert_eq!(
        git_url(Path::new("/tmp/vex fixture#1")),
        "file:///tmp/vex%20fixture%231"
    );
}

#[cfg(windows)]
#[test]
fn local_git_url_normalizes_verbatim_drive_paths() {
    assert_eq!(
        git_url(Path::new(r"\\?\C:\vex fixture#1")),
        "file:///C:/vex%20fixture%231"
    );
}

#[cfg(windows)]
#[test]
fn local_git_url_normalizes_verbatim_unc_paths() {
    assert_eq!(
        git_url(Path::new(r"\\?\UNC\server\share\vex fixture#1")),
        "file://server/share/vex%20fixture%231"
    );
}
