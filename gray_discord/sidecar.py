"""NDJSON protocol 1.1. No credentials required for the manifest handshake."""
import asyncio
import json
import sys
from .config import load_config
from .transport import rest_send

MANIFEST = dict(name='discord', version='0.1.0', protocol='1.1', commands=[],
                hooks=['prompt/context'], tools=[dict(name='discord_send',
                description='Send text to the owner-configured Discord channel.',
                parameters=dict(type='object', properties=dict(content=dict(type='string')),
                                required=['content']))])


async def dispatch(method, params, path):
    if method == 'plugin/manifest':
        return MANIFEST
    if method == 'prompt/context':
        return dict(text='discord_send sends to your configured home channel; never send secrets.')
    if method == 'tool/call':
        try:
            if params.get('name') != 'discord_send':
                raise ValueError('Unknown tool')
            await asyncio.wait_for(rest_send(load_config(path), params.get('args', {}).get('content')), 20)
            return dict(content='Sent to the configured Discord channel.')
        except Exception:
            return dict(content='Discord delivery failed. Run gray-discord doctor; do not blindly retry partial sends.', is_error=True)
    return dict(error='Unsupported method')


def serve(path):
    while True:
        line = sys.stdin.buffer.readline(256 * 1024 + 1)
        if not line:
            return
        if len(line) > 256 * 1024:
            raise ValueError('Wire frame exceeds 256 KiB')
        try:
            req = json.loads(line)
        except ValueError:
            continue
        if not isinstance(req, dict):
            continue
        if req.get('method') == 'plugin/shutdown':
            return
        if not isinstance(req.get('id'), int):
            continue
        params = req.get('params', {})
        if not isinstance(params, dict):
            params = {}
        result = asyncio.run(dispatch(req.get('method'), params, path))
        print(json.dumps(dict(id=req['id'], result=result)), flush=True)
