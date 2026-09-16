"""Gray subprocesses with isolated conversation homes; never guess a shared ID."""
import asyncio
import fcntl
import hashlib
import json
import os
import signal
import sys
from pathlib import Path
from .config import atomic_json


def final_reply(rows, start):
    for row in reversed(rows[start:]):
        msg = row.get('message', {})
        if msg.get('role') == 'assistant':
            text = ''.join(b.get('text', '') for b in msg.get('content', []) if b.get('type') == 'text')
            if text.strip():
                return text
    raise ValueError('gray produced no final assistant reply')


def session_rows(path):
    if path.stat().st_size > 32 * 1024 * 1024:
        raise ValueError('Session exceeds the 32 MiB read limit')
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


async def run_gray(config, config_path, conversation, prompt, timeout=600):
    if not prompt.strip() or len(prompt) > 32000:
        raise ValueError('Prompt must contain 1–32000 characters')
    key = hashlib.sha256(conversation.encode()).hexdigest()
    home = Path(config_path).parent / 'conversations' / key
    home.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (home / 'run.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('This conversation is busy') from None
        provider = json.loads((Path(config['gray_home']) / 'config.json').read_text())
        atomic_json(home / 'config.json', provider)
        # Dedicated workdir/profile: no lockfile plugins that might start another gateway.
        work = home / 'work'
        work.mkdir(exist_ok=True, mode=0o700)
        launcher = home / 'discord-sidecar'
        import shlex
        launcher.write_text('#!/bin/sh\nexec ' + shlex.join([
            sys.executable, '-m', 'gray_discord', '--config', str(Path(config_path).resolve()), 'sidecar'
        ]) + '\n')
        launcher.chmod(0o700)
        (work / 'gray.yml').write_text('plugins:\n  - builtin: tools-minimal\n  - sidecar: ' + str(launcher) + '\n')
        sessions = home / 'sessions'
        files = list(sessions.glob('*.jsonl')) if sessions.exists() else []
        if len(files) > 1:
            raise ValueError('Ambiguous conversation store; refusing to select a session')
        args = [config['gray_bin'], '-p', prompt]
        if files:
            args += ['--session', files[0].stem]
        env = {k: v for k, v in os.environ.items() if not k.startswith(('GRAY_', 'DISCORD_', 'OPENAI_'))}
        env.update(GRAY_HOME=str(home), GRAY_SHOW_REASONING='0', GRAY_MAX_WALL_SECS=str(timeout))
        # Do not collect CLI stdout: it can contain tool output or reasoning.
        child = await asyncio.create_subprocess_exec(
            *args, cwd=work, env=env, stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL,
            start_new_session=True)
        try:
            await asyncio.wait_for(child.wait(), timeout)
        except (TimeoutError, asyncio.CancelledError):
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            await child.wait()
            raise
        if child.returncode != 0:
            raise ValueError(f'gray exited with code {child.returncode}; not retrying possible side effects')
        files = list(sessions.glob('*.jsonl'))
        if len(files) != 1:
            raise ValueError('gray did not save exactly one conversation session')
        rows = session_rows(files[0])
        # Match the newly submitted user message, even after compaction. Ignore tool results.
        starts = [i for i, r in enumerate(rows) if r.get('message', {}).get('role') == 'user'
                  and any(b.get('type') == 'text' and b.get('text') == prompt
                          for b in r['message'].get('content', []))]
        if not starts:
            raise ValueError('Cannot identify this turn in the saved session; reply withheld')
        return final_reply(rows, starts[-1] + 1)
