#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Validate only contribution commits in base..head; never modify Git or GitHub."""
import argparse
from email.utils import parseaddr
import json
import os
import re
import subprocess
import urllib.request


def git(*args, cwd=None, input=None):
    return subprocess.check_output(['git', *args], cwd=cwd, input=input, text=True, encoding='utf-8')


def github_merge(sha):
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    token = os.environ.get('GITHUB_TOKEN')
    if not token or not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        return False
    request = urllib.request.Request(f'https://api.github.com/repos/{repository}/commits/{sha}',
        headers={'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json', 'User-Agent': 'Vex-DCO-check'})
    with urllib.request.urlopen(request, timeout=30) as response:
        value = json.load(response)
    return (value.get('sha') == sha and len(value.get('parents', [])) > 1
        and (value.get('committer') or {}).get('login') == 'web-flow'
        and value.get('commit', {}).get('verification', {}).get('verified') is True)


def has_signoff(name, email, message, cwd=None):
    trailers = git('interpret-trailers', '--parse', cwd=cwd, input=message)
    for line in trailers.splitlines():
        key, separator, value = line.partition(':')
        if separator and key.lower() == 'signed-off-by':
            signed_name, signed_email = parseaddr(value.strip())
            if signed_name == name and signed_email.casefold() == email.casefold():
                return True
    return False


def validate(base, head, cwd=None, verify_merge=github_merge):
    for value in [base, head]:
        if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', value):
            raise ValueError('DCO requires exact base/head commit IDs')
        git('cat-file', '-e', value + '^{commit}', cwd=cwd)
    commits = git('rev-list', '--reverse', f'{base}..{head}', cwd=cwd).splitlines()
    failures = []
    for sha in commits:
        raw = git('show', '-s', '--format=%P%x00%an%x00%ae%x00%cn%x00%ce%x00%B', sha, cwd=cwd)
        parents, name, email, committer, committer_email, message = raw.split('\0', 5)
        if (len(parents.split()) > 1 and committer == 'GitHub'
                and committer_email == 'noreply@github.com' and verify_merge(sha)):
            continue
        # Bots and ordinary merges follow the same author sign-off contract.
        if not has_signoff(name, email, message, cwd=cwd):
            failures.append(f'{sha}: missing author Signed-off-by: {name!r} <{email}>')
    if failures:
        raise ValueError('\n'.join(failures) + '\nAdd your own certification with git commit --amend -s, or rebase the affected contribution commits with sign-off; then push your contribution branch.')
    return len(commits)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--head', required=True)
    args = parser.parse_args()
    print(f'Verified DCO for {validate(args.base, args.head)} contribution commits.')
