"""Transactional inbox/outbox. Never retry uncertain agent actions automatically."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import sqlite3
import time
from .hermes_text import split_message


class Store:
    def __init__(self, path):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd = os.open(self.path, os.O_CREAT | os.O_WRONLY, 0o600)
        os.close(fd)
        with self.connection() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS inbox (
                    id TEXT PRIMARY KEY, channel TEXT NOT NULL, conversation TEXT NOT NULL,
                    prompt TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued',
                    created REAL NOT NULL, error TEXT, receipt TEXT, cancel INTEGER DEFAULT 0);
                CREATE TABLE IF NOT EXISTS outbox (
                    id TEXT NOT NULL, part INTEGER NOT NULL, content TEXT NOT NULL,
                    message_id TEXT, attempts INTEGER NOT NULL DEFAULT 0,
                    next_at REAL NOT NULL DEFAULT 0, error TEXT,
                    PRIMARY KEY(id,part));
                CREATE TABLE IF NOT EXISTS schedules (
                    id TEXT PRIMARY KEY, interval INTEGER NOT NULL, prompt TEXT NOT NULL,
                    next_at REAL NOT NULL, status TEXT NOT NULL DEFAULT 'scheduled');
                CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
            ''')

    @contextmanager
    def connection(self):
        db = sqlite3.connect(self.path, timeout=10)
        db.row_factory = sqlite3.Row
        try:
            db.execute('PRAGMA synchronous=FULL')
            db.execute('BEGIN IMMEDIATE')
            yield db
            db.commit()
        except BaseException:
            db.rollback()
            raise
        finally:
            db.close()

    def enqueue(self, id, channel, prompt, conversation=None, capacity=1000):
        if not isinstance(prompt, str) or not prompt.strip() or len(prompt) > 32000:
            raise ValueError('Prompt must contain 1–32000 characters')
        with self.connection() as db:
            if db.execute('SELECT 1 FROM inbox WHERE id=?', (id,)).fetchone():
                return False
            pending = db.execute("SELECT count(*) FROM inbox WHERE state IN ('queued','running','delivery')").fetchone()[0]
            if pending >= capacity:
                raise ValueError('Queue is full; message was not accepted')
            db.execute('INSERT INTO inbox(id,channel,conversation,prompt,created) VALUES(?,?,?,?,?)',
                       (id, channel, conversation or 'chat:' + channel, prompt, time.time()))
        return True

    def claim(self):
        with self.connection() as db:
            item = db.execute("""SELECT * FROM inbox q WHERE state='queued' AND cancel=0 AND NOT EXISTS
                (SELECT 1 FROM inbox r WHERE r.conversation=q.conversation AND r.state='running')
                ORDER BY created,id LIMIT 1""").fetchone()
            if item:
                db.execute("UPDATE inbox SET state='running' WHERE id=?", (item['id'],))
                return dict(item)

    def get(self, id):
        with self.connection() as db:
            row = db.execute('SELECT * FROM inbox WHERE id=?', (id,)).fetchone()
            return dict(row) if row else None

    def items(self):
        with self.connection() as db:
            return [dict(r) for r in db.execute('SELECT id,channel,state,error FROM inbox ORDER BY created DESC LIMIT 100')]

    def cancel(self, id):
        with self.connection() as db:
            cursor = db.execute("""UPDATE inbox SET cancel=1,
                state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END
                WHERE id=? AND state IN ('queued','running')""", (id,))
            if not cursor.rowcount:
                raise ValueError('No queued/running item with that ID')

    def complete(self, id, text, receipt):
        if not text.strip() or len(text) > 200000:
            raise ValueError('Completed answer must contain 1–200000 characters')
        with self.connection() as db:
            changed = db.execute("UPDATE inbox SET state='delivery',receipt=? WHERE id=? AND state='running'",
                                 (json.dumps(receipt), id)).rowcount
            if not changed:
                raise ValueError('Item is not running')
            db.executemany('INSERT INTO outbox(id,part,content) VALUES(?,?,?)',
                           [(id, part, chunk) for part, chunk in enumerate(split_message(text))])

    def fail(self, id, code):
        # Code is a controlled category, not an exception body or a provider response.
        if code not in ('interrupted', 'cancelled', 'timeout', 'agent_failed', 'budget_blocked'):
            code = 'agent_failed'
        with self.connection() as db:
            db.execute("UPDATE inbox SET state='uncertain',error=? WHERE id=? AND state='running'", (code, id))
            db.execute('INSERT OR IGNORE INTO outbox(id,part,content) VALUES(?,0,?)',
                       (id, f'Turn {id}: {code}. Actions may already have happened; no automatic retry.'))

    def recover(self):
        with self.connection() as db:
            ids = [r[0] for r in db.execute("SELECT id FROM inbox WHERE state='running'")]
        for id in ids:
            self.fail(id, 'interrupted')

    def next_delivery(self, now=None):
        now = time.time() if now is None else now
        with self.connection() as db:
            row = db.execute('''SELECT o.*,i.channel FROM outbox o JOIN inbox i ON i.id=o.id
                WHERE message_id IS NULL AND next_at<=? AND NOT EXISTS
                (SELECT 1 FROM outbox p WHERE p.id=o.id AND p.part<o.part AND p.message_id IS NULL)
                ORDER BY i.created,o.part LIMIT 1''', (now,)).fetchone()
            return dict(row) if row else None

    def ack(self, id, part, message_id):
        with self.connection() as db:
            db.execute('UPDATE outbox SET message_id=?,error=NULL WHERE id=? AND part=?', (message_id, id, part))
            pending = db.execute('SELECT 1 FROM outbox WHERE id=? AND message_id IS NULL', (id,)).fetchone()
            if not pending:
                db.execute("UPDATE inbox SET state='sent' WHERE id=? AND state='delivery'", (id,))

    def delivery_failed(self, part, code, now=None):
        now = time.time() if now is None else now
        with self.connection() as db:
            db.execute('UPDATE outbox SET attempts=attempts+1,next_at=?,error=? WHERE id=? AND part=?',
                       (now + min(3600, 2 ** min(part['attempts'] + 1, 12)), code, part['id'], part['part']))

    def schedule_add(self, id, interval, prompt, now=None):
        if interval < 60 or not prompt.strip() or len(prompt) > 32000:
            raise ValueError('Interval must be >=60s and prompt 1–32000 characters')
        with self.connection() as db:
            db.execute('INSERT INTO schedules(id,interval,prompt,next_at) VALUES(?,?,?,?)',
                       (id, interval, prompt, (time.time() if now is None else now) + interval))

    def schedules(self):
        with self.connection() as db:
            return [dict(r) for r in db.execute('SELECT * FROM schedules ORDER BY id')]

    def schedule_remove(self, id):
        with self.connection() as db:
            if not db.execute('DELETE FROM schedules WHERE id=?', (id,)).rowcount:
                raise ValueError('Schedule not found')

    def enqueue_due(self, channel, now=None):
        now = time.time() if now is None else now
        with self.connection() as db:
            for job in db.execute('SELECT * FROM schedules WHERE next_at<=?', (now,)).fetchall():
                id = f"job:{job['id']}:{job['next_at']}"
                db.execute('INSERT OR IGNORE INTO inbox(id,channel,conversation,prompt,created) VALUES(?,?,?,?,?)',
                           (id, channel, 'job:' + job['id'], job['prompt'], now))
                db.execute("UPDATE schedules SET next_at=?,status='queued' WHERE id=?", (now + job['interval'], job['id']))

    def migrate_jobs(self, path):
        path = Path(path)
        with self.connection() as db:
            if db.execute("SELECT 1 FROM meta WHERE key='jobs_migrated'").fetchone():
                return
            jobs = json.loads(path.read_text()) if path.exists() else []
            for job in jobs:
                if job['interval'] < 60:
                    raise ValueError('Invalid legacy schedule interval')
                db.execute('INSERT OR IGNORE INTO schedules(id,interval,prompt,next_at,status) VALUES(?,?,?,?,?)',
                           (job['id'], job['interval'], job['prompt'], job['next_at'], job['status']))
            db.execute("INSERT INTO meta VALUES('jobs_migrated','1')")
