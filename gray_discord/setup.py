"""Interactive setup owns credentials; the model never receives the token."""
import asyncio
import getpass
import json
import os
import shutil
import sys
from pathlib import Path
from .config import save_config, snowflake
from .policy import Pairing
from .transport import client


def invite(app_id):
    return f'https://discord.com/oauth2/authorize?client_id={app_id}&scope=bot&permissions=68608'


async def setup(path):
    if not sys.stdin.isatty() or not sys.stderr.isatty():
        raise ValueError('Setup needs a terminal for hidden token input')
    if path.exists() and input('Replace existing configuration? [y/N] ').lower() != 'y':
        return False
    default_gray = os.environ.get('GRAY_BIN', 'gray')
    gray = shutil.which(input(f'gray binary [{default_gray}]: ').strip() or default_gray)
    if not gray:
        raise ValueError('Install gray first; executable not found')
    default_home = os.environ.get('GRAY_HOME', '~/.gray')
    gray_home = Path(input(f'Gray provider home [{default_home}]: ').strip() or default_home).expanduser().resolve()
    provider = json.loads((gray_home / 'config.json').read_text())
    if not provider.get('model'):
        raise ValueError('Configure a model in gray before setup')
    token = getpass.getpass('Discord BOT token (hidden): ').strip()
    async with client() as bot:
        await asyncio.wait_for(bot.login(token), 30)
        print('Invite your bot:', invite(bot.user.id))
        print('Enable Message Content Intent in the Discord developer portal.')
        input('Press Enter after inviting the bot and enabling the intent. ')
        pairing = Pairing()
        print('DM this one-time code to the bot within five minutes:', pairing.code)
        loop = asyncio.get_running_loop()
        paired = loop.create_future()

        @bot.event
        async def on_message(message):
            if not message.author.bot and message.guild is None and pairing.accept(message.content.strip()):
                if not paired.done():
                    paired.set_result((str(message.author.id), str(message.channel.id)))

        connection = asyncio.create_task(bot.connect(reconnect=True))
        async def wait_pair():
            done, _ = await asyncio.wait([paired, connection], timeout=300, return_when=asyncio.FIRST_COMPLETED)
            if paired in done:
                return paired.result()
            if connection in done:
                connection.result()
            raise ValueError('Pairing failed or expired; check intent and DM permissions')
        try:
            owner, dm = await wait_pair()
        finally:
            await bot.close()
            connection.cancel()
            await asyncio.gather(connection, return_exceptions=True)
    print('Candidate owner ID:', owner)
    if input('Confirm this is your Discord account? [y/N] ').lower() != 'y':
        raise ValueError('Pairing not confirmed; nothing saved')
    channel = input(f'Home channel ID [your DM: {dm}]: ').strip() or dm
    if not snowflake(channel):
        raise ValueError('Invalid channel ID')
    save_config(path, dict(token=token, owner_id=owner, channel_id=channel,
                           gray_bin=str(Path(gray).absolute()), gray_home=str(gray_home),
                           workdir=str(path.parent.resolve())))
    print('Configuration saved privately.')
    return True
