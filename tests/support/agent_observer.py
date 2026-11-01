"""Strictly reconcile actual Agent rows and final owner/work/query counters."""

import json
from collections import Counter
from pathlib import Path

from canonical_observer import ObservationError, identity, integer
from agent_observer_checks import GaugeHistory, bind_queries, validate_faults


GAUGES = frozenset(('watchers', 'source_charged_bytes', 'source_charged_count',
                   'catalog_source_bytes', 'catalog_source_count', 'catalog_metadata_bytes',
                   'catalog_entries', 'control_staging_charged_bytes', 'control_reservations',
                   'retained_page_refs', 'retained_page_reference_weighted_bytes',
                   'active_owner_objects', 'completion_values', 'late_wait_slots',
                   'registered_wait_receivers', 'driver_report_owners', 'result_queries_inflight',
                   'stored_identity_charged_bytes', 'stored_identity_charged_count',
                   'paging_prefix_workspace_bytes'))
WORK = frozenset(('normalize_durable', 'normalize_public', 'source_prepare', 'source_hash',
                 'result_projection_hash', 'stored_record_decode', 'result_range_encode',
                 'query_encode', 'page_prefix_scan'))
PAGE_COUNTS = ('result_available_pages', 'result_pending_pages', 'result_unavailable_pages')
COUNTS = ('result_queries', 'result_pages', 'result_errors', *PAGE_COUNTS)
BASE = frozenset(('schema', 'event_seq', 'event', 'mono_ns'))


def gauge(value, name):
    if not isinstance(value, dict) or set(value) != {'revision', 'current', 'peak'}:
        raise ObservationError('Agent gauge schema mismatch: ' + name)
    integer(value['revision'], name + ' revision')
    current, peak = (integer(value[key], name + ' ' + key) for key in ('current', 'peak'))
    if peak < current:
        raise ObservationError('Agent peak below actual current: ' + name)


def work_fields(name):
    return {'calls', 'items', 'duration_ns', 'peak_ns'} | (set() if name.startswith('normalize_') else {'bytes'})


def counters(value):
    required = {'gauges', 'work', 'invalid', 'trace_dropped', *COUNTS}
    if not isinstance(value, dict) or set(value) - BASE != required:
        raise ObservationError('Agent counter schema mismatch')
    if value['invalid'] is not False or integer(value['trace_dropped'], 'trace_dropped'):
        raise ObservationError('Agent counters invalid/dropped')
    if not isinstance(value['gauges'], dict) or set(value['gauges']) != GAUGES:
        raise ObservationError('Agent exact gauge inventory missing/unknown')
    for name, meter in value['gauges'].items():
        gauge(meter, name)
    if not isinstance(value['work'], dict) or set(value['work']) != WORK:
        raise ObservationError('Agent exact work inventory missing/unknown')
    for name, meter in value['work'].items():
        if not isinstance(meter, dict) or set(meter) != work_fields(name):
            raise ObservationError('Agent work fields mismatch: ' + name)
        for key, amount in meter.items():
            integer(amount, name + ' ' + key)
        if meter['peak_ns'] > meter['duration_ns'] or (meter['calls'] == 0 and any(meter.values())):
            raise ObservationError('Agent work peak/call accounting mismatch: ' + name)
    for name in COUNTS:
        integer(value[name], name)


def snapshot_key(row):
    return (identity(row.get('session_id'), 'snapshot session_id'),
            identity(row.get('load'), 'snapshot load'), integer(row.get('revision'), 'snapshot revision'))


def turn_key(row):
    return tuple(identity(row.get(name), name) for name in ('session_id', 'load', 'loop_id'))


class AgentTrace:
    EVENTS = {'observer_ready', 'observer_end', 'counters', 'gauge', 'snapshot_sample', 'snapshot_encoded',
              'snapshot_sent', 'load_registered', 'load_unregistered', 'loop_installed',
              'completion_registered', 'result_query', 'work', 'fault_ready',
              'fault_applied', 'fault_released', 'runtime_tail_dropped',
              'input_fixture_request', 'input_fixture_answer', 'responsiveness_probe'}
    MAX_BYTES, MAX_ROW, MAX_ROWS = 16 * 1024 * 1024, 8192, 65536

    def __init__(self, path, plan=None):
        self.path = Path(path)
        sidecar = self.path.with_name('fault-plan.json')
        self.plan = json.loads(json.dumps(plan)) if plan is not None else (
            json.loads(sidecar.read_text()) if sidecar.exists() else None)

    def records(self):
        if not self.path.exists():
            return []
        if self.path.stat().st_size > self.MAX_BYTES:
            raise ObservationError('Agent live trace budget exceeded')
        data = self.path.read_bytes()
        if len(data) > self.MAX_BYTES:
            raise ObservationError('Agent live trace budget exceeded')
        lines = data.splitlines()
        if data and not data.endswith(b'\n'):
            lines = lines[:-1]
        try:
            return [json.loads(line) for line in lines]
        except (ValueError, UnicodeError) as error:
            raise ObservationError('malformed Agent trace JSON') from error

    def completed(self):
        return [row for row in self.records() if row.get('event') == 'completion_registered']

    def fault(self, step, event='fault_ready'):
        return next((row for row in reversed(self.records())
                     if row.get('event') == event and row.get('id') == step), None)

    def gauges(self):
        values = {}
        rows = self.records()
        if not rows or rows[0].get('gauge_order') != 'per-gauge-revision-v1':
            raise ObservationError('live gauge read lacks actual producer causal ownership contract')
        for row in rows:
            if row.get('event') == 'counters':
                observed = row['gauges']
            elif row.get('event') == 'gauge':
                observed = {row['name']: {key: row[key] for key in ('revision', 'current', 'peak')}}
            else:
                continue
            for name, value in observed.items():
                gauge(value, name)
                previous = values.get(name)
                if previous is not None and previous['revision'] == value['revision'] and previous != value:
                    raise ObservationError('same causal gauge revision has conflicting state: ' + name)
                if previous is None or value['revision'] > previous['revision']:
                    values[name] = value
        return values

    def finish(self, tui_installs, expected_incomplete=None):
        if not self.path.exists():
            raise ObservationError('missing actual Agent feature trace')
        if self.path.stat().st_size > self.MAX_BYTES:
            raise ObservationError('Agent trace oversized')
        data = self.path.read_bytes()
        if not data or len(data) > self.MAX_BYTES or not data.endswith(b'\n'):
            raise ObservationError('Agent trace incomplete/oversized')
        lines = data.splitlines()
        if len(lines) > self.MAX_ROWS or any(len(line) + 1 > self.MAX_ROW for line in lines):
            raise ObservationError('Agent row/event budget exceeded')
        rows = self.records()
        if rows[0].get('event') != 'observer_ready' or rows[-1].get('event') != 'observer_end':
            raise ObservationError('Agent observer lifecycle incomplete')
        ready = rows[0]
        if (ready.get('max_row_bytes'), ready.get('max_trace_bytes'), ready.get('max_rows')) != (
                self.MAX_ROW, self.MAX_BYTES, self.MAX_ROWS):
            raise ObservationError('Agent producer limits missing/changed')
        if ready.get('gauge_order') != 'per-gauge-revision-v1' or ready.get('query_order') != 'query-id-v1':
            raise ObservationError('missing actual Agent causal gauge/query contract; old traces are historical only')
        samples, encodes, sends, checkpoints = {}, {}, [], []
        gauge_history = GaugeHistory(GAUGES)
        outcomes = Counter()
        per_loop = Counter()
        measured_work = {name: {field: 0 for field in work_fields(name)} for name in WORK}
        gauge_peaks = Counter()
        for seq, row in enumerate(rows, 1):
            if not isinstance(row, dict) or row.get('schema') != 'agent-e2e-1' or (
                    type(row.get('event_seq')) is not int or row['event_seq'] != seq) or row.get('event') not in self.EVENTS:
                raise ObservationError('Agent observer schema/sequence/event mismatch')
            integer(row.get('mono_ns'), 'Agent mono_ns')
            event = row['event']
            if event == 'observer_ready' and seq != 1 or event == 'observer_end' and seq != len(rows):
                raise ObservationError('duplicate/early Agent lifecycle event')
            if row.get('invalid', False) is not False or integer(row.get('trace_dropped', 0), 'trace_dropped') or integer(row.get('unconsumed_steps', 0), 'unconsumed_steps'):
                raise ObservationError('Agent observer invalid/dropped/unconsumed fault')
            if event in ('fault_ready', 'fault_applied', 'fault_released') and (
                    row.get('status') in ('timeout', 'failed', 'error') or row.get('reason') in (
                        'timeout', 'invalid_release', 'release_io_error', 'awaiter_dropped')):
                raise ObservationError('Agent fault gate failed: ' + str(row.get('reason')))
            if event == 'counters':
                counters(row)
                checkpoints.append({key: value for key, value in row.items() if key not in BASE})
            elif event == 'gauge':
                name = row.get('name')
                if name not in GAUGES:
                    raise ObservationError('unknown Agent gauge')
                gauge({key: row.get(key) for key in ('revision', 'current', 'peak')}, name)
                gauge_history.record(row)
                gauge_peaks[name] = max(gauge_peaks[name], row['peak'])
            elif event == 'snapshot_sample':
                key = snapshot_key(row)
                if key in samples:
                    raise ObservationError('duplicate Agent snapshot sample identity')
                start, end = (integer(row.get(field), field) for field in ('sample_started_ns', 'sampled_ns'))
                if start > end or end > row['mono_ns'] or integer(row.get('duration_ns'), 'sample duration') != end - start:
                    raise ObservationError('Agent sample timing mismatch')
                samples[key] = row
            elif event == 'snapshot_encoded':
                key = snapshot_key(row)
                integer(row.get('duration_ns'), 'encode duration')
                if integer(row.get('encoded_bytes'), 'encoded_bytes') == 0 or key in encodes:
                    raise ObservationError('Agent duplicate/empty encoded snapshot')
                encodes[key] = row
            elif event == 'snapshot_sent':
                snapshot_key(row)
                start, end = (integer(row.get(field), field) for field in ('write_started_ns', 'written_ns'))
                if start > end or end > row['mono_ns'] or integer(row.get('duration_ns'), 'flush duration') != end - start:
                    raise ObservationError('Agent flush timing mismatch')
                integer(row.get('encoded_bytes'), 'sent encoded_bytes')
                sends.append(row)
            elif event == 'result_query':
                identity(row.get('session_id'), 'query session_id')
                identity(row.get('loop_id'), 'query loop_id')
                if row.get('load') is not None:
                    identity(row['load'], 'query load')
                for field in ('duration_ns', 'chunks', 'chunk_bytes', 'total'):
                    integer(row.get(field), 'query ' + field)
                if type(row.get('has_next')) is not bool or row['has_next'] != (row.get('next_cursor') is not None):
                    raise ObservationError('Agent query cursor/has_next mismatch')
                availability, error = row.get('availability'), row.get('error')
                if error is not None:
                    identity(error, 'query error')
                    if availability is not None or any(row[field] for field in ('chunks', 'chunk_bytes', 'total', 'has_next')) or row.get('result_id') is not None:
                        raise ObservationError('Agent error query also claimed a page')
                    outcomes['result_errors'] += 1
                else:
                    if availability not in ('live', 'stored', 'pending', 'unavailable'):
                        raise ObservationError('Agent query missing valid outcome')
                    outcomes['result_pages'] += 1
                    category = 'available' if availability in ('live', 'stored') else availability
                    outcomes['result_' + category + '_pages'] += 1
                    if category == 'available':
                        identity(row.get('result_id'), 'query result_id')
                    elif row.get('result_id') is not None or row['chunks'] or row['chunk_bytes'] or row['has_next']:
                        raise ObservationError('Agent nonavailable query claimed body data')
                outcomes['result_queries'] += 1
                per_loop[row['loop_id']] += 1
            elif event == 'work':
                name = row.get('kind')
                if name not in WORK:
                    raise ObservationError('unknown Agent work kind')
                fields = work_fields(name) - {'calls', 'peak_ns'}
                for field in fields:
                    measured_work[name][field] += integer(row.get(field), name + ' ' + field)
                if name.startswith('normalize_') and 'bytes' in row:
                    raise ObservationError('unmeasured normalization bytes claimed')
                if 'started_ns' in row or 'finished_ns' in row:
                    start, end = (integer(row.get(field), field) for field in ('started_ns', 'finished_ns'))
                    if end < start or end - start != row['duration_ns']:
                        raise ObservationError('Agent work interval mismatch')
                measured_work[name]['calls'] += 1
                measured_work[name]['peak_ns'] = max(measured_work[name]['peak_ns'], row['duration_ns'])
        final = rows[-1].get('counters')
        counters(final)
        if not checkpoints or checkpoints[-1] != final:
            raise ObservationError('Agent missing/mismatched final counter checkpoint')
        for name, meter in final['gauges'].items():
            if meter['current'] or meter['peak'] != gauge_peaks[name]:
                raise ObservationError('Agent retained owner or peak differs from actual gauge rows: ' + name)
        causal_updates = gauge_history.reconcile(checkpoints, final)
        if any(final[name] != outcomes[name] for name in COUNTS):
            raise ObservationError('Agent actual query outcomes disagree with final counts')
        if final['work'] != measured_work:
            raise ObservationError('Agent actual work rows disagree with final work counters')
        if not samples or not encodes or not sends:
            raise ObservationError('missing actual Agent sample/encode/flush chain')
        for key, encoded in encodes.items():
            if key not in samples or samples[key]['sampled_ns'] > encoded['mono_ns']:
                raise ObservationError('Agent encoded snapshot has no matching prior sample')
        flushed_loads = set()
        for sent in sends:
            key = snapshot_key(sent)
            encoded = encodes.get(key)
            if encoded is None or encoded['encoded_bytes'] + 1 != sent['encoded_bytes'] or encoded['mono_ns'] > sent['write_started_ns']:
                raise ObservationError('Agent flushed snapshot has no matching encoded frame')
            flushed_loads.add(key[:2])
        started = [turn_key(row) for row in rows if row['event'] == 'loop_installed']
        expected = {turn_key(row) for row in tui_installs} | set(expected_incomplete or {})
        if len(started) != len(set(started)) or set(started) != expected:
            raise ObservationError('actual Agent started-loop set differs from TUI installed-loop set')
        if any(target[:2] not in flushed_loads for target in expected):
            raise ObservationError('installed load lacks actual Agent sample/encode/flush evidence')
        query_proofs, query_targets = bind_queries([row for row in rows if row['event'] == 'result_query'], tui_installs, expected_incomplete)
        fault_events = validate_faults(rows, self.plan)
        return {'trace_events': len(rows), **{name: final[name] for name in COUNTS},
                'snapshot_samples': len(samples), 'snapshot_encodes': len(encodes), 'snapshot_flushes': len(sends),
                'result_queries_by_loop': dict(per_loop), 'result_queries_by_target': query_targets,
                'canonical_query_proofs': query_proofs, 'causal_gauge_updates': causal_updates,
                'gauges': final['gauges'], 'work': final['work'],
                'availability_pages': {name: final[name] for name in PAGE_COUNTS},
                'fault_events': fault_events}
