"""Single gateway, owner-only turns, persistent interval jobs, bounded concurrency."""
import asyncio
import fcntl
import json
import time
from pathlib import Path
from .config import atomic_json
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


async def run(config, path):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (path.parent / 'gateway.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('Another instance or schedule editor is running') from None
        async with client() as bot:
            busy = set()
            active = set()
            jobs = jobs_read(jobs_path(path))

            @bot.event
            async def on_ready():
                print('Discord connected; owner-only access enabled.', flush=True)

            @bot.event
            async def on_message(message):
                prompt = incoming(str(message.author.id), config['owner_id'], message.author.bot,
                                  message.guild is None, message.content, str(bot.user.id))
                if prompt is None:
                    return
                key = str(message.channel.id)
                if key in busy or len(busy) >= 2:
                    await send(message.channel, 'Busy. Please try again after the current turn.')
                    return
                busy.add(key)
                task = asyncio.current_task()
                active.add(task)
                try:
                    async with message.channel.typing():
                        result = await run_gray(config, path, 'chat:' + key, prompt)
                    await send(message.channel, result)
                except Exception:
                    await send(message.channel, 'The turn failed; actions may already have happened. No automatic retry.')
                finally:
                    busy.discard(key)
                    active.discard(task)

            async def tick():
                await bot.wait_until_ready()
                while True:
                    for job in jobs:
                        if job['next_at'] <= time.time():
                            # Advance before work: never replay possibly executed side effects on restart.
                            job['next_at'] = time.time() + job['interval']
                            job['status'] = 'running'
                            atomic_json(jobs_path(path), jobs)
                            try:
                                result = await run_gray(config, path, 'job:' + job['id'], job['prompt'])
                                channel = await bot.fetch_channel(int(config['channel_id']))
                                await send(channel, result)
                                job['status'] = 'sent'
                            except Exception:
                                job['status'] = 'failed'
                            atomic_json(jobs_path(path), jobs)
                    await asyncio.sleep(5)

            await bot.login(config['token'])
            ticker = asyncio.create_task(tick())
            try:
                await bot.connect(reconnect=True)
            finally:
                ticker.cancel()
                for task in active:
                    task.cancel()
                await asyncio.gather(ticker, *active, return_exceptions=True)
