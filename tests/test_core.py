import json
import tempfile
import unittest
from pathlib import Path
from gray_discord.config import save_config, load_config, validate_config
from gray_discord.hermes_text import split_message, utf16_len
from gray_discord.policy import incoming, Pairing
from gray_discord.runner import final_reply


class CoreTests(unittest.TestCase):
    def test_private_config_and_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'config.json'
            data = dict(token='fixture', owner_id='123456789', channel_id='987654321',
                        gray_bin='/bin/true', gray_home=tmp, workdir=tmp)
            save_config(path, data)
            self.assertEqual(load_config(path), data)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(ValueError):
                validate_config({**data, 'owner_id': ''})
            path.write_text('{bad')
            with self.assertRaises(ValueError):
                load_config(path)

    def test_unicode_split_preserves_all_text(self):
        text = ('😀\nhello ' * 1200)
        chunks = split_message(text)
        self.assertEqual(''.join(chunks), text)
        self.assertTrue(all(utf16_len(c) <= 2000 for c in chunks))
        with self.assertRaises(ValueError):
            split_message('😀', 1)

    def test_owner_only_dm_or_mention(self):
        self.assertEqual(incoming('123', '123', False, True, 'hello', '456'), 'hello')
        self.assertEqual(incoming('123', '123', False, False, '<@!456> hello', '456'), 'hello')
        for author, bot, dm, text in [('999',False,True,'x'), ('123',True,True,'x'),
                                      ('123',False,False,'hello'), ('123',False,False,'<@456>')]:
            self.assertIsNone(incoming(author,'123',bot,dm,text,'456'))

    def test_pairing_expires_and_only_consumes_once(self):
        p = Pairing(now=0)
        self.assertFalse(p.accept('wrong', now=1))
        self.assertTrue(p.accept(p.code, now=2))
        self.assertFalse(p.accept(p.code, now=3))
        p = Pairing(now=0)
        self.assertFalse(p.accept(p.code, now=301))

    def test_final_reply_excludes_tool_output_and_prior_turn(self):
        rows = [dict(message=dict(role='assistant', content=[dict(type='text',text='old')])),
                dict(message=dict(role='user',content=[])),
                dict(message=dict(role='assistant',content=[dict(type='tool_use',name='bash')])),
                dict(message=dict(role='user',content=[dict(type='tool_result',content='secret')])),
                dict(message=dict(role='assistant',content=[dict(type='thinking',text='private'),dict(type='text',text='answer')]))]
        self.assertEqual(final_reply(rows, 1), 'answer')
        with self.assertRaises(ValueError):
            final_reply(rows[:4], 1)
