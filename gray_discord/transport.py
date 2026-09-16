"""Discord SDK transport; safe mentions and bounded send sizes as in Hermes."""
import discord
from .hermes_text import split_message


def client():
    intents = discord.Intents.default()
    intents.message_content = True
    intents.dm_messages = True
    # Stricter than Hermes: even user/reply pings are disabled.
    return discord.Client(intents=intents, allowed_mentions=discord.AllowedMentions.none())


async def send(channel, text):
    if not isinstance(text, str) or not text.strip() or len(text) > 20000:
        raise ValueError('Reply must contain 1–20000 characters')
    for chunk in split_message(text):
        await channel.send(chunk, allowed_mentions=discord.AllowedMentions.none())


async def rest_send(config, text):
    """REST only, with cleanup even when authentication or sending fails."""
    if not isinstance(text, str) or not text.strip() or len(text) > 20000:
        raise ValueError('Content must contain 1–20000 characters')
    async with client() as bot:
        await bot.login(config['token'])
        channel = await bot.fetch_channel(int(config['channel_id']))
        await send(channel, text)
