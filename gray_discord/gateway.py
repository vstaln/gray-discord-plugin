"""Owner-only gateway with durable generation and delivery workers."""
import asyncio
import fcntl
import hashlib
import json
from pathlib import Path
import discord
from .durable import Store
from .budget import BudgetBlocked
from .policy import incoming
from .runner import run_gray
from .transport import client, send


def jobs_path(config_path):
    return Path(config_path).parent / 'jobs.json'


def jobs_read(path):
    if not path.exists():
        return []
    data = json.loads(path.read_text())
    if not isinstance(data, list):
        raise ValueError('Invalid schedules file')
    return data


def open_store(path):
    store = Store(Path(path).parent / 'queue.sqlite')
    store.migrate_jobs(jobs_path(path))
    return store


class Runtime:
    def __init__(self, config, path, store, deliver, runner=run_gray, progress=None):
        self.config, self.path, self.store = config, path, store
        self.deliver, self.runner, self.progress = deliver, runner, progress

    async def generate_one(self):
        item = self.store.claim()
        if not item:
            return False
        receipt = {}
        async def progress(phase):
            if self.progress:
                # UI failures must not cancel paid agent execution.
                try:
                    await self.progress(item, phase)
                except Exception:
                    pass
        task = asyncio.create_task(self.runner(self.config, self.path, item['conversation'],
                                              item['prompt'], progress=progress, receipt=receipt))
        try:
            while not task.done():
                await asyncio.wait([task], timeout=0.25)
                if self.store.get(item['id'])['cancel']:
                    task.cancel()
                    await asyncio.gather(task, return_exceptions=True)
                    self.store.fail(item['id'], 'cancelled')
                    return True
            result = task.result()
            self.store.complete(item['id'], result, receipt)
        except asyncio.CancelledError:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            self.store.fail(item['id'], 'interrupted')
            raise
        except BudgetBlocked:
            self.store.fail(item['id'], 'budget_blocked')
        except TimeoutError:
            self.store.fail(item['id'], 'timeout')
        except Exception:
            self.store.fail(item['id'], 'agent_failed')
        return True

    async def deliver_one(self):
        part = self.store.next_delivery()
        if not part:
            return False
        try:
            message_id = await self.deliver(part)
            if not message_id:
                raise ValueError('Delivery returned no message ID')
            self.store.ack(part['id'], part['part'], str(message_id))
        except Exception:
            self.store.delivery_failed(part, 'delivery_failed')
        return True

    async def run(self):
        self.store.recover()
        async def generation():
            while True:
                if not await self.generate_one():
                    await asyncio.sleep(0.25)
        async def delivery():
            while True:
                if not await self.deliver_one():
                    await asyncio.sleep(0.25)
        async def schedules():
            while True:
                self.store.enqueue_due(self.config['channel_id'])
                await asyncio.sleep(1)
        tasks = [asyncio.create_task(generation()) for _ in range(self.config.get('concurrency', 2))]
        tasks += [asyncio.create_task(delivery()), asyncio.create_task(schedules())]
        try:
            # Unexpected worker failure must stop/restart the service, not leave
            # a connected bot silently running without a scheduler or outbox.
            await asyncio.gather(*tasks)
        finally:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)


async def run(config, path):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (path.parent / 'gateway.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('Another gateway is running') from None
        from .budget import validate
        provider = json.loads((Path(config['gray_home']) / 'config.json').read_text())
        validate(config.get('budget'), provider.get('model'))
        config = {**config, 'budget_required': True}
        store = open_store(path)
        async with client() as bot:
            @bot.event
            async def on_ready():
                print('Discord connected; durable owner-only queue enabled.', flush=True)

            @bot.event
            async def on_message(message):
                prompt = incoming(str(message.author.id), config['owner_id'], message.author.bot,
                                  message.guild is None, message.content, str(bot.user.id))
                if prompt is None:
                    return
                try:
                    store.enqueue(str(message.id), str(message.channel.id), prompt,
                                  capacity=config.get('queue_capacity', 1000))
                except ValueError:
                    await send(message.channel, 'Queue full or message invalid; this message was not accepted.')

            async def deliver(part):
                await bot.wait_until_ready()
                channel = await bot.fetch_channel(int(part['channel']))
                nonce = hashlib.sha256(f"{part['id']}:{part['part']}".encode()).hexdigest()[:24]
                # A stable nonce aids reconciliation, but the SDK does not expose
                # enforce_nonce. Ambiguous successful sends can still duplicate.
                message = await channel.send(part['content'], nonce=nonce,
                                             allowed_mentions=discord.AllowedMentions.none())
                return str(message.id)

            last_typing = {}
            async def progress(item, phase):
                now = asyncio.get_running_loop().time()
                if now - last_typing.get(item['channel'], -10) >= 8:
                    channel = await bot.fetch_channel(int(item['channel']))
                    await channel.typing()
                    last_typing[item['channel']] = now

            await bot.login(config['token'])
            runtime = Runtime(config, path, store, deliver, progress=progress)
            async def workers():
                await bot.wait_until_ready()
                await runtime.run()
            tasks = [asyncio.create_task(bot.connect(reconnect=True)), asyncio.create_task(workers())]
            try:
                done, _ = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
                for task in done:
                    task.result()
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
