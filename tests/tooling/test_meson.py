# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Exercise actual Meson/Ninja scheduling without rebuilding Rust in each test."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch
from tools import automation_step

ROOT = Path(__file__).resolve().parents[2]


class CargoEnvironmentTests(unittest.TestCase):
    def test_riscv_linker_override_is_preserved_and_missing_linker_fails(self):
        key = 'CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER'
        with patch.dict(os.environ, {key: '/explicit/linker'}):
            with patch.object(automation_step.shutil, 'which', return_value=None):
                self.assertEqual(automation_step.environment(['cargo', 'build', '--target=riscv64gc-unknown-linux-gnu'])[key], '/explicit/linker')
        env = dict(os.environ)
        env.pop(key, None)
        with patch.dict(os.environ, env, clear=True):
            with patch.object(automation_step.shutil, 'which', return_value='/sdk/riscv64-linux-gnu-gcc'):
                self.assertEqual(automation_step.environment(['cargo', 'build', '--target', 'riscv64gc-unknown-linux-gnu'])[key], '/sdk/riscv64-linux-gnu-gcc')
            with patch.object(automation_step.shutil, 'which', return_value=None):
                with self.assertRaisesRegex(ValueError, 'RISC-V builds require'):
                    automation_step.environment(['cargo', 'build', '--target', 'riscv64gc-unknown-linux-gnu'])


class MesonAutomationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='vex meson ')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in ['meson.build', 'meson.options', 'Cargo.toml', 'rust-toolchain.toml', 'platforms.json']:
            shutil.copy2(ROOT / name, self.root / name)
        shutil.copytree(ROOT / 'tools', self.root / 'tools', ignore=shutil.ignore_patterns('__pycache__'))
        self.log = self.root / 'cargo.jsonl'
        fake = self.root / 'fake cargo.py'
        fake.write_text('''import json, os, pathlib, sys
with open(os.environ['VEX_MESON_TEST_LOG'], 'a') as log:
    log.write(json.dumps(dict(args=sys.argv[1:], toolchain=os.environ.get('RUSTUP_TOOLCHAIN'))) + '\\n')
if os.environ.get('VEX_MESON_TEST_FAIL') in sys.argv[1:]:
    sys.exit(7)
''')
        self.env = dict(os.environ, VEX_MESON_TEST_LOG=str(self.log))
        self.env.pop('VEX_MESON_TEST_FAIL', None)
        self.cargo = '-Dcargo=' + json.dumps([sys.executable, str(fake)])

    def run_meson(self, *args, success=True):
        result = subprocess.run(['meson', *map(str, args)], cwd=self.root,
                                env=self.env, text=True, capture_output=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def setup(self, *options):
        return self.run_meson('setup', 'build', self.cargo,
                              '-Drelease_targets=x86_64-unknown-linux-gnu', *options)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_default_compile_builds_the_cargo_workspace(self):
        self.setup()
        self.run_meson('compile', '-C', 'build')
        self.assertEqual([call['args'] for call in self.calls()], [['build', '--workspace', '--locked']])

    def test_repeated_build_and_source_change_always_reach_cargo(self):
        self.setup()
        self.run_meson('compile', '-C', 'build', 'build-release')
        self.run_meson('compile', '-C', 'build', 'build-release')
        (self.root / 'src').mkdir()
        (self.root / 'src/main.rs').write_text('fn main() {}\n')
        self.run_meson('compile', '-C', 'build', 'build-release')
        self.assertEqual(len(self.calls()), 3)
        for call in self.calls():
            self.assertEqual(call['args'], ['build', '--locked', '--release', '--target', 'x86_64-unknown-linux-gnu'])
            self.assertEqual(call['toolchain'], tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel'])
        # A previous successful stamp must not conceal a later failed build.
        self.env['VEX_MESON_TEST_FAIL'] = 'build'
        self.run_meson('compile', '-C', 'build', 'build-release', success=False)
        self.assertFalse((self.root / 'build/build-x86_64-unknown-linux-gnu.stamp').exists())

    def test_check_stops_before_tests_when_formatting_fails(self):
        self.setup()
        self.env['VEX_MESON_TEST_FAIL'] = 'fmt'
        self.run_meson('compile', '-C', 'build', 'check', success=False)
        self.assertEqual([c['args'] for c in self.calls()], [['fmt', '--check']])
        self.assertFalse((self.root / 'build/check-tooling.stamp').exists())

    def test_release_source_gate_precedes_every_cargo_command(self):
        self.setup()
        subprocess.run(['git', 'init', '--quiet', str(self.root)], check=True)
        result = self.run_meson('compile', '-C', 'build', 'release', success=False)
        self.assertIn('official releases require a clean Git working tree', result.stdout + result.stderr)
        self.assertEqual(self.calls(), [])

    def test_release_rejects_development_verifier_override(self):
        self.setup('-Dallow_unverified=true')
        result = self.run_meson('compile', '-C', 'build', 'release', success=False)
        self.assertIn('official releases require -Dallow_unverified=false', result.stdout + result.stderr)
        self.assertEqual(self.calls(), [])

    def test_unknown_target_fails_configuration_without_cargo(self):
        result = self.run_meson('setup', 'build', self.cargo,
                                '-Drelease_targets=imaginary-target', success=False)
        self.assertIn('Unsupported release target: imaginary-target', result.stdout)
        self.assertEqual(self.calls(), [])

    def minimal_tooling_suite(self):
        suite = self.root / 'tests/tooling'
        suite.mkdir(parents=True)
        (suite / 'test_fixture.py').write_text(
            "import pathlib, unittest\nclass Fixture(unittest.TestCase):\n"
            "    def test_marker(self): pathlib.Path('tooling-marker').touch()\n")

    def test_successful_check_runs_the_complete_ordered_suite(self):
        self.minimal_tooling_suite()
        self.setup()
        self.run_meson('compile', '-C', 'build', 'check')
        self.assertTrue((self.root / 'tooling-marker').is_file())
        self.assertEqual([call['args'][0] for call in self.calls()], ['fmt', 'test', 'clippy', 'build'])
        self.assertIn('--no-fail-fast', self.calls()[1]['args'])
        for call in self.calls()[1:]:
            self.assertIn('--workspace', call['args'])
            self.assertIn('--locked', call['args'])

    def test_tagged_release_runs_checks_before_release_build_and_preserves_archive_on_failure(self):
        self.minimal_tooling_suite()
        self.setup()
        (self.root / '.gitignore').write_text('/build/\n/cargo.jsonl\n/tooling-marker\n/dist/\n')
        def git(*args):
            return subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                                   '-c', 'commit.gpgsign=false', '-c', 'tag.gpgsign=false', *args],
                                  cwd=self.root, check=True, capture_output=True)
        git('init', '--quiet')
        git('add', '.')
        git('commit', '--quiet', '-m', 'fixture')
        version = tomllib.loads((self.root / 'Cargo.toml').read_text())['workspace']['package']['version']
        git('tag', '-a', 'v' + version, '-m', 'fixture release')
        (self.root / 'dist').mkdir()
        archive = self.root / 'dist/previous-archive'
        archive.write_bytes(b'preserve')
        self.env['VEX_MESON_TEST_FAIL'] = '--release'
        self.run_meson('compile', '-C', 'build', 'release', success=False)
        self.assertEqual([call['args'][0] for call in self.calls()], ['fmt', 'test', 'clippy', 'build', 'build'])
        self.assertIn('--release', self.calls()[-1]['args'])
        self.assertEqual(archive.read_bytes(), b'preserve')

    def test_multiple_targets_are_ordered_deduplicated_and_fail_fast(self):
        self.setup('-Drelease_targets=x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu,x86_64-unknown-linux-gnu')
        self.run_meson('compile', '-C', 'build', 'build-release')
        self.assertEqual([call['args'][-1] for call in self.calls()],
                         ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu'])
        self.env['VEX_MESON_TEST_FAIL'] = 'x86_64-unknown-linux-gnu'
        self.run_meson('compile', '-C', 'build', 'build-release', success=False)
        self.assertEqual(len(self.calls()), 3)

    def test_clean_cleans_cargo_before_removing_distribution(self):
        self.setup()
        (self.root / 'dist').mkdir()
        (self.root / 'dist/owned-archive').write_text('fixture')
        self.env['VEX_MESON_TEST_FAIL'] = 'clean'
        self.run_meson('compile', '-C', 'build', 'clean-artifacts', success=False)
        self.assertTrue((self.root / 'dist/owned-archive').is_file())
        self.env.pop('VEX_MESON_TEST_FAIL')
        self.run_meson('compile', '-C', 'build', 'clean-artifacts')
        self.assertFalse((self.root / 'dist').exists())
        self.assertEqual([call['args'] for call in self.calls()], [['clean'], ['clean']])

    def test_manifest_version_change_regenerates_configuration(self):
        self.setup()
        manifest = self.root / 'Cargo.toml'
        version = tomllib.loads(manifest.read_text())['workspace']['package']['version']
        manifest.write_text(manifest.read_text().replace('version = ' + json.dumps(version), 'version = "9.8.7-rc.1"'))
        result = self.run_meson('compile', '-C', 'build', 'version')
        self.assertIn('Cargo version', result.stdout)
        self.assertIn('9.8.7-rc.1', result.stdout)
        self.assertEqual(self.calls(), [])


if __name__ == '__main__':
    unittest.main()
