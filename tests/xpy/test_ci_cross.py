# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
import importlib.util
import json
import struct
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from tools import ci_cross, repository_checks


def elf(machine):
    return b'\x7fELF\x02\x01' + bytes(12) + struct.pack('<H', machine)


class CrossAcceptanceTests(unittest.TestCase):
    def test_optimized_cross_tests_preserve_debug_and_overflow_checks(self):
        config = dict(target='riscv64gc-unknown-linux-gnu', bin='/cross/bin',
                      temporary='/cross/tmp', fixture_dir='/cross/fixtures')
        with patch.dict('os.environ', CARGO_PROFILE_TEST_OPT_LEVEL='3',
                        CARGO_PROFILE_TEST_DEBUG_ASSERTIONS='false',
                        CARGO_PROFILE_TEST_OVERFLOW_CHECKS='false', VEX_CI_EXECUTION_PLATFORM='host'):
            env = ci_cross.build_environment(config)
        self.assertEqual(env['CARGO_PROFILE_TEST_OPT_LEVEL'], '1')
        self.assertEqual(env['CARGO_PROFILE_TEST_DEBUG_ASSERTIONS'], 'true')
        self.assertEqual(env['CARGO_PROFILE_TEST_OVERFLOW_CHECKS'], 'true')
        self.assertNotIn('VEX_CI_EXECUTION_PLATFORM', env)

    def test_wrong_architecture_or_non_elf_cannot_enter_target_runner(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory)/'test'
            executable.write_bytes(elf(243))
            ci_cross.check_elf(executable, 243)
            with self.assertRaises(ValueError): ci_cross.check_elf(executable, 258)
            executable.write_bytes(b'#!/bin/sh\nexit 0\n')
            with self.assertRaises(ValueError): ci_cross.check_elf(executable, 243)

    def test_guest_gets_runtime_overrides_but_no_host_path_or_credentials(self):
        config = dict(user='1000:1000', guest='fixture', platform='linux-riscv64')
        env = dict(GH_TOKEN='secret', GITHUB_TOKEN='secret', CARGO_REGISTRY_TOKEN='secret',
                   LD_LIBRARY_PATH='/host-libraries', PATH='/host-tools',
                   VEX_TOOLCHAIN_HOME='/shared/compiler', VEX_CROSS_CONFIG='/host-config',
                   VEX_TEST_TARGET_RUNNER='/host-runner', VEX_TEST_GUEST_PATH='/shared/compiler:/usr/bin',
                   NO_COLOR='1')
        command = ci_cross.guest_command(config, ['/shared/vex', 'run', '', 'two words'], env, '/shared/project')
        self.assertNotIn('secret', ' '.join(command))
        self.assertNotIn('/host-', ' '.join(command))
        self.assertIn('PATH=/shared/compiler:/usr/bin', command)
        self.assertIn('VEX_TOOLCHAIN_HOME=/shared/compiler', command)
        self.assertIn('VEX_CI_EXECUTION_PLATFORM=linux-riscv64', command)
        self.assertEqual(command[-4:], ['/shared/vex', 'run', '', 'two words'])

    def test_cross_report_cannot_claim_native_build_or_another_runtime(self):
        row = dict(id='linux-riscv64', rust_target='riscv64gc-unknown-linux-gnu',
                   executor='qemu', arch='riscv64')
        pin = dict(version='fixture', archives={row['id']: 'd'*64})
        report = dict(schema_version=1, executor='qemu', os='Linux',
                      source='a'*40, platform=row['id'], rust_target=row['rust_target'], status='passed',
                      wave_version='fixture', compiler_sha256='b'*64, wave_archive_digest='sha256:'+'d'*64,
                      build_method='cross', build_host='x86_64-unknown-linux-gnu', execution_arch='riscv64',
                      sdk_image='sha256:'+'e'*64, cross_compiler='GCC fixture', cross_packages=['libc fixture'],
                      runtime_image='sha256:'+'f'*64,
                      fixtures={name: dict(source_sha256='f'*64, binary_sha256='c'*64) for name in ci_cross.FIXTURES})
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            path = directory/f'acceptance-{row["id"]}.json'
            path.write_text(json.dumps(report))
            repository_checks.acceptance(directory, 'a'*40, [row], pin)
            for field, value in [('build_method', 'native'), ('build_host', row['rust_target']),
                                 ('execution_arch', 'x86_64'), ('fixtures', {}), ('sdk_image', 'mutable-tag')]:
                path.write_text(json.dumps(dict(report, **{field: value})))
                with self.assertRaises(ValueError): repository_checks.acceptance(directory, 'a'*40, [row], pin)

    def test_prebuilt_fixture_source_and_binary_are_bound_to_target_and_checkout(self):
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root/'tests/fixtures'
            source.mkdir(parents=True)
            binaries = root/'binaries'
            binaries.mkdir()
            fixtures = {}
            for name in ci_cross.FIXTURES:
                (source/f'{name}.rs').write_bytes(b'fixture source')
                (binaries/name).write_bytes(elf(243))
                fixtures[name] = dict(source_sha256=hashlib.sha256(b'fixture source').hexdigest(),
                                      binary_sha256=hashlib.sha256(elf(243)).hexdigest())
            target = dict(id='linux-riscv64', rust_target='riscv64gc-unknown-linux-gnu')
            config = dict(target=target['rust_target'], platform=target['id'], fixture_dir=str(binaries),
                          build_host='x86_64-unknown-linux-gnu', fixtures=fixtures)
            with patch.object(ci_cross, 'ROOT', root):
                ci_cross.validate_build(config, target)
                (binaries/'fake_wavec').write_bytes(elf(258))
                with self.assertRaises(ValueError): ci_cross.validate_build(config, target)
                (binaries/'fake_wavec').write_bytes(elf(243))
                (source/'fake_wavec.rs').write_bytes(b'changed checkout')
                with self.assertRaises(ValueError): ci_cross.validate_build(config, target)

    def test_foreign_compilers_stay_on_host_but_consumers_execute_in_guest(self):
        path = Path(__file__).resolve().parents[1]/'native_interop.py'
        spec = importlib.util.spec_from_file_location('cross_native_interop', path)
        interop = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(interop)
        env = dict(VEX_TEST_TARGET_RUNNER='/host/runner')
        with patch.object(interop.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout='', stderr='')) as run:
            interop.run(['/host/cc', 'input.c', '-o', '/shared/consumer'], Path('/shared'), env, native_tool=True)
            self.assertEqual(run.call_args.args[0][0], '/host/cc')
            interop.run(['/shared/consumer', 'argument with spaces'], Path('/shared'), env)
            self.assertEqual(run.call_args.args[0], ['/host/runner', '/shared/consumer', 'argument with spaces'])


if __name__ == '__main__':
    unittest.main()
