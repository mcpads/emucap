import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('native_build', Path(__file__).with_name('native_build.py'))
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class NativeBuildTests(unittest.TestCase):
    def test_native_changes_invalidate_but_core_and_documentation_do_not(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(['git', 'init', '-q', tmp], check=True)
            def write(name, value):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(value)
            write('adapters/ppsspp/upstream.lock', 'pinned source')
            write('adapters/ppsspp/README.md', 'first docs')
            write('adapters/_common/build-lock.sh', 'MSYS2 helper')
            write('Cargo.toml', 'version = "1.0"')
            subprocess.run(['git', '-C', tmp, 'add', '.'], check=True)
            first = build.identity(root, 'ppsspp', 'compiler-a')[0]
            write('Cargo.toml', 'version = "2.0"')
            write('adapters/ppsspp/README.md', 'second docs')
            write('adapters/_common/build-lock.sh', 'different MSYS2 helper')
            self.assertEqual(first, build.identity(root, 'ppsspp', 'compiler-a')[0])
            self.assertNotEqual(first, build.identity(root, 'ppsspp', 'compiler-b')[0])
            write('adapters/ppsspp/upstream.lock', 'different source')
            self.assertNotEqual(first, build.identity(root, 'ppsspp', 'compiler-a')[0])

    def test_reuse_rejects_changed_missing_extra_and_wrong_identity(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / 'native.exe'
            binary.write_bytes(b'original')
            (root / 'NATIVE-BUILD.json').write_text(json.dumps({
                'identity': 'expected', 'files': build.payload_hashes(root)}))
            build.verify(root, 'expected')
            with self.assertRaises(ValueError):
                build.verify(root, 'different')
            binary.write_bytes(b'changed')
            with self.assertRaises(ValueError):
                build.verify(root, 'expected')
            binary.unlink()
            with self.assertRaises(ValueError):
                build.verify(root, 'expected')
            binary.write_bytes(b'original')
            (root / 'unexpected.dll').write_bytes(b'extra')
            with self.assertRaises(ValueError):
                build.verify(root, 'expected')


    def test_stage_retains_runtime_dependencies_and_rejects_missing_executable(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            previous = build.ROOT
            build.ROOT = root
            try:
                source = root / 'adapters/ppsspp' / build.OUTPUTS['ppsspp'][0]
                source.mkdir(parents=True)
                with self.assertRaises(ValueError):
                    build.stage('ppsspp', root / 'missing', 'identity', {})
                (source / 'PPSSPPHeadless.exe').write_bytes(b'executable')
                (source / 'runtime.dll').write_bytes(b'dependency')
                (source / 'debug.pdb').write_bytes(b'debug symbols')
                (source / 'assets').mkdir()
                (source / 'assets/font.bin').write_bytes(b'font')
                (source / 'assets/.nojekyll').write_bytes(b'')
                (source / 'assets/.git').write_text('gitdir: private build location')
                build.stage('ppsspp', root / 'payload', 'identity', {})
                record = build.verify(root / 'payload', 'identity')
                self.assertEqual(set(record['files']), {
                    'PPSSPPHeadless.exe', 'runtime.dll', 'assets/font.bin', 'assets/.nojekyll'})
                (root / 'payload/assets/NATIVE-BUILD.json').write_text('{}')
                with self.assertRaises(ValueError):
                    build.verify(root / 'payload', 'identity')
            finally:
                build.ROOT = previous


if __name__ == '__main__':
    unittest.main()
