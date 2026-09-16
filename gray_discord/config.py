"""Private configuration. Never format configuration objects in error messages."""
import json
import os
import tempfile
from pathlib import Path


def default_path():
    return Path.home() / '.config' / 'gray-discord' / 'config.json'


def atomic_json(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, tmp = tempfile.mkstemp(dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as f:
            json.dump(data, f, indent=2)
            f.write('\n')
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def snowflake(value):
    return isinstance(value, str) and value.isascii() and value.isdigit() and 0 < int(value) < 2**64


def validate_config(data):
    if not isinstance(data, dict):
        raise ValueError('Configuration must be an object')
    if not isinstance(data.get('token'), str) or not data['token'].strip():
        raise ValueError('Bot token is missing; run setup')
    for key in ('owner_id', 'channel_id'):
        if not snowflake(data.get(key)):
            raise ValueError(f'{key} must be a Discord ID')
    for key in ('gray_bin', 'gray_home', 'workdir'):
        if not isinstance(data.get(key), str) or not Path(data[key]).is_absolute():
            raise ValueError(f'{key} must be an absolute path')
    return data


def load_config(path):
    try:
        data = json.loads(Path(path).read_text())
    except (OSError, ValueError):
        raise ValueError('Configuration missing or invalid; run setup') from None
    return validate_config(data)


def save_config(path, data):
    atomic_json(path, validate_config(data))
