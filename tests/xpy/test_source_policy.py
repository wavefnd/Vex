# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT/'tools'/f'{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module
policy = load('source_policy')
dco = load('check_dco')

class SourcePolicyTests(unittest.TestCase):
    def test_headers_and_line_endings(self):
        text = ('\n'.join('# '+line for line in policy.NOTICE.splitlines())+'\n# '+policy.SPDX+'\n').encode()
        policy.check_file(Path('tools/sample.py'), text)
        with self.assertRaises(ValueError): policy.check_file(Path('tools/sample.py'), b'print(1)\n')
        with self.assertRaises(ValueError): policy.check_file(Path('tools/sample.py'), text.replace(b'\n',b'\r\n'))
        policy.check_file(Path('tests/fixtures/vex.ws'), b'{format=4}\n')
        self.assertFalse(policy.needs_header(Path('docs/completions/vex.bash')))
        self.assertFalse(policy.needs_header(Path('Cargo.lock')))
        policy.check_file(Path('image.png'), b'\r\n\0')

    def test_dco_contribution_range_and_no_bot_or_merge_exemption(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(['git','init','--quiet',str(root)],check=True)
            def git(*args): return subprocess.check_output(['git','-c','user.name=Author','-c','user.email=author@example.invalid',*args],cwd=root,text=True).strip()
            def commit(message):
                git('commit','--quiet','--allow-empty','-m',message)
                return git('rev-parse','HEAD')
            base = commit('old unsigned base')
            signed = commit('change\n\nSigned-off-by: Author <author@example.invalid>')
            self.assertEqual(dco.validate(base,signed,cwd=root),1)
            unsigned = commit('bot update\n\nSigned-off-by: Someone Else <other@example.invalid>')
            with self.assertRaisesRegex(ValueError,'amend -s'): dco.validate(base,unsigned,cwd=root)
            self.assertFalse(dco.has_signoff('bot[bot]','bot@example.invalid','no certification',cwd=root))
            self.assertFalse(dco.has_signoff('Author','author@example.invalid','Signed-off-by: Author <author@example.invalid>\n\nThis is not a trailer',cwd=root))
            git('reset','--hard',signed)
            git('branch','side')
            main = commit('main change\n\nSigned-off-by: Author <author@example.invalid>')
            git('checkout','--quiet','side')
            side = commit('side change\n\nSigned-off-by: Author <author@example.invalid>')
            git('checkout','--quiet','--detach',main)
            subprocess.run(['git','-c','user.name=Author','-c','user.email=author@example.invalid','merge','--no-ff','--quiet',side,'-m','GitHub merge'],cwd=root,check=True,env={**os.environ,'GIT_COMMITTER_NAME':'GitHub','GIT_COMMITTER_EMAIL':'noreply@github.com'})
            merge = git('rev-parse','HEAD')
            with self.assertRaises(ValueError): dco.validate(signed,merge,cwd=root,verify_merge=lambda _:False)
            self.assertEqual(dco.validate(signed,merge,cwd=root,verify_merge=lambda _:True),3)
            with self.assertRaises(ValueError): dco.validate('master',signed,cwd=root)

if __name__ == '__main__': unittest.main()
