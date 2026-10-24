#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Generate or verify the command reference against the actual CLI help."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
COMMANDS = [''] + [command['name'] for command in json.loads((ROOT/'tools/command-spec.json').read_text(encoding='utf-8'))['commands']]


def render(binary):
    parts = ['# Command reference\n\nGenerated from Vex command help. Regenerate with `python3 tools/cli_reference.py --write --vex target/debug/vex`.\n\nSee [manifest and operational contracts](reference.md) for field types, environment, streams and exit codes.\n']
    for command in COMMANDS:
        result = subprocess.run([str(binary), *([command] if command else []), '--help'], cwd=ROOT, check=True, capture_output=True, text=True)
        if result.stderr:
            raise ValueError(f'help wrote stderr: {command}')
        parts.append(f'\n## {command or "Global options"}\n\n```text\n{result.stdout.rstrip()}\n```\n')
    return ''.join(parts)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vex', type=Path, required=True)
    parser.add_argument('--write', action='store_true')
    args = parser.parse_args()
    text = render(args.vex.resolve())
    for shell in ['bash', 'zsh', 'fish', 'powershell']:
        artifact = ROOT/'docs/completions'/f'vex.{shell}'
        generated = subprocess.check_output([str(args.vex.resolve()), 'completions', shell], text=True)
        if args.write:
            artifact.parent.mkdir(parents=True, exist_ok=True)
            artifact.write_text(generated, encoding='utf-8')
        elif artifact.read_text(encoding='utf-8') != generated:
            raise SystemExit(f'completion drift: {artifact}')
    manual = subprocess.check_output([str(args.vex.resolve()), 'manpage'], text=True)
    if args.write:
        (ROOT/'docs/vex.1').write_text(manual, encoding='utf-8')
    elif (ROOT/'docs/vex.1').read_text(encoding='utf-8') != manual:
        raise SystemExit('manual drift; regenerate CLI references')
    path = ROOT/'docs/commands.md'
    if args.write:
        path.write_text(text, encoding='utf-8')
    elif path.read_text(encoding='utf-8') != text:
        raise SystemExit('command reference differs from CLI help; regenerate docs/commands.md')
