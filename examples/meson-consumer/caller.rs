// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
unsafe extern "C" { fn wave_answer() -> i32; }
fn main() { assert_eq!(unsafe { wave_answer() }, 42); }
