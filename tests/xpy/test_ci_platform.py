# SPDX-License-Identifier: MPL-2.0
import importlib.util
import io
import json
import os
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
