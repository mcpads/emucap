"""A stale or modified native payload must not become a release archive."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from package_windows import verify_payload, matching_source
from package_native import source_archive, sha
from native_build import payload_hashes


class WindowsPackageTests(unittest.TestCase):
    def fixture(self, root):
        source, payload = root / 'source', root / 'payload'
        source.mkdir(); payload.mkdir()
        subprocess.run(['git', 'init', '-q', str(source)], check=True)
        subprocess.run(['git', '-C', str(source), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        'commit', '-qm', 'fixture', '--allow-empty'], check=True)
        revision = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
        (payload / 'game.exe').write_bytes(b'native executable')
        record = dict(revision=revision, adapter='example', executable='game.exe', files=payload_hashes(payload))
        (payload / 'NATIVE-BUILD.json').write_text(json.dumps(record))
        return source, payload, record

    def test_modified_missing_and_wrong_producer_payloads_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            source, payload, record = self.fixture(Path(d))
            self.assertEqual(verify_payload(payload, source, 'example'), 'game.exe')
            (payload / 'game.exe').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'manifest mismatch'): verify_payload(payload, source, 'example')
            (payload / 'game.exe').write_bytes(b'native executable')
            record['revision'] = 'wrong'
            (payload / 'NATIVE-BUILD.json').write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, 'revision mismatch'): verify_payload(payload, source, 'example')
            (payload / 'NATIVE-BUILD.json').unlink()
            with self.assertRaisesRegex(ValueError, 'lacks build evidence'): verify_payload(payload, source, 'example')

    def test_executable_cannot_escape_verified_payload(self):
        with tempfile.TemporaryDirectory() as d:
            source, payload, record = self.fixture(Path(d))
            (Path(d) / 'outside.exe').write_bytes(b'outside')
            record['executable'] = '../outside.exe'
            (payload / 'NATIVE-BUILD.json').write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, 'escapes payload'): verify_payload(payload, source, 'example')

    def test_shared_mame_source_authority(self):
        self.check_source('mame-neogeo', 'mame-pc98')

    def test_source_digest_and_patch_identity_required(self):
        self.check_source('example', 'example')

    def check_source(self, adapter, owner):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            source, _, _ = self.fixture(root)
            lock = source / f'adapters/{owner}/upstream.lock'
            lock.parent.mkdir(parents=True)
            lock.write_text('COMMIT=pinned\n')
            subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
            upstream, output = root / 'upstream', root / 'output'
            upstream.mkdir(); output.mkdir()
            (upstream / 'source.c').write_text('source')
            archive = source_archive(source, upstream, adapter, output, 'emucap-example')
            record = dict(adapter=adapter, artifacts={archive.name: sha(archive)})
            (output / 'emucap-example.json').write_text(json.dumps(record))
            self.assertEqual(matching_source(output, source, adapter)[0], archive)
            lock.write_text('COMMIT=other\n')
            with self.assertRaisesRegex(ValueError, 'source input mismatch'): matching_source(output, source, adapter)
            archive.write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'archive digest mismatch'): matching_source(output, source, adapter)


if __name__ == '__main__': unittest.main()
