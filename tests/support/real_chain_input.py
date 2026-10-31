#!/usr/bin/env python3
"""Actual Runtime ToolInput -> real phone answer -> same-tool continuation."""

import argparse
import json
import os
import signal
import sys
import time
from pathlib import Path

from canonical_observer import ObservationError, text_part, tool_part
from real_chain_load import Chain, Plan, text_response, tool_response, percentile
from real_chain_scenarios import phone, phone_submit, ack, new_loop, verify_install, latest_control


class InputPlan(Plan):
    def __init__(self, kind, count):
        super().__init__()
        self.kind, self.request_limit = kind, count * 2
        self.continuations = 0

    def is_tool(self, index):
        # Continuation verification below is specific to ToolInput, not read.
        return False

    def answer_text(self, index):
        return '   ' if index % 2 == 0 else 'literal  雪  answer'

    def text(self, index):
        return f'INPUT{self.kind.upper()}{index:04d}END'

    def respond(self, handler, index, request):
        turn = index // 2
        call = f'input-call-{turn:04d}'
        if index % 2 == 0:
            body = tool_response('e2e_input', {'kind': self.kind}, call)
        else:
            outputs = [item for item in request.get('input', [])
                       if item.get('type') == 'function_call_output' and item.get('call_id') == call]
            if len(outputs) != 1:
                raise AssertionError('actual input continuation lost its original ToolCallId')
            returned = json.loads(outputs[0]['output'])
            expected = {'answer': self.answer_text(turn)} if self.kind == 'text' else {
                'choice_index': 1, 'choice': 'Second choice'}
            if returned != expected:
                raise AssertionError('actual Runtime input answer was trimmed, retyped or retargeted')
            self.continuations += 1
            body = text_response(self.text(turn))
        handler.send_response(200)
        handler.send_header('Content-Type', 'text/event-stream')
        handler.send_header('Content-Length', str(len(body)))
        handler.send_header('Connection', 'close')
        handler.end_headers()
        handler.wfile.write(body)
        handler.wfile.flush()
        return True


def run_input(binary, kind, count, negative_targets=False):
    provider = InputPlan(kind, count)
    latency = []
    with Chain(binary, None, provider, remote=True,
               agent_plan={'schema': 1, 'input_fixture': True}) as chain:
        peer = phone(chain, 'real-input-phone')
        seen = []
        try:
            for index in range(count):
                submitted = time.monotonic()
                phone_submit(chain, peer, f'actual-input-{index}')
                loop = new_loop(chain, excluded=seen)
                seen.append(loop)
                chain.wait_for('actual Runtime input snapshot projected to phone', lambda:
                               peer.state.get('interaction') is not None
                               and peer.state['interaction']['loop_id'] == loop)
                pending = peer.state['interaction']
                expected_kind = 'text' if kind == 'text' else 'choice'
                if pending['kind'] != expected_kind:
                    raise ObservationError('actual text/choice question was projected as approval')
                control = latest_control(chain)['interaction']
                if control is None or control['interaction_id'] != pending['id'] or control['loop_id'] != loop:
                    raise ObservationError('phone question differs from installed authoritative control')
                if control['tool_call_id'] != f'input-call-{index:04d}' or control['request_index'] != 0:
                    raise ObservationError('input question belongs to another actual tool request')
                if negative_targets:
                    operation = peer.intent('answer', loop_id=loop, interaction_id='stale-fixture-interaction', choice='allow')
                    chain.wait_for('stale actual-phone answer refusal', lambda: operation in peer.acks)
                    refusal = peer.acks.pop(operation)
                    if refusal.get('ok') is not False or peer.state['interaction']['id'] != pending['id']:
                        raise ObservationError('stale answer consumed or replaced the actual question')
                choice = {'text': provider.answer_text(index)} if kind == 'text' else 1
                operation = peer.intent('answer', loop_id=loop, interaction_id=pending['id'], choice=choice)
                ack(chain, peer, operation)
                verify_install(chain, loop, [tool_part(f'input-call-{index:04d}', 0), text_part(provider.text(index), 1)])
                accepted = [row for row in chain.agent_trace.records() if row['event'] == 'input_fixture_answer'
                            and row['loop_id'] == loop and row.get('accepted')]
                if len(accepted) != 1 or accepted[0]['interaction_id'] != pending['id']:
                    raise ObservationError('actual Runtime answer boundary did not confirm same interaction')
                if accepted[0]['answer_class'] != kind:
                    raise ObservationError('actual Runtime accepted another answer class')
                latency.append(time.monotonic() - submitted)
            if provider.continuations != count:
                raise ObservationError('not every actual input resumed its original tool continuation')
            chain.shutdown()
            report = chain.observation_report(count)
            if report['agent_observation']['fault_events']:
                raise ObservationError('actionable-input fixture unexpectedly activated fault injection')
            return {'class': 'input_' + kind, 'completions': count, 'actual_tool_continuations': provider.continuations,
                    'latency_seconds': latency, **report}
        finally:
            peer.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary')
    parser.add_argument('case', choices=('input-text', 'input-choice'))
    parser.add_argument('--count', type=int, default=2)
    parser.add_argument('--natural', action='store_true')
    args = parser.parse_args()
    if not 1 <= args.count <= 200 or (args.natural and args.count < 100):
        parser.error('bounded count; natural needs >=100 real completions')
    if os.environ.get('MINICORE_E2E_CAPTURE_AGENT') != '1':
        parser.error('actual Agent fixture/answer boundary capture required')
    kind = 'text' if args.case == 'input-text' else 'single_choice'
    base = Path(os.environ['MINICORE_E2E_EVIDENCE_DIR'])
    remaining, results = args.count, []
    while remaining:
        count = min(16, remaining)
        directory = base / f'batch-{len(results):03d}'
        directory.mkdir()
        os.environ['MINICORE_E2E_EVIDENCE_DIR'] = str(directory)
        try:
            result = run_input(args.binary, kind, count, negative_targets=not args.natural)
            (directory / 'result.json').write_text(json.dumps(result, sort_keys=True, indent=2) + '\n')
            results.append(result)
        finally:
            os.environ['MINICORE_E2E_EVIDENCE_DIR'] = str(base)
        remaining -= count
    latency = [value for row in results for value in row['latency_seconds']]
    print(json.dumps({'class': args.case, 'mode': 'natural_fixture' if args.natural else 'correctness',
                      'completions': args.count, 'canonical_installs_verified': sum(row['canonical_installs_verified'] for row in results),
                      'injected_faults': [], 'seconds_p50': percentile(latency, .5), 'seconds_p95': percentile(latency, .95),
                      'seconds_p99': percentile(latency, .99), 'seconds_max': max(latency)}, sort_keys=True))


def alarm(_signum, _frame):
    raise TimeoutError('real actionable-input chain exceeded outer bound')


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, alarm)
    signal.signal(signal.SIGALRM, alarm)
    signal.alarm(600)
    try:
        main()
    except Exception as error:
        print('real-chain input: ' + type(error).__name__ + ': ' + str(error), file=sys.stderr)
        sys.exit(1)
    finally:
        signal.alarm(0)
