"""Opt-in capability sharing. Session/work state stays separate; this is not a sandbox."""
import hashlib
import json
from pathlib import Path
import shlex


def absolute(value):
    path = Path(value)
    if not path.is_absolute() or not path.exists():
        raise ValueError('Shared capability paths must be absolute and exist')
    return path


def prepare(config, home):
    skills = home / 'skills'
    skills.mkdir(exist_ok=True, mode=0o700)
    desired = {}
    for value in config.get('shared_skills', []):
        path = absolute(value)
        if not (path / 'SKILL.md').is_file():
            raise ValueError('Shared skill must be a directory containing SKILL.md')
        desired[hashlib.sha256(str(path).encode()).hexdigest()] = path
    for link in skills.iterdir():
        if link.is_symlink() and link.name not in desired:
            link.unlink()
    for name, path in desired.items():
        link = skills / name
        if not link.is_symlink():
            if link.exists():
                raise ValueError('Shared skill path collision; refusing to overwrite')
            link.symlink_to(path, target_is_directory=True)

    context = []
    total = 0
    for value in config.get('shared_context', []):
        path = absolute(value)
        with path.open('rb') as stream:
            raw = stream.read(128 * 1024 + 1)
        total += len(raw)
        if total > 128 * 1024:
            raise ValueError('Shared context exceeds 128 KiB; select smaller files')
        context.append(raw.decode('utf-8'))
    marker = home / 'shared-context.json'
    if context:
        (home / 'AGENTS.md').write_text('You are gray. Follow the user request. Never disclose secrets.\n\n' + '\n\n'.join(context))
        marker.write_text('{}\n')
    elif marker.exists():
        (home / 'AGENTS.md').unlink(missing_ok=True)
        marker.unlink()

    profile = ''
    for index, argv in enumerate(config.get('shared_plugins', [])):
        if not isinstance(argv, list) or not argv or any(not isinstance(a, str) or '\x00' in a for a in argv):
            raise ValueError('Shared plugins require nonempty argv arrays')
        if not argv[0]:
            raise ValueError('Shared plugin executable is empty')
        launcher = home / f'shared-plugin-{index}'
        launcher.write_text('#!/bin/sh\nexec ' + shlex.join(argv) + '\n')
        launcher.chmod(0o700)
        profile += '  - sidecar: ' + json.dumps(str(launcher)) + '\n'
    return profile
