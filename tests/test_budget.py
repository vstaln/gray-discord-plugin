import tempfile
import unittest
from pathlib import Path
from gray_discord.budget import Budget, BudgetBlocked


class BudgetTests(unittest.TestCase):
    def test_reservations_survive_restart_and_never_treat_unknown_as_free(self):
        with tempfile.TemporaryDirectory() as tmp:
            policy = dict(daily_usd=1, turn_usd=0.6, input_per_million=1, output_per_million=2, model='fixture')
            budget = Budget(Path(tmp)/'budget.sqlite')
            with self.assertRaises(BudgetBlocked):
                budget.reserve('unknown', None, 'fixture')
            with self.assertRaises(BudgetBlocked):
                budget.reserve('wrong-model', policy, 'other')
            budget.reserve('one', policy, 'fixture')
            with self.assertRaises(BudgetBlocked):
                Budget(budget.path).reserve('two', policy, 'fixture')
            budget.settle('one', dict(cost_micros=100000, usage_complete=True))
            budget.reserve('two', policy, 'fixture')
            budget.settle('two', dict(cost_micros=0, usage_complete=False))
            self.assertEqual(budget.total(), 700000)

    def test_invalid_prices_fail_closed_and_overspend_is_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            budget=Budget(Path(tmp)/'ledger.sqlite')
            policy=dict(daily_usd=1, turn_usd=0.5, input_per_million=1, output_per_million=1, model='fixture')
            for value in (-1, float('nan'), float('inf')):
                with self.assertRaises(BudgetBlocked):
                    budget.reserve('bad', {**policy, 'output_per_million':value}, 'fixture')
            budget.reserve('one',policy,'fixture')
            budget.settle('one',dict(cost_micros=1500000,usage_complete=True))
            self.assertEqual(budget.total(),1500000)
            with self.assertRaises(BudgetBlocked):
                budget.reserve('two',policy,'fixture')

    def test_unsettled_old_reservations_still_block_new_day(self):
        with tempfile.TemporaryDirectory() as tmp:
            budget=Budget(Path(tmp)/'ledger.sqlite')
            policy=dict(daily_usd=1,turn_usd=0.6,input_per_million=1,output_per_million=1,model='fixture')
            budget.reserve('yesterday',policy,'fixture')
            with budget.connection() as db:
                db.execute("UPDATE reservations SET day='2000-01-01'")
            with self.assertRaises(BudgetBlocked):
                budget.reserve('today',policy,'fixture')
