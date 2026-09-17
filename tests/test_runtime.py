import asyncio
import tempfile
import unittest
from pathlib import Path
from gray_discord.durable import Store
from gray_discord.gateway import Runtime


class RuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_failed_delivery_restart_does_not_repeat_agent(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'config.json'
            store = Store(Path(tmp)/'queue.sqlite')
            calls, sent = [], []
            async def runner(config, path, conversation, prompt, **kwargs):
                calls.append(prompt)
                return 'answer'
            async def broken(part):
                raise OSError('private error body')
            runtime = Runtime({}, path, store, broken, runner=runner)
            store.enqueue('message1', '42', 'question')
            await runtime.generate_one()
            await runtime.deliver_one()
            self.assertEqual(calls, ['question'])
            async def deliver(part):
                sent.append(part['content'])
                return 'reply1'
            runtime = Runtime({}, path, Store(store.path), deliver, runner=runner)
            runtime.store.recover()
            with store.connection() as db:
                db.execute('UPDATE outbox SET next_at=0')
            await runtime.generate_one()
            await runtime.deliver_one()
            self.assertEqual(calls, ['question'])
            self.assertEqual(sent, ['answer'])
            self.assertEqual(store.get('message1')['state'], 'sent')

    async def test_cancel_running_work_and_shutdown_marks_uncertain(self):
        with tempfile.TemporaryDirectory() as tmp:
            path=Path(tmp)/'config.json'
            store=Store(Path(tmp)/'queue.sqlite')
            started=asyncio.Event()
            async def runner(*args, **kwargs):
                started.set()
                await asyncio.Event().wait()
            async def deliver(part):
                return 'reply'
            runtime=Runtime({}, path, store, deliver, runner=runner)
            store.enqueue('1','42','question')
            task=asyncio.create_task(runtime.generate_one())
            await asyncio.wait_for(started.wait(), 2)
            store.cancel('1')
            await asyncio.wait_for(task, 3)
            self.assertEqual(store.get('1')['error'], 'cancelled')
            self.assertIsNone(store.claim())

    @unittest.skipUnless(__import__('os').environ.get('GRAY_TEST_BIN'), 'Set GRAY_TEST_BIN')
    async def test_background_real_gray_delivery_restart(self):
        import json
        import os
        from gray_discord.config import atomic_json
        requests = []
        async def provider(reader, writer):
            header = await reader.readuntil(b'\r\n\r\n')
            size = next(int(l.split(b':',1)[1]) for l in header.split(b'\r\n') if l.lower().startswith(b'content-length:'))
            requests.append(json.loads(await reader.readexactly(size)))
            chunk = dict(id='test',object='chat.completion.chunk',created=1,model='test-model',
                         choices=[dict(index=0,delta=dict(content='durable answer'),finish_reason='stop')],
                         usage=dict(prompt_tokens=100, completion_tokens=20, total_tokens=120))
            body = ('data: '+json.dumps(chunk)+'\n\ndata: [DONE]\n\n').encode()
            writer.write(b'HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: '+str(len(body)).encode()+b'\r\n\r\n'+body)
            await writer.drain()
            writer.close()
            await writer.wait_closed()
        server = await asyncio.start_server(provider, '127.0.0.1', 0)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                atomic_json(root/'config.json', dict(model='test-model',api_key='fixture',
                    base_url=f'http://127.0.0.1:{server.sockets[0].getsockname()[1]}/v1',context_window=128000))
                path = root/'plugin.json'
                store = Store(root/'queue.sqlite')
                config = dict(gray_bin=os.environ['GRAY_TEST_BIN'], gray_home=tmp, channel_id='42',
                    budget=dict(model='test-model',daily_usd=0.0001,turn_usd=0.0001,
                                input_per_million=1,output_per_million=1))
                failed = asyncio.Event()
                async def broken(part):
                    failed.set()
                    raise OSError('fixture disconnection')
                store.enqueue('real-message', '42', 'Say durable answer')
                task = asyncio.create_task(Runtime(config,path,store,broken).run())
                try:
                    await asyncio.wait_for(failed.wait(), 40)
                finally:
                    task.cancel()
                    await asyncio.gather(task, return_exceptions=True)
                self.assertEqual(len(requests), 1)
                sent = asyncio.Event()
                async def deliver(part):
                    self.assertEqual(part['content'], 'durable answer')
                    sent.set()
                    return 'discord-reply'
                with store.connection() as db:
                    db.execute('UPDATE outbox SET next_at=0')
                task = asyncio.create_task(Runtime(config,path,Store(store.path),deliver).run())
                try:
                    await asyncio.wait_for(sent.wait(), 5)
                    self.assertEqual(store.get('real-message')['state'], 'sent')
                finally:
                    task.cancel()
                    await asyncio.gather(task, return_exceptions=True)
                self.assertEqual(len(requests), 1, 'Delivery restart must not regenerate the answer')
                from gray_discord.budget import Budget
                self.assertEqual(Budget(root/'budget.sqlite').total(), 120)
                store.enqueue('second-message','42','Should be blocked before provider')
                await Runtime(config,path,store,deliver).generate_one()
                self.assertEqual(store.get('second-message')['error'], 'budget_blocked')
                self.assertEqual(len(requests), 1)
        finally:
            server.close()
            await server.wait_closed()
