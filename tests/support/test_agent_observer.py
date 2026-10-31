"""Agent telemetry consumer regressions; synthetic records, not chain evidence."""

import copy
import json
import tempfile
import unittest
from pathlib import Path

from agent_observer import AgentTrace
from canonical_observer import ObservationError


GAUGES = ('watchers', 'source_charged_bytes', 'source_charged_count',
          'catalog_source_bytes', 'catalog_source_count', 'catalog_metadata_bytes',
          'catalog_entries', 'control_staging_charged_bytes', 'control_reservations',
          'retained_page_refs', 'retained_page_reference_weighted_bytes',
          'active_owner_objects', 'completion_values', 'late_wait_slots',
          'registered_wait_receivers', 'driver_report_owners', 'result_queries_inflight',
          'stored_identity_charged_bytes', 'stored_identity_charged_count', 'paging_prefix_workspace_bytes')
WORK = ('normalize_durable', 'normalize_public', 'source_prepare', 'source_hash',
        'result_projection_hash', 'stored_record_decode', 'result_range_encode',
        'query_encode', 'page_prefix_scan')


class AgentConsumerTests(unittest.TestCase):
    def fixture(self):
        counters = {'gauges': {name: {'revision': 0, 'current': 0, 'peak': 0} for name in GAUGES},
                    'work': {name: dict(calls=0, items=0, duration_ns=0, peak_ns=0,
                                       **({} if name.startswith('normalize_') else {'bytes': 0})) for name in WORK},
                    'result_queries': 3, 'result_pages': 2, 'result_errors': 1,
                    'result_available_pages': 1, 'result_pending_pages': 1, 'result_unavailable_pages': 0,
                    'invalid': False, 'trace_dropped': 0}
        counters['gauges']['watchers'].update(revision=2, peak=2)
        snapshot = dict(session_id='s', load='l', revision=1)
        query = dict(session_id='s', load='l', loop_id='a', cursor=None, next_cursor=None,
                     duration_ns=1, result_id='r1:test', availability='live', error=None,
                     chunks=1, chunk_bytes=2, total=1, has_next=False,
                     requested_result_id=None, unavailable_reason=None)
        rows = [dict(event='observer_ready', max_row_bytes=8192, max_rows=65536, max_trace_bytes=16777216,
                     gauge_order='per-gauge-revision-v1', query_order='query-id-v1'),
                dict(event='loop_installed', session_id='s', load='l', loop_id='a', session=1, turn=1),
                dict(event='snapshot_sample', **snapshot, sample_started_ns=1, sampled_ns=2, duration_ns=1),
                dict(event='snapshot_encoded', **snapshot, duration_ns=1, encoded_bytes=100),
                dict(event='snapshot_sent', **snapshot, write_started_ns=4, written_ns=5, duration_ns=1, encoded_bytes=101),
                dict(event='result_query', **query),
                dict(event='result_query', **{**query, 'availability': 'pending', 'result_id': None,
                                             'chunks': 0, 'chunk_bytes': 0, 'total': 0}),
                dict(event='result_query', **{**query, 'availability': None, 'error': 'query_limit',
                                             'result_id': None, 'chunks': 0, 'chunk_bytes': 0, 'total': 0}),
                dict(event='gauge', name='watchers', revision=1, previous_current=0, action='add', amount=2, current=2, peak=2),
                dict(event='gauge', name='watchers', revision=2, previous_current=2, action='sub', amount=2, current=0, peak=2),
                dict(event='counters', **copy.deepcopy(counters)),
                dict(event='observer_end', counters=copy.deepcopy(counters), invalid=False,
                     trace_dropped=0, unconsumed_steps=0)]
        query_id = 0
        for seq, row in enumerate(rows, 1):
            row.update(schema='agent-e2e-1', event_seq=seq, mono_ns=seq)
            if row['event'] == 'result_query':
                query_id += 1
                row['query_id'] = query_id
        return rows

    def consume(self, rows):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'agent.jsonl'
            path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
            return AgentTrace(path).finish([{'session_id': 's', 'load': 'l', 'loop_id': 'a', 'result_id': 'r1:test', 'read_item_count': 1}])

    def test_complete_observed_counters_and_identity(self):
        self.assertEqual(self.consume(self.fixture())['gauges']['watchers']['peak'], 2)

    def test_empty_gauges_cannot_claim_released_owners(self):
        rows = self.fixture()
        rows[-2]['gauges'] = rows[-1]['counters']['gauges'] = {}
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_fabricated_query_counts_cannot_claim_measured_pages(self):
        rows = self.fixture()
        for counters in (rows[-2], rows[-1]['counters']):
            counters.update(result_queries=999, result_pages=998, result_available_pages=997)
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_no_snapshot_samples_encodes_or_flushes_cannot_pass(self):
        rows = [row for row in self.fixture() if not row['event'].startswith('snapshot_')]
        for seq, row in enumerate(rows, 1):
            row['event_seq'] = seq
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_phantom_peak_without_matching_actual_gauge_rows_is_rejected(self):
        rows = self.fixture()
        for counter in (rows[-2], rows[-1]['counters']):
            counter['gauges']['watchers']['peak'] = 999
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_last_live_owner_cannot_be_hidden_by_zero_terminal_checkpoints(self):
        rows = self.fixture()
        rows.insert(-2, dict(event='gauge', name='watchers', revision=3, previous_current=0,
                             action='add', amount=2, current=2, peak=2))
        for counter in (rows[-2], rows[-1]['counters']):
            counter['gauges']['watchers']['revision'] = 3
        for seq, row in enumerate(rows, 1):
            row.update(schema='agent-e2e-1', event_seq=seq, mono_ns=seq)
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_redirected_query_targets_cannot_prove_an_unchanged_ui_install(self):
        rows = self.fixture()
        for row in rows:
            if row['event'] == 'result_query':
                row.update(session_id='unrelated-session', load='unrelated-load', loop_id='unrelated-loop')
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_available_result_identity_must_match_actual_ui_install(self):
        rows = self.fixture()
        next(row for row in rows if row['event'] == 'result_query' and row['availability'] == 'live')['result_id'] = 'r1:unrelated-result'
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_unpaired_unplanned_fault_with_unknown_outcome_cannot_pass(self):
        rows = self.fixture()
        rows.insert(-2, dict(event='fault_applied', id='unplanned', point='append', action='rollback',
                             reason='unknown-fault-result', session_id='s', load='l', loop_id='a', session=1, turn=1))
        for seq, row in enumerate(rows, 1):
            row.update(schema='agent-e2e-1', event_seq=seq, mono_ns=seq)
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_gauge_enqueue_order_may_differ_from_actual_ownership_revision(self):
        rows = self.fixture()
        indices = [index for index, row in enumerate(rows) if row['event'] == 'gauge']
        rows[indices[0]], rows[indices[1]] = rows[indices[1]], rows[indices[0]]
        for seq, row in enumerate(rows, 1):
            row.update(event_seq=seq, mono_ns=seq)
        # Last writer row has current2, but causal revision2 really released it.
        self.assertEqual(rows[indices[-1]]['current'], 2)
        result = self.consume(rows)
        self.assertEqual(result['causal_gauge_updates']['watchers'], 2)
        self.assertEqual(result['gauges']['watchers']['current'], 0)

    def test_intermediate_checkpoint_uses_its_causal_state_not_arrival_order(self):
        rows = self.fixture()
        old = copy.deepcopy(rows[-2])
        old['gauges']['watchers'] = dict(revision=1, current=2, peak=2)
        rows.insert(-2, old)
        for seq, row in enumerate(rows, 1):
            row.update(event_seq=seq, mono_ns=seq)
        self.consume(rows)
        old['gauges']['watchers']['current'] = 0
        with self.assertRaises(ObservationError):
            self.consume(rows)

    def test_gauge_revision_gaps_actions_and_checkpoint_versions_fail(self):
        def gauges(rows):
            return [row for row in rows if row['event'] == 'gauge']
        changes = [lambda r: gauges(r)[1].update(revision=3),
                   lambda r: gauges(r)[1].update(previous_current=0),
                   lambda r: gauges(r)[1].update(amount=1),
                   lambda r: r[-1]['counters']['gauges']['watchers'].update(revision=1),
                   lambda r: r[0].pop('gauge_order'),
                   lambda r: r[0].pop('query_order')]
        for change in changes:
            with self.subTest(change=change):
                rows = self.fixture()
                change(rows)
                with self.assertRaises(ObservationError):
                    self.consume(rows)

    def multipage_fixture(self):
        rows = self.fixture()
        queries = [row for row in rows if row['event'] == 'result_query']
        queries[0].update(next_cursor=dict(item=0, offset=1), has_next=True)
        queries[1].update(cursor=dict(item=0, offset=1), availability='stored', result_id='r1:test',
                          requested_result_id='r1:test', chunks=1, chunk_bytes=1, total=1)
        for checkpoint in (rows[-2], rows[-1]['counters']):
            checkpoint.update(result_available_pages=2, result_pending_pages=0)
        return rows

    def test_exact_cursor_chain_and_ui_result_item_count_are_required(self):
        result = self.consume(self.multipage_fixture())
        self.assertEqual(result['canonical_query_proofs'][0]['actual_query_ids'], [1, 2])
        changes = [lambda q: q[1].update(cursor=dict(item=0, offset=2)),
                   lambda q: q[1].update(requested_result_id='r1:wrong'),
                   lambda q: q[1].update(result_id='r1:wrong'),
                   lambda q: q[1].update(total=999),
                   lambda q: q[1].update(query_id=1),
                   lambda q: q[1].update(query_id=99),
                   lambda q: q[0].update(next_cursor=dict(item=0, offset=0))]
        for change in changes:
            with self.subTest(change=change):
                rows = self.multipage_fixture()
                change([row for row in rows if row['event'] == 'result_query'])
                with self.assertRaises(ObservationError):
                    self.consume(rows)

    def test_concurrent_query_outcomes_may_arrive_in_reverse_admission_order(self):
        rows = self.fixture()
        indices = [index for index, row in enumerate(rows) if row['event'] == 'result_query']
        rows[indices[0]], rows[indices[-1]] = rows[indices[-1]], rows[indices[0]]
        for seq, row in enumerate(rows, 1):
            row.update(event_seq=seq, mono_ns=seq)
        result = self.consume(rows)
        self.assertEqual(result['canonical_query_proofs'][0]['actual_query_ids'], [1])

    def test_missing_wrong_or_unmeasured_work_is_rejected(self):
        mutations = [lambda c: c['gauges'].pop('source_charged_bytes'),
                     lambda c: c['gauges'].update(unknown={'current': 0, 'peak': 0}),
                     lambda c: c.update(work={}),
                     lambda c: c['work']['source_hash'].update(calls=1),
                     lambda c: c['work']['normalize_durable'].update(bytes=0),
                     lambda c: c['gauges']['watchers'].update(current=True)]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                rows = self.fixture()
                mutate(rows[-1]['counters'])
                with self.assertRaises(ObservationError):
                    self.consume(rows)

    def test_sample_encode_flush_target_bytes_and_times_must_match(self):
        mutations = [lambda r: r[3].update(revision=2),
                     lambda r: r[4].update(load='wrong'),
                     lambda r: r[4].update(encoded_bytes=100),
                     lambda r: r[4].update(write_started_ns=0),
                     lambda r: r[2].update(duration_ns=0)]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                rows = self.fixture()
                mutate(rows)
                with self.assertRaises(ObservationError):
                    self.consume(rows)

    def test_query_outcomes_and_availability_counts_must_match_rows(self):
        mutations = [lambda r: r[5].update(error='query_limit'),
                     lambda r: r[5].update(availability='bogus'),
                     lambda r: r[6].update(availability='unavailable'),
                     lambda r: r[7].update(error=None),
                     lambda r: r[5].update(has_next=True),
                     lambda r: r[-1]['counters'].pop('result_pending_pages')]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                rows = self.fixture()
                mutate(rows)
                with self.assertRaises(ObservationError):
                    self.consume(rows)

    def test_dropped_invalid_unused_or_live_owner_fails(self):
        mutations = [lambda rows: rows[-1].update(invalid=True),
                     lambda rows: rows[-1].update(trace_dropped=1),
                     lambda rows: rows[-1].update(unconsumed_steps=1),
                     lambda rows: rows[-1]['counters']['gauges']['watchers'].update(current=1),
                     lambda rows: rows[1].update(load='wrong'),
                     lambda rows: rows[2].update(event_seq=99),
                     lambda rows: rows[1].update(event='observer_ready'),
                     lambda rows: rows[1].update(event='observer_end')]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                rows = self.fixture()
                mutate(rows)
                with self.assertRaises(ObservationError):
                    self.consume(rows)

    def test_missing_trace_and_end_are_not_success(self):
        with self.assertRaises(ObservationError):
            AgentTrace('/does/not/exist').finish([])
        with self.assertRaises(ObservationError):
            self.consume(self.fixture()[:-1])


if __name__ == '__main__':
    unittest.main(verbosity=2)
