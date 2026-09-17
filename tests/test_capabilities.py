import tempfile
import unittest
from pathlib import Path
from gray_discord.capabilities import prepare


class CapabilityTests(unittest.TestCase):
    def test_explicit_skills_context_and_plugins_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); home=root/'conversation';home.mkdir()
            skill=root/'my-skill';skill.mkdir();(skill/'SKILL.md').write_text('---\nname: chosen\ndescription: chosen skill\n---\nhello\n')
            context=root/'memory.md';context.write_text('Shared preference: concise')
            config=dict(shared_skills=[str(skill)], shared_context=[str(context)], shared_plugins=[['/bin/cat']])
            profile=prepare(config,home)
            self.assertIn('sidecar:',profile)
            self.assertTrue(list((home/'skills').glob('*/SKILL.md')))
            self.assertIn('Shared preference: concise',(home/'AGENTS.md').read_text())
            prepare({},home)
            self.assertEqual(list((home/'skills').iterdir()),[])
            self.assertFalse((home/'AGENTS.md').exists())

    def test_rejects_missing_relative_or_oversize_context(self):
        with tempfile.TemporaryDirectory() as tmp:
            home=Path(tmp)/'home';home.mkdir()
            for config in [dict(shared_skills=['relative']),dict(shared_plugins=[[]]),dict(shared_context=['/missing/context'])]:
                with self.assertRaises(ValueError): prepare(config,home)
