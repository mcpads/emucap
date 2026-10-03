import io
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

from deploy_windows_payloads import deploy, sha


class DeploymentTests(unittest.TestCase):
    def test_resume_preserves_files_and_rejects_changed_download_or_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            archive = base / 'runtime.zip'
            nested = io.BytesIO()
            with zipfile.ZipFile(nested, 'w') as zipped:
                zipped.writestr('bundle/target/release/emucap.exe', b'producer')
                zipped.writestr('bundle/README.md', b'not selected')
            with zipfile.ZipFile(archive, 'w') as zipped:
                zipped.writestr('core.zip', nested.getvalue())
            plan = base / 'plan.json'
            plan.write_text(json.dumps({'archives': [dict(path='runtime.zip', sha256=sha(archive),
                destination='repo/target/release', inner_zip='core.zip', prefix='bundle/target/release')]}))
            root = base / 'deployed'
            deploy(root, plan)
            deploy(root, plan)
            self.assertFalse((root / 'repo/target/release/README.md').exists())
            executable = root / 'repo/target/release/emucap.exe'
            executable.write_bytes(b'other build')
            with self.assertRaisesRegex(ValueError, 'existing payload differs'):
                deploy(root, plan)
            self.assertEqual(executable.read_bytes(), b'other build')
            archive.write_bytes(b'partial download')
            with self.assertRaisesRegex(ValueError, 'archive digest mismatch'):
                deploy(root, plan)

    def test_archive_cannot_write_outside_owned_root(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            archive = base / 'runtime.zip'
            with zipfile.ZipFile(archive, 'w') as zipped:
                zipped.writestr('../escape.exe', b'bad')
            plan = base / 'plan.json'
            plan.write_text(json.dumps({'archives': [dict(path='runtime.zip', sha256=sha(archive),
                                                         destination='native/test')]}))
            with self.assertRaisesRegex(ValueError, 'unsafe payload path'):
                deploy(base / 'deployed', plan)
            self.assertFalse((base / 'escape.exe').exists())


if __name__ == '__main__':
    unittest.main()
