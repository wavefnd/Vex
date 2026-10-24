# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
FROM debian@sha256:4876fdea29cac1a33a3195f1eb1952feb1fca7dd6dd00be3e2f1879cf20d34d2
ARG GNU_TARGET
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    gcc-${GNU_TARGET} g++-${GNU_TARGET} binutils-${GNU_TARGET} \
    && rm -rf /var/lib/apt/lists/*
