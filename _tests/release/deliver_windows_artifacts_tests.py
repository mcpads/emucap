import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import deliver_windows_artifacts as delivery


class DeliveryTests(unittest.TestCase):
    def test_resume_observes_existing_command_without_issuing_duplicate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan = root / 'plan.json'
            output = root / 'delivery.json'
            digest = 'a' * 64
            plan.write_text(json.dumps({'pending': [], 'archives': [
                {'artifact_id': 123, 'path': 'downloads/123.zip', 'sha256': digest}]}))
            argv = ['deliver', '--plan', str(plan), '--output', str(output),
                    '--repository', 'owner/repo', '--instance', 'i-test',
                    '--region', 'test-region', '--root', 'C:\\validation']
            with patch('sys.argv', argv), contextlib.redirect_stdout(io.StringIO()), \
                    patch.object(delivery.subprocess, 'check_output', return_value=json.dumps(
                        {'expired': False, 'digest': 'sha256:' + digest})), \
                    patch.object(delivery, 'artifact_download_url', return_value='https://signed.invalid/short-lived') as url, \
                    patch.object(delivery, 'aws', return_value={'Command': {'CommandId': 'command-1'}}) as aws, \
                    patch.object(delivery, 'wait_command', side_effect=RuntimeError('observation interrupted')):
                with self.assertRaisesRegex(RuntimeError, 'interrupted'):
                    delivery.main()
                self.assertEqual(aws.call_count, 1)
                self.assertEqual(url.call_count, 1)
            saved = output.read_text()
            self.assertIn('command-1', saved)
            self.assertNotIn('short-lived', saved)
            with patch('sys.argv', argv), contextlib.redirect_stdout(io.StringIO()), \
                    patch.object(delivery, 'aws') as aws, \
                    patch.object(delivery, 'artifact_download_url') as url, \
                    patch.object(delivery, 'wait_command', return_value={
                        'Status': 'Success', 'ResponseCode': 0,
                        'StandardOutputContent': json.dumps({'sha256': digest, 'bytes': 20, 'reused': False})}):
                delivery.main()
                delivery.main()
                aws.assert_not_called()
                url.assert_not_called()
            self.assertEqual(json.loads(output.read_text())['archives']['123'][0]['status'], 'Success')

    def test_destination_escape_rejected_before_command_creation(self):
        with self.assertRaisesRegex(ValueError, 'destination'):
            delivery.download_command('C:\\validation',
                {'path': '../escape.zip', 'sha256': 'a' * 64}, 'https://invalid')


if __name__ == '__main__':
    unittest.main()
