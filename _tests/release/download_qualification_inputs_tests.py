import hashlib
from pathlib import Path
import tempfile
import unittest

from download_qualification_inputs import transfer


class Client:
    def __init__(self, data):
        self.data = data
        self.calls = []

    def download_file(self, bucket, key, destination):
        self.calls.append((bucket, key))
        Path(destination).write_bytes(self.data)


class DownloadTests(unittest.TestCase):
    def test_resume_verifies_bytes_and_refuses_changed_existing_input(self):
        entry = {'member': 'media/game.bin', 'bytes': 4,
                 'sha256': hashlib.sha256(b'game').hexdigest()}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            client = Client(b'game')
            self.assertFalse(transfer(client, 'test', root, entry)['reused'])
            self.assertTrue(transfer(client, 'test', root, entry)['reused'])
            self.assertEqual(client.calls, [('test', 'inputs/media/game.bin')])
            (root / entry['member']).write_bytes(b'edit')
            with self.assertRaisesRegex(ValueError, 'existing input differs'):
                transfer(client, 'test', root, entry)

    def test_bad_download_is_not_published_and_escape_is_rejected(self):
        entry = {'member': 'media/game.bin', 'bytes': 4,
                 'sha256': hashlib.sha256(b'game').hexdigest()}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, 'download digest'):
                transfer(Client(b'bad!'), 'test', root, entry)
            self.assertEqual(list((root / 'media').iterdir()), [])
            with self.assertRaisesRegex(ValueError, 'unsafe payload path'):
                transfer(Client(b'game'), 'test', root, {**entry, 'member': '../game'})


if __name__ == '__main__':
    unittest.main()
