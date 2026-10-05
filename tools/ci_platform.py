#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Execute the complete acceptance lane inside the target OS/architecture."""
import argparse
import hashlib
import json
import os
import platform
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request
import urllib.error

ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = json.loads((ROOT / 'platforms.json').read_text())['platforms']
WAVE_PIN = json.loads((ROOT / 'tools/wave-release.json').read_text())
WAVE_VERSION = WAVE_PIN['version']


def run(*args, env=None, capture=False):
    print('+', ' '.join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), cwd=ROOT, env=env, check=True,
                          text=True, stdout=subprocess.PIPE if capture else None)


def main():
    # Native CI may authenticate API queries. Never inherit that credential in
    # builds, tests, compilers or user programs, or forward it into a guest.
    api_token = os.environ.pop('GH_TOKEN', None)
    os.environ.pop('GITHUB_TOKEN', None)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=[p['id'] for p in PLATFORMS])
    args = parser.parse_args()
    target = next(p for p in PLATFORMS if p['id'] == args.platform)
    host = run('rustc', '-vV', capture=True).stdout
    if f"host: {target['rust_target']}\n" not in host:
        raise RuntimeError('acceptance must execute inside the target environment, not a cross-build host')
    run(sys.executable, '-m', 'unittest', 'discover', '-s', 'tests/xpy', '-v')
    run('cargo', 'test', '--workspace', '--locked')
    run(sys.executable, 'tools/cli_reference.py', '--vex', ROOT/'target/debug'/('vex.exe' if target['os'] == 'windows' else 'vex'))
    run(sys.executable, 'x.py', 'build', target['rust_target'])
    run(sys.executable, 'x.py', 'package', target['rust_target'])
    binary = ROOT / 'target' / target['rust_target'] / 'release' / ('vex.exe' if target['os'] == 'windows' else 'vex')
    # Authentication changes API rate limits, never public-release acceptance.
    url = f'https://api.github.com/repos/wavefnd/Wave/releases/tags/v{WAVE_VERSION}'
    headers = {'User-Agent': 'Vex-release-acceptance'}
    if api_token:
        headers['Authorization'] = 'Bearer ' + api_token
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            release = json.load(response)
    except urllib.error.HTTPError as error:
        detail = error.read(4096).decode('utf-8', errors='replace')
        raise RuntimeError(f'cannot query official Wave v{WAVE_VERSION} (HTTP {error.code}): {detail}') from error
    if release.get('draft') is not False or release.get('tag_name') != f'v{WAVE_VERSION}':
        raise RuntimeError('required official Wave release is not public')
    asset_name = f"wave-v{WAVE_VERSION}-{target['wave_target']}.{target['archive']}"
    assets = [a for a in release['assets'] if a['name'] == asset_name]
    if len(assets) != 1:
        raise RuntimeError('official compiler archive is missing or ambiguous')
    with tempfile.TemporaryDirectory(prefix='vex-compiler-acceptance-') as temporary:
        env = dict(os.environ, VEX_TOOLCHAIN_HOME=temporary, VEX_WAVEC_ARCHIVE_SHA256=WAVE_PIN['archives'][target['id']])
        env.pop('VEX_WAVEC', None)
        setup_env = dict(env, GH_TOKEN=api_token) if api_token else env
        run(binary, 'setup', 'wavec', '--version', WAVE_VERSION, env=setup_env)
        compiler = Path(temporary) / (Path(temporary) / 'current').read_text().strip()
        compiler_hash = hashlib.sha256(compiler.read_bytes()).hexdigest()
        run(sys.executable, 'tests/wave_compatibility.py', '--vex', binary,
            '--wavec-bin', compiler.parent, '--reexports', '--expected-version', WAVE_VERSION,
            '--expected-sha256', compiler_hash, env=env)
    report = dict(schema_version=1, platform=target['id'], rust_target=target['rust_target'],
                  executor=target['executor'], source=run('git', 'rev-parse', 'HEAD', capture=True).stdout.strip(),
                  os=platform.platform(), libc=platform.libc_ver(), wave_version=WAVE_VERSION,
                  wave_release_id=release['id'], wave_asset_id=assets[0]['id'],
                  wave_archive_digest='sha256:'+WAVE_PIN['archives'][target['id']], compiler_sha256=compiler_hash,
                  status='passed')
    (ROOT / 'dist' / f"acceptance-{target['id']}.json").write_text(json.dumps(report, indent=2)+'\n')


if __name__ == '__main__':
    main()
