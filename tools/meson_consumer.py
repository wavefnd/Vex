#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Copy one verified Vex library into a Meson-owned bridge without mtime churn."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
import signal
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import time


def path_form(path):
    text = str(path)
    if os.name == 'nt':
        if text.startswith('\\\\?\\UNC\\'):
            text = '\\\\' + text[8:]
        elif text.startswith('\\\\?\\'):
            text = text[4:]
        text = os.path.normcase(text)
    return Path(text)


def regular(path):
    for parent in (path, *path.parents):
        if parent.is_symlink() or (hasattr(parent, 'is_junction') and parent.is_junction()):
            raise ValueError(f'consumer paths must not contain links: {parent}')
    if not stat.S_ISREG(path.stat().st_mode):
        raise ValueError(f'consumer input is not a regular file: {path}')


def digest(path):
    regular(path)
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def read_json(path):
    regular(path)
    with path.open('rb') as stream:
        data = stream.read(8 * 1024 * 1024 + 1)
    if len(data) > 8 * 1024 * 1024:
        raise ValueError('consumer metadata exceeds 8 MiB')
    return json.loads(data)


def sync_directory(path):
    if os.name != 'nt':
        fd = os.open(path, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def atomic_bytes(path, data):
    fd, temporary = tempfile.mkstemp(prefix='.vex-consumer-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


@contextmanager
def generation_lease(build, generation):
    """Use Vex's existing inode/Windows share lease, never an existence lock."""
    base = build / 'target/.vex-run'
    if generation.parent != base or not generation.name or any(c not in '0123456789-' for c in generation.name):
        raise ValueError('invalid artifact generation')
    lock = build / '.vex/run-locks' / (generation.name + '.lock')
    regular(lock)
    if os.name == 'nt':
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                      wintypes.LPVOID, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
        kernel.CreateFileW.restype = wintypes.HANDLE
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        deadline = time.monotonic() + 30
        while True:
            handle = kernel.CreateFileW(str(lock), 0x80000000, 1, None, 3, 0x00200000, None)
            if handle != wintypes.HANDLE(-1).value:
                break
            code = ctypes.get_last_error()
            if code != 32 or time.monotonic() >= deadline:
                raise OSError(code, 'cannot hold Vex generation lease')
            time.sleep(.04)
        try:
            yield
        finally:
            kernel.CloseHandle(handle)
    else:
        import fcntl
        fd = os.open(lock, os.O_RDONLY | os.O_NOFOLLOW)
        try:
            fcntl.flock(fd, fcntl.LOCK_SH)
            yield
        finally:
            os.close(fd)


def escape(path):
    text = str(path)
    if any(c in text for c in '\r\n\0'):
        raise ValueError('depfile cannot represent newlines or NUL')
    return text.replace('\\', '\\\\').replace('$', '$$').replace(' ', '\\ ').replace('#', '\\#').replace(':', '\\:')


def publish_bridge(output, data, depfile, dependencies, rust_main=None, rust_source=None):
    owner = output.with_name(output.name + '.vex-owner.json')
    registry = read_json(owner) if owner.exists() else {'schema_version': 1, 'files': {}}
    if registry.get('schema_version') != 1 or not isinstance(registry.get('files'), dict):
        raise ValueError('invalid bridge ownership record')
    files = [(output, data), (depfile, (escape(output) + ': ' + ' '.join(escape(p) for p in sorted(set(dependencies))) + '\n').encode())]
    if rust_main:
        source = str(rust_source.resolve())
        if any(ord(c) < 32 for c in source):
            raise ValueError('Rust consumer source path contains control characters')
        shim = '// Vex bridge sha256: ' + hashlib.sha256(data).hexdigest() + '\ninclude!(' + json.dumps(source, ensure_ascii=False) + ');\n'
        files.append((rust_main, shim.encode('utf-8')))
    updated = {'schema_version': 1, 'files': {}}
    for path, content in files:
        current = digest(path) if path.exists() else None
        allowed = registry['files'].get(str(path), [])
        if not isinstance(allowed, list) or (current is not None and current not in allowed):
            raise ValueError(f'refusing to overwrite unowned/modified bridge file: {path}')
        new = hashlib.sha256(content).hexdigest()
        updated['files'][str(path)] = [new] if current is None else sorted({current, new})
    # Record both recoverable states before atomic replacement. An interrupted
    # copy can leave only a complete previously verified bridge, never a prefix.
    atomic_bytes(owner, (json.dumps(updated, sort_keys=True) + '\n').encode())
    for path, content in files:
        if not path.exists() or digest(path) != hashlib.sha256(content).hexdigest():
            atomic_bytes(path, content)
    updated['files'] = {str(p): [hashlib.sha256(data).hexdigest()] for p, data in files}
    atomic_bytes(owner, (json.dumps(updated, sort_keys=True) + '\n').encode())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vex', required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--build-dir', type=Path, required=True)
    parser.add_argument('--state-dir', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--depfile', type=Path, required=True)
    parser.add_argument('--rust-main', type=Path)
    parser.add_argument('--rust-source', type=Path)
    parser.add_argument('--kind', choices=['object', 'static'], required=True)
    parser.add_argument('--profile', default='dev')
    parser.add_argument('--target')
    parser.add_argument('--features', action='append', default=[])
    args = parser.parse_args()
    if bool(args.rust_main) != bool(args.rust_source):
        parser.error("--rust-main and --rust-source must be provided together")
    build = path_form(args.build_dir.resolve())
    report = build / 'consumer-result.json'
    command = [args.vex, '--manifest-path', str(args.manifest.resolve()), '--build-dir', str(build)]
    if args.state_dir:
        command += ['--state-dir', str(args.state_dir.resolve())]
    for feature in args.features:
        command += ['--features', feature]
    command += ['build', '--lib', '--locked', '--offline', '--profile', args.profile,
                '--artifact-manifest', str(report), '--depfile', str(build / 'consumer-inputs.d')]
    if args.target:
        command += ['--target', args.target]
    # Leave stdout/stderr and process cancellation connected to Meson/Ninja.
    child = subprocess.Popen(command)
    previous = {}
    cancelled = False
    def forward(signum, frame):
        nonlocal cancelled
        cancelled = True
        if child.poll() is None:
            child.send_signal(signum)
    try:
        for signum in (signal.SIGINT, signal.SIGTERM):
            previous[signum] = signal.signal(signum, forward)
        code = child.wait()
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
    if cancelled:
        return 130
    if code:
        return code if code > 0 else 128 - code
    value = read_json(report)
    if value.get('schema') != 'vex.artifacts' or value.get('schema_version') != 1 or len(value.get('results', [])) != 1:
        raise ValueError('expected one version-1 artifact result')
    result = value['results'][0]
    if result.get('schema') != 'vex.artifact-generation' or result.get('schema_version') != 1 or result['metadata']['artifact_kind'] != args.kind:
        raise ValueError('artifact result kind/schema differs from the declared Meson bridge')
    generation = path_form(Path(result['generation']))
    with generation_lease(build, generation):
        primary = []
        for artifact in result['artifacts']:
            if '..' in Path(artifact['path']).parts:
                raise ValueError('artifact result contains parent traversal')
            path = path_form(Path(artifact['path']))
            if not path.is_relative_to(generation) or '..' in path.parts or digest(path) != artifact['sha256']:
                raise ValueError('artifact escaped its generation or failed content verification')
            if artifact['role'] == 'primary':
                primary.append(path)
        if len(primary) != 1:
            raise ValueError('expected exactly one primary library')
        inputs = result['metadata']['inputs']
        dependencies = inputs['files'] + inputs['directories']
        publish_bridge(args.output.absolute(), primary[0].read_bytes(), args.depfile.absolute(), dependencies, args.rust_main.absolute() if args.rust_main else None, args.rust_source)
    return 0


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'error: Vex Meson consumer: {error}', file=sys.stderr)
        raise SystemExit(1)
