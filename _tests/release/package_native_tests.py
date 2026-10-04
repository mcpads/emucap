"""Native package boundaries: preserve relocatable links; reject path escape and ambiguity."""
import tempfile
from pathlib import Path
import unittest
from package_native import files_manifest, one, format_of, copy_library


class PackageBoundaries(unittest.TestCase):
    def test_internal_link_and_content_identity(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / 'library').write_bytes(b'first')
            (root / 'alias').symlink_to('library')
            before = files_manifest(root)
            self.assertEqual(before['alias'], {'symlink': 'library'})
            (root / 'library').write_bytes(b'second')
            self.assertNotEqual(before['library'], files_manifest(root)['library'])

    def test_external_link_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / 'payload'
            root.mkdir()
            (Path(d) / 'private').write_bytes(b'outside')
            (root / 'escape').symlink_to('../private')
            with self.assertRaisesRegex(ValueError, 'escaping package symlink'):
                files_manifest(root)

    def test_library_alias_resolves_one_binary(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            ((root / 'lib.so.1').resolve()).write_bytes(b'ELF')
            (root / 'lib.so').symlink_to('lib.so.1')
            self.assertEqual(one(root, 'lib.so*').resolve(), (root / 'lib.so.1').resolve())
            (root / 'lib.so.other').write_bytes(b'different')
            with self.assertRaisesRegex(ValueError, 'expected one'):
                one(root, 'lib.so*')

    def test_shared_dependency_reuses_original_identity_after_relocation(self):
        with tempfile.TemporaryDirectory() as d:
            source, target = Path(d) / 'source', Path(d) / 'target'
            source.write_bytes(b'original library')
            origins = {}
            self.assertTrue(copy_library(source, target, origins))
            target.write_bytes(b'relocated library')
            self.assertFalse(copy_library(source, target, origins))
            source.write_bytes(b'different library')
            with self.assertRaisesRegex(ValueError, 'dependency collision'):
                copy_library(source, target, origins)

    def test_file_type_uses_header(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / 'no-extension'
            path.write_bytes(b'\x7fELFpayload')
            self.assertEqual(format_of(path), 'elf')
            path.write_bytes(b'\xcf\xfa\xed\xfepayload')
            self.assertEqual(format_of(path), 'macho')
            path.write_bytes(b'ordinary resource')
            self.assertIsNone(format_of(path))


if __name__ == '__main__':
    unittest.main()
