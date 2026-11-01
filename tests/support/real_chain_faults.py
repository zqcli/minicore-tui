#!/usr/bin/env python3
"""Actual-main Agent fault plans; never mixed with no-injection statistics."""

import argparse
import json
import os
from pathlib import Path

from canonical_observer import ObservationError, text_part, tool_part
from process_support import run_bounded
from real_chain_load import Chain, completed, delta, sse
from real_chain_scenarios import ScenarioPlan, latest_control, new_loop, phone, phone_submit, verify_install


def step(name, turn, point, action, session=1):
    return {'id': name, 'session': session, 'turn': turn, 'at': point, 'action': action}


def plan(steps, limits=None):
    return {'schema': 1, 'timeout_ms': 20000, 'steps': steps, 'limits': limits or {}}


def fault_ready(chain, name):
    chain.wait_for('actual Agent fault ready ' + name, lambda: chain.agent_trace.fault(name))
    row = chain.agent_trace.fault(name)
    installed = [row for row in chain.agent_trace.records() if row['event'] == 'loop_installed']
    if not any((value['session_id'], value['load'], value['loop_id']) ==
               (row['session_id'], row['load'], row['loop_id']) for value in installed):
        raise ObservationError('fault targeted an uninstalled Runtime turn')
    return row


def completion(chain, loop):
    return next((row for row in chain.agent_trace.completed() if row['loop_id'] == loop), None)


def queue_while_finishing(chain, before='finish-a'):
    ready = fault_ready(chain, before)
    chain.queue('already-accepted-follow-up-B')
    chain.wait_for('actual queued FollowUp while persistence held', lambda:
                   latest_control(chain) and len(latest_control(chain)['queue_ids']) == 1)
    control = latest_control(chain)
    if control['session_id'] != ready['session_id'] or control['load'] != ready['load']:
        raise ObservationError('queued FollowUp belongs to another load')
    if control['execution']['kind'] != 'finishing':
        raise ObservationError('Runtime join was prematurely projected as idle')
    return ready, control['queue_ids'][0]


def save_failure(binary, unknown=False):
    responses = ScenarioPlan('pagination', 1 if unknown else 2)
    configured = plan([step('finish-a', 1, 'before_persist', 'pause'),
                       step('fail-a', 1, 'append', 'unknown' if unknown else 'rollback')])
    with Chain(binary, None, responses, agent_plan=configured) as chain:
        chain.submit('save-failure-A')
        first, queued_id = queue_while_finishing(chain)
        a = first['loop_id']
        chain.release_fault('finish-a')
        verify_install(chain, a, [text_part(responses.text(0))], 'failed', unknown)
        chain.wait_for('failure pauses remaining accepted queue', lambda:
                       latest_control(chain)['execution']['kind'] == 'none'
                       and latest_control(chain)['paused']
                       and latest_control(chain)['queue_ids'] == [queued_id])
        chain.wait_for('actual Agent failure completion', lambda: completion(chain, a))
        fact = completion(chain, a)
        if fact['persistence'] != 'failed' or fact['persistence_uncertain'] != unknown or fact.get('next_loop_id') is not None:
            raise ObservationError('save failure changed actual successor/Saving facts')
        if unknown:
            if latest_control(chain).get('block_reason') != 'persistence':
                raise ObservationError('Unknown append lost actual installed Persistence block')
            chain.wait_for('actual uncertain Saving label', lambda: chain.screen.contains('save unconfirmed'))
            # The exact blocked label is production UI state, not inferred from
            # a missing request. Resume must fail without consuming queued B.
            chain.wait_for('actual persistence block label', lambda:
                           chain.screen.contains('Persistence blocked: reopen the session to reconcile the last save'))
            if chain.screen.contains('session is blocked'):
                raise ObservationError('resume refusal notice existed before the resume operation')
            chain.key(b'\r')
            chain.wait_for('new exact blocked resume refusal', lambda: chain.screen.contains('session is blocked'))
            if chain.provider.count() != 1 or latest_control(chain)['queue_ids'] != [queued_id]:
                raise ObservationError('Unknown append allowed a successor or consumed its queue')
            chain.shutdown()
            report = chain.observation_report(1)
        else:
            chain.wait_for('actual not-saved label', lambda: chain.screen.contains('not saved'))
            chain.key(b'\r')  # Production empty Enter -> input.resume.
            chain.wait_for('B actual provider request after explicit resume', lambda: chain.provider.count() == 2)
            b = new_loop(chain, excluded=[a])
            row = verify_install(chain, b, [text_part(responses.text(1))])
            if row['input_id'] != queued_id:
                raise ObservationError('resume started a different accepted input')
            # Recheck original saving after later persisted content is installed.
            verify_install(chain, a, [text_part(responses.text(0))], 'failed', False)
            chain.shutdown()
            view = chain.trace.views[(first['session_id'], first['load'])]['turns']
            if [turn['loop_id'] for turn in view] != [a, b] or [turn['persistence'] for turn in view] != ['failed', 'persisted']:
                raise ObservationError('A-unsaved/B-saved final order or original Saving changed')
            report = chain.observation_report(2)
        return {'case': 'append_unknown_blocked' if unknown else 'rollback_resume_order',
                'mode': 'injected_correctness', 'queued_input_id': queued_id, **report}


def before_persist(binary):
    responses = ScenarioPlan('before-persist', 2)
    configured = plan([step('finish-a', 1, 'before_persist', 'pause')])
    with Chain(binary, None, responses, agent_plan=configured) as chain:
        chain.submit('before-persist-A')
        first, queued_id = queue_while_finishing(chain)
        if chain.trace.installs or chain.agent_trace.completed():
            raise ObservationError('completion escaped before the actual persistence owner released')
        chain.release_fault('finish-a')
        chain.wait_for('persisted automatic successor', lambda: chain.provider.count() == 2)
        for seq in (1, 2):
            def found(seq=seq):
                return next((row for row in chain.trace.installs if row['completion_seq'] == seq), None)
            chain.wait_for('complete installed result', found)
            row = found()
            verify_install(chain, row['loop_id'], [text_part(responses.text(seq - 1))])
            if seq == 2 and row['input_id'] != queued_id:
                raise ObservationError('actual automatic successor input mismatch')
        chain.shutdown()
        return {'case': 'before_persist_finishing_then_successor', 'mode': 'injected_correctness',
                **chain.observation_report(2)}


def terminal_loss(binary):
    responses = ScenarioPlan('pagination', 1)
    configured = plan([step('tail-a', 1, 'runtime_events', 'drop_terminal')])
    with Chain(binary, None, responses, agent_plan=configured) as chain:
        chain.submit('terminal-loss-A')
        loop = new_loop(chain)
        verify_install(chain, loop, [text_part(responses.text(0))])
        dropped = lambda: next((row for row in chain.agent_trace.records()
                                if row['event'] == 'runtime_tail_dropped' and row['loop_id'] == loop), None)
        chain.wait_for('actual discarded terminal Runtime envelopes', dropped)
        if dropped()['buffered_events_discarded'] <= 0:
            raise ObservationError('terminal-loss fixture discarded no actual Runtime event')
        chain.shutdown()
        return {'case': 'terminal_tail_without_later_gap', 'mode': 'injected_correctness',
                'discarded_runtime_events': dropped()['buffered_events_discarded'],
                **chain.observation_report(1)}


def incomplete(chain, target, reason=None):
    key = target['session_id'], target['load'], target['loop_id']
    def observed():
        return next((row for row in reversed(chain.trace.recovery_events)
                     if (row['session_id'], row['load'], row['loop_id']) == key
                     and row.get('status') == 'unavailable'), None)
    chain.wait_for('explicit exact-target unavailable UI observation', observed, timeout=15)
    row = observed()
    terminal = row.get('terminal')
    if row.get('canonical') is not False or not isinstance(terminal, dict) or terminal.get('saving') != 'not saved':
        raise ObservationError('unavailable UI lost original actual unsaved/noncanonical Saving')
    original = completion(chain, target['loop_id'])
    if original is None or original.get('persistence') != 'failed' or original.get('persistence_uncertain') is not False:
        raise ObservationError('UI unavailability disagrees with original Agent completion')
    if terminal.get('outcome') != original.get('outcome_kind'):
        raise ObservationError('incomplete terminal changed original outcome classification')
    fragments = {'expired': 'expired', 'too_large': 'too large', 'resource_limit': 'read budget'}
    if reason is not None and fragments[reason] not in row.get('reason', '').lower():
        raise ObservationError('UI unavailability reason differs from actual source state')
    chain.expected_incomplete[key] = 'unavailable'
    if any(item['loop_id'] == target['loop_id'] for item in chain.trace.installs):
        raise ObservationError('partial/unavailable prefix was installed as complete canonical')
    return row


def held_page_pressure(binary):
    responses = ScenarioPlan('pagination', 2)
    configured = plan([step('finish-a', 1, 'before_persist', 'pause'),
                       step('rollback-a', 1, 'append', 'rollback'),
                       step('rollback-b', 2, 'append', 'rollback'),
                       step('page-a', 1, 'result_page', 'pause')],
                      {'sources_per_load': 1, 'sources': 1})
    with Chain(binary, None, responses, agent_plan=configured) as chain:
        chain.submit('held-page-A')
        first, queued_id = queue_while_finishing(chain)
        chain.release_fault('finish-a')
        held = fault_ready(chain, 'page-a')
        if held['loop_id'] != first['loop_id']:
            raise ObservationError('held page targets another Runtime loop')
        gauges = chain.agent_trace.gauges()
        charged = gauges['source_charged_bytes']['current']
        if not charged or gauges['retained_page_refs']['current'] < 1:
            raise ObservationError('actual held immutable source was not charged')
        chain.wait_for('failed A queue paused before explicit resume', lambda:
                       latest_control(chain)['paused'] and latest_control(chain)['queue_ids'] == [queued_id])
        chain.key(b'\r')
        chain.wait_for('B started while A page reference is held', lambda: chain.provider.count() == 2)
        chain.wait_for('B failure fully registered', lambda: len(chain.agent_trace.completed()) == 2)
        second = chain.agent_trace.completed()[-1]
        b = {'session_id': first['session_id'], 'load': first['load'], 'loop_id': second['loop_id']}
        chain.wait_for('catalog source evicted while original immutable reference held', lambda:
                       chain.agent_trace.gauges()['catalog_source_bytes']['current'] == 0)
        gauges = chain.agent_trace.gauges()
        if gauges['source_charged_bytes']['current'] != charged or gauges['retained_page_refs']['current'] < 1:
            raise ObservationError('eviction returned charge before last actual page reference')
        # Release in the real query deadline, not after a guessed timeout.
        chain.release_fault('page-a')
        incomplete(chain, first, 'expired')
        incomplete(chain, b, 'resource_limit')
        chain.wait_for('last source reference releases charge', lambda:
                       chain.agent_trace.gauges()['source_charged_bytes']['current'] == 0)
        chain.shutdown()
        report = chain.observation_report(0)
        work = report['agent_observation']['work']
        if work['source_prepare']['calls'] != 1 or work['source_hash']['calls'] != 1:
            raise ObservationError('held retained source was normalized/hashed repeatedly')
        return {'case': 'eviction_mid_page_and_reference_charge', 'mode': 'injected_correctness',
                'held_source_charged_bytes': charged, 'logical_completions': 2, **report}


def unavailable(binary):
    responses = ScenarioPlan('pagination', 1)
    configured = plan([step('rollback-a', 1, 'append', 'rollback')], {'source_bytes_per_load': 1024})
    with Chain(binary, None, responses, agent_plan=configured) as chain:
        chain.submit('unavailable-A')
        chain.wait_for('actual too-large unsaved completion', lambda: chain.agent_trace.completed())
        fact = chain.agent_trace.completed()[0]
        records = [row for row in chain.agent_trace.records() if row['event'] == 'loop_installed']
        target = next(row for row in records if row['loop_id'] == fact['loop_id'])
        if fact['unavailable_reason'] != 'too_large':
            raise ObservationError('lowered real retention limit did not produce too_large')
        incomplete(chain, target, 'too_large')
        chain.shutdown()
        return {'case': 'explicit_unsaved_unavailable', 'mode': 'injected_correctness',
                'logical_completions': 1, **chain.observation_report(0)}


def phone_recovery_reconnect(binary):
    responses = ScenarioPlan('pagination', 1)
    configured = plan([step('rollback-a', 1, 'append', 'rollback')], {'source_bytes_per_load': 1024})
    with Chain(binary, None, responses, remote=True, agent_plan=configured) as chain:
        peer = phone(chain, 'recovery-classification-phone')
        replacement = None
        try:
            receipt = phone_submit(chain, peer, 'actual unavailable phone result')
            chain.wait_for('actual unavailable Agent completion', lambda: len(chain.agent_trace.completed()) == 1)
            fact = chain.agent_trace.completed()[0]
            target = dict(session_id=receipt['session_id'], load=receipt['load'], loop_id=fact['loop_id'])
            note = incomplete(chain, target, 'too_large')
            def annotated(client):
                return next((block for block in client.blocks if block and block.get('type') == 'turn_end'
                             and block.get('loop_id') == fact['loop_id'] and block.get('recovery') is not None), None)
            chain.wait_for('actual persistent unavailable phone classification', lambda: annotated(peer))
            initial = annotated(peer)
            if initial['recovery'] != dict(status='unavailable', reason=note['reason']) or initial.get('saving') != 'not saved':
                raise ObservationError('phone changed recovery reason/status or original Saving before reconnect')
            token = peer.token
            peer.close()
            replacement = phone(chain, 'recovery-classification-reconnect', token=token)
            chain.wait_for('full reconnect keeps unavailable classification', lambda: annotated(replacement))
            current = annotated(replacement)
            if current['recovery'] != initial['recovery'] or current.get('saving') != initial['saving'] or replacement.snapshots != 1:
                raise ObservationError('phone reconnect lost persistent recovery classification/Saving')
            session = replacement.state.get('session') or {}
            if session.get('id') != receipt['session_id'] or session.get('load') != receipt['load']:
                raise ObservationError('phone reconnect classification belongs to another load')
            chain.shutdown()
            return dict(case='actual_phone_unavailable_recovery_full_reconnect', mode='injected_correctness',
                        reconnect_snapshots=replacement.snapshots, actual_recovery_status=current['recovery']['status'],
                        actual_recovery_reason=current['recovery']['reason'], actual_saving=current['saving'],
                        **chain.observation_report(0))
        finally:
            peer.close()
            if replacement:
                replacement.close()


class LargeSourcePlan(ScenarioPlan):
    def __init__(self):
        super().__init__('large-unsaved', 6)
        self.tool_name = 'read'
        self.context_window = 10_000_000

    def is_tool(self, index):
        return index < 5

    def text(self, index):
        return f'REQUEST{index:02d}' + 'x' * (220 * 1024 - 9)

    def respond(self, handler, index, request):
        text = self.text(index)
        events = [delta(text[start:start + 8192]) for start in range(0, len(text), 8192)]
        output = [{'type': 'message', 'id': f'msg-{index}', 'role': 'assistant',
                   'content': [{'type': 'output_text', 'text': text}]}]
        if index < 5:
            call = {'type': 'function_call', 'id': f'fc-{index}', 'call_id': f'call_tool_{index:04d}',
                    'name': 'read', 'arguments': json.dumps({'path': 'fixture.txt', 'limit': 32})}
            events.append({'type': 'response.output_item.done', 'output_index': 1, 'item': call})
            output.append(call)
        terminal = completed()
        terminal['response']['output'] = output
        body = sse(events + [terminal])
        handler.send_response(200)
        handler.send_header('Content-Type', 'text/event-stream')
        handler.send_header('Content-Length', str(len(body)))
        handler.send_header('Connection', 'close')
        handler.end_headers()
        handler.wfile.write(body)
        handler.wfile.flush()
        return True


def large_unsaved(binary):
    responses = LargeSourcePlan()
    configured = plan([step('rollback-a', 1, 'append', 'rollback'),
                       step('large-page', 1, 'result_page', 'pause')])
    with Chain(binary, None, responses, tools='"read"', agent_plan=configured) as chain:
        (chain.workspace / 'fixture.txt').write_text('fixture read content\n')
        chain.submit('large-unsaved-A')
        target = fault_ready(chain, 'large-page')
        charged = chain.agent_trace.gauges()['source_charged_bytes']['current']
        if charged <= 1024 * 1024:
            raise ObservationError('fixture did not prepare a real megabyte-scale retained source')
        before = next(row for row in reversed(chain.agent_trace.records()) if row['event'] == 'counters')
        normalization_calls = before['work']['normalize_durable']['calls']
        chain.release_fault('large-page')
        expected = []
        for index in range(6):
            expected.append(text_part(responses.text(index), index))
            if index < 5:
                expected.append(tool_part(f'call_tool_{index:04d}', index))
        verify_install(chain, target['loop_id'], expected, 'failed', False)
        chain.shutdown()
        report = chain.observation_report(1)
        work = report['agent_observation']['work']
        if (work['source_prepare']['calls'], work['source_hash']['calls'], work['result_projection_hash']['calls']) != (1, 1, 0):
            raise ObservationError('retained-source paging repeated whole-source preparation/hash/projection')
        if work['normalize_durable']['calls'] != normalization_calls:
            raise ObservationError('later retained pages re-normalized the whole turn')
        pages = [row for row in chain.agent_trace.records() if row['event'] == 'result_query'
                 and row['loop_id'] == target['loop_id'] and row.get('availability') == 'live']
        if len(pages) < 5 or len({row['result_id'] for row in pages}) != 1:
            raise ObservationError('large retained result pages lack stable identity or expected scale')
        return {'case': 'megabyte_unsaved_prepared_once', 'mode': 'injected_correctness',
                'retained_source_charged_bytes': charged, 'actual_available_pages': len(pages), **report}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary')
    parser.add_argument('case', choices=('save-rollback', 'save-unknown', 'before-persist', 'tail-loss', 'page-pressure', 'unavailable', 'large-unsaved', 'phone-recovery'))
    args = parser.parse_args()
    if os.environ.get('MINICORE_E2E_CAPTURE_AGENT') != '1':
        parser.error('real Agent faults require combined feature and capture=1')
    functions = {'save-rollback': lambda: save_failure(args.binary),
                 'save-unknown': lambda: save_failure(args.binary, True),
                 'before-persist': lambda: before_persist(args.binary),
                 'tail-loss': lambda: terminal_loss(args.binary),
                 'page-pressure': lambda: held_page_pressure(args.binary),
                 'unavailable': lambda: unavailable(args.binary),
                 'large-unsaved': lambda: large_unsaved(args.binary),
                 'phone-recovery': lambda: phone_recovery_reconnect(args.binary)}
    print(json.dumps(functions[args.case](), sort_keys=True))


if __name__ == "__main__":
    run_bounded(main, 150, "real-main fault")
