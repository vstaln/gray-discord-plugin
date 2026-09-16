"""Self-service entry point. No AI or credentials in command-line arguments."""
import argparse
import asyncio
import fcntl
import json
import os
import signal
import sys
import time
import uuid
from pathlib import Path
from .config import default_path, load_config, atomic_json
from . import service


def parser():
    p = argparse.ArgumentParser(prog='gray discord')
    p.add_argument('--config', type=Path, default=default_path())
    sub = p.add_subparsers(dest='command', required=True)
    for name in ('setup', 'run', 'sidecar', 'register', 'install', 'status', 'stop', 'restart', 'doctor', 'uninstall'):
        sub.add_parser(name)
    jobs = sub.add_parser('schedule').add_subparsers(dest='action', required=True)
    add = jobs.add_parser('add')
    add.add_argument('--every', required=True, type=int, help='Interval in seconds (minimum 60)')
    add.add_argument('prompt')
    jobs.add_parser('list')
    remove = jobs.add_parser('remove')
    remove.add_argument('id')
    return p


def register(config, path):
    # Match gray_plugin::lock::LockEntry; use explicit argv, not a fake Git skills install.
    lock = Path(config['gray_home']) / 'plugins' / 'lock.json'
    data = json.loads(lock.read_text()) if lock.exists() else dict(schema=1, plugins={})
    if data.get('schema') != 1 or not isinstance(data.get('plugins'), dict):
        raise ValueError('Unsupported gray plugin lock; not changing it')
    argv = [sys.executable, '-m', 'gray_discord', '--config', str(path.resolve()), 'sidecar']
    previous = data['plugins'].get('discord')
    if previous and previous.get('argv') != argv:
        raise ValueError('Another discord plugin is registered; refusing to overwrite it')
    data['plugins']['discord'] = dict(ecosystem='gray-native', version='0.1.0', hash='',
        source='https://github.com/vstaln/gray-discord-plugin', argv=argv,
        adapter_version='1.1', installed_at=str(int(time.time())), scope='user', enabled=True)
    atomic_json(lock, data)


async def doctor(config):
    from .transport import client
    import discord
    if not os.access(config['gray_bin'], os.X_OK):
        raise ValueError('gray executable is missing or not executable')
    provider = json.loads((Path(config['gray_home']) / 'config.json').read_text())
    if not provider.get('model'):
        raise ValueError('gray has no configured model')
    async with client() as bot:
        await bot.login(config['token'])
        app = await bot.application_info()
        if not (app.flags.gateway_message_content or app.flags.gateway_message_content_limited):
            raise ValueError('Enable Message Content Intent in the Discord developer portal')
        channel = await bot.fetch_channel(int(config['channel_id']))
        if isinstance(channel, discord.abc.GuildChannel):
            me = await channel.guild.fetch_member(bot.user.id)
            perms = channel.permissions_for(me)
            if not (perms.view_channel and perms.send_messages and perms.read_message_history):
                raise ValueError('Home channel requires View, Send and Read History permissions')
    print('Bot token, intent, channel access and local gray configuration verified.')
    print('Provider generation and live gateway connection were not tested.')


def main():
    args = parser().parse_args()
    path = args.config.expanduser().absolute()
    try:
        if args.command == 'setup':
            from .setup import setup
            if asyncio.run(setup(path)):
                register(load_config(path), path)
                print('Outgoing tool registered with gray.')
                if input('Enable and start the background service now? [Y/n] ').strip().lower() in ('', 'y', 'yes'):
                    service.install(path)
                    print('Service enabled. Run gray discord status to check it.')
                    print('For operation after logout: loginctl enable-linger "$USER"')
                else:
                    print('Run gray discord install when ready, or gray discord run in the foreground.')
        elif args.command == 'sidecar':
            from .sidecar import serve
            serve(path)
        elif args.command in ('status', 'stop', 'restart'):
            service.control(args.command, service.NAME)
        elif args.command == 'uninstall':
            service.uninstall()
            print('Service removed. Private configuration and sessions retained; registered outgoing tool retained.')
        else:
            config = load_config(path)
            if args.command == 'register':
                register(config, path)
                print('Outgoing tool registered. Restart existing gray sessions to load it.')
            elif args.command == 'install':
                service.install(path)
                print('Service enabled. To survive logout, enable user linger: loginctl enable-linger "$USER"')
            elif args.command == 'doctor':
                asyncio.run(asyncio.wait_for(doctor(config), 30))
            elif args.command == 'run':
                from .gateway import run
                async def start():
                    task = asyncio.current_task()
                    asyncio.get_running_loop().add_signal_handler(signal.SIGTERM, task.cancel)
                    await run(config, path)
                asyncio.run(start())
            elif args.command == 'schedule':
                from .gateway import jobs_path, jobs_read
                with (path.parent / 'gateway.lock').open('a') as lock:
                    try:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    except BlockingIOError:
                        raise ValueError('Stop the plugin before editing/listing schedules') from None
                    jobs = jobs_read(jobs_path(path))
                    if args.action == 'add':
                        if args.every < 60 or not args.prompt.strip() or len(args.prompt) > 32000:
                            raise ValueError('Interval must be >=60s and prompt 1–32000 characters')
                        job = dict(id=uuid.uuid4().hex, interval=args.every, prompt=args.prompt,
                                   next_at=time.time() + args.every, status='scheduled')
                        jobs.append(job)
                        atomic_json(jobs_path(path), jobs)
                        print(job['id'])
                    elif args.action == 'remove':
                        remaining = [job for job in jobs if job['id'] != args.id]
                        if len(remaining) == len(jobs):
                            raise ValueError('Schedule not found')
                        atomic_json(jobs_path(path), remaining)
                    else:
                        for job in jobs:
                            print(job['id'], job['interval'], job['status'])
    except (KeyboardInterrupt, asyncio.CancelledError):
        return
    except Exception as exc:
        # SDK/HTTP exceptions can contain request context. Do not print arbitrary reprs.
        if isinstance(exc, ValueError) and not isinstance(exc, json.JSONDecodeError):
            print(str(exc), file=sys.stderr)
        else:
            print(f'{type(exc).__name__}: operation failed. Check configuration/connectivity; credentials withheld.', file=sys.stderr)
        raise SystemExit(1)
