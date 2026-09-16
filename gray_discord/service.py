"""Plugin-owned systemd user lifecycle. Never places tokens in unit files."""
import os
import subprocess
import sys
from pathlib import Path

NAME = 'gray-discord-plugin.service'


def unit_path():
    return Path(os.environ.get('XDG_CONFIG_HOME', Path.home() / '.config')) / 'systemd/user' / NAME


def quote(value):
    if any(c in str(value) for c in '\n\r\x00'):
        raise ValueError('Invalid service path')
    return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%').replace('$', '$$') + '"'


def unit(config_path):
    return ('[Unit]\nDescription=gray Discord plugin\nAfter=network-online.target\n'
            '[Service]\nType=simple\nExecStart=' + ' '.join(quote(x) for x in
             [sys.executable, '-m', 'gray_discord', '--config', Path(config_path).resolve(), 'run']) +
            '\nRestart=on-failure\nRestartSec=10\nUMask=0077\nKillMode=control-group\n'
            'TimeoutStopSec=15\n[Install]\nWantedBy=default.target\n')


def control(*args):
    result = subprocess.run(['systemctl', '--user', *args], check=False)
    if result.returncode:
        raise ValueError('systemctl failed; check the user session and service status')


def install(path):
    target = unit_path()
    body = unit(path)
    if target.exists() and target.read_text() != body:
        raise ValueError('A different plugin service already exists; uninstall it first')
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(body)
    control('daemon-reload')
    control('enable', '--now', NAME)


def uninstall():
    if not unit_path().exists():
        return
    control('disable', '--now', NAME)
    unit_path().unlink()
    control('daemon-reload')
