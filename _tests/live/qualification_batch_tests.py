import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import qualification_batch as batch


class BatchTests(unittest.TestCase):
    def test_checkpoint_survives_transient_windows_reader_and_preserves_denied_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'batch.json'
            batch.persist(path, {'generation': 1})
            denied = PermissionError('reader holds the previous checkpoint')
            denied.winerror = 32
            replace = Path.replace
            calls = 0

            def reader_releases(source, destination):
                nonlocal calls
                calls += 1
                if calls == 1:
                    self.assertEqual(json.loads(destination.read_text()), {'generation': 1})
                    raise denied
                return replace(source, destination)

            with patch.object(Path, 'replace', reader_releases):
                batch.persist(path, {'generation': 2})
            self.assertEqual(json.loads(path.read_text()), {'generation': 2})
            with patch.object(Path, 'replace', side_effect=denied), \
                    patch.object(batch.time, 'monotonic', side_effect=[0, 3]):
                with self.assertRaises(PermissionError):
                    batch.persist(path, {'generation': 3})
            self.assertEqual(json.loads(path.read_text()), {'generation': 2})
            self.assertEqual(json.loads(path.with_suffix('.tmp').read_text()), {'generation': 3})
            with patch.object(Path, 'replace', side_effect=PermissionError('permissions')):
                with self.assertRaises(PermissionError):
                    batch.persist(path, {'generation': 4})

    def test_failed_row_does_not_skip_later_checks_and_resume_retries_only_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp)
            rows = [{'id': 'first', 'profile': 'first.json', 'checks': ['observation_speed']},
                    {'id': 'second', 'profile': 'second.json', 'checks': ['input_pacing']}]
            with patch.object(batch, 'run_check', side_effect=[{'passed': False}, {'passed': True}]) as run:
                self.assertFalse(batch.execute(rows, {'frozen': 'inputs'}, output))
                self.assertEqual(run.call_count, 2)
            with patch.object(batch, 'run_check', return_value={'passed': True}) as run:
                self.assertTrue(batch.execute(rows, {'frozen': 'inputs'}, output))
                self.assertEqual(run.call_count, 1)
            with self.assertRaisesRegex(ValueError, 'inputs changed'):
                batch.execute(rows, {'frozen': 'changed'}, output)

    def test_zero_exit_without_managed_stop_receipt_is_not_a_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            script = root / 'witness.py'
            script.write_text('import sys,json\nfrom pathlib import Path\n'
                              'out=Path(sys.argv[sys.argv.index("--output")+1]);out.mkdir()\n'
                              '(out/"result.json").write_text(json.dumps({"passed":True}))\n')
            result = batch.run_check(script, root / 'profile.json', root / 'attempt')
            self.assertEqual(result['exit_code'], 0)
            self.assertFalse(result['passed'])
            with script.open('a') as stream:
                stream.write('(out/"stop.json").write_text(json.dumps({"stopped":False}))\n')
            result = batch.run_check(script, root / 'profile.json', root / 'refused-stop')
            self.assertFalse(result['passed'])
            with script.open('a') as stream:
                stream.write('(out/"stop.json").write_text(json.dumps({"stopped":True}))\n')
            result = batch.run_check(script, root / 'profile.json', root / 'confirmed-stop')
            self.assertTrue(result['passed'])

    def test_changed_artifact_fails_before_any_launch(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'profile.json').write_text('{}')
            artifact = root / 'emulator.exe'
            artifact.write_bytes(b'original')
            manifest = root / 'matrix.json'
            manifest.write_text(json.dumps({'rows': [{'id': 'guest', 'profile': 'profile.json',
                                                       'checks': ['observation_speed']}],
                                           'artifacts': [{'path': 'emulator.exe', 'sha256': batch.sha(artifact)}]}))
            batch.prepare(manifest)
            artifact.write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'changed runtime artifact'):
                batch.prepare(manifest)


if __name__ == '__main__':
    unittest.main()
