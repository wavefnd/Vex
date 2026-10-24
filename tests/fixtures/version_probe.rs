// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

            fn main() {
                if std::env::current_exe().unwrap().file_stem().unwrap() == "broken" {
                    eprintln!("missing compiler runtime");
                    std::process::exit(7);
                }
                if std::env::var_os("NO_COLOR").is_some() {
                    println!("wavec 0.2.1-pre-beta (platform)\n  backend: LLVM 21");
                } else {
                    println!("\x1b[32mwavec\x1b[0m \x1b[32m0.2.1-pre-beta\x1b[0m");
                }
            }
