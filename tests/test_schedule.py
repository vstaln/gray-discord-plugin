import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from gray_discord.config import save_config


class ScheduleTests(unittest.TestCase):
    def test_persist_list_remove_and_invalid_interval(self):
        with tempfile.TemporaryDirectory() as tmp:
            path=Path(tmp)/'config.json'
            save_config(path,dict(token='fixture',owner_id='123',channel_id='456',gray_bin='/bin/true',gray_home=tmp,workdir=tmp))
            def invoke(*args):
                return subprocess.run([sys.executable,'-m','gray_discord','--config',str(path),'schedule',*args],capture_output=True,text=True)
            self.assertEqual(invoke('add','--every','0','hello').returncode,1)
            added=invoke('add','--every','60','hello')
            self.assertEqual(added.returncode,0,added.stderr)
            job_id=added.stdout.strip()
            self.assertIn(job_id,invoke('list').stdout)
            jobs=json.loads((path.parent/'jobs.json').read_text())
            self.assertEqual(jobs[0]['prompt'],'hello')
            self.assertEqual(invoke('remove',job_id).returncode,0)
            self.assertEqual(invoke('list').stdout,'')
            self.assertEqual(invoke('remove',job_id).returncode,1)
