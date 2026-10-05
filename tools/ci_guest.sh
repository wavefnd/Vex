#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
set -eu
export CARGO_BUILD_JOBS=2
export RUST_TEST_THREADS=2
case "$(uname -s)" in
  Linux)
    vex_guest_python=python3
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl git python3 build-essential pkg-config gh procps
    ;;
  FreeBSD)
    vex_guest_python=python3.11
    pkg install -y ca_root_nss curl git python311 gmake pkgconf gh
    ;;
  *) echo 'unsupported guest OS' >&2; exit 1;;
esac
curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/vex-rustup.sh
sh /tmp/vex-rustup.sh -y --profile minimal --default-toolchain 1.96.0
export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"
# The whole process tree (Git, fake compiler, real compiler, user programs)
# executes in this target environment through binfmt or the FreeBSD kernel.
git config --global --add safe.directory "$(pwd)"
"$vex_guest_python" tools/ci_platform.py "$1"
