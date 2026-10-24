// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
pub fn update(
    args: &[String],
    selection: &crate::project::Selection,
) -> Result<(), diagnostic::Error> {
    super::fetch::dependency_command(true, args, selection)
}
