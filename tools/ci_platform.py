#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
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
sys.path.insert(0, str(ROOT))
from tools import ci_cross
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
    parser.add_argument('--cross-config', type=Path)
    args = parser.parse_args()
    target = next(p for p in PLATFORMS if p['id'] == args.platform)
    cross = ci_cross.load(args.cross_config) if args.cross_config else None
    host = run('rustc', '-vV', capture=True).stdout
    if cross:
        ci_cross.validate_build(cross, target)
        if f"host: {cross['build_host']}\n" not in host or target['executor'] != 'qemu':
            raise RuntimeError('cross acceptance requires its recorded amd64 compiler host')
        identity = ci_cross.execution_identity(cross)
    else:
        if f"host: {target['rust_target']}\n" not in host:
            raise RuntimeError('acceptance must execute inside the target environment, not a cross-build host')
        identity = dict(os=platform.platform(), libc=platform.libc_ver(), machine=platform.machine())

    def execute(*command, env=None):
        if cross:
            return ci_cross.run_target(cross, command, env, cwd=ROOT)
        return run(*command, env=env)

    python = 'python3' if cross else sys.executable
    execute(python, '-m', 'unittest', 'discover', '-s', 'tests/xpy', '-v')
    if cross:
        ci_cross.verify_doctest_runner(cross)
    test_args = ['--target', target['rust_target']] if cross else []
    debug = ROOT/'target'/target['rust_target']/'debug/vex' if cross else ROOT/'target/debug'/('vex.exe' if target['os'] == 'windows' else 'vex')
    if cross:
        run('cargo', 'test', '--workspace', '--locked', *test_args, '--no-run')
        ci_cross.check_elf(debug, ci_cross.TARGETS[target['id']][1])
    run('cargo', 'test', '--workspace', '--locked', *test_args)
    execute(python, 'tools/cli_reference.py', '--vex', debug)
    run(sys.executable, 'x.py', 'build', target['rust_target'])
    binary = ROOT / 'target' / target['rust_target'] / 'release' / ('vex.exe' if target['os'] == 'windows' else 'vex')
    if cross:
        ci_cross.check_elf(binary, ci_cross.TARGETS[target['id']][1])
    # Packaging verifies execution in the target userspace without installing a
    # target Rust compiler. The report records the separate amd64 build host.
    package_env = dict(os.environ, VEX_RELEASE_HOST=target['rust_target']) if cross else None
    execute(python, 'x.py', 'package', target['rust_target'], env=package_env)
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
        setup_env = dict(env, GH_TOKEN=api_token) if api_token and not cross else env
        execute(binary, 'setup', 'wavec', '--version', WAVE_VERSION, env=setup_env)
        compiler = Path(temporary) / (Path(temporary) / 'current').read_text().strip()
        if cross:
            ci_cross.check_elf(compiler, ci_cross.TARGETS[target['id']][1])
        compiler_hash = hashlib.sha256(compiler.read_bytes()).hexdigest()
        execute(python, 'tests/wave_compatibility.py', '--vex', binary,
            '--wavec-bin', compiler.parent, '--reexports', '--expected-version', WAVE_VERSION,
            '--expected-sha256', compiler_hash, env=env)
        interop_env = ci_cross.interop_environment(cross, dict(env, PATH=str(compiler.parent)+os.pathsep+env.get('PATH', ''))) if cross else env
        run(sys.executable, 'tests/native_interop.py', '--vex', binary,
            '--wavec-bin', compiler.parent, '--expected-version', WAVE_VERSION,
            '--expected-sha256', compiler_hash, env=interop_env)
    report = dict(schema_version=1, platform=target['id'], rust_target=target['rust_target'],
                  executor=target['executor'], source=run('git', 'rev-parse', 'HEAD', capture=True).stdout.strip(),
                  os=identity['os'], libc=identity['libc'], wave_version=WAVE_VERSION,
                  wave_release_id=release['id'], wave_asset_id=assets[0]['id'],
                  wave_archive_digest='sha256:'+WAVE_PIN['archives'][target['id']], compiler_sha256=compiler_hash,
                  native_interop=['c', 'cpp-c-abi', 'rust-c-abi', 'wave-object', 'wave-shared'],
                  status='passed')
    if cross:
        report.update(build_method='cross', build_host=cross['build_host'], execution_arch=identity['machine'],
                      sdk_image=cross['sdk_image'], cross_compiler=cross['cross_compiler'],
                      runtime_image=cross['runtime_image'],
                      cross_packages=cross['cross_packages'], fixtures=cross['fixtures'])
    (ROOT / 'dist' / f"acceptance-{target['id']}.json").write_text(json.dumps(report, indent=2)+'\n')


if __name__ == '__main__':
    main()
