"""Causal owner/query proofs; writer arrival order is not ownership order."""

from collections import defaultdict

from canonical_observer import ObservationError, identity, integer


def target(row):
    return tuple(identity(row.get(field), field) for field in ('session_id', 'load', 'loop_id'))


class GaugeHistory:
    def __init__(self, names):
        self.rows = {name: {} for name in names}

    def record(self, row):
        name = row.get('name')
        if name not in self.rows:
            raise ObservationError('unknown causal Agent gauge')
        revision = integer(row.get('revision'), 'gauge revision')
        if revision == 0 or revision in self.rows[name]:
            raise ObservationError('duplicate/zero ownership gauge revision')
        integer(row.get('previous_current'), 'previous owner current')
        if row.get('action') not in ('add', 'sub') or integer(row.get('amount'), 'owner amount') == 0:
            raise ObservationError('missing actual gauge ownership action')
        self.rows[name][revision] = row

    def reconcile(self, checkpoints, final):
        for name, revisions in self.rows.items():
            states = {0: dict(revision=0, current=0, peak=0)}
            previous, peak = 0, 0
            for expected, (revision, row) in enumerate(sorted(revisions.items()), 1):
                if revision != expected or row['previous_current'] != previous:
                    raise ObservationError('missing/inconsistent causal ownership update: ' + name)
                current = previous + row['amount'] if row['action'] == 'add' else previous - row['amount']
                if current < 0 or row['current'] != current:
                    raise ObservationError('gauge action/current contradiction: ' + name)
                peak = max(peak, current)
                if row['peak'] != peak:
                    raise ObservationError('gauge causal peak contradiction: ' + name)
                states[revision] = dict(revision=revision, current=current, peak=peak)
                previous = current
            for checkpoint in [*checkpoints, final]:
                meter = checkpoint['gauges'][name]
                if states.get(meter['revision']) != meter:
                    raise ObservationError('counter checkpoint contradicts actual gauge revision/current: ' + name)
            if final['gauges'][name] != states[max(states)] or previous != 0:
                raise ObservationError('terminal checkpoint hides a latest live owner: ' + name)
        return {name: len(rows) for name, rows in self.rows.items()}


def cursor(value):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {'item', 'offset'}:
        raise ObservationError('invalid actual query cursor')
    return integer(value['item'], 'cursor item'), integer(value['offset'], 'cursor offset')


def bind_queries(rows, installs, incomplete):
    installed = {target(row): row for row in installs}
    known = set(installed) | set(incomplete or {})
    available = defaultdict(list)
    by_target = defaultdict(list)
    attempts = set()
    counts = defaultdict(int)
    for row in rows:
        query_id = integer(row.get('query_id'), 'actual query_id')
        if query_id == 0 or query_id in attempts:
            raise ObservationError('duplicate/zero actual query attempt')
        attempts.add(query_id)
        key = target(row)
        if key not in known:
            raise ObservationError('query outcome targets an unrelated UI session/load/loop')
        counts[key] += 1
        by_target[key].append(row)
        requested = row.get('requested_result_id')
        if 'requested_result_id' not in row:
            raise ObservationError('missing actual query requested result identity')
        if requested is not None:
            identity(requested, 'requested_result_id')
        current, next_cursor = cursor(row.get('cursor')), cursor(row.get('next_cursor'))
        if next_cursor is not None and next_cursor <= (current or (0, 0)):
            raise ObservationError('actual query cursor did not advance')
        if 'unavailable_reason' not in row or row['unavailable_reason'] not in (
                None, 'expired', 'too_large', 'resource_limit', 'internal_error', 'not_retained'):
            raise ObservationError('missing/unknown actual query unavailable reason')
        if row.get('availability') != 'unavailable' and row['unavailable_reason'] is not None:
            raise ObservationError('query claims an unavailable reason for another outcome')
        if row.get('availability') not in ('live', 'stored'):
            continue
        if current is not None and requested is None:
            raise ObservationError('nonfirst actual result page lacks requested immutable identity')
        if requested is not None and requested != row['result_id']:
            raise ObservationError('successful query changed requested immutable identity')
        available[key].append(row)
    if attempts != set(range(1, len(rows) + 1)):
        raise ObservationError('actual query attempt IDs/counts are discontinuous')
    for key, status in (incomplete or {}).items():
        outcomes = by_target[key]
        matches = (any(row.get('availability') == 'unavailable' and row.get('unavailable_reason') is not None for row in outcomes)
                   if status == 'unavailable' else
                   any(row.get('availability') in ('live', 'stored') for row in outcomes) if status == 'partial' else
                   any(row.get('error') is not None or row.get('availability') == 'pending' for row in outcomes))
        if not matches:
            raise ObservationError('actual UI incomplete target lacks its own matching query outcome')
    proofs = []
    for key, installed_row in installed.items():
        result_id = identity(installed_row.get('result_id'), 'actual UI installed result_id')
        count = integer(installed_row.get('read_item_count'), 'actual UI verified read item count')
        matching = [row for row in available[key] if row['result_id'] == result_id]
        if any(row['total'] != count for row in matching):
            raise ObservationError('actual Agent page total differs from installed verified read item count')
        pages = sorted(matching, key=lambda row: row['query_id'])
        # Reachability is by exact immutable cursor/result identity and actual
        # admission ID, never by completion enqueue order or another clock.
        reached = {}
        complete = None
        for page in pages:
            current = cursor(page['cursor'])
            prefix = [] if current is None else reached.get((current, page['total']))
            if prefix is None:
                continue
            path = [*prefix, page['query_id']]
            following = cursor(page['next_cursor'])
            if following is None:
                complete = path
                break
            reached.setdefault((following, page['total']), path)
        if complete is None:
            raise ObservationError('actual UI canonical result lacks its own full successful query-page chain')
        proofs.append(dict(session_id=key[0], load=key[1], loop_id=key[2], result_id=result_id,
                           actual_query_ids=complete, actual_available_pages=len(complete)))
    return proofs, [dict(session_id=key[0], load=key[1], loop_id=key[2], queries=count)
                    for key, count in sorted(counts.items())]


def validate_faults(rows, plan):
    steps = {step['id']: step for step in (plan or {}).get('steps', [])}
    if len(steps) != len((plan or {}).get('steps', [])):
        raise ObservationError('duplicate expected fault step')
    loops = {target(row): row for row in rows if row['event'] == 'loop_installed'}
    seen = defaultdict(dict)
    reasons = {('append', 'rollback'): 'append_rolled_back', ('append', 'unknown'): 'append_unknown',
               ('runtime_events', 'drop_terminal'): 'hold_until_runtime_join',
               ('before_persist', 'pause'): 'paused', ('result_page', 'pause'): 'paused'}
    events = [row for row in rows if row['event'].startswith('fault_') or row['event'] == 'runtime_tail_dropped']
    for row in events:
        step = steps.get(row.get('id'))
        if step is None:
            raise ObservationError('unplanned actual Agent fault observation')
        actual = loops.get(target(row))
        if actual is None or (row.get('session'), row.get('turn')) != (step['session'], step['turn']) or (
                actual.get('session'), actual.get('turn')) != (step['session'], step['turn']):
            raise ObservationError('fault identity/ordinal differs from its actual installed target')
        event = row['event']
        if event in seen[step['id']]:
            raise ObservationError('duplicate actual fault transition')
        seen[step['id']][event] = row
        if event == 'runtime_tail_dropped':
            if (step['at'], step['action']) != ('runtime_events', 'drop_terminal') or integer(row.get('buffered_events_discarded'), 'discarded events') == 0:
                raise ObservationError('tail-drop observation did not match the actual planned terminal loss')
            continue
        if (row.get('point'), row.get('action')) != (step['at'], step['action']):
            raise ObservationError('actual fault point/action differs from expected plan')
        reason = row.get('reason')
        if event == 'fault_ready':
            if step['action'] != 'pause' or reason != 'paused':
                raise ObservationError('invalid readiness for actual fault plan')
        elif event == 'fault_applied':
            if reasons.get((step['at'], step['action'])) != reason:
                raise ObservationError('actual fault outcome differs from expected plan')
        elif event == 'fault_released':
            if step['action'] != 'pause' or reason not in ('release', 'close_or_cancel'):
                raise ObservationError('unpaired/failed actual fault release')
    for name, step in steps.items():
        actual = seen[name]
        required = {'fault_ready', 'fault_applied', 'fault_released'} if step['action'] == 'pause' else (
            {'fault_applied', 'runtime_tail_dropped'} if step['action'] == 'drop_terminal' else {'fault_applied'})
        if set(actual) != required:
            raise ObservationError('missing/unpaired actual planned fault transitions: ' + name)
        if len({target(row) for row in actual.values()}) != 1:
            raise ObservationError('fault transitions changed actual target')
        if step['action'] == 'pause' and not (actual['fault_ready']['event_seq'] < actual['fault_applied']['event_seq'] < actual['fault_released']['event_seq']):
            raise ObservationError('actual pause lifecycle did not follow ready/apply/release')
    return events
