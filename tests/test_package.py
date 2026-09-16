import asyncio
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from gray_discord.config import save_config
from gray_discord.cli import register
from gray_discord.service import unit, quote
from gray_discord.sidecar import dispatch


class PackageTests(unittest.TestCase):
    def test_help_and_missing_config(self):
        result = subprocess.run([sys.executable, '-m', 'gray_discord', '--help'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('setup', result.stdout)
        with tempfile.TemporaryDirectory() as tmp:
            result = subprocess.run([sys.executable, '-m', 'gray_discord', '--config', tmp+'/missing', 'doctor'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertNotIn('Traceback', result.stderr)

    def test_sidecar_real_process_without_token(self):
        requests = [dict(id=1,method='plugin/manifest'),dict(id=2,method='tool/call',params=dict(name='unknown'))]
        with tempfile.TemporaryDirectory() as tmp:
            result = subprocess.run([sys.executable, '-m','gray_discord','--config',tmp+'/missing','sidecar'],
                input=''.join(json.dumps(r)+'\n' for r in requests), text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = [json.loads(line) for line in result.stdout.splitlines()]
            self.assertEqual(rows[0]['result']['name'], 'discord')
            self.assertTrue(rows[1]['result']['is_error'])

    def test_delivery_failure_is_not_success_or_secret_disclosure(self):
        with patch('gray_discord.sidecar.load_config', side_effect=ValueError('PRIVATE-TOKEN')):
            reply = asyncio.run(dispatch('tool/call', dict(name='discord_send',args=dict(content='hello')),Path('missing')))
        self.assertTrue(reply['is_error'])
        self.assertNotIn('PRIVATE-TOKEN', json.dumps(reply))

    def test_service_escapes_paths_and_does_not_embed_token(self):
        text = unit(Path('/tmp/percent% and space/config.json'))
        self.assertIn('percent%% and space', text)
        self.assertIn('KillMode=control-group', text)
        self.assertNotIn('token', text)
        with self.assertRaises(ValueError):
            quote('/tmp/new\nline')

    def test_registration_preserves_other_plugins(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            lock = home/'plugins/lock.json'
            lock.parent.mkdir()
            lock.write_text(json.dumps(dict(schema=1,plugins=dict(other=dict(enabled=False)))))
            config = dict(gray_home=tmp)
            register(config, home/'config.json')
            data=json.loads(lock.read_text())
            self.assertEqual(data['plugins']['other'], dict(enabled=False))
            self.assertEqual(data['plugins']['discord']['argv'][-1], 'sidecar')
            register(config, home/'config.json')
            with self.assertRaises(ValueError):
                register(config, home/'different.json')
