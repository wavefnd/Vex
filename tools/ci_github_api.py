#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Keep authenticated compiler API queries on the cross-build host."""
import base64
import json
import os
from pathlib import Path
import sys
import urllib.error
import urllib.request

API = 'https://api.github.com/repos/wavefnd/Wave/'
LIMIT = 8 * 1024 * 1024


def query(url, token):
    """Return a public API response, preserving an authoritative 404."""
    if not url.startswith(API):
        raise ValueError('unexpected compiler API endpoint')
    headers = {'User-Agent': 'Vex-release-acceptance'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            status, body = response.status, response.read(LIMIT + 1)
    except urllib.error.HTTPError as error:
        try:
            if error.code != 404:
                raise RuntimeError(f'compiler API query failed with HTTP {error.code}') from None
            status, body = error.code, error.read(LIMIT + 1)
        finally:
            error.close()
    if status not in (200, 404):
        raise RuntimeError(f'compiler API query failed with HTTP {status}')
    if len(body) > LIMIT:
        raise RuntimeError('compiler API response exceeds 8 MiB')
    return {'status': status, 'body': base64.b64encode(body).decode('ascii')}


def snapshots(version, digest, release_body, token):
    """Fetch only the pinned installer's possible public API inputs."""
    if (not version or any(c not in '0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-' for c in version)
            or len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest)):
        raise ValueError('invalid pinned compiler identity')
    release_url = API + 'releases/tags/v' + version
    attestations_url = API + 'attestations/sha256:' + digest
    responses = {release_url: {'status': 200, 'body': base64.b64encode(release_body).decode('ascii')}}
    response = query(attestations_url, token)
    responses[attestations_url] = response
    if response['status'] == 200:
        document = json.loads(base64.b64decode(response['body']))
        if not isinstance(document.get('attestations'), list):
            raise ValueError('invalid provenance response')
        if document['attestations']:
            commit_url = API + 'commits/v' + version
            commit = query(commit_url, token)
            if commit['status'] != 200:
                raise RuntimeError('release tag source commit is unavailable')
            responses[commit_url] = commit
    return responses


def write_curl(directory, responses):
    directory = Path(directory)
    directory.mkdir()
    record = directory / 'responses.json'
    record.write_text(json.dumps(responses), encoding='utf-8')
    executable = directory / 'curl'
    script = str(Path(__file__).resolve())
    executable.write_text('#!/usr/bin/python3\nimport os, sys\n'
                          'os.execv("/usr/bin/python3", ["/usr/bin/python3", '
                          + repr(script) + ', "curl", ' + repr(str(record)) + '] + sys.argv[1:])\n', encoding='utf-8')
    executable.chmod(0o755)
    return str(directory) + ':/usr/local/bin:/usr/bin:/bin'


def curl(record, arguments):
    # The snapshot contains public response bodies only. No credential is needed
    # by the guest, archive downloader, verifier, compiler or user program.
    os.environ.pop('GH_TOKEN', None)
    os.environ.pop('GITHUB_TOKEN', None)
    urls = [arg for arg in arguments if arg.startswith(('http://', 'https://'))]
    api_urls = [url for url in urls if url.startswith('https://api.github.com/')]
    if api_urls:
        responses = json.loads(Path(record).read_text(encoding='utf-8'))
        if len(urls) != 1 or api_urls[0] not in responses:
            raise ValueError('unexpected compiler API request; no anonymous fallback is permitted')
        if '--output' in arguments or '--write-out' not in arguments:
            raise ValueError('unexpected compiler API output mode')
        index = arguments.index('--write-out')
        if arguments[index + 1:index + 2] != ['\n%{http_code}']:
            raise ValueError('unexpected compiler API output mode')
        response = responses[api_urls[0]]
        if response['status'] not in (200, 404):
            raise ValueError('invalid public compiler API snapshot status')
        sys.stdout.buffer.write(base64.b64decode(response['body'], validate=True))
        sys.stdout.buffer.write(('\n' + str(response['status'])).encode('ascii'))
        return
    # Archive/checksum transfers retain their original HTTPS, size and redirect
    # restrictions and execute the real target curl in the target userspace.
    os.execv('/usr/bin/curl', ['/usr/bin/curl', *arguments])


if __name__ == '__main__':
    try:
        if len(sys.argv) < 4 or sys.argv[1] != 'curl':
            raise ValueError('expected curl snapshot invocation')
        curl(sys.argv[2], sys.argv[3:])
    except (ValueError, RuntimeError, OSError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
