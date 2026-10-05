#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Install an exact, provenance-verified Vex release without administrator access."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
REPO = 'wavefnd/Vex'


def download(url, output=None):
    if not url.startswith(('https://api.github.com/', 'https://github.com/wavefnd/Vex/')):
        raise ValueError('unexpected release URL')
    request = urllib.request.Request(url, headers={'User-Agent': 'Vex-verified-installer'})
    with urllib.request.urlopen(request, timeout=60) as response:
        if response.url.split(':', 1)[0] != 'https':
            raise ValueError('non-HTTPS download redirect')
        if output is None:
            return response.read(8 * 1024 * 1024 + 1)
        count = 0
        with output.open('xb') as target:
            while chunk := response.read(65536):
                count += len(chunk)
                if count > 512 * 1024 * 1024:
                    raise ValueError('release archive exceeds 512 MiB')
                target.write(chunk)


def host():
    os_name = {'Linux': 'linux', 'Darwin': 'macos', 'Windows': 'windows', 'FreeBSD': 'freebsd'}.get(platform.system())
    arch = {'AMD64': 'x86_64', 'arm64': 'aarch64', 'ARM64': 'aarch64'}.get(platform.machine(), platform.machine())
    rows = json.loads((ROOT / 'platforms.json').read_text())['platforms']
    found = [row for row in rows if row['os'] == os_name and row['arch'] == arch]
    if len(found) != 1:
        raise ValueError(f'unsupported host {os_name}/{arch}')
    return found[0]


def payload(archive, root, executable):
    member = f'{root}/{executable}'
    if archive.suffix == '.zip':
        with zipfile.ZipFile(archive) as package:
            matches = [item for item in package.infolist() if item.filename == member]
            if len(matches) != 1 or matches[0].is_dir() or stat.S_ISLNK(matches[0].external_attr >> 16):
                raise ValueError('archive lacks one regular Vex executable')
            if matches[0].file_size > 256 * 1024 * 1024:
                raise ValueError('executable exceeds size limit')
            return package.read(matches[0])
    with tarfile.open(archive) as package:
        matches = [item for item in package.getmembers() if item.name == member]
        if len(matches) != 1 or not matches[0].isfile() or matches[0].size > 256 * 1024 * 1024:
            raise ValueError('archive lacks one bounded regular Vex executable')
        with package.extractfile(matches[0]) as source:
            return source.read()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True, help='Exact version without v, e.g. 0.0.2-beta')
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--replace', action='store_true', help='Atomically replace an existing Vex executable')
    args = parser.parse_args()
    sys.path.insert(0, str(ROOT))
    from tools.release_version import valid_version
    if not valid_version(args.version):
        raise ValueError('invalid release version')
    target = host()
    if shutil.which('gh') is None:
        raise ValueError('GitHub CLI (gh) is required to verify release provenance')
    tag = 'v' + args.version
    release = json.loads(download(f'https://api.github.com/repos/{REPO}/releases/tags/{tag}'))
    if release.get('draft') is not False or release.get('tag_name') != tag:
        raise ValueError('release is draft or mismatched')
    root = f"vex-{tag}-{target['rust_target']}"
    archive_name = root + '.' + target['archive']
    assets = release['assets']
    def asset(name):
        found = [a for a in assets if a['name'] == name]
        expected = f'https://github.com/{REPO}/releases/download/{tag}/{name}'
        if len(found) != 1 or found[0]['browser_download_url'] != expected:
            raise ValueError(f'missing or ambiguous official release asset: {name}')
        return expected
    sums = download(asset('SHA256SUMS')).decode()
    matches = [line.split()[0] for line in sums.splitlines() if len(line.split()) == 2 and line.split()[1].lstrip('*') == archive_name]
    if len(matches) != 1 or len(matches[0]) != 64:
        raise ValueError('missing or ambiguous release checksum')
    expected = matches[0].lower()
    executable = 'vex.exe' if target['os'] == 'windows' else 'vex'
    prefix = args.prefix.expanduser().resolve()
    destination = prefix / 'bin' / executable
    if destination.parent.is_symlink():
        raise ValueError("installation bin directory must not be a symlink")
    if destination.is_symlink() or (destination.exists() and not args.replace):
        raise ValueError(f'{destination} already exists; use --replace for a regular Vex executable')
    with tempfile.TemporaryDirectory(prefix='vex-install-') as temporary:
        archive = Path(temporary) / archive_name
        download(asset(archive_name), archive)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != expected:
            raise ValueError('release archive checksum mismatch')
        # GitHub attestation includes the immutable source commit. Verify the
        # exact tag's commit, rather than trusting release target_commitish.
        ref = json.loads(download(f'https://api.github.com/repos/{REPO}/git/ref/tags/{tag}'))['object']
        for _ in range(8):
            if ref['type'] == 'commit':
                break
            if ref['type'] != 'tag':
                raise ValueError('release tag does not resolve to a commit')
            ref = json.loads(download(f'https://api.github.com/repos/{REPO}/git/tags/{ref["sha"]}'))['object']
        if ref['type'] != 'commit':
            raise ValueError('release tag nesting limit exceeded')
        attestations = json.loads(download(f'https://api.github.com/repos/{REPO}/attestations/sha256:{digest}')).get('attestations')
        if not isinstance(attestations, list) or not attestations:
            raise ValueError('release provenance is missing')
        bundle = Path(temporary)/'attestations.jsonl'
        with bundle.open('x') as output:
            for attestation in attestations:
                if not isinstance(attestation.get('bundle'), dict):
                    raise ValueError('invalid release provenance bundle')
                output.write(json.dumps(attestation['bundle'])+'\n')
        subprocess.run(['gh', 'attestation', 'verify', str(archive), '--bundle', str(bundle), '--repo', REPO,
                        '--signer-workflow', REPO+'/.github/workflows/release.yml',
                        '--source-ref', 'refs/heads/master', '--source-digest', ref['sha'],
                        '--signer-digest', ref['sha'], '--deny-self-hosted-runners'], check=True)
        data = payload(archive, root, executable)
        candidate = Path(temporary) / executable
        candidate.write_bytes(data)
        candidate.chmod(0o755)
        actual = subprocess.check_output([str(candidate), '--version'], text=True).strip()
        if actual != 'vex '+args.version:
            raise ValueError('binary version does not match requested release')
        destination.parent.mkdir(parents=True, exist_ok=True)
        # Keep the previous installation until verification and syncing succeed.
        fd, staged = tempfile.mkstemp(prefix='.vex-install-', dir=destination.parent)
        try:
            with os.fdopen(fd, 'wb') as output:
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
            os.chmod(staged, 0o755)
            if args.replace:
                os.replace(staged, destination)
            else:
                os.link(staged, destination)  # exclusive first installation
        finally:
            if os.path.exists(staged):
                try:
                    os.unlink(staged)
                except OSError as error:
                    print(f'warning: temporary cleanup failed: {error}', file=sys.stderr)
    print(f'Installed {destination}')
    print(f'Add {destination.parent} to PATH. Upgrade with --replace; remove {destination} to uninstall.')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        sys.exit(f'error: {error}')
