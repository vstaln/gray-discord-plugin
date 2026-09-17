"""Gray subprocesses with isolated conversation homes; never guess a shared ID."""
import asyncio
import fcntl
import hashlib
import json
import os
import signal
import sys
import uuid
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


async def run_gray(config, config_path, conversation, prompt, timeout=None, progress=None, receipt=None):
    timeout = config.get("timeout_seconds", 600) if timeout is None else timeout
    if timeout <= 0:
        raise ValueError("Timeout must be positive")
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
        from .capabilities import prepare
        shared = prepare(config, home)
        (work / 'gray.yml').write_text('plugins:\n  - builtin: tools-minimal\n  - sidecar: ' + json.dumps(str(launcher)) + '\n' + shared)
        # Bound project discovery at the dedicated work directory.
        (work / '.git').mkdir(exist_ok=True)
        sessions = home / 'sessions'
        files = list(sessions.glob('*.jsonl')) if sessions.exists() else []
        if len(files) > 1:
            raise ValueError('Ambiguous conversation store; refusing to select a session')
        state_path = home / 'session.json'
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        args = [config['gray_bin'], '-p', prompt, '--json', '--max-requests', str(config.get('max_requests', 32))]
        policy = config.get('budget')
        ledger = None
        reservation = uuid.uuid4().hex
        if policy is not None or config.get('budget_required', False):
            from .budget import Budget
            ledger = Budget(Path(config_path).parent / 'budget.sqlite')
            ledger.reserve(reservation, policy, provider.get('model'))
            args += ['--max-cost-usd', str(policy['turn_usd']),
                     '--input-price', str(policy['input_per_million']),
                     '--output-price', str(policy['output_per_million'])]
        session_id = state.get('session_id') or (files[0].stem if files else None)
        if session_id:
            args += ['--session', session_id]
        env = {k: v for k, v in os.environ.items() if not k.startswith(('GRAY_', 'DISCORD_', 'OPENAI_'))}
        env.update(GRAY_HOME=str(home), GRAY_SKILLS_ONLY='1', GRAY_SHOW_REASONING='0', GRAY_MAX_WALL_SECS=str(int(timeout) or 1))
        # NDJSON is the transport contract, not rendered stdout or session JSONL.
        child = await asyncio.create_subprocess_exec(
            *args, cwd=work, env=env, stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
            limit=1024 * 1024, start_new_session=True)
        final = None
        protocol_error = None
        turn_id = None

        async def consume():
            nonlocal final, protocol_error, turn_id
            async for line in child.stdout:
                try:
                    row = json.loads(line)
                    if not isinstance(row, dict) or row.get('protocol') != 1:
                        raise ValueError('Unsupported agent JSON protocol')
                    if not row.get('turn_id') or (turn_id and turn_id != row['turn_id']):
                        raise ValueError('Inconsistent agent turn ID')
                    turn_id = row['turn_id']
                    if final is not None:
                        raise ValueError('Data after terminal agent result')
                    sid = row.get('session_id')
                    if sid:
                        import uuid
                        uuid.UUID(sid)
                        if state.get('session_id') and sid != state['session_id']:
                            raise ValueError('Agent returned a different session')
                        state['session_id'] = sid
                        atomic_json(state_path, state)
                    if row['type'] in ('result', 'error'):
                        if final is not None:
                            raise ValueError('Multiple agent results')
                        final = row
                    elif row['type'] == 'progress':
                        if progress:
                            await progress(row.get('phase', 'working'))
                    else:
                        raise ValueError('Unknown agent record type')
                except (ValueError, KeyError, TypeError):
                    protocol_error = 'Invalid agent JSON output; upgrade gray to a compatible version'
            await child.wait()
        try:
            await asyncio.wait_for(consume(), timeout)
        except BaseException:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            await child.wait()
            raise
        if final:
            if receipt is not None:
                receipt.update(final)
            if ledger and not protocol_error:
                ledger.settle(reservation, final.get('accounting', {}))
        if child.returncode != 0:
            raise ValueError(f'gray exited with code {child.returncode}; not retrying possible side effects')
        if protocol_error:
            raise ValueError(protocol_error)
        if not final or final['type'] != 'result' or not final.get('session_id'):
            raise ValueError('Agent did not return a completed result; actions may already have occurred')
        text = final.get('text')
        if not isinstance(text, str) or not text.strip():
            raise ValueError('Agent returned no final answer')
        if receipt is not None:
            receipt.update(final)
        return text
