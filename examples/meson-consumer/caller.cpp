// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
#include <cstdint>
extern "C" std::int32_t wave_answer();
int main() { return wave_answer() == 42 ? 0 : 1; }
