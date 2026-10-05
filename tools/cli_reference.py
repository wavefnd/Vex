#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Generate or verify the command reference against the actual CLI help."""
import argparse
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
COMMANDS = ['', 'init', 'build', 'check', 'run', 'fetch', 'update', 'info', 'tree', 'metadata', 'setup']


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
    path = ROOT/'docs/commands.md'
    if args.write:
        path.write_text(text, encoding='utf-8')
    elif path.read_text(encoding='utf-8') != text:
        raise SystemExit('command reference differs from CLI help; regenerate docs/commands.md')
