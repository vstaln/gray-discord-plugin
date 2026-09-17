"""Self-service entry point. No AI or credentials in command-line arguments."""
import argparse
import asyncio
import json
import os
import signal
import sys
import time
import uuid
from pathlib import Path
from .config import default_path, load_config, atomic_json, save_config
from . import service


def parser():
    p = argparse.ArgumentParser(prog='gray discord')
    p.add_argument('--config', type=Path, default=default_path())
    sub = p.add_subparsers(dest='command', required=True)
    for name in ('setup', 'run', 'sidecar', 'register', 'install', 'status', 'stop', 'restart', 'doctor', 'uninstall'):
        sub.add_parser(name)
    limits = sub.add_parser('limits')
    limits.add_argument('--timeout-seconds', type=int)
    limits.add_argument('--concurrency', type=int)
    limits.add_argument('--max-requests', type=int)
    budget = sub.add_parser('budget').add_subparsers(dest='action', required=True)
    budget.add_parser('status')
    set_budget = budget.add_parser('set')
    for flag in ('daily-usd', 'turn-usd', 'input-per-million', 'output-per-million'):
        set_budget.add_argument('--' + flag, type=float, required=True)
    share = sub.add_parser('share')
    share.add_argument('--skill', action='append', default=[])
    share.add_argument('--context', action='append', default=[])
    share.add_argument('--plugin-argv', action='append', default=[], help='JSON argv array, e.g. ["python3","-m","memory_plugin"]')
    share.add_argument('--clear', action='store_true')
    queue = sub.add_parser('queue').add_subparsers(dest='action', required=True)
    queue.add_parser('list')
    cancel = queue.add_parser('cancel')
    cancel.add_argument('id')
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
            if args.command == 'share':
                from .capabilities import prepare
                import tempfile
                if args.clear:
                    for key in ('shared_skills', 'shared_context', 'shared_plugins'):
                        config[key] = []
                config['shared_skills'] = list(dict.fromkeys(config.get('shared_skills', []) + args.skill))
                config['shared_context'] = list(dict.fromkeys(config.get('shared_context', []) + args.context))
                config['shared_plugins'] = config.get('shared_plugins', []) + [json.loads(value) for value in args.plugin_argv]
                with tempfile.TemporaryDirectory() as tmp:
                    prepare(config, Path(tmp))
                save_config(path, config)
                print('Shared capabilities saved. Only select trusted code/non-secret context. Restart to apply.')
            elif args.command == 'limits':
                for key in ('timeout_seconds', 'concurrency', 'max_requests'):
                    if getattr(args, key) is not None:
                        config[key] = getattr(args, key)
                save_config(path, config)
                print('Limits saved; restart the gateway to apply.')
            elif args.command == 'budget':
                from .budget import Budget
                if args.action == 'set':
                    provider = json.loads((Path(config['gray_home']) / 'config.json').read_text())
                    config['budget'] = dict(model=provider.get('model'), daily_usd=args.daily_usd,
                        turn_usd=args.turn_usd, input_per_million=args.input_per_million,
                        output_per_million=args.output_per_million)
                    save_config(path, config)
                    print('Budget saved for selected model; restart the gateway to apply.')
                else:
                    print('Accounted/reserved micro-USD:', Budget(path.parent/'budget.sqlite').total())
            elif args.command == 'register':
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
            elif args.command in ('schedule', 'queue'):
                from .gateway import open_store
                store = open_store(path)
                if args.command == 'queue':
                    if args.action == 'cancel':
                        store.cancel(args.id)
                    else:
                        for item in store.items():
                            print(item['id'], item['channel'], item['state'], item['error'] or '')
                elif args.action == 'add':
                    job_id = uuid.uuid4().hex
                    store.schedule_add(job_id, args.every, args.prompt)
                    print(job_id)
                elif args.action == 'remove':
                    store.schedule_remove(args.id)
                else:
                    for job in store.schedules():
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
