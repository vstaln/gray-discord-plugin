import tempfile
import unittest
from pathlib import Path
from gray_discord.durable import Store


class DurableTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'queue.sqlite'
        self.store = Store(self.path)

    def test_deduplication_serial_claim_and_restart(self):
        self.assertTrue(self.store.enqueue('1', '42', 'one'))
        self.assertFalse(self.store.enqueue('1', '42', 'duplicate'))
        self.store.enqueue('2', '42', 'two')
        self.store.enqueue('3', '43', 'three')
        first = self.store.claim()
        self.assertEqual(first['id'], '1')
        self.assertEqual(self.store.claim()['id'], '3')
        self.assertIsNone(self.store.claim())
        restarted = Store(self.path)
        restarted.recover()
        self.assertEqual(restarted.get('1')['state'], 'uncertain')
        self.assertEqual(restarted.claim()['id'], '2')

    def test_delivery_retry_never_reruns_generation(self):
        self.store.enqueue('1', '42', 'one')
        item = self.store.claim()
        self.store.complete(item['id'], 'a' * 2100, {})
        self.assertIsNone(self.store.claim())
        part = self.store.next_delivery(now=0)
        self.store.ack(part['id'], part['part'], 'discord-message-1')
        remaining = self.store.next_delivery(now=0)
        self.assertEqual(remaining['part'], 1)
        self.store.delivery_failed(remaining, 'network', now=0)
        self.assertIsNone(self.store.next_delivery(now=0))
        restarted = Store(self.path)
        restarted.recover()
        self.assertIsNone(restarted.claim())
        part = restarted.next_delivery(now=1000)
        self.assertEqual(part['part'], 1)
        restarted.ack(part['id'], part['part'], 'discord-message-2')
        self.assertEqual(restarted.get('1')['state'], 'sent')

    def test_online_schedule_and_atomic_due_enqueue(self):
        other = Store(self.path)
        self.store.schedule_add('job', 60, 'check', now=0)
        self.assertEqual(other.schedules()[0]['id'], 'job')
        other.enqueue_due('42', now=61)
        self.store.enqueue_due('42', now=61)
        self.assertIsNotNone(self.store.claim())
        self.assertIsNone(self.store.claim())
        other.schedule_remove('job')
        self.assertEqual(self.store.schedules(), [])

    def test_cancel_and_queue_capacity(self):
        self.store.enqueue('1', '42', 'one', capacity=1)
        with self.assertRaises(ValueError):
            self.store.enqueue('2', '42', 'two', capacity=1)
        self.store.cancel('1')
        self.assertIsNone(self.store.claim())
        self.assertEqual(self.store.get('1')['state'], 'cancelled')
