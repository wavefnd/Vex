#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Check source licensing and intentional worktree line endings without mutation."""
import argparse
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
NOTICE = 'This Source Code Form is subject to the terms of the Mozilla Public\nLicense, v. 2.0. If a copy of the MPL was not distributed with this\nfile, You can obtain one at https://mozilla.org/MPL/2.0/.'
SPDX = 'SPDX-License-Identifier: MPL-2.0'
SOURCE_SUFFIXES = {'.rs', '.py', '.sh', '.ps1', '.cmd', '.bat', '.toml', '.yml', '.yaml'}
BINARY_SUFFIXES = {'.png', '.jpg', '.jpeg', '.gif', '.webp', '.ico', '.pdf', '.zip', '.gz', '.xz', '.bz2', '.tar', '.7z', '.exe', '.dll', '.so', '.a', '.o', '.lib', '.wasm', '.woff', '.woff2', '.bin'}


def paths(root=ROOT):
    raw = subprocess.check_output(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], cwd=root)
    return sorted({Path(name.decode('utf-8')) for name in raw.split(b'\0') if name})


def needs_header(path):
    # WSON/lock/Wave fixtures test exact syntax and diagnostics. JSON cannot
    # carry comments. Generated CLI tools/docs and third-party notices retain
    # their own provenance; no project copyright is asserted over them.
    return path.suffix in SOURCE_SUFFIXES and not str(path).startswith(('vendor/', 'third_party/'))


def check_file(path, data):
    if path.suffix in BINARY_SUFFIXES or (b'\0' in data and not needs_header(path)):
        return
    text = data.decode('utf-8')
    if path.suffix in {'.cmd', '.bat'}:
        if b'\n' in data.replace(b'\r\n', b'') or b'\r' in data.replace(b'\r\n', b''):
            raise ValueError('Windows cmd/bat scripts require CRLF')
    elif b'\r' in data:
        raise ValueError('text must use LF, not CRLF or mixed endings')
    if needs_header(path):
        marker = '//' if path.suffix == '.rs' else '@rem' if path.suffix in {'.cmd', '.bat'} else '#'
        comments = []
        for line in text.splitlines():
            if line.startswith('#!') or not line.strip():
                continue
            if not line.startswith(marker):
                break
            comments.append(line[len(marker):].strip())
        if SPDX not in comments or not all(line in comments for line in NOTICE.splitlines()):
            raise ValueError('missing MPL 2.0 notice and SPDX header')


def validate(root=ROOT):
    errors = []
    for path in paths(root):
        absolute = root / path
        if absolute.is_symlink():
            continue  # Git records a link target, not ownership of its contents.
        try:
            check_file(path, absolute.read_bytes())
        except (OSError, UnicodeError, ValueError) as error:
            errors.append(f'{path}: {error}')
    if errors:
        raise ValueError('\n'.join(errors))


if __name__ == '__main__':
    argparse.ArgumentParser(description=__doc__).parse_args()
    validate()
    print('Verified source headers and repository line endings.')
