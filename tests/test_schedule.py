import fcntl
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from gray_discord.config import save_config
from gray_discord.gateway import open_store


class ScheduleTests(unittest.TestCase):
    def test_online_crud_via_store_and_cli(self):
        with tempfile.TemporaryDirectory() as tmp:
            path=Path(tmp)/'config.json'
            save_config(path,dict(token='TESTTOKEN',owner_id='123',channel_id='42',gray_bin='/bin/gray',gray_home=tmp,workdir=tmp))
            gateway_lock = (path.parent/'gateway.lock').open('a')
            self.addCleanup(gateway_lock.close)
            fcntl.flock(gateway_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            store=open_store(path)
            store.schedule_add('legacy-job',120,'legacy',now=1000)
            migrated=open_store(path)
            self.assertEqual([job['id'] for job in migrated.schedules()],['legacy-job'])
            def invoke(*args):
                return subprocess.run([sys.executable,'-m','gray_discord','--config',str(path),'schedule',*args],capture_output=True,text=True)
            self.assertEqual(invoke('add','--every','0','hello').returncode,1)
            added=invoke('add','--every','60','hello')
            self.assertEqual(added.returncode,0,added.stderr)
            job_id=added.stdout.strip()
            self.assertIn(job_id,invoke('list').stdout)
            jobs={job['id']:job for job in store.schedules()}
            self.assertEqual(jobs[job_id]['prompt'],'hello')
            self.assertEqual(invoke('remove',job_id).returncode,0)
            self.assertEqual(invoke('list').stdout,'legacy-job 120 scheduled\n')
            self.assertEqual(invoke('remove',job_id).returncode,1)
