// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
pub fn check(
    args: &[String],
    runtime: &[std::ffi::OsString],
    selection: &crate::project::Selection,
    messages: &mut crate::messages::Messages,
) -> Result<crate::outcome::Outcome, diagnostic::Error> {
    super::build::build(
        super::build::BuildMode::Check,
        args,
        runtime,
        selection,
        messages,
    )
}
