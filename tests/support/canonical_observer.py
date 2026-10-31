"""Strict test-only consumer for the real-main installed-content observer v1.

Fixture text is only an oracle. Nothing in this module queries an Agent or
promotes generated text, a result-read response, or terminal markers to proof
of an installed canonical projection.
"""

import hashlib
import json
import re
from pathlib import Path


class ObservationError(AssertionError):
    pass


def text_part(text, request_index=0, channel="text"):
    body = text.replace("\r\n", "\n").encode("utf-8")
    return {
        "kind": "assistant",
        "request_index": request_index,
        "channel": channel,
        "bytes": len(body),
        "sha256": hashlib.sha256(body).hexdigest(),
    }


def body_digest(text):
    part = text_part(text)
    return {field: part[field] for field in ('bytes', 'sha256')}


def tool_part(call_id, request_index=0, state="succeeded"):
    return {
        "kind": "tool", "request_index": request_index,
        "tool_call_id": call_id, "state": state,
    }


def integer(value, name):
    if type(value) is not int or value < 0:
        raise ObservationError(f"invalid {name}")
    return value


def identity(value, name):
    if not isinstance(value, str) or not value or len(value.encode()) > 4096:
        raise ObservationError(f"invalid {name}")
    return value


def parts(value):
    if not isinstance(value, list) or len(value) > 4096:
        raise ObservationError("invalid parts")
    result = []
    for part in value:
        if not isinstance(part, dict):
            raise ObservationError("invalid part")
        integer(part.get("request_index"), "request_index")
        if part.get("kind") == "assistant":
            if set(part) != {"kind", "request_index", "channel", "bytes", "sha256"}:
                raise ObservationError("invalid assistant part fields")
            if part["channel"] not in ("text", "reasoning"):
                raise ObservationError("invalid assistant channel")
            if integer(part["bytes"], "bytes") == 0:
                raise ObservationError("empty assistant channel must be excluded")
            if not isinstance(part["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", part["sha256"]):
                raise ObservationError("invalid installed hash")
        elif part.get("kind") == "tool":
            if set(part) != {"kind", "request_index", "tool_call_id", "state"}:
                raise ObservationError("invalid tool part fields")
            identity(part["tool_call_id"], "tool_call_id")
            identity(part["state"], "tool state")
        else:
            raise ObservationError("invalid part kind")
        result.append(part.copy())
    return result


TUI_COUNTERS = frozenset(('client_query_requests', 'client_query_in_flight', 'client_query_peak',
                          'client_read_pages', 'client_recovery_queries', 'client_recovery_pages',
                          'client_workspaces', 'client_workspaces_peak', 'client_workspace_reserved_bytes',
                          'client_workspace_reserved_peak_bytes', 'client_recovery_workspaces',
                          'client_recovery_workspaces_peak', 'client_recovery_workspace_reserved_bytes',
                          'client_recovery_workspace_reserved_peak_bytes', 'client_early_body_bytes',
                          'client_early_body_peak_bytes', 'early_body_bytes', 'early_body_peak_bytes',
                          'pending_recovery_count', 'recovery_in_flight'))


class Trace:
    MAX_BYTES = 32 * 1024 * 1024
    MAX_LINE = 1024 * 1024
    MAX_EVENTS = 20000

    def __init__(self, path):
        self.path = Path(path)
        self.offset = 0
        self.pending = b""
        self.ready = self.ended = False
        self.event_seq = 0
        self.mono_ns = 0
        self.installs = []
        self.controls = []
        self.views = {}
        self.counters = []
        self.identities = set()
        self.revisions = {}
        self.application_pause = None
        self.recovery_states = {}
        self.recovery_events = []
        self.phone_pauses = {}
        self.phone_events = []
        self.input_oracles = {}
        self.steering_oracles = {}
        self.completion_order = {}
        self.canonical_oracles = {}
        self.delivered_facts = {}
        self.delivered_sequences = {}

    def register_input(self, receipt, text, kind='prompt', target_loop=None, request_index=None):
        target = tuple(identity(receipt.get(field), field) for field in ('session_id', 'load', 'input_id'))
        expected = {'body': body_digest(text), 'user_kind': kind}
        if target in self.input_oracles and self.input_oracles[target] != expected:
            raise ObservationError('one accepted input identity has conflicting fixture bytes')
        self.input_oracles[target] = expected
        if kind == 'steering':
            loop = identity(target_loop, 'fixture steering target loop')
            boundary = integer(request_index, 'independent expected steering request boundary')
            self.steering_oracles.setdefault((*target[:2], loop), []).append(dict(expected, input_id=target[2], request_index=boundary))

    def register_canonical(self, row, expected_parts, persistence='persisted', uncertain=False):
        target = tuple(identity(row.get(field), field) for field in ('session_id', 'load', 'loop_id'))
        expected = dict(parts=parts(expected_parts), persistence=persistence, persistence_uncertain=uncertain)
        self._saving(expected)
        if target in self.canonical_oracles and self.canonical_oracles[target] != expected:
            raise ObservationError('one turn has conflicting independent canonical oracles')
        self.canonical_oracles[target] = expected

    def oracle_document(self, incomplete=None, agent_plan=None):
        document = dict(schema='fixture-oracle-r3-1', agent_plan=agent_plan,
            inputs=[dict(session_id=key[0], load=key[1], input_id=key[2], **value)
                    for key, value in sorted(self.input_oracles.items())],
            steering=[dict(session_id=key[0], load=key[1], loop_id=key[2], **value)
                      for key, values in sorted(self.steering_oracles.items()) for value in values],
            canonical=[dict(session_id=key[0], load=key[1], loop_id=key[2], **value)
                       for key, value in sorted(self.canonical_oracles.items())],
            incomplete=[dict(session_id=key[0], load=key[1], loop_id=key[2], status=value)
                        for key, value in sorted((incomplete or {}).items())])
        if len(json.dumps(document).encode()) > 16 * 1024 * 1024:
            raise ObservationError('independent fixture oracle budget exceeded')
        return document

    def load_oracle(self, document):
        if not isinstance(document, dict) or set(document) != {'schema', 'agent_plan', 'inputs', 'steering', 'canonical', 'incomplete'} or document['schema'] != 'fixture-oracle-r3-1':
            raise ObservationError('missing/invalid independent fixture oracle schema')
        if document['agent_plan'] is not None and not isinstance(document['agent_plan'], dict):
            raise ObservationError('invalid independent expected Agent plan')
        for name in ('inputs', 'steering', 'canonical', 'incomplete'):
            if not isinstance(document[name], list) or len(document[name]) > 4096:
                raise ObservationError('invalid independent fixture oracle inventory')
        for row in document['inputs']:
            key = tuple(identity(row.get(field), field) for field in ('session_id', 'load', 'input_id'))
            body = row.get('body')
            if not isinstance(body, dict) or set(body) != {'bytes', 'sha256'} or not isinstance(body['sha256'], str) or not re.fullmatch('[0-9a-f]{64}', body['sha256']):
                raise ObservationError('invalid independent input hash')
            integer(body['bytes'], 'expected input bytes')
            if row.get('user_kind') not in ('prompt', 'steering') or key in self.input_oracles:
                raise ObservationError('duplicate/invalid independent input oracle')
            self.input_oracles[key] = {field: row[field] for field in ('body', 'user_kind')}
        for row in document['steering']:
            key = tuple(identity(row.get(field), field) for field in ('session_id', 'load', 'loop_id'))
            input_id = identity(row.get('input_id'), 'expected steering input_id')
            expected = self.input_oracles.get((*key[:2], input_id))
            if expected != {field: row.get(field) for field in ('body', 'user_kind')} or row.get('user_kind') != 'steering':
                raise ObservationError('steering oracle differs from independently accepted input')
            boundary = integer(row.get('request_index'), 'expected steering boundary')
            self.steering_oracles.setdefault(key, []).append(dict(expected, input_id=input_id, request_index=boundary))
        for row in document['canonical']:
            self.register_canonical(row, row.get('parts'), row.get('persistence'), row.get('persistence_uncertain'))
        incomplete = {}
        for row in document['incomplete']:
            key = tuple(identity(row.get(field), field) for field in ('session_id', 'load', 'loop_id'))
            if key in incomplete or row.get('status') not in ('partial', 'unavailable', 'retryable_error'):
                raise ObservationError('duplicate/invalid expected incomplete target')
            incomplete[key] = row['status']
        return incomplete

    def poll(self):
        if not self.path.exists():
            return
        size = self.path.stat().st_size
        if size < self.offset:
            raise ObservationError("observer trace was truncated")
        if size > self.MAX_BYTES:
            raise ObservationError("observer trace byte budget exceeded")
        with self.path.open("rb") as handle:
            handle.seek(self.offset)
            data = handle.read(self.MAX_BYTES + 1 - self.offset)
        self.offset += len(data)
        self.pending += data
        while b"\n" in self.pending:
            line, self.pending = self.pending.split(b"\n", 1)
            if len(line) > self.MAX_LINE:
                raise ObservationError("observer line budget exceeded")
            try:
                row = json.loads(line)
            except (ValueError, UnicodeError) as error:
                raise ObservationError("malformed observer JSON") from error
            self.consume(row)
        if len(self.pending) > self.MAX_LINE:
            raise ObservationError("observer line budget exceeded")

    def consume(self, row):
        if not isinstance(row, dict) or row.get("schema") != 1:
            raise ObservationError("unsupported observer schema")
        if self.ended:
            raise ObservationError("observer event after end")
        seq = integer(row.get("event_seq"), "event_seq")
        if seq != self.event_seq + 1 or seq > self.MAX_EVENTS:
            raise ObservationError("observer event sequence gap/overflow")
        stamp = integer(row.get("mono_ns"), "mono_ns")
        if stamp < self.mono_ns:
            raise ObservationError("observer monotonic clock regressed")
        self.event_seq, self.mono_ns = seq, stamp
        event = row.get("event")
        if event == "observer_ready":
            if self.ready or seq != 1:
                raise ObservationError("duplicate/late observer ready")
            self.ready = True
            return
        if not self.ready:
            raise ObservationError("observer not ready")
        if event == "observer_end":
            self.ended = True
            return
        if event in ('phone_send_paused', 'phone_send_resumed', 'phone_overflow_closed'):
            connection = integer(row.get('connection_id'), 'connection_id')
            view = identity(row.get('view_epoch'), 'view_epoch')
            target = (view, row.get('session_id'), row.get('load'))
            if event == 'phone_send_paused':
                if connection in self.phone_pauses:
                    raise ObservationError('phone drain pause duplicated')
                self.phone_pauses[connection] = target
            else:
                if self.phone_pauses.pop(connection, None) != target:
                    raise ObservationError('phone drain terminal targets another connection/view')
                if event == 'phone_overflow_closed' and row.get('reason') != 'overflow':
                    raise ObservationError('phone close was not a real queue overflow')
            self.phone_events.append(row)
            return
        session = identity(row.get("session_id"), "session_id")
        load = identity(row.get("load"), "load")
        key = session, load
        if event in ("application_paused", "application_resumed"):
            held = identity(row.get("held_kind"), "held_kind")
            target = session, load, held
            if event == "application_paused":
                if self.application_pause is not None:
                    raise ObservationError("overlapping main application pause")
                self.application_pause = target
            else:
                if self.application_pause != target:
                    raise ObservationError("main application release target mismatch")
                self.application_pause = None
            return
        if event == "recovery_status":
            loop = identity(row.get("loop_id"), "loop_id")
            status = row.get("status")
            if status not in ("pending", "partial", "unavailable", "retryable_error", "complete"):
                raise ObservationError("unknown recovery status")
            if status in ('partial', 'unavailable', 'retryable_error'):
                terminal = row.get('terminal')
                if row.get('canonical') is not False or not isinstance(terminal, dict):
                    raise ObservationError('noncomplete recovery lacks actual noncanonical terminal')
                integer(terminal.get('block_index'), 'recovery terminal index')
                if terminal.get('saving') not in ('saved', 'not saved', 'save unconfirmed'):
                    raise ObservationError('noncomplete recovery lacks actual Saving classification')
                identity(row.get('reason'), 'recovery reason')
            self.recovery_states[(session, load, loop)] = status
            self.recovery_events.append(row)
            return
        if event == "control_applied":
            revision = integer(row.get("revision"), "revision")
            if revision <= self.revisions.get(key, -1):
                raise ObservationError("applied revision did not advance")
            self.revisions[key] = revision
            ids = row.get("queue_ids")
            if not isinstance(ids, list) or len(ids) > 32:
                raise ObservationError("invalid queue IDs")
            for value in ids:
                identity(value, "input_id")
            if len(ids) != len(set(ids)) or type(row.get("paused")) is not bool:
                raise ObservationError("invalid queue observation")
            observed = integer(row.get("observed_through"), "observed_through")
            if integer(row.get("settled_through"), "settled_through") > observed:
                raise ObservationError("settled exceeds observed")
            execution = row.get("execution")
            if not isinstance(execution, dict) or execution.get("kind") not in ("none", "starting", "running", "finishing"):
                raise ObservationError("invalid applied execution")
            if execution["kind"] in ("running", "finishing"):
                identity(execution.get("loop_id"), "execution loop_id")
            interaction = row.get("interaction")
            if interaction is not None:
                if not isinstance(interaction, dict):
                    raise ObservationError("invalid interaction")
                for field in ("loop_id", "tool_call_id", "interaction_id", "kind"):
                    identity(interaction.get(field), field)
                integer(interaction.get("request_index"), "interaction request_index")
                if execution.get("loop_id") != interaction["loop_id"]:
                    raise ObservationError("interaction targets another execution")
            if 'block_reason' not in row or row['block_reason'] not in (None, 'persistence', 'internal'):
                raise ObservationError('missing/invalid actual applied block reason')
            self.controls.append(row)
        elif event == "canonical_install":
            loop = identity(row.get("loop_id"), "loop_id")
            target = session, load, loop
            if key not in self.revisions:
                raise ObservationError("install without applied control")
            if target in self.identities:
                raise ObservationError("duplicate canonical install")
            identity(row.get("result_id"), "result_id")
            integer(row.get('read_item_count'), 'actual fully verified read item count')
            if row.get("completion_seq") is not None:
                integer(row["completion_seq"], "completion_seq")
            if row.get("input_id") is not None:
                identity(row["input_id"], "input_id")
            self._saving(row)
            if row.get("recovery_status") != "complete":
                raise ObservationError("non-complete canonical install")
            parts(row.get("parts"))
            self.identities.add(target)
            self.installs.append(row)
        elif event == "canonical_view":
            turns = row.get("turns")
            if not isinstance(turns, list) or len(turns) > 256:
                raise ObservationError("invalid canonical view")
            seen = set()
            for turn in turns:
                loop = identity(turn.get("loop_id"), "loop_id")
                if loop in seen:
                    raise ObservationError("duplicate turn in installed view")
                seen.add(loop)
                parts(turn.get("parts"))
                self._saving(turn)
            self._inventory(row.get('inventory'))
            delivered = row.get('delivered_inputs')
            if not isinstance(delivered, list) or len(delivered) > 256:
                raise ObservationError('missing/invalid actual delivered input identity suffix')
            seen_inputs = set()
            for item in delivered:
                if not isinstance(item, dict) or set(item) != {'loop_id', 'input_id', 'request_index'}:
                    raise ObservationError('invalid actual delivered steering metadata')
                input_id = identity(item['input_id'], 'delivered input_id')
                fact = dict(loop_id=identity(item['loop_id'], 'delivered loop_id'),
                            request_index=integer(item['request_index'], 'delivered request boundary'))
                target = (*key, input_id)
                if input_id in seen_inputs or target in self.delivered_facts and self.delivered_facts[target] != fact:
                    raise ObservationError('actual delivered identity has conflicting loop/boundary')
                self.delivered_facts[target] = fact
                self.delivered_sequences.setdefault(target, row['event_seq'])
                seen_inputs.add(input_id)
            if len(self.delivered_facts) > 4096:
                raise ObservationError('consumer delivered identity observation budget exceeded')
            self.views[key] = row
        elif event == "counters":
            values = row.get("values")
            if row.get('scope') != 'client_counters_and_session_gauges' or not isinstance(values, dict) or set(values) != TUI_COUNTERS:
                raise ObservationError("missing/unknown actual TUI counter schema")
            for name, value in values.items():
                identity(name, "counter name")
                integer(value, name)
            self.counters.append(row)
        else:
            raise ObservationError("unknown observer event")

    @staticmethod
    def _inventory(value):
        if not isinstance(value, list) or len(value) > 16384:
            raise ObservationError('missing/invalid global ordered installed inventory')
        previous = -1
        for entry in value:
            if not isinstance(entry, dict):
                raise ObservationError('invalid global inventory entry')
            index = integer(entry.get('block_index'), 'installed block_index')
            if index < previous or type(entry.get('preserved')) is not bool:
                raise ObservationError('installed inventory indices/provenance invalid')
            previous = index
            kind = entry.get('kind')
            if kind not in ('user', 'assistant', 'tool', 'turn_end', 'earlier_history', 'compaction'):
                raise ObservationError('unknown installed block kind')
            if kind in ('assistant', 'tool', 'turn_end'):
                identity(entry.get('loop_id'), 'installed loop_id')
            if kind in ('assistant', 'user'):
                body = entry.get('body')
                if not isinstance(body, dict) or set(body) != {'bytes', 'sha256'}:
                    raise ObservationError('invalid installed body digest')
                integer(body['bytes'], 'installed body bytes')
                if not isinstance(body['sha256'], str) or not re.fullmatch('[0-9a-f]{64}', body['sha256']):
                    raise ObservationError('invalid installed body hash')
            if kind == 'assistant':
                if entry.get('channel') not in ('reasoning', 'text') or type(entry.get('preserved_channel')) is not bool:
                    raise ObservationError('invalid installed channel provenance')
                integer(entry.get('request_index'), 'installed request_index')
            elif kind == 'tool':
                integer(entry.get('request_index'), 'installed tool request_index')
                identity(entry.get('tool_call_id'), 'installed tool_call_id')
                identity(entry.get('state'), 'installed tool state')
            elif kind == 'user':
                if entry.get('user_kind') not in ('prompt', 'steering', 'unknown') or 'request_index' not in entry:
                    raise ObservationError('invalid installed user classification/boundary metadata')
                if entry['request_index'] is not None:
                    integer(entry['request_index'], 'actual user request boundary')
            elif kind == 'turn_end' and (type(entry.get('canonical')) is not bool or entry.get('outcome') not in ('completed', 'cancelled', 'failed', 'unknown')):
                raise ObservationError('invalid actual terminal outcome/canonical classification')
        return value

    def _verify_inventory(self, key, view, installed, expected_incomplete):
        registry = {row['loop_id']: row for row in installed}
        incomplete = {target[2] for target in expected_incomplete if target[:2] == key}
        order = {target[2]: seq for target, seq in self.completion_order.items() if target[:2] == key}
        order.update({row['loop_id']: row.get('completion_seq') for row in installed})
        for row in self.recovery_events:
            if (row['session_id'], row['load']) == key and row['loop_id'] in incomplete:
                if row.get('completion_seq') is not None:
                    order[row['loop_id']] = row['completion_seq']
        previous = -1
        actual_parts, prompts, terminals, seen_users, steering_offsets = {}, {}, {}, set(), {}
        actual_sequences = {}
        for entry in self._inventory(view.get('inventory')):
            kind, loop = entry['kind'], entry.get('loop_id')
            if kind in ('earlier_history', 'compaction'):
                continue
            if loop not in registry and loop not in incomplete:
                raise ObservationError('unregistered extra loop/user in actual installed inventory')
            seq = order.get(loop)
            if seq is not None:
                if seq < previous:
                    raise ObservationError('global installed body/user/terminal order crossed completion regions')
                previous = seq
            if kind in ('user', 'assistant', 'tool') and terminals.get(loop) and not entry['preserved']:
                raise ObservationError('global canonical body/user occurs after its installed terminal')
            if kind == 'user':
                input_id = entry.get('input_id')
                expected = None
                if entry['user_kind'] == 'steering':
                    offset = steering_offsets.get(loop, 0)
                    oracles = self.steering_oracles.get((*key, loop), [])
                    if offset < len(oracles):
                        oracle = oracles[offset]
                        if input_id is None or input_id == oracle['input_id']:
                            expected = {field: oracle[field] for field in ('body', 'user_kind')}
                        observed_boundary = entry['request_index']
                        if observed_boundary is not None and observed_boundary != oracle['request_index']:
                            raise ObservationError('actual delivered steering request boundary changed')
                        if input_id is not None and observed_boundary is None:
                            raise ObservationError('known delivered steering identity lost its actual request boundary')
                    steering_offsets[loop] = offset + 1
                    # A canonical Runtime history user lacks Agent input_id.
                    # Verify its loop/kind/bytes, but never invent that identity.
                else:
                    identity(input_id, 'installed prompt input_id')
                    expected = self.input_oracles.get((*key, input_id))
                if input_id is not None:
                    target = (*key, identity(input_id, 'installed user input_id'))
                    if target in seen_users:
                        raise ObservationError('duplicate installed accepted user input')
                    seen_users.add(target)
                if expected is None or {field: entry[field] for field in ('body', 'user_kind')} != expected:
                    raise ObservationError('installed user/steering bytes or identity differ from accepted input oracle')
                actual_sequences.setdefault(loop, []).append({field: entry[field] for field in ('kind', 'user_kind', 'body')})
                if entry['user_kind'] == 'prompt':
                    prompts.setdefault(loop, []).append(input_id)
                continue
            if entry['preserved'] or kind == 'assistant' and entry['preserved_channel']:
                continue
            if loop in registry and not prompts.get(loop):
                raise ObservationError('global canonical body/terminal precedes its installed prompt')
            if kind == 'turn_end':
                terminals.setdefault(loop, []).append(entry)
            else:
                part = ({'kind': kind, 'request_index': entry['request_index'], 'channel': entry['channel'], **entry['body']}
                        if kind == 'assistant' else
                        {field: entry[field] for field in ('kind', 'request_index', 'tool_call_id', 'state')})
                actual_parts.setdefault(loop, []).append(part)
                actual_sequences.setdefault(loop, []).append(part)
        for target, oracles in self.steering_oracles.items():
            if target[:2] == key and steering_offsets.get(target[2], 0) != len(oracles):
                raise ObservationError('accepted steering body missing from actual installed inventory')
            if target[:2] == key:
                for oracle in oracles:
                    fact = self.delivered_facts.get((*key, oracle['input_id']))
                    if fact is not None and fact != dict(loop_id=target[2], request_index=oracle['request_index']):
                        raise ObservationError('actual delivered input targets another expected loop/request boundary')
        for loop, row in registry.items():
            oracle = self.canonical_oracles.get((*key, loop))
            if oracle is None or {field: row[field] for field in ('parts', 'persistence', 'persistence_uncertain')} != oracle:
                raise ObservationError('actual installed canonical result lacks its independent fixture oracle')
            expected_parts = oracle['parts']
            if actual_parts.get(loop, []) != expected_parts:
                raise ObservationError('global installed body differs from verified canonical parts')
            if prompts.get(loop) != [row['input_id']]:
                raise ObservationError('global installed prompt missing/duplicated/retargeted')
            prompt = self.input_oracles[(*key, row['input_id'])]
            expected_sequence = [dict(kind='user', **prompt)]
            steers = self.steering_oracles.get((*key, loop), [])
            if any(left['request_index'] > right['request_index'] for left, right in zip(steers, steers[1:])):
                raise ObservationError('independent expected steering boundaries regressed')
            next_steer = 0
            for part in expected_parts:
                while next_steer < len(steers) and steers[next_steer]['request_index'] <= part['request_index']:
                    oracle = steers[next_steer]
                    expected_sequence.append(dict(kind='user', **{field: oracle[field] for field in ('user_kind', 'body')}))
                    next_steer += 1
                expected_sequence.append(part)
            for oracle in steers[next_steer:]:
                expected_sequence.append(dict(kind='user', **{field: oracle[field] for field in ('user_kind', 'body')}))
            if actual_sequences.get(loop) != expected_sequence:
                raise ObservationError('actual global user/steering/request boundary order differs from independent fixture')
            ends = terminals.get(loop, [])
            saving = 'saved' if row['persistence'] == 'persisted' else 'save unconfirmed' if row['persistence_uncertain'] else 'not saved'
            if len(ends) != 1 or ends[0]['saving'] != saving or ends[0]['canonical'] is not True:
                raise ObservationError('global installed actual Saving/canonical terminal mismatch')
        for loop in incomplete:
            ends = terminals.get(loop, [])
            if len(ends) != 1 or ends[0]['canonical'] is not False or ends[0]['saving'] not in ('saved', 'not saved', 'save unconfirmed'):
                raise ObservationError('global incomplete region lacks persistent classified terminal')

    @staticmethod
    def _saving(row):
        if row.get("persistence") not in ("persisted", "failed"):
            raise ObservationError("missing persistence fact")
        if type(row.get("persistence_uncertain")) is not bool:
            raise ObservationError("missing persistence uncertainty")
        if row["persistence_uncertain"] and row["persistence"] == "persisted":
            raise ObservationError("uncertain result promoted to persisted")

    def expect_install(self, index, expected_parts, persistence="persisted", uncertain=False):
        self.poll()
        if len(self.installs) <= index:
            return None
        row = self.installs[index]
        self.register_canonical(row, expected_parts, persistence, uncertain)
        if parts(row["parts"]) != parts(expected_parts):
            raise ObservationError(f"installed canonical content/identity mismatch at {index}")
        if (row["persistence"], row["persistence_uncertain"]) != (persistence, uncertain):
            raise ObservationError(f"installed persistence mismatch at {index}")
        return row

    def finish(self, expected_count, expected_incomplete=None):
        self.poll()
        if self.pending or not self.ready or not self.ended:
            raise ObservationError("missing/unfinished real-main observer (requires test build)")
        if self.application_pause is not None:
            raise ObservationError("main application pause never released")
        if self.phone_pauses:
            raise ObservationError('phone drain pause neither released nor overflow-closed')
        expected_incomplete = expected_incomplete or {}
        for target, status in expected_incomplete.items():
            if self.recovery_states.get(target) != status or target in self.identities:
                raise ObservationError("expected explicit non-canonical UI status was not installed")
        incomplete = {target: status for target, status in self.recovery_states.items()
                      if status in ("partial", "unavailable", "retryable_error")
                      and target not in self.identities and target not in expected_incomplete}
        if incomplete:
            raise ObservationError("explicit non-complete recovery: " + repr(incomplete))
        if len(self.installs) != expected_count:
            raise ObservationError("canonical installation count mismatch")
        if not self.counters:
            raise ObservationError("missing actual recovery counters")
        grouped = {}
        for row in self.installs:
            grouped.setdefault((row["session_id"], row["load"]), []).append(row)
        for key, installed in grouped.items():
            if all(row.get("completion_seq") is not None for row in installed):
                installed = sorted(installed, key=lambda row: row["completion_seq"])
            view = self.views.get(key)
            if view is None or view["event_seq"] <= max(row["event_seq"] for row in installed):
                raise ObservationError("missing final installed canonical view")
            # The fixture deliberately stays within its retained-view budget.
            expected = [
                {field: row[field] for field in ("loop_id", "parts", "persistence", "persistence_uncertain")}
                for row in installed
            ]
            actual = [{field: turn[field] for field in ("loop_id", "parts", "persistence", "persistence_uncertain")}
                      for turn in view["turns"]]
            if actual != expected:
                raise ObservationError("final installed projection changed/lost/reordered canonical turns")
            self._verify_inventory(key, view, installed, expected_incomplete)
        for key, view in self.views.items():
            if key not in grouped:
                self._verify_inventory(key, view, [], expected_incomplete)
        for row in self.counters:
            values = row["values"]
            if values['client_query_peak'] > 4 or values['client_recovery_workspaces_peak'] > 2:
                raise ObservationError("actual query/recovery concurrency exceeded contract")
            for current, peak in (('client_query_in_flight', 'client_query_peak'),
                                  ('client_workspaces', 'client_workspaces_peak'),
                                  ('client_workspace_reserved_bytes', 'client_workspace_reserved_peak_bytes'),
                                  ('client_recovery_workspaces', 'client_recovery_workspaces_peak'),
                                  ('client_recovery_workspace_reserved_bytes', 'client_recovery_workspace_reserved_peak_bytes'),
                                  ('client_early_body_bytes', 'client_early_body_peak_bytes'),
                                  ('early_body_bytes', 'early_body_peak_bytes')):
                if values[current] > values[peak]:
                    raise ObservationError('TUI current exceeds measured peak: ' + current)
            if values['client_recovery_pages'] > values['client_read_pages'] or values['client_read_pages'] > values['client_query_requests'] or values['client_recovery_queries'] > values['client_query_requests']:
                raise ObservationError('TUI actual page/query classifications inconsistent')
        encoded = json.dumps(
            [dict(session_id=key[0], load=key[1], inventory=view['inventory'])
             for key, view in sorted(self.views.items())], sort_keys=True, separators=(",", ":")
        ).encode()
        return {
            "canonical_installs_verified": expected_count,
            "explicit_incomplete_targets": [dict(session_id=target[0], load=target[1], loop_id=target[2], status=status)
                                            for target, status in expected_incomplete.items()],
            "observed_projection_sha256": hashlib.sha256(encoded).hexdigest(),
            "global_inventory_entries_verified": sum(len(view['inventory']) for view in self.views.values()),
            "canonical_channels_verified": sum(part['kind'] == 'assistant' for row in self.installs for part in row['parts']),
            "canonical_tools_verified": sum(part['kind'] == 'tool' for row in self.installs for part in row['parts']),
            "user_bodies_verified": sum(entry['kind'] == 'user' for view in self.views.values() for entry in view['inventory']),
            "user_input_identities_verified": sum(entry['kind'] == 'user' and entry.get('input_id') is not None for view in self.views.values() for entry in view['inventory']),
            "user_input_identities_unavailable": sum(entry['kind'] == 'user' and entry.get('input_id') is None for view in self.views.values() for entry in view['inventory']),
            "steering_boundaries_verified": sum(len(value) for value in self.steering_oracles.values()),
            "steering_boundaries_unavailable_on_canonical_rows": sum(entry['kind'] == 'user' and entry.get('user_kind') == 'steering' and entry.get('request_index') is None for view in self.views.values() for entry in view['inventory']),
            "actual_delivered_steering_identities_observed": len(self.delivered_facts),
            "observer_events": self.event_seq,
            "recovery_counters": [row["values"] for row in self.counters[-2:]],
        }
