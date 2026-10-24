# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
# RUNTIME_IMAGE comes only from the immutable platform table.
ARG RUNTIME_IMAGE=debian@sha256:e3b4cb1b0056896c4b43e17472b906b10007ab94bc533d37a47e9a5795eebbc0
FROM ${RUNTIME_IMAGE}
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    ca-certificates curl git python3 build-essential pkg-config gh procps \
    && rm -rf /var/lib/apt/lists/*
