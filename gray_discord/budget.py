"""Persistent conservative reservations. Not a provider-side invoice guarantee."""
from contextlib import contextmanager
from decimal import Decimal, InvalidOperation, ROUND_CEILING
import datetime
import os
from pathlib import Path
import sqlite3


class BudgetBlocked(ValueError):
    pass


def amount(value, scale=1):
    try:
        number = Decimal(str(value))
        if not number.is_finite() or number < 0:
            raise ValueError()
        return int((number * scale).to_integral_value(rounding=ROUND_CEILING))
    except (ValueError, InvalidOperation, OverflowError):
        raise BudgetBlocked('Budget and prices must be finite nonnegative numbers') from None


def validate(policy, model):
    if not isinstance(policy, dict) or policy.get('model') != model:
        raise BudgetBlocked('Configure an explicit budget and prices for the selected model')
    for key in ('input_per_million', 'output_per_million'):
        amount(policy.get(key))
    daily, turn = (amount(policy.get(key), 1000000) for key in ('daily_usd', 'turn_usd'))
    if daily <= 0 or turn <= 0 or turn > daily:
        raise BudgetBlocked('Budget requires 0 < turn_usd <= daily_usd')
    return daily, turn


class Budget:
    def __init__(self, path):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd = os.open(self.path, os.O_CREAT | os.O_WRONLY, 0o600)
        os.close(fd)
        with self.connection() as db:
            db.execute('CREATE TABLE IF NOT EXISTS reservations (id TEXT PRIMARY KEY, day TEXT, amount INTEGER, settled INTEGER DEFAULT 0)')

    @contextmanager
    def connection(self):
        db = sqlite3.connect(self.path, timeout=10)
        try:
            with db:
                yield db
        finally:
            db.close()

    def reserve(self, id, policy, model):
        daily, turn = validate(policy, model)
        day = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
        with self.connection() as db:
            db.execute('BEGIN IMMEDIATE')
            if db.execute('SELECT 1 FROM reservations WHERE id=?', (id,)).fetchone():
                raise BudgetBlocked('This run already has a budget reservation; do not replay it')
            used = db.execute('SELECT coalesce(sum(amount),0) FROM reservations WHERE day=? OR settled=0', (day,)).fetchone()[0]
            if used + turn > daily:
                raise BudgetBlocked('Daily budget exhausted or reserved by unfinished work')
            db.execute('INSERT INTO reservations(id,day,amount) VALUES(?,?,?)', (id, day, turn))
        return turn

    def settle(self, id, accounting):
        # Missing usage (crash, cancellation, absent report) retains the entire
        # reservation. Never release an uncertain reservation as if it were free.
        cost = accounting.get('cost_micros')
        if accounting.get('usage_complete') is not True or type(cost) is not int or cost < 0:
            return
        with self.connection() as db:
            db.execute('UPDATE reservations SET amount=?,settled=1 WHERE id=? AND settled=0', (cost, id))

    def total(self):
        with self.connection() as db:
            return db.execute('SELECT coalesce(sum(amount),0) FROM reservations').fetchone()[0]
