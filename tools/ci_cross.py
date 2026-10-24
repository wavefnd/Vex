#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Compile on amd64; run complete target process trees in pinned QEMU guests."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {'linux-riscv64': ('riscv64-linux-gnu', 243, 'riscv64'),
           'linux-loongarch64': ('loongarch64-linux-gnu', 258, 'loongarch64')}
FIXTURES = ('fake_wavec', 'target_wavec', 'native_wavec', 'artifact_wavec', 'doctor_tools', 'version_probe')


def call(*args, **kwargs):
    print('+', shlex.join(map(str, args)), flush=True)
    try:
        return subprocess.run(list(map(str, args)), check=True, **kwargs)
    except subprocess.CalledProcessError as error:
        if error.stdout:
            print(error.stdout, end='', flush=True)
        if error.stderr:
            print(error.stderr, end='', file=sys.stderr, flush=True)
        raise


def load(path):
    config = json.loads(Path(path).read_text())
    if config.get('schema_version') != 1 or config.get('platform') not in TARGETS:
        raise ValueError('invalid cross execution configuration')
    return config


def check_elf(path, machine):
    with Path(path).open('rb') as binary:
        header = binary.read(20)
    if (len(header) != 20 or header[:6] != b'\x7fELF\x02\x01'
            or struct.unpack('<H', header[18:20])[0] != machine):
        raise ValueError(f'wrong target ELF architecture: {path}')


def guest_command(config, args, env=None, cwd=None):
    # Only execution settings cross the boundary. In particular, do not forward
    # Cargo credentials, host PATH/LD_LIBRARY_PATH, or GitHub authentication.
    forwarded = {key: value for key, value in (env or {}).items()
                 if (key.startswith('VEX_') and not key.startswith('VEX_CROSS_'))
                 or key in {'NO_COLOR', 'TERM', 'RUST_BACKTRACE', 'RUST_TEST_THREADS'}}
    forwarded.pop('VEX_TEST_TARGET_RUNNER', None)
    forwarded.pop('VEX_TEST_GUEST_PATH', None)
    if env and env.get('VEX_TEST_GUEST_PATH'):
        forwarded['PATH'] = env['VEX_TEST_GUEST_PATH']
    if config.get('platform'):
        forwarded['VEX_CI_EXECUTION_PLATFORM'] = config['platform']
    command = ['docker', 'exec', '-i', '--user', config['user'], '--workdir', str(cwd or Path.cwd())]
    for key, value in forwarded.items():
        command.extend(['--env', f'{key}={value}'])
    return [*command, config['guest'], *map(str, args)]


def run_target(config, args, env=None, cwd=None, **kwargs):
    return call(*guest_command(config, args, env, cwd), **kwargs)


def wrapper(path, mode, config, tool=None):
    arguments = [sys.executable, str(Path(__file__).resolve()), mode, str(config)]
    if tool:
        arguments.append(tool)
    path.write_text('#!' + sys.executable + '\nimport os, sys\nos.execv(' + repr(sys.executable)
                    + ', ' + repr(arguments) + ' + sys.argv[1:])\n')
    path.chmod(0o755)


def build_environment(config):
    env = dict(os.environ)
    key = config['target'].upper().replace('-', '_')
    env[f'CARGO_TARGET_{key}_LINKER'] = str(Path(config['bin']) / 'cc')
    env[f'CARGO_TARGET_{key}_RUNNER'] = str(Path(config['bin']) / 'runner')
    env['TMPDIR'] = config['temporary']
    env['VEX_TEST_FIXTURE_DIR'] = config['fixture_dir']
    env.pop('VEX_CI_EXECUTION_PLATFORM', None)
    env['RUST_TEST_THREADS'] = '2'
    env['CARGO_BUILD_JOBS'] = str(min(os.cpu_count() or 2, 4))
    # Keep the dev/test correctness checks while avoiding emulated unoptimized
    # hashing/graph loops. Native platform lanes still exercise opt-level 0.
    env['CARGO_PROFILE_TEST_OPT_LEVEL'] = '1'
    env['CARGO_PROFILE_TEST_DEBUG_ASSERTIONS'] = 'true'
    env['CARGO_PROFILE_TEST_OVERFLOW_CHECKS'] = 'true'
    return env


def interop_environment(config, env):
    return dict(env, CC=str(Path(config['bin'])/'cc'), CXX=str(Path(config['bin'])/'c++'),
                AR=str(Path(config['bin'])/'ar'), RUSTC=str(Path(config['bin'])/'rustc'),
                VEX_TEST_TARGET_RUNNER=str(Path(config['bin'])/'runner'),
                VEX_TEST_GUEST_PATH=str(Path(env['PATH'].split(os.pathsep)[0]))+':/usr/local/bin:/usr/bin:/bin')


def execution_identity(config):
    script = 'import json, platform; print(json.dumps(dict(os=platform.platform(), libc=platform.libc_ver(), machine=platform.machine())))'
    identity = run_target(config, ['python3', '-c', script], capture_output=True, text=True).stdout
    identity = json.loads(identity)
    if identity['machine'] != TARGETS[config['platform']][2]:
        raise ValueError('acceptance is not executing inside the selected QEMU guest')
    return identity


def validate_build(config, target):
    if config['target'] != target['rust_target'] or config['platform'] != target['id']:
        raise ValueError('cross build target does not match acceptance target')
    if config['build_host'] != 'x86_64-unknown-linux-gnu':
        raise ValueError('QEMU lanes must compile on amd64')
    for name, fingerprint in config['fixtures'].items():
        source = ROOT/'tests/fixtures'/f'{name}.rs'
        executable = Path(config['fixture_dir'])/name
        if (hashlib.sha256(source.read_bytes()).hexdigest() != fingerprint['source_sha256']
                or hashlib.sha256(executable.read_bytes()).hexdigest() != fingerprint['binary_sha256']):
            raise ValueError(f'stale or corrupt target fixture: {name}')
        check_elf(executable, TARGETS[target['id']][1])
    if set(config['fixtures']) != set(FIXTURES):
        raise ValueError('cross fixture inventory is incomplete')


def verify_doctest_runner(config):
    # Cargo 1.96 must really execute cross doctests through its runner. A zero
    # doctest workspace alone cannot detect a regression that silently skips it.
    probe = Path(config['temporary'])/'doctest-runner-probe'
    (probe/'src').mkdir(parents=True, exist_ok=True)
    (probe/'Cargo.toml').write_text('[package]\nname="cross-doctest-probe"\nversion="0.0.0"\nedition="2021"\n[workspace]\n')
    arch = TARGETS[config['platform']][2]
    (probe/'src/lib.rs').write_text('//! ```\n//! assert_eq!(std::env::consts::ARCH, '+json.dumps(arch)+');\n'
                                  '//! assert_eq!(std::env::var("VEX_CI_EXECUTION_PLATFORM").unwrap(), '+json.dumps(config['platform'])+');\n'
                                  '//! assert!(cfg!(debug_assertions));\n'
                                  '//! let maximum = std::hint::black_box(u8::MAX);\n'
                                  '//! assert!(std::panic::catch_unwind(|| maximum + 1).is_err());\n'
                                  '//! assert!(std::path::Path::new(&std::env::var("VEX_TEST_FIXTURE_DIR").unwrap()).is_dir());\n//! ```\n')
    env = build_environment(config)
    call('cargo', 'generate-lockfile', '--offline', '--manifest-path', probe/'Cargo.toml', env=env)
    result = call('cargo', 'test', '--doc', '--locked', '--target', config['target'],
                  '--manifest-path', probe/'Cargo.toml', env=env, capture_output=True, text=True)
    print(result.stdout, end='', flush=True)
    if '1 passed;' not in result.stdout:
        raise ValueError('Cargo did not execute the target doctest through QEMU')


def orchestrate(platform_id, api_token=None):
    if sys.platform != 'linux' or platform.machine() != 'x86_64':
        raise ValueError('cross build orchestration requires Linux amd64')
    rows = json.loads((ROOT/'platforms.json').read_text())['platforms']
    target = next(row for row in rows if row['id'] == platform_id)
    gnu, machine, _ = TARGETS[platform_id]
    host = subprocess.check_output(['rustc', '-vV'], text=True)
    if 'host: x86_64-unknown-linux-gnu\n' not in host or 'release: 1.96.0\n' not in host:
        raise ValueError('cross acceptance requires amd64 Rust 1.96.0')
    sysroot = subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip()
    temporary_root = ROOT/'.tmp'
    temporary_root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f'cross-{platform_id}-', dir=temporary_root) as working:
        work = Path(working)
        for name in ('bin', 'tmp', 'home', 'cache', 'fixtures'):
            (work/name).mkdir()
        config_path = work/'config.json'
        identifier = 'vex-cross-'+uuid.uuid4().hex
        sdk, guest = identifier+'-sdk', identifier+'-guest'
        sdk_image = 'vex-ci-cross-'+platform_id+':local'
        runtime_image = 'vex-ci-runtime-'+platform_id+':local'
        config = dict(schema_version=1, platform=platform_id, target=target['rust_target'],
                      build_host='x86_64-unknown-linux-gnu', rustc=shutil.which('rustc'),
                      sdk=sdk, guest=guest, gnu_target=gnu, user=f'{os.getuid()}:{os.getgid()}',
                      bin=str(work/'bin'), temporary=str(work/'tmp'), fixture_dir=str(work/'fixtures'),
                      fixtures={})
        containers = []
        try:
            call('docker', 'build', '--platform', 'linux/amd64', '--build-arg', f'GNU_TARGET={gnu}',
                 '-f', ROOT/'tools/ci_cross.Dockerfile', '-t', sdk_image, ROOT/'tools')
            mount = ['--mount', f'type=bind,src={ROOT},dst={ROOT}']
            call('docker', 'run', '-d', '--name', sdk, '--platform', 'linux/amd64',
                 '--user', config['user'], *mount,
                 '--mount', f'type=bind,src={sysroot},dst={sysroot},readonly',
                 '--workdir', ROOT, sdk_image, 'sleep', 'infinity')
            containers.append(sdk)
            config['sdk_image'] = subprocess.check_output(['docker', 'inspect', sdk, '--format', '{{.Image}}'], text=True).strip()
            config['cross_compiler'] = subprocess.check_output(['docker', 'exec', sdk, gnu+'-gcc', '--version'], text=True).splitlines()[0]
            config['cross_packages'] = subprocess.check_output(['docker', 'exec', sdk, 'dpkg-query', '-W', '-f=${Package}=${Version}\n'], text=True).splitlines()
            call('docker', 'build', '--platform', target['oci_platform'], '--build-arg', f'RUNTIME_IMAGE={target["image"]}',
                 '-f', ROOT/'tools/ci_qemu.Dockerfile', '-t', runtime_image, ROOT/'tools')
            call('docker', 'run', '-d', '--name', guest, '--platform', target['oci_platform'], *mount,
                 '--workdir', ROOT, '--env', f'TMPDIR={work/"tmp"}', '--env', f'HOME={work/"home"}',
                 '--env', f'VEX_CACHE_HOME={work/"cache"}', '--env', 'RUST_TEST_THREADS=2',
                 '--env', f'VEX_TEST_FIXTURE_DIR={work/"fixtures"}', runtime_image, 'sleep', 'infinity')
            containers.append(guest)
            config['runtime_image'] = subprocess.check_output(['docker', 'inspect', guest, '--format', '{{.Image}}'], text=True).strip()
            # Runtime subprocesses get no host toolchain, Docker socket or auth.
            run_target(config, ['git', 'config', '--global', '--add', 'safe.directory', str(ROOT)])
            config_path.write_text(json.dumps(config))
            for tool in ('cc', 'c++', 'ar', 'rustc'):
                wrapper(work/'bin'/tool, 'tool', config_path, tool)
            wrapper(work/'bin'/'runner', 'run', config_path)
            env = build_environment(config)
            for name in FIXTURES:
                source = ROOT/'tests/fixtures'/f'{name}.rs'
                executable = work/'fixtures'/name
                call('rustc', '--edition=2021', '--target', target['rust_target'], '-C',
                     f'linker={work/"bin/cc"}', source, '-o', executable, env=env)
                check_elf(executable, machine)
                shutil.copy2(source, executable.with_suffix('.rs'))
                config['fixtures'][name] = dict(source_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),
                                               binary_sha256=hashlib.sha256(executable.read_bytes()).hexdigest())
            config_path.write_text(json.dumps(config))
            acceptance_env = dict(env, GH_TOKEN=api_token) if api_token else env
            call(sys.executable, ROOT/'tools/ci_platform.py', platform_id, '--cross-config', config_path, env=acceptance_env)
        finally:
            for container in reversed(containers):
                subprocess.run(['docker', 'rm', '-f', container], check=False)


def main():
    # Tool wrappers inherit no GitHub authentication, even if a local caller has
    # exported it. The native acceptance entry point handles API auth separately.
    api_token = os.environ.pop('GH_TOKEN', None)
    os.environ.pop('GITHUB_TOKEN', None)
    if len(sys.argv) > 2 and sys.argv[1] in {'run', 'tool'}:
        mode, path, *args = sys.argv[1:]
        config = load(path)
        if mode == 'run':
            check_elf(args[0], TARGETS[config['platform']][1])
            os.execvp('docker', guest_command(config, args, os.environ))
        tool, *arguments = args
        if tool == 'rustc':
            command = [config['rustc'], '--target', config['target'], '-C',
                       f'linker={Path(config["bin"])/"cc"}', *arguments]
        else:
            program = config['gnu_target']+{'cc': '-gcc', 'c++': '-g++', 'ar': '-ar'}[tool]
            command = ['docker', 'exec', '--workdir', str(Path.cwd()), config['sdk'], program, *arguments]
        os.execvp(command[0], command)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=TARGETS)
    orchestrate(parser.parse_args().platform, api_token)


if __name__ == '__main__':
    main()
