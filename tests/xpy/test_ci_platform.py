# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
import importlib.util
import io
import json
import os
import sys
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('ci_platform', ROOT / 'tools/ci_platform.py')
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


class PlatformAcceptanceTests(unittest.TestCase):
    def test_cross_lane_retains_all_phases_and_never_sends_authentication_to_guest(self):
        native, guest = [], []
        target = next(row for row in ci.PLATFORMS if row['id'] == 'linux-riscv64')
        config = dict(platform=target['id'], target=target['rust_target'],
                      build_host='x86_64-unknown-linux-gnu', bin='/cross/tools',
                      sdk_image='sha256:'+'c'*64, cross_compiler='cross GCC', cross_packages=['SDK libc'],
                      runtime_image='sha256:'+'e'*64,
                      fixtures={})

        def host_run(*args, env=None, capture=False):
            self.assertNotIn('GH_TOKEN', os.environ)
            self.assertNotIn('GITHUB_TOKEN', os.environ)
            native.append((args, env))
            if env is not None:
                self.assertNotIn('GH_TOKEN', env)
                self.assertNotIn('GITHUB_TOKEN', env)
            return SimpleNamespace(stdout='host: x86_64-unknown-linux-gnu\n' if args[0] == 'rustc' else 'a'*40)

        def guest_run(configuration, command, env=None, cwd=None):
            self.assertIs(configuration, config)
            guest.append((command, env))
            if env is not None:
                self.assertNotIn('GH_TOKEN', env)
                self.assertNotIn('GITHUB_TOKEN', env)
            if len(command) > 1 and command[1] == 'setup':
                home = Path(env['VEX_TOOLCHAIN_HOME'])
                (home/'current').write_text('wavec')
                (home/'wavec').write_bytes(b'compiler fixture')

        release = dict(draft=False, tag_name='v'+ci.WAVE_VERSION, id=1,
                       assets=[dict(id=2, name=f'wave-v{ci.WAVE_VERSION}-{target["wave_target"]}.tar.gz')])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root/'dist').mkdir()
            with (patch.dict(os.environ, GH_TOKEN='host-api-secret', GITHUB_TOKEN='never-forward'),
                  patch.object(ci, 'ROOT', root), patch.object(ci, 'run', side_effect=host_run),
                  patch('sys.argv', ['ci_platform.py', target['id'], '--cross-config', '/cross/config']),
                  patch.object(ci.ci_cross, 'load', return_value=config),
                  patch.object(ci.ci_cross, 'validate_build') as validate,
                  patch.object(ci.ci_cross, 'verify_doctest_runner') as doctest,
                  patch.object(ci.ci_cross, 'check_elf') as check_elf,
                  patch.object(ci.ci_cross, 'execution_identity', return_value=dict(os='QEMU Linux', libc=['glibc', '2.41'], machine='riscv64')),
                  patch.object(ci.ci_cross, 'run_target', side_effect=guest_run),
                  patch.object(ci.urllib.request, 'urlopen', return_value=io.BytesIO(json.dumps(release).encode())) as request):
                ci.main()
            validate.assert_called_once_with(config, target)
            doctest.assert_called_once_with(config)
            self.assertEqual(check_elf.call_count, 3)  # debug Vex, release Vex, public Wave
            self.assertEqual(request.call_args.args[0].get_header('Authorization'), 'Bearer host-api-secret')
            self.assertIn(('cargo', 'test', '--workspace', '--locked', '--target', target['rust_target']), [args for args, _ in native])
            self.assertIn(('cargo', 'test', '--workspace', '--locked', '--target', target['rust_target'], '--no-run'), [args for args, _ in native])
            self.assertTrue(any(args[:3] == (sys.executable, 'x.py', 'build') for args, _ in native))
            interop = next(env for args, env in native if len(args) > 1 and args[1] == 'tests/native_interop.py')
            self.assertEqual(Path(interop['CC']), Path('/cross/tools')/'cc')
            self.assertEqual(Path(interop['VEX_TEST_TARGET_RUNNER']), Path('/cross/tools')/'runner')
            self.assertTrue(any(command[1:3] == ('-m', 'unittest') for command, _ in guest))
            for script in ['tools/cli_reference.py', 'x.py', 'tests/wave_compatibility.py']:
                self.assertTrue(any(len(command) > 1 and command[1] == script for command, _ in guest))
            self.assertFalse(any(command[0] in ['cargo', 'rustc'] for command, _ in guest))
            report = json.loads((root/'dist/acceptance-linux-riscv64.json').read_text())
            self.assertEqual((report['build_host'], report['execution_arch']), ('x86_64-unknown-linux-gnu', 'riscv64'))

    def test_native_api_token_is_not_inherited_by_builds_or_smoke(self):
        calls = []
        def run(*args, env=None, capture=False):
            self.assertNotIn('GH_TOKEN', os.environ)
            self.assertNotIn('GITHUB_TOKEN', os.environ)
            calls.append((args, env))
            if len(args) > 1 and args[1] == 'setup':
                self.assertEqual(env['GH_TOKEN'], 'test-api-token')
                home = Path(env['VEX_TOOLCHAIN_HOME'])
                (home / 'current').write_text('wavec')
                (home / 'wavec').write_bytes(b'compiler fixture')
            elif env is not None:
                self.assertNotIn('GH_TOKEN', env)
                self.assertNotIn('GITHUB_TOKEN', env)
            return SimpleNamespace(stdout='host: x86_64-unknown-linux-gnu\n' if args[0] == 'rustc' else 'a' * 40)

        release = {'draft': False, 'tag_name': 'v' + ci.WAVE_VERSION, 'id': 1,
                   'assets': [{'id': 2, 'name': f'wave-v{ci.WAVE_VERSION}-x86_64-linux-gnu.tar.gz'}]}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'dist').mkdir()
            with (patch.dict(os.environ, GH_TOKEN='test-api-token', GITHUB_TOKEN='unused-token'),
                  patch.object(ci, 'ROOT', root), patch.object(ci, 'run', side_effect=run),
                  patch('sys.argv', ['ci_platform.py', 'linux-amd64']),
                  patch.object(ci.urllib.request, 'urlopen', return_value=io.BytesIO(json.dumps(release).encode())) as request):
                ci.main()
            self.assertEqual(request.call_args.args[0].get_header('Authorization'), 'Bearer test-api-token')
            self.assertEqual(sum(env is not None and 'GH_TOKEN' in env for _, env in calls), 1)
            self.assertTrue((root / 'dist/acceptance-linux-amd64.json').is_file())


if __name__ == '__main__':
    unittest.main()
