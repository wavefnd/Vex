# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""The single official release identity policy (no build metadata)."""
import re
import sys

VERSION_PATTERN = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?")


def valid_version(value: str) -> bool:
    match = VERSION_PATTERN.fullmatch(value)
    return bool(match) and all(not (part.isdigit() and len(part) > 1 and part.startswith("0")) for part in (match.group(1) or "").split("."))


if __name__ == "__main__":
    if len(sys.argv) != 2 or not valid_version(sys.argv[1]):
        sys.exit("error: expected a normal or prerelease version without build metadata")
