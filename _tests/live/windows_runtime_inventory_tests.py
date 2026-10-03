import json
from pathlib import Path
import tempfile
import unittest

from qualification_batch import sha
from windows_runtime_inventory import freeze


class InventoryTests(unittest.TestCase):
    def test_freeze_checks_media_firmware_hosts_and_profile_bindings(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in ('media/game/test.rom', 'firmware/test/bios.bin',
                         'native/test/emulator.exe', 'repo/target/release/emucap-mcp.exe',
                         'repo/target/release/emucap-bridge.exe'):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
            (root / 'repo/Cargo.toml').write_text(
                '[[bin]]\nname="emucap-mcp"\n[[bin]]\nname="emucap-bridge"\n')
            def entry(name):
                path = root / name
                return {'member': path.name, 'bytes': path.stat().st_size, 'sha256': sha(path)}
            media = {'rows': [{'id': 'game', 'adapter': 'test', 'content': 'test.rom',
                               'status': 'media_ready', 'files': [entry('media/game/test.rom')]}]}
            firmware = {'files': [dict(entry('firmware/test/bios.bin'), group='test')]}
            layout = {'core_directory': 'repo/target/release', 'native_directory': 'native',
                      'adapter_env': {'test': {'TEST_BIN': 'native/test/emulator.exe'}}}
            profile = {'launch_plan': {'content_path': str(root / 'media/game/test.rom')},
                       'env': {'TEST_BIN': str(root / 'native/test/emulator.exe')}}
            (root / 'game.json').write_text(json.dumps(profile))
            draft = {'rows': [{'id': 'game', 'profile': 'game.json', 'checks': ['observation_speed']}]}
            result = freeze(root, layout, media, firmware, draft)
            self.assertEqual(len(result['artifacts']), 6)
            (root / 'repo/target/release/emucap-bridge.exe').unlink()
            with self.assertRaisesRegex(ValueError, 'missing Windows host'):
                freeze(root, layout, media, firmware, draft)
            (root / 'media/game/test.rom').write_bytes(b'wrong')
            with self.assertRaisesRegex(ValueError, 'deployed bytes differ'):
                freeze(root, layout, media, firmware, draft)


if __name__ == '__main__':
    unittest.main()
