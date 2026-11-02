#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Real Wave contract acceptance; requires package_graph/path_remap/source_inputs v1.

This is supplemental until a compatible public compiler is pinned for release.
It never downloads or builds a compiler and never converts a missing capability
into a passed check. Meson owns all C/C++/Rust compilation and final linking.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
EXAMPLE = ROOT / 'examples/meson-consumer'


def run(command, env, *, expected=0):
    print('+', *map(str, command), flush=True)
    output = subprocess.run(list(map(str, command)), env=env, capture_output=True,
                            text=True, encoding='utf-8', timeout=300)
    if output.returncode != expected:
        raise RuntimeError(f'exit {output.returncode}, expected {expected}: {output.stdout}\n{output.stderr}')
    return output


def file_hash(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def result(build):
    return json.loads((build/'vex/consumer-result.json').read_text(encoding='utf-8'))['results'][0]


def primaries(record):
    return [Path(a['path']) for a in record['artifacts'] if a['role'] == 'primary']


def mtimes(build):
    return {str(p): p.stat().st_mtime_ns for p in build.iterdir()
            if p.is_file() and (p.name.startswith('caller_') or p.suffix in {'.a', '.lib'})}


def prepare(vex, source, build, env):
    run([vex, '--manifest-path', source/'wave/vex.ws', '--build-dir', build/'vex', 'fetch', '--offline'], env)


def configure(vex, source, build, env, profile='dev', features=''):
    run(['meson', 'setup', '--reconfigure', build, source, '-Dvex='+str(vex),
         '-Dadapter='+str(ROOT/'tools/meson_consumer.py'), '-Dwave_profile='+profile,
         '-Dwave_features='+features], env)


def build_and_run(build, env):
    run(['meson', 'compile', '-C', build], env)
    run(['meson', 'test', '-C', build, '--no-rebuild', '--print-errorlogs'], env)


def meson_contract(vex, temporary, env):
    first = temporary/'source é one'
    second = temporary/'source é two'
    shutil.copytree(EXAMPLE, first)
    manifest = first/'wave/vex.ws'
    manifest.write_text(manifest.read_text(encoding='utf-8').replace('features =', 'build = { reproducible = true }, features ='), encoding='utf-8')
    a = temporary/'build é one'
    b = temporary/'build é two'
    prepare(vex, first, a, env)
    run([vex, '--manifest-path', manifest, '--build-dir', a/'vex', 'toolchain', 'capture'], env)
    shutil.copytree(first, second)
    prepare(vex, second, b, env)
    for source, build in [(first, a), (second, b)]:
        configure(vex, source, build, env)
        build_and_run(build, env)
    ra, rb = result(a), result(b)
    if [p.read_bytes() for p in primaries(ra)] != [p.read_bytes() for p in primaries(rb)] or ra['provenance'] != rb['provenance']:
        raise RuntimeError('separate roots produced different artifact bytes or portable provenance')
    portable = json.dumps(ra['provenance'], ensure_ascii=False)
    if str(temporary) in portable:
        raise RuntimeError('portable provenance leaked a physical source/build path')
    before = mtimes(a)
    build_and_run(a, env)
    if before != mtimes(a):
        raise RuntimeError('unchanged verified inputs relinked native consumers: '+repr((before,mtimes(a))))
    # A renamed transitive import must be rediscovered without configuring Meson.
    (first/'math/src/value.wave').rename(first/'math/src/renamed.wave')
    (first/'math/src/lib.wave').write_text('pub import("./renamed.wave")::{answer};\n', encoding='utf-8')
    build_and_run(a, env)
    inputs = result(a)['metadata']['inputs']['files']
    if not any(p.endswith('renamed.wave') for p in inputs) or any(p.endswith('value.wave') for p in inputs):
        raise RuntimeError('scheduler inputs did not follow an import addition/removal')
    # Compilation failure must preserve reports, complete bridge and consumers.
    report = a/'vex/consumer-result.json'
    saved = report.read_bytes()
    bridge = a/('wave_bridge.lib' if os.name == 'nt' else 'libwave_bridge.a')
    saved_bridge = bridge.read_bytes()
    value = first/'math/src/renamed.wave'
    saved_source = value.read_bytes()
    value.write_text('this is invalid Wave source', encoding='utf-8')
    failure = subprocess.run(['meson', 'compile', '-C', str(a)], env=env, capture_output=True, timeout=300)
    if failure.returncode == 0 or report.read_bytes() != saved or bridge.read_bytes() != saved_bridge:
        raise RuntimeError('failed rebuild replaced a complete consumer result')
    value.write_bytes(saved_source)
    # Prepared read-only source uses external leases/checkouts/output state only.
    permissions = []
    if os.name != 'nt':
        for path in sorted(first.rglob('*'), reverse=True):
            permissions.append((path, path.stat().st_mode))
            path.chmod(0o555 if path.is_dir() else 0o444)
        permissions.append((first, first.stat().st_mode))
        first.chmod(0o555)
    try:
        build_and_run(a, env)
    finally:
        for path, mode in reversed(permissions):
            path.chmod(mode)
    configure(vex, first, a, env, 'release', 'tuned')
    build_and_run(a, env)
    record = result(a)
    if record['metadata']['profile']['opt_level'] != 2 or 'tuned' not in json.dumps(record['metadata']['features']):
        raise RuntimeError('profile/feature selection was not recorded')
    if (first/'wave/target').exists() or (first/'wave/.vex').exists():
        raise RuntimeError('external builds wrote generated state into source')
    print('PASS: Meson C/C++/Rust, independent roots/provenance, restat, imports, failure, readonly, profile/features', flush=True)


def scoped_aliases(vex, temporary, env):
    base = temporary/'scoped aliases é'
    declarations = {
        'one': ('math', '1.0.0', [], 'pub fun value()->i32{return 11;}'),
        'two': ('math', '2.0.0', [], 'pub fun value()->i32{return 31;}'),
        'left': ('left', '1.0.0', [('math', '../one')], 'import("math")::{value}; pub fun left()->i32{return value();}'),
        'right': ('right', '1.0.0', [('math', '../two')], 'import("math")::{value}; pub fun right()->i32{return value();}')}
    for directory, (name, version, dependencies, source) in declarations.items():
        package = base/directory
        (package/'src').mkdir(parents=True)
        deps = ','.join('{name='+json.dumps(alias)+',path='+json.dumps(path)+'}' for alias, path in dependencies)
        (package/'vex.ws').write_text('{format=5,name='+json.dumps(name)+',version='+json.dumps(version)+',targets={lib={}},dependencies=['+deps+']}', encoding='utf-8')
        (package/'src/lib.wave').write_text(source, encoding='utf-8')
    app = base/'app'
    (app/'src').mkdir(parents=True)
    (app/'vex.ws').write_text('{format=5,name="app",version="1.0.0",targets={bins=[{name="app",path="src/main.wave"}]},dependencies=[{name="left",path="../left"},{name="right",path="../right"}]}', encoding='utf-8')
    (app/'src/main.wave').write_text('import("left")::{left};import("right")::{right};fun main(){println("{}",left()+right());}', encoding='utf-8')
    command = [vex, '--manifest-path', app/'vex.ws', '--build-dir', base/'build']
    run(command+['fetch', '--offline'], env)
    output = run(command+['run', '--locked', '--offline'], env)
    if output.stdout.strip() != '42':
        raise RuntimeError('package-scoped aliases did not select their distinct transitive versions')
    print('PASS: Vex scoped aliases and simultaneous transitive versions execute as 42', flush=True)


def cross_archives(vex, temporary, env, selected=None):
    rows = json.loads((ROOT/'platforms.json').read_text())['platforms']
    for row in rows:
        if selected is not None and row['id'] not in selected:
            continue
        triple = row['rust_target']
        if triple.startswith('riscv64gc-'):
            triple = triple.replace('riscv64gc-', 'riscv64-', 1)
        records = []
        for directory in ['first é', 'second é']:
            source = temporary/row['id']/directory/'source'
            (source/'src').mkdir(parents=True)
            relocation = 'default' if row['os'] == 'windows' else 'pic'
            (source/'vex.ws').write_text('{format=5,name="portable",version="1.0.0",targets={lib={output="static",mode="freestanding",relocation="'+relocation+'"}},build={reproducible=true}}', encoding='utf-8')
            (source/'src/lib.wave').write_text('export(c) fun wave_add(a:i32,b:i32)->i32{return a+b;}\n', encoding='utf-8')
            build = source.parent/'build'
            command = [vex, '--manifest-path', source/'vex.ws', '--build-dir', build]
            run(command+['fetch', '--offline'], env)
            outputs = []
            for profile in ['dev', 'release']:
                report = build/(profile+'.json')
                run(command+['build', '--locked', '--offline', '--target', triple, '--profile', profile,
                             '--artifact-manifest', report], env)
                record = json.loads(report.read_text(encoding='utf-8'))['results'][0]
                outputs.append(([p.read_bytes() for p in primaries(record)], record['provenance']))
            records.append(outputs)
        if records[0] != records[1]:
            raise RuntimeError('non-deterministic archive bytes/provenance for '+triple)
        print('PASS: O0/O2 deterministic indexed archive and object machine inspection: '+triple, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vex', type=Path, required=True)
    parser.add_argument('--wavec', type=Path, required=True)
    parser.add_argument('--std', type=Path, required=True)
    parser.add_argument('--cross-formats', action='store_true')
    args = parser.parse_args()
    fixture = json.loads((EXAMPLE/'fixture.json').read_text(encoding='utf-8'))
    for name, expected in fixture['sha256'].items():
        if file_hash(EXAMPLE/name) != expected:
            raise RuntimeError('reviewed Whale-shaped fixture changed: '+name)
    with tempfile.TemporaryDirectory(prefix='vex-external-contract-') as path:
        temporary = Path(path).resolve()
        env = dict(os.environ, VEX_WAVEC=str(args.wavec.resolve()), VEX_WAVE_STD=str(args.std.resolve()),
                   VEX_CACHE_HOME=str(temporary/'cache'), XDG_STATE_HOME=str(temporary/'state'))
        for key in ['GH_TOKEN', 'GITHUB_TOKEN', 'VEX_BUILD_DIR', 'VEX_STATE_DIR']:
            env.pop(key, None)
        capabilities = json.loads(run([args.wavec.resolve(), 'print', 'vex-capabilities', '--format=json'], env).stdout)
        if capabilities.get('schema_version') != 1 or any(capabilities.get(k) != 1 for k in ['package_graph', 'path_remap', 'source_inputs']):
            raise RuntimeError('compiler lacks the required explicit build contract')
        scoped_aliases(args.vex.resolve(), temporary, env)
        meson_contract(args.vex.resolve(), temporary, env)
        if args.cross_formats:
            cross_archives(args.vex.resolve(), temporary, env)


if __name__ == '__main__':
    main()
