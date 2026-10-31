"""Consumer negative controls; synthetic observations are NOT real-chain evidence."""

import copy
import json
import tempfile
import unittest
from pathlib import Path

from canonical_observer import ObservationError, Trace, TUI_COUNTERS, body_digest, text_part, tool_part


def inventory(turn, input_id, text, start=0):
    rows = [{'kind': 'user', 'loop_id': turn['loop_id'], 'input_id': input_id,
             'user_kind': 'prompt', 'request_index': None, 'body': body_digest(text)}]
    for part in turn['parts']:
        row = dict(part, loop_id=turn['loop_id'])
        if part['kind'] == 'assistant':
            row['body'] = {field: row.pop(field) for field in ('bytes', 'sha256')}
            row['preserved_channel'] = False
        rows.append(row)
    rows.append({'kind': 'turn_end', 'loop_id': turn['loop_id'], 'saving': 'saved',
                 'outcome': 'completed', 'canonical': True})
    for index, row in enumerate(rows, start):
        row.update(block_index=index, preserved=False)
    return rows


def register_inputs(trace):
    trace.register_input(dict(session_id='ses_fixture', load='load_fixture', input_id='inp_fixture'), 'fixture prompt')
    trace.register_input(dict(session_id='ses_fixture', load='load_fixture', input_id='inp_second'), 'second prompt')
    trace.register_canonical(dict(session_id='ses_fixture', load='load_fixture', loop_id='loop_fixture'),
                             [tool_part('call_fixture'), text_part('a\r\nb', 1)])
    trace.register_canonical(dict(session_id='ses_fixture', load='load_fixture', loop_id='loop_second'),
                             [text_part('second canonical')])


def fixture():
    common = {"schema": 1, "session_id": "ses_fixture", "load": "load_fixture"}
    body = [tool_part("call_fixture"), text_part("a\r\nb", 1)]
    turn = {"loop_id": "loop_fixture", "parts": body,
            "persistence": "persisted", "persistence_uncertain": False}
    rows = [
        {"event": "observer_ready"},
        {**common, "event": "control_applied", "revision": 1, "queue_ids": [],
         "paused": False, "execution": {"kind": "none"}, "interaction": None,
         "observed_through": 1, "settled_through": 0, "block_reason": None},
        {**common, **turn, "event": "canonical_install", "result_id": "r1:fixture",
         "completion_seq": 1, "input_id": "inp_fixture", "read_item_count": 4, "recovery_status": "complete"},
        {**common, "event": "canonical_view", "turns": [turn],
         "inventory": inventory(turn, 'inp_fixture', 'fixture prompt'), "delivered_inputs": []},
        {**common, "event": "counters", "scope": "client_counters_and_session_gauges",
         "values": {field: 0 for field in TUI_COUNTERS}},
        {"event": "observer_end"},
    ]
    for index, row in enumerate(rows, 1):
        row.update(schema=1, event_seq=index, mono_ns=index)
    return rows, body


class ConsumerTests(unittest.TestCase):
    def check_rows(self, rows, expected=None):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "observer.jsonl"
            path.write_text("".join(json.dumps(row) + "\n" for row in rows))
            trace = Trace(path)
            register_inputs(trace)
            trace.poll()
            if expected is not None:
                self.assertIsNotNone(trace.expect_install(0, expected))
            return trace.finish(1)

    def test_complete_observation(self):
        rows, body = fixture()
        self.assertEqual(self.check_rows(rows, body)["canonical_installs_verified"], 1)

    def test_normalization_preserves_spaces_and_non_ascii(self):
        self.assertEqual(text_part("a\r\n雪"), text_part("a\n雪"))
        self.assertNotEqual(text_part("a  b"), text_part("a b"))

    def test_negative_controls(self):
        changes = {
            "wrong_hash": lambda r: r[2]["parts"][1].update(sha256="0" * 64),
            "missing_channel": lambda r: r[2]["parts"].pop(),
            "duplicate_channel": lambda r: r[2]["parts"].append(r[2]["parts"][1].copy()),
            "wrong_order": lambda r: r[2]["parts"].reverse(),
            "wrong_request": lambda r: r[2]["parts"][1].update(request_index=7),
            "wrong_tool": lambda r: r[2]["parts"][0].update(tool_call_id="other"),
            "wrong_state": lambda r: r[2]["parts"][0].update(state="failed"),
            "unsaved": lambda r: r[2].update(persistence="failed"),
            "uncertain_saved": lambda r: r[2].update(persistence_uncertain=True),
            "wrong_load": lambda r: r[2].update(load="other"),
            "final_body_drift": lambda r: r[3]["turns"][0]["parts"].pop(),
            "final_body_loss": lambda r: r[3].update(turns=[]),
            "duplicate_final_turn": lambda r: r[3]["turns"].append(r[3]["turns"][0]),
            "stale_revision": lambda r: r[1].update(revision=-1),
            "settled_ahead": lambda r: r[1].update(settled_through=2),
            "unknown_event": lambda r: r[4].update(event="unknown"),
            "missing_counters": lambda r: r[4].update(values={}),
            "sequence_gap": lambda r: r[2].update(event_seq=9),
            "clock_regression": lambda r: r[2].update(mono_ns=0),
        }
        for name, mutate in changes.items():
            with self.subTest(name=name):
                rows, body = fixture()
                # Break shared fixture references so final-view checks are independent.
                rows = json.loads(json.dumps(rows))
                mutate(rows)
                with self.assertRaises(ObservationError):
                    self.check_rows(rows, body)

    def test_missing_ready_end_install_and_view_fail(self):
        for index in (0, 2, 3, 5):
            with self.subTest(index=index):
                rows, _ = fixture()
                rows.pop(index)
                for seq, row in enumerate(rows, 1):
                    row["event_seq"] = seq
                with self.assertRaises(ObservationError):
                    self.check_rows(rows)

    def test_duplicate_install_fails(self):
        rows, _ = fixture()
        rows.insert(3, copy.deepcopy(rows[2]))
        for seq, row in enumerate(rows, 1):
            row.update(event_seq=seq, mono_ns=seq)
        with self.assertRaises(ObservationError):
            self.check_rows(rows)

    def test_explicit_partial_and_unavailable_are_not_canonical_success(self):
        for status in ('partial', 'unavailable', 'retryable_error'):
            with self.subTest(status=status):
                rows, _ = fixture()
                row = dict(rows[2], event='recovery_status', loop_id='uninstalled', status=status,
                           canonical=False, reason='expired', terminal=dict(block_index=9, saving='not saved', outcome='completed'))
                rows.insert(3, row)
                for seq, row in enumerate(rows, 1):
                    row.update(event_seq=seq, mono_ns=seq)
                with self.assertRaisesRegex(ObservationError, 'explicit non-complete'):
                    self.check_rows(rows)

    def test_application_pause_must_have_exact_matching_release(self):
        for target in ('same', 'wrong'):
            with self.subTest(target=target):
                rows, body = fixture()
                pause = dict(rows[1], event='application_paused', held_kind='session_snapshot')
                resume = dict(pause, event='application_resumed', load='load_fixture' if target == 'same' else 'wrong')
                rows[2:2] = [pause, resume]
                for seq, row in enumerate(rows, 1):
                    row.update(event_seq=seq, mono_ns=seq)
                if target == 'same':
                    self.check_rows(rows, body)
                else:
                    with self.assertRaisesRegex(ObservationError, 'release target mismatch'):
                        self.check_rows(rows)

    def test_parallel_install_arrival_does_not_define_final_turn_order(self):
        rows, _ = fixture()
        second = copy.deepcopy(rows[2])
        second.update(loop_id='loop_second', completion_seq=2, input_id='inp_second', result_id='r1:second')
        second['parts'] = [text_part('second canonical')]
        rows.insert(2, second)
        view = next(row for row in rows if row['event'] == 'canonical_view')
        view['turns'].append({field: second[field] for field in ('loop_id', 'parts', 'persistence', 'persistence_uncertain')})
        view['inventory'].extend(inventory(second, 'inp_second', 'second prompt', len(view['inventory'])))
        for seq, row in enumerate(rows, 1):
            row.update(event_seq=seq, mono_ns=seq)
        for wrong_order in (False, True):
            observed = copy.deepcopy(rows)
            if wrong_order:
                next(row for row in observed if row['event'] == 'canonical_view')['turns'].reverse()
            trace = Trace('/nonexistent-synthetic-consumer-test')
            register_inputs(trace)
            for row in observed:
                trace.consume(row)
            if wrong_order:
                with self.assertRaisesRegex(ObservationError, 'reordered canonical'):
                    trace.finish(2)
            else:
                self.assertEqual(trace.finish(2)['canonical_installs_verified'], 2)

    def test_grouped_hashes_cannot_hide_cross_region_body_swaps(self):
        rows, _ = fixture()
        second = copy.deepcopy(rows[2])
        second.update(loop_id='loop_second', completion_seq=2, input_id='inp_second', result_id='r1:second')
        second['parts'] = [text_part('second canonical')]
        rows.insert(3, second)
        view = next(row for row in rows if row['event'] == 'canonical_view')
        view['turns'].append({field: second[field] for field in ('loop_id', 'parts', 'persistence', 'persistence_uncertain')})
        view['inventory'].extend(inventory(second, 'inp_second', 'second prompt', len(view['inventory'])))
        # Exactly the prior blind spot: all per-loop hashes and TurnEnd order
        # stay unchanged; only the actual globally installed body slots swap.
        entries = view['inventory']
        body_a = next(i for i, e in enumerate(entries) if e['kind'] == 'assistant' and e['loop_id'] == 'loop_fixture')
        body_b = next(i for i, e in enumerate(entries) if e['kind'] == 'assistant' and e['loop_id'] == 'loop_second')
        entries[body_a], entries[body_b] = entries[body_b], entries[body_a]
        for index, entry in enumerate(entries):
            entry['block_index'] = index
        for seq, row in enumerate(rows, 1):
            row.update(event_seq=seq, mono_ns=seq)
        trace = Trace('/synthetic-global-order-negative')
        register_inputs(trace)
        for row in rows:
            trace.consume(row)
        with self.assertRaisesRegex(ObservationError, 'global (installed.*order|canonical.*prompt)'):
            trace.finish(2)

    def test_global_inventory_exposes_unknown_loops_users_and_saving_drift(self):
        mutations = {
            'extra_loop': lambda entries: entries.append(dict(entries[2], loop_id='extra-loop', block_index=10)),
            'wrong_user_hash': lambda entries: entries[0].update(body=body_digest('different input')),
            'wrong_steering_kind': lambda entries: entries[0].update(user_kind='steering'),
            'wrong_input': lambda entries: entries[0].update(input_id='other accepted input'),
            'wrong_saving': lambda entries: entries[-1].update(saving='not saved'),
            'not_canonical': lambda entries: entries[-1].update(canonical=False),
            'missing_inventory': lambda entries: entries.clear(),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                rows, body = fixture()
                mutate(next(row for row in rows if row['event'] == 'canonical_view')['inventory'])
                with self.assertRaises(ObservationError):
                    self.check_rows(rows, body)

    def test_runtime_steering_without_agent_input_id_is_explicitly_unavailable_not_invented(self):
        for observed_id in (None, 'steer-fixture', 'wrong-input'):
            with self.subTest(observed_id=observed_id):
                rows, _ = fixture()
                entries = next(row for row in rows if row['event'] == 'canonical_view')['inventory']
                entries.insert(2, dict(kind='user', loop_id='loop_fixture', input_id=observed_id,
                                       user_kind='steering', request_index=1 if observed_id is not None else None,
                                       body=body_digest('literal steering bytes'), preserved=False))
                for index, entry in enumerate(entries):
                    entry['block_index'] = index
                trace = Trace('/synthetic-steering-identity-test')
                register_inputs(trace)
                trace.register_input(dict(session_id='ses_fixture', load='load_fixture', input_id='steer-fixture'),
                                     'literal steering bytes', 'steering', 'loop_fixture', request_index=1)
                for row in rows:
                    trace.consume(row)
                if observed_id == 'wrong-input':
                    with self.assertRaises(ObservationError):
                        trace.finish(1)
                else:
                    result = trace.finish(1)
                    self.assertEqual(result['user_bodies_verified'], 2)
                    self.assertEqual(result['user_input_identities_unavailable'], int(observed_id is None))
                    self.assertEqual(entries[2]['input_id'], observed_id)

    def test_steering_must_stay_between_its_previous_request_tool_and_next_request_answer(self):
        for location in ('before_request0_tool', 'after_request1_answer'):
            with self.subTest(location=location):
                rows, _ = fixture()
                entries = next(row for row in rows if row['event'] == 'canonical_view')['inventory']
                steering = dict(kind='user', loop_id='loop_fixture', input_id=None,
                                user_kind='steering', request_index=1,
                                body=body_digest('literal steering bytes'), preserved=False)
                entries.insert(1 if location == 'before_request0_tool' else len(entries) - 1, steering)
                for index, entry in enumerate(entries):
                    entry['block_index'] = index
                trace = Trace('/synthetic-request-boundary-order-regression')
                register_inputs(trace)
                trace.register_input(dict(session_id='ses_fixture', load='load_fixture', input_id='steer-fixture'),
                                     'literal steering bytes', 'steering', 'loop_fixture', request_index=1)
                # Independent fixture: request1 delivery must remain between
                # request0's tool and request1's canonical answer.
                for row in rows:
                    trace.consume(row)
                with self.assertRaises(ObservationError):
                    trace.finish(1)

    def test_independent_hashed_oracle_roundtrip_rechecks_global_steering_order(self):
        rows, body = fixture()
        entries = next(row for row in rows if row['event'] == 'canonical_view')['inventory']
        entries.insert(2, dict(kind='user', loop_id='loop_fixture', input_id='steer-fixture',
                               request_index=1, user_kind='steering', body=body_digest('literal steering bytes'), preserved=False))
        for index, entry in enumerate(entries):
            entry['block_index'] = index
        source = Trace('/unused-source-oracle')
        register_inputs(source)
        source.register_input(dict(session_id='ses_fixture', load='load_fixture', input_id='steer-fixture'),
                              'literal steering bytes', 'steering', 'loop_fixture', request_index=1)
        document = source.oracle_document()
        encoded = json.dumps(document)
        self.assertNotIn('literal steering bytes', encoded)
        self.assertNotIn('fixture prompt', encoded)
        restored = Trace('/unused-restored-oracle')
        expected_incomplete = restored.load_oracle(json.loads(encoded))
        for row in rows:
            restored.consume(row)
        self.assertEqual(restored.finish(1, expected_incomplete)['steering_boundaries_verified'], 1)
        moved = copy.deepcopy(rows)
        actual = next(row for row in moved if row['event'] == 'canonical_view')['inventory']
        actual.insert(1, actual.pop(2))
        for index, entry in enumerate(actual):
            entry['block_index'] = index
        invalid = Trace('/unused-mutated-oracle')
        invalid.load_oracle(json.loads(encoded))
        for row in moved:
            invalid.consume(row)
        with self.assertRaisesRegex(ObservationError, 'request boundary order'):
            invalid.finish(1)

    def test_known_delivered_boundary_cannot_be_silently_lost_or_changed(self):
        for boundary in (None, 0, 2):
            with self.subTest(boundary=boundary):
                rows, _ = fixture()
                entries = next(row for row in rows if row['event'] == 'canonical_view')['inventory']
                entries.insert(2, dict(kind='user', loop_id='loop_fixture', input_id='steer-fixture',
                                       request_index=boundary, user_kind='steering', body=body_digest('steer'), preserved=False))
                for index, entry in enumerate(entries):
                    entry['block_index'] = index
                trace = Trace('/unused-known-steering-boundary')
                register_inputs(trace)
                trace.register_input(dict(session_id='ses_fixture', load='load_fixture', input_id='steer-fixture'),
                                     'steer', 'steering', 'loop_fixture', request_index=1)
                for row in rows:
                    trace.consume(row)
                with self.assertRaises(ObservationError):
                    trace.finish(1)

    def test_retained_actual_delivery_identity_is_not_inferred_onto_unknown_canonical_rows(self):
        for observed_loop, boundary in (('loop_fixture', 1), ('other-loop', 1), ('loop_fixture', 0)):
            with self.subTest(loop=observed_loop, boundary=boundary):
                rows, _ = fixture()
                view = next(row for row in rows if row['event'] == 'canonical_view')
                view['inventory'].insert(2, dict(kind='user', loop_id='loop_fixture', input_id=None,
                    request_index=None, user_kind='steering', body=body_digest('steer'), preserved=False))
                view['delivered_inputs'] = [dict(loop_id=observed_loop, input_id='steer-fixture', request_index=boundary)]
                for index, entry in enumerate(view['inventory']):
                    entry['block_index'] = index
                trace = Trace('/unused-retained-delivery-proof')
                register_inputs(trace)
                trace.register_input(dict(session_id='ses_fixture', load='load_fixture', input_id='steer-fixture'),
                                     'steer', 'steering', 'loop_fixture', request_index=1)
                for row in rows:
                    trace.consume(row)
                if observed_loop != 'loop_fixture' or boundary != 1:
                    with self.assertRaises(ObservationError):
                        trace.finish(1)
                else:
                    result = trace.finish(1)
                    self.assertEqual(result['actual_delivered_steering_identities_observed'], 1)
                    self.assertEqual(result['user_input_identities_unavailable'], 1)
                    self.assertEqual(result['steering_boundaries_unavailable_on_canonical_rows'], 1)
                    self.assertIsNone(view['inventory'][2]['input_id'])

    def test_incremental_partial_lines(self):
        rows, body = fixture()
        encoded = "".join(json.dumps(row) + "\n" for row in rows).encode()
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "trace"
            trace = Trace(path)
            register_inputs(trace)
            for byte in encoded:
                with path.open("ab") as handle:
                    handle.write(bytes([byte]))
                trace.poll()
            self.assertIsNotNone(trace.expect_install(0, body))
            trace.finish(1)

    def test_unfinished_and_truncated_files_fail(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "trace"
            trace = Trace(path)
            with self.assertRaises(ObservationError):
                trace.finish(0)
            path.write_bytes(b'{"schema":1')
            trace.poll()
            with self.assertRaises(ObservationError):
                trace.finish(0)
            path.write_bytes(b"")
            with self.assertRaises(ObservationError):
                trace.poll()


if __name__ == "__main__":
    unittest.main()
