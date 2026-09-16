"""Real Discord SDK against loopback HTTP; no test-only production send path."""
import asyncio
import json
import os
import shlex
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from aiohttp import web
from discord.http import Route
from gray_discord.config import atomic_json, save_config
from gray_discord.transport import rest_send

USER = dict(id='123', username='fixture', discriminator='0000', avatar=None, bot=True)


class DeliveryTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.sent = []
        self.model_requests = []
        self.fail_auth = False
        self.fail_send = False
        app = web.Application()
        async def discord_content_type(request, response):
            # discord.py parses JSON only for this exact header, as Discord serves it.
            if response.content_type == 'application/json':
                response.headers['Content-Type'] = 'application/json'
        app.on_response_prepare.append(discord_content_type)
        app.router.add_route('*', '/{tail:.*}', self.handle)
        self.server = web.AppRunner(app)
        await self.server.setup()
        site = web.TCPSite(self.server, '127.0.0.1', 0)
        await site.start()
        port = site._server.sockets[0].getsockname()[1]
        self.base = f'http://127.0.0.1:{port}'

    async def asyncTearDown(self):
        await self.server.cleanup()

    async def handle(self, request):
        if request.path == '/users/@me':
            if self.fail_auth:
                return web.json_response(dict(message='Unauthorized', code=0), status=401)
            return web.json_response(USER)
        if request.path == '/oauth2/applications/@me':
            return web.json_response(dict(id='123', name='fixture', description='', icon=None,
                bot_public=False, bot_require_code_grant=False, owner=USER, verify_key='fixture'))
        if request.path == '/channels/42':
            return web.json_response(dict(id='42', type=1, recipients=[USER]))
        if request.path == '/channels/42/messages' and request.method == 'POST':
            body = await request.json()
            self.sent.append((request.headers.get('Authorization'), body))
            if self.fail_send:
                return web.json_response(dict(message='Missing Permissions', code=50013), status=403)
            return web.json_response(dict(id=str(1000 + len(self.sent)), channel_id='42', author=USER,
                content=body['content'], timestamp='2026-01-01T00:00:00+00:00', edited_timestamp=None,
                tts=False, mention_everyone=False, mentions=[], mention_roles=[], attachments=[],
                embeds=[], pinned=False, type=0))
        if request.path == '/v1/chat/completions':
            body = await request.json()
            self.model_requests.append(body)
            if len(self.model_requests) == 1:
                delta = dict(tool_calls=[dict(index=0, id='send1', type='function',
                    function=dict(name='discord_send', arguments=json.dumps(dict(content='grey lives'))))])
                reason = 'tool_calls'
            else:
                delta, reason = dict(content='DONE'), 'stop'
            chunk = dict(id='fixture', object='chat.completion.chunk', created=1, model='test-model',
                         choices=[dict(index=0, delta=delta, finish_reason=reason)])
            return web.Response(text=f'data: {json.dumps(chunk)}\n\ndata: [DONE]\n\n', content_type='text/event-stream')
        return web.json_response(dict(message='Unknown fixture endpoint'), status=404)

    async def test_sdk_sends_split_text_without_mentions(self):
        text = '😀' * 1100 + '@everyone'
        with patch.object(Route, 'BASE', self.base):
            await rest_send(dict(token='TESTTOKEN', channel_id='42'), text)
        self.assertEqual(len(self.sent), 2)
        self.assertEqual(''.join(body['content'] for _, body in self.sent), text)
        for auth, body in self.sent:
            self.assertEqual(auth, 'Bot TESTTOKEN')
            self.assertEqual(body['allowed_mentions']['parse'], [])
            self.assertNotEqual(body['allowed_mentions'].get('replied_user'), True)
            self.assertNotIn('message_reference', body)

    async def test_auth_failure_is_not_success(self):
        self.fail_auth = True
        import discord
        with patch.object(Route, 'BASE', self.base):
            with self.assertRaises(discord.LoginFailure):
                await rest_send(dict(token='TESTTOKEN', channel_id='42'), 'hello')
        self.assertEqual(self.sent, [])

    async def test_send_failure_is_not_success(self):
        self.fail_send = True
        import discord
        with patch.object(Route, 'BASE', self.base):
            with self.assertRaises(discord.Forbidden):
                await rest_send(dict(token='TESTTOKEN', channel_id='42'), 'hello')
        self.assertEqual(len(self.sent), 1)

    async def test_invalid_content_rejected_before_http(self):
        for text in (' ', 'x' * 20001, None):
            with self.assertRaises(ValueError):
                await rest_send({}, text)
        self.assertEqual(self.sent, [])

    @unittest.skipUnless(os.environ.get('GRAY_TEST_BIN'), 'Set GRAY_TEST_BIN for real gray integration')
    async def test_real_gray_tool_call_reaches_discord_sdk_http(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / 'gray-home'
            atomic_json(home / 'config.json', dict(model='test-model', api_key='fixture',
                                                  base_url=self.base + '/v1', context_window=128000))
            config = root / 'plugin.json'
            save_config(config, dict(token='TESTTOKEN', owner_id='123', channel_id='42',
                gray_bin=os.environ['GRAY_TEST_BIN'], gray_home=str(home), workdir=tmp))
            # Only the test launcher redirects the SDK. Production code has no api_base setting.
            launcher = root / 'plugin.sh'
            script = ('from discord.http import Route; Route.BASE=' + repr(self.base) + '; '
                      'from gray_discord.sidecar import serve; serve(' + repr(str(config)) + ')')
            launcher.write_text('#!/bin/sh\nexec ' + shlex.join([sys.executable, '-c', script]) + '\n')
            launcher.chmod(0o700)
            (root / 'gray.yml').write_text('plugins:\n  - builtin: tools-minimal\n  - sidecar: ' + str(launcher) + '\n')
            env = {k:v for k,v in os.environ.items() if not k.startswith(('GRAY_', 'OPENAI_', 'DISCORD_'))}
            env['GRAY_HOME'] = str(home)
            process = await asyncio.create_subprocess_exec(os.environ['GRAY_TEST_BIN'], '-p', 'Send grey lives to Discord.',
                cwd=root, env=env, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
            try:
                out, err = await asyncio.wait_for(process.communicate(), 45)
            finally:
                if process.returncode is None:
                    process.kill()
                    await process.wait()
            self.assertEqual(process.returncode, 0, err.decode())
            self.assertIn(b'DONE', out)
            self.assertEqual(len(self.sent), 1)
            self.assertEqual(self.sent[0][1]['content'], 'grey lives')
            self.assertEqual(self.sent[0][1]['allowed_mentions']['parse'], [])
            self.assertEqual(len(self.model_requests), 2)
            tools = [m for m in self.model_requests[1]['messages'] if m['role'] == 'tool']
            self.assertTrue(any('Sent to the configured Discord channel.' in m['content'] for m in tools))
