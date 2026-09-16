import asyncio
import json
import os
import tempfile
import unittest
from pathlib import Path
from gray_discord.runner import run_gray
from gray_discord.config import atomic_json


class RunnerTests(unittest.IsolatedAsyncioTestCase):
    async def test_nonzero_exit_is_failure_even_if_stdout_exists(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            atomic_json(root/'config.json', dict(model='test'))
            exe=root/'fail'
            exe.write_text('#!/bin/sh\necho partial-response\nexit 7\n')
            exe.chmod(0o700)
            config=dict(gray_bin=str(exe),gray_home=tmp)
            with self.assertRaisesRegex(ValueError,'code 7'):
                await run_gray(config,root/'plugin.json','chat:one','hello')

    async def test_timeout_kills_child_and_releases_conversation_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            atomic_json(root/'config.json',dict(model='test'))
            exe=root/'sleep'
            exe.write_text('#!/bin/sh\nsleep 60\n')
            exe.chmod(0o700)
            config=dict(gray_bin=str(exe),gray_home=tmp)
            for _ in range(2):
                with self.assertRaises(TimeoutError):
                    await run_gray(config,root/'plugin.json','chat:one','hello',timeout=0.05)

    @unittest.skipUnless(os.environ.get('GRAY_TEST_BIN'), 'Set GRAY_TEST_BIN for real gray integration')
    async def test_real_gray_two_turn_resume_and_conversation_isolation(self):
        requests=[]
        async def serve(reader, writer):
            headers=await reader.readuntil(b'\r\n\r\n')
            length=next(int(line.split(b':',1)[1]) for line in headers.split(b'\r\n') if line.lower().startswith(b'content-length:'))
            request=json.loads(await reader.readexactly(length))
            requests.append(request)
            chunk=dict(id='fixture',object='chat.completion.chunk',created=1,model='test-model',
                       choices=[dict(index=0,delta=dict(content='LOCAL-MARKER'),finish_reason=None)])
            done=dict(id='fixture',object='chat.completion.chunk',created=1,model='test-model',
                      choices=[dict(index=0,delta={},finish_reason='stop')])
            body=f'data: {json.dumps(chunk)}\n\ndata: {json.dumps(done)}\n\ndata: [DONE]\n\n'.encode()
            writer.write(b'HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: '+str(len(body)).encode()+b'\r\n\r\n'+body)
            await writer.drain()
            writer.close()
            await writer.wait_closed()
        server=await asyncio.start_server(serve,'127.0.0.1',0)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp)
                port=server.sockets[0].getsockname()[1]
                atomic_json(root/'config.json',dict(model='test-model',api_key='fixture',base_url=f'http://127.0.0.1:{port}/v1',context_window=128000))
                config=dict(gray_bin=os.environ['GRAY_TEST_BIN'],gray_home=tmp)
                path=root/'plugin.json'
                self.assertEqual(await run_gray(config,path,'chat:one','Remember a violet bicycle',timeout=40),'LOCAL-MARKER')
                self.assertEqual(await run_gray(config,path,'chat:one','What did I ask you to remember?',timeout=40),'LOCAL-MARKER')
                self.assertEqual(await run_gray(config,path,'chat:two','A different conversation',timeout=40),'LOCAL-MARKER')
                self.assertEqual(len(requests),3)
                def text(request):
                    return json.dumps([m for m in request['messages'] if m['role']!='system'])
                self.assertIn('Remember a violet bicycle',text(requests[1]))
                self.assertIn('LOCAL-MARKER',text(requests[1]))
                self.assertNotIn('Remember a violet bicycle',text(requests[2]))
                sessions=list((root/'conversations').glob('*/sessions/*.jsonl'))
                self.assertEqual(len(sessions),2)
        finally:
            server.close()
            await server.wait_closed()
