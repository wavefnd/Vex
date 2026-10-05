#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Enforce reviewable workflow pins, platform coverage and release evidence."""
import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def validate(root=ROOT):
    rows = json.loads((root / 'platforms.json').read_text())['platforms']
    if len(rows) != 9 or len({p['id'] for p in rows}) != 9 or len({p['rust_target'] for p in rows}) != 9:
        raise ValueError('the supported platform table must contain exactly nine unique hosts')
    for workflow in (root / '.github/workflows').glob('*.yml'):
        text = workflow.read_text()
        for value in re.findall(r'^\s*(?:-\s*)?uses:\s*([^\s#]+)', text, re.M):
            if not value.startswith('./') and not re.fullmatch(r'[\w.-]+/[\w./-]+@[0-9a-f]{40}', value):
                raise ValueError(f'{workflow.name}: mutable action reference {value}')
    for name in ('ci.yml', 'release.yml'):
        text = (root / '.github/workflows' / name).read_text()
        for row in rows:
            blocks = re.findall(r'^\s*- id: '+re.escape(row['id'])+r'\n((?:[ ]{12}[^\n]*\n)+)', text, re.M)
            expected = dict(os=row['runner'], target=row['rust_target'], executor=row['executor'], extension=row['archive'])
            expected.update({key: row[key] for key in ('image', 'oci_platform') if key in row})
            if len(blocks) != 1 or any(f"            {key}: {value}\n" not in blocks[0] for key, value in expected.items()):
                raise ValueError(f'{name}: missing/duplicate platform {row["id"]}')
        if 'continue-on-error: true' in text:
            raise ValueError(f'{name}: platform failures may not be ignored')
    return rows


def acceptance(directory, commit, rows, pin=None):
    if pin is None:
        pin = json.loads((ROOT / "tools/wave-release.json").read_text())
    if not re.fullmatch('[0-9a-f]{40}', commit or ''):
        raise ValueError('acceptance requires an exact source commit')
    for row in rows:
        report = json.loads((directory / f"acceptance-{row['id']}.json").read_text())
        if (report.get('schema_version') != 1 or report.get('executor') != row['executor']
                or not report.get('os')
                or report.get('wave_archive_digest') != 'sha256:'+pin['archives'][row['id']]
                or report.get('status') != 'passed' or report.get('source') != commit
                or report.get('platform') != row['id'] or report.get('rust_target') != row['rust_target']
                or report.get('wave_version') != pin['version']
                or not re.fullmatch('[0-9a-f]{64}', report.get('compiler_sha256', ''))):
            raise ValueError(f'invalid acceptance evidence for {row["id"]}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--acceptance', type=Path)
    parser.add_argument('--commit')
    args = parser.parse_args()
    rows = validate()
    if args.acceptance:
        acceptance(args.acceptance, args.commit, rows)
    print('Verified immutable action pins and all nine platform contracts.')
