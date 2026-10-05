# SPDX-License-Identifier: MPL-2.0
import io
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import subprocess
from tools import repository_checks, install


class RepositoryContracts(unittest.TestCase):
    def test_every_host_is_required_and_actions_are_immutable(self):
        self.assertEqual(len(repository_checks.validate()), 9)

    def test_acceptance_rejects_missing_failed_and_wrong_commit_evidence(self):
        row = dict(id='fixture', rust_target='fixture-target', executor='native')
        pin = dict(version='0.2.1-pre-beta', archives={'fixture': 'd'*64})
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            with self.assertRaises(FileNotFoundError):
                repository_checks.acceptance(directory, 'a'*40, [row], pin)
            report = dict(schema_version=1, executor='native', os='fixture-os', wave_archive_digest='sha256:'+'d'*64, status='passed', source='a'*40, platform='fixture', rust_target='fixture-target',
                          wave_version='0.2.1-pre-beta', compiler_sha256='b'*64)
            path = directory/'acceptance-fixture.json'
            path.write_text(json.dumps(report))
            repository_checks.acceptance(directory, 'a'*40, [row], pin)
            for field, value in [('source', 'c'*40), ('status', 'skipped'), ('compiler_sha256', 'unknown'), ('wave_archive_digest', 'sha256:'+'e'*64), ('executor', 'cross-build')]:
                path.write_text(json.dumps(dict(report, **{field: value})))
                with self.assertRaises(ValueError):
                    repository_checks.acceptance(directory, 'a'*40, [row], pin)

    def test_installer_reads_only_one_regular_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory)/'fixture.tar.gz'
            for kind in ['regular', 'duplicate', 'symlink', 'hardlink', 'missing']:
                with tarfile.open(archive, 'w:gz') as package:
                    entry = tarfile.TarInfo('root/vex' if kind != 'missing' else '../vex')
                    if kind in ['symlink', 'hardlink']:
                        entry.type = tarfile.SYMTYPE if kind == 'symlink' else tarfile.LNKTYPE
                        entry.linkname = '/etc/passwd'
                        package.addfile(entry)
                    else:
                        entry.size = 3
                        package.addfile(entry, io.BytesIO(b'vex'))
                        if kind == 'duplicate': package.addfile(entry, io.BytesIO(b'vex'))
                if kind == 'regular': self.assertEqual(install.payload(archive, 'root', 'vex'), b'vex')
                else:
                    with self.assertRaises(ValueError): install.payload(archive, 'root', 'vex')

    def test_installer_verification_failures_preserve_existing_binary(self):
        from contextlib import ExitStack
        import hashlib
        target = dict(os='linux', rust_target='fixture-target', archive='tar.gz')
        root = 'vex-v0.0.2-beta-fixture-target'
        archive_name = root+'.tar.gz'
        with tempfile.TemporaryDirectory() as temporary:
            temporary = Path(temporary)
            archive = temporary/archive_name
            with tarfile.open(archive, 'w:gz') as package:
                entry = tarfile.TarInfo(root+'/vex')
                entry.size = 3
                package.addfile(entry, io.BytesIO(b'vex'))
            data = archive.read_bytes()
            digest = hashlib.sha256(data).hexdigest()
            prefix = temporary/'prefix'
            (prefix/'bin').mkdir(parents=True)
            destination = prefix/'bin/vex'
            destination.write_bytes(b'previous release')
            release = dict(draft=False, tag_name='v0.0.2-beta', assets=[
                dict(name=name, browser_download_url='https://github.com/wavefnd/Vex/releases/download/v0.0.2-beta/'+name)
                for name in [archive_name, 'SHA256SUMS']])
            for failure in ['checksum', 'provenance', 'version', 'replace', 'success']:
                def download(url, output=None):
                    if output:
                        output.write_bytes(data)
                        return
                    if url.endswith('/SHA256SUMS'):
                        return ((('0'*64 if failure == 'checksum' else digest)+'  '+archive_name+'\n').encode())
                    if '/attestations/' in url:
                        return json.dumps(dict(attestations=[dict(bundle={})])).encode()
                    if '/git/ref/' in url:
                        return json.dumps(dict(object=dict(type='commit', sha='a'*40))).encode()
                    return json.dumps(release).encode()
                with ExitStack() as stack:
                    stack.enter_context(patch('sys.argv', ['install.py', '--version', '0.0.2-beta', '--prefix', str(prefix), '--replace']))
                    stack.enter_context(patch.object(install, 'host', return_value=target))
                    stack.enter_context(patch.object(install.shutil, 'which', return_value='/fixture/gh'))
                    stack.enter_context(patch.object(install, 'download', side_effect=download))
                    attest = stack.enter_context(patch.object(install.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'gh') if failure == 'provenance' else None))
                    stack.enter_context(patch.object(install.subprocess, 'check_output', return_value='vex '+('0.0.1' if failure == 'version' else '0.0.2-beta')))
                    if failure == 'replace': stack.enter_context(patch.object(install.os, 'replace', side_effect=OSError('replace failed')))
                    if failure == 'success':
                        install.main()
                        self.assertEqual(destination.read_bytes(), b'vex')
                        self.assertIn('--source-digest', attest.call_args[0][0])
                    else:
                        with self.assertRaises((ValueError, OSError, subprocess.CalledProcessError)): install.main()
                        self.assertEqual(destination.read_bytes(), b'previous release')
                    self.assertEqual(list(destination.parent.glob('.vex-install-*')), [])
