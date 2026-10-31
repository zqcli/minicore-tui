"""Test-side protocol/fixture checks; not substitutes for real-chain cases."""

import json
import socket
import unittest
from unittest.mock import patch

from phone_client import Phone
from real_chain_scenarios import PAGINATED_BYTES, ScenarioPlan


class ScenarioTests(unittest.TestCase):
    def test_long_fixture_is_legal_max_and_requires_chunked_default_page(self):
        plan = ScenarioPlan('pagination', 2)
        for index in range(2):
            body = plan.text(index)
            self.assertEqual(len(body.encode()), PAGINATED_BYTES)
            item = {'item': {'type': 'assistant', 'data': {'loop_id': 'fixture', 'request_index': 0,
                    'content': [{'type': 'text', 'text': body}]}}, 'timestamp': None, 'tool_summaries': []}
            self.assertGreater(len(json.dumps(item, ensure_ascii=False).encode()), 256 * 1024)
        self.assertNotEqual(plan.text(0), plan.text(1))

    def test_paced_phone_reads_only_after_its_real_byte_budget_interval(self):
        server, client = socket.socketpair()
        peer = Phone.__new__(Phone)
        peer.socket, peer.buffer = client, b''
        peer.paused = peer.closed = False
        peer.read_rate, peer.next_read = 64, 0
        peer.read_calls = peer.throttled_polls = peer.bytes = peer.messages = 0
        peer.acks = {}
        def send(operation):
            body = json.dumps({'t': 'ack', 'id': operation, 'ok': True}).encode()
            server.sendall(bytes([0x81, len(body)]) + body)
        try:
            with patch('phone_client.time.monotonic', return_value=0):
                send('first')
                peer.poll()
                self.assertIn('first', peer.acks)
                send('second')
                peer.poll()
                self.assertNotIn('second', peer.acks)
                self.assertEqual(peer.read_calls, 1)
                self.assertEqual(peer.throttled_polls, 1)
                self.assertGreater(peer.next_read, 0)
            with patch('phone_client.time.monotonic', return_value=peer.next_read):
                peer.poll()
                self.assertIn('second', peer.acks)
                self.assertEqual(peer.read_calls, 2)
        finally:
            server.close()
            client.close()

    def test_paced_fixture_keeps_a_bounded_exact_unicode_body(self):
        for case in ('paced-terminal', 'paced-phone'):
            self.assertEqual(len(ScenarioPlan(case, 1).text(0).encode()), 16 * 1024)

    def test_phone_uses_server_snapshot_target_and_applies_real_wire_blocks(self):
        peer = Phone.__new__(Phone)
        peer.messages = peer.snapshots = 0
        peer.acks = {}
        peer.message({'t': 'snapshot', 'view_epoch': 'view_1', 'state': {
            'session': {'id': 'ses_A', 'load': 'load_A'}}, 'blocks': [
                {'type': 'assistant', 'text': 'first', 'reasoning': ''}]})
        self.assertEqual(peer.target(), {'view_epoch': 'view_1', 'session_id': 'ses_A', 'load': 'load_A'})
        peer.message({'t': 'blocks', 'len': 2, 'upserts': [
            {'i': 0, 'append': {'text': ' tail'}},
            {'i': 1, 'block': {'type': 'turn_end', 'loop_id': 'loop_A', 'saving': 'saved'}}]})
        self.assertEqual(peer.blocks[0]['text'], 'first tail')
        self.assertEqual(peer.blocks[1]['saving'], 'saved')
        peer.message({'t': 'blocks', 'len': 1, 'upserts': [
            {'i': 0, 'block': {'type': 'assistant', 'text': 'canonical', 'reasoning': ''}}]})
        self.assertEqual(peer.blocks, [{'type': 'assistant', 'text': 'canonical', 'reasoning': ''}])
        peer.message({'t': 'ack', 'id': 'operation_A', 'ok': True})
        with self.assertRaises(AssertionError):
            peer.message({'t': 'ack', 'id': 'operation_A', 'ok': True})


if __name__ == '__main__':
    unittest.main(verbosity=2)
