# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
import base64
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
from tools import ci_github_api as api


def encoded(status, body):
    return dict(status=status, body=base64.b64encode(body).decode())


class CompilerApiTransportTests(unittest.TestCase):
    def test_host_authenticates_without_persisting_credentials_and_preserves_404(self):
        url = api.API + 'attestations/sha256:' + 'a'*64
        failure = urllib.error.HTTPError(url, 404, 'Not Found', {}, io.BytesIO(b'{"message":"Not Found"}'))
        with patch.object(api.urllib.request, 'urlopen', side_effect=failure) as request:
            result = api.query(url, 'host-secret')
        self.assertEqual(request.call_args.args[0].get_header('Authorization'), 'Bearer host-secret')
        self.assertEqual(result, encoded(404, b'{"message":"Not Found"}'))
        self.assertNotIn('host-secret', json.dumps(result))
        for status in [401, 403, 429, 500]:
            failure = urllib.error.HTTPError(url, status, 'Denied', {}, io.BytesIO(b'denied'))
            with patch.object(api.urllib.request, 'urlopen', side_effect=failure):
                with self.assertRaisesRegex(RuntimeError, f'HTTP {status}'):
                    api.query(url, 'host-secret')
        with patch.object(api.urllib.request, 'urlopen', side_effect=OSError('connection closed')):
            with self.assertRaises(OSError): api.query(url, 'host-secret')

    def test_snapshots_are_limited_to_pinned_release_digest_and_required_commit(self):
        release = b'{"tag_name":"v0.2.1-pre-beta","draft":false}'
        for response, commit_expected in [(encoded(404, b'{}'), False),
                                          (encoded(200, b'{"attestations":[]}'), False),
                                          (encoded(200, b'{"attestations":[{"bundle":{}}]}'), True)]:
            replies = [response, encoded(200, b'{"sha":"source"}')]
            with patch.object(api, 'query', side_effect=replies) as query:
                records = api.snapshots('0.2.1-pre-beta', 'a'*64, release, 'host-secret')
            self.assertEqual(len(records), 3 if commit_expected else 2)
            self.assertEqual(query.call_args_list[0].args, (api.API+'attestations/sha256:'+'a'*64, 'host-secret'))
            self.assertEqual(records[api.API+'releases/tags/v0.2.1-pre-beta'], encoded(200, release))
            if commit_expected:
                self.assertEqual(query.call_args_list[1].args, (api.API+'commits/v0.2.1-pre-beta', 'host-secret'))
        with self.assertRaises(ValueError): api.snapshots('../other', 'a'*64, release, None)
        with self.assertRaises(ValueError): api.snapshots('0.2.1-pre-beta', '../other', release, None)

    def test_shim_replays_exact_public_status_and_rejects_unknown_api_requests(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)/'transport space'
            records = {api.API+'release': encoded(200, b'{"release":true}'),
                       api.API+'attestations': encoded(404, b'{"message":"Not Found"}')}
            api.write_curl(directory, records)
            command = ([str(directory/'curl')] if sys.platform.startswith('linux') else
                       [sys.executable, api.__file__, 'curl', str(directory/'responses.json')])
            for endpoint, response in records.items():
                result = subprocess.run([*command, '--write-out', '\n%{http_code}', endpoint], capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, base64.b64decode(response['body']) + b'\n' + str(response['status']).encode())
            result = subprocess.run([*command, '--write-out', '\n%{http_code}', api.API+'unexpected'], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'no anonymous fallback', result.stderr)

    def test_archive_curl_retains_https_arguments_and_receives_no_authentication(self):
        arguments = ['--proto', '=https', '--proto-redir', '=https', '--output', '/shared/archive',
                     'https://github.com/wavefnd/Wave/releases/download/v0.2.1-pre-beta/archive.tar.gz']
        with patch.dict(os.environ, GH_TOKEN='secret', GITHUB_TOKEN='secret'):
            with patch.object(api.os, 'execv') as execute:
                api.curl('/not-read-for-archive', arguments)
                self.assertNotIn('GH_TOKEN', os.environ)
                self.assertNotIn('GITHUB_TOKEN', os.environ)
            execute.assert_called_once_with('/usr/bin/curl', ['/usr/bin/curl', *arguments])


if __name__ == '__main__':
    unittest.main()
