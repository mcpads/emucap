from pathlib import Path
import tempfile
import unittest

from windows_native_payload import collect_imports, stage


class PayloadTests(unittest.TestCase):
    def test_transitive_imports_preserve_assets_and_exclude_system_libraries(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source, deps, system, payload = [root / name for name in ('source', 'deps', 'system', 'payload')]
            for path in (source, deps, system):
                path.mkdir()
            (source / 'emulator.exe').write_bytes(b'executable')
            (source / 'data').mkdir()
            (source / 'data/font.bin').write_bytes(b'font')
            (source / 'debug.pdb').write_bytes(b'debug')
            (deps / 'SDL2.dll').write_bytes(b'sdl')
            (deps / 'libwinpthread-1.dll').write_bytes(b'threads')
            (system / 'kernel32.dll').write_bytes(b'OS')
            graph = {'emulator.exe': ['SDL2.dll', 'KERNEL32.dll', 'api-ms-win-core.dll'],
                     'SDL2.dll': ['libwinpthread-1.dll'], 'libwinpthread-1.dll': []}
            sidecar = root / 'emucap-build.json'
            sidecar.write_text('{"identity":"pinned"}')
            stage(source / 'emulator.exe', payload, tree=True, includes=[sidecar])
            collect_imports(payload, [deps], [system], lambda p: graph[p.name])
            self.assertEqual({p.relative_to(payload).as_posix() for p in payload.rglob('*') if p.is_file()},
                             {'emulator.exe', 'data/font.bin', 'SDL2.dll', 'libwinpthread-1.dll', 'emucap-build.json'})
            self.assertEqual((payload / 'SDL2.dll').read_bytes(), b'sdl')
            with self.assertRaisesRegex(ValueError, 'missing or symlink runtime input'):
                stage(source / 'emulator.exe', root / 'missing-sidecar', includes=[root / 'absent.json'])
            (payload / 'libwinpthread-1.dll').unlink()
            (deps / 'libwinpthread-1.dll').unlink()
            with self.assertRaisesRegex(ValueError, 'unresolved import'):
                collect_imports(payload, [deps], [system], lambda p: graph[p.name])


if __name__ == '__main__':
    unittest.main()
