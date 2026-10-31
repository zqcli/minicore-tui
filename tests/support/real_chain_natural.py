#!/usr/bin/env python3
"""Ungated automatic-successor observations through real main and phone server.

Real tool requests keep an ordinary turn active; submit/queue frames are sent
back-to-back without waiting for either ACK. The Agent's actual completion
next_loop_id MUST identify the queued successor. No sleeps, holds, releases,
apply gates, small limits or fault plans establish natural execution order.
"""

import argparse
import json
import os
import signal
import sys
import time
from pathlib import Path

from canonical_observer import ObservationError, text_part, tool_part
from real_chain_load import Chain, Plan, percentile
from real_chain_scenarios import ack, new_loop
from real_chain_scenarios import phone, phone_submit, verify_install


class SuccessorPlan(Plan):
    def __init__(self, pairs):
        super().__init__(prefix='SUCCESSOR', tool_every=True)
        self.request_limit = pairs * 5 + 1

    def is_tool(self, index):
        return index > 0 and (index - 1) % 5 < 3


def run_batch(binary, pairs):
    provider = SuccessorPlan(pairs)
    latencies = []
    links = []
    with Chain(binary, None, provider, tools='"read"', remote=True) as chain:
        (chain.workspace / 'fixture.txt').write_text('fixture read content\n')
        peer = phone(chain)
        try:
            phone_submit(chain, peer, 'natural-successor-bootstrap')
            warmup = new_loop(chain)
            verify_install(chain, warmup, [text_part(provider.answer(0))])
            target = peer.target()
            for index in range(pairs):
                started = time.monotonic()
                operation_a = peer.intent('submit', text=f'natural-active-{index}')
                operation_b = peer.intent('queue', text=f'natural-follow-up-{index}')
                operation_count = peer.seq
                a = ack(chain, peer, operation_a, accepted=True)
                b = ack(chain, peer, operation_b, accepted=True)
                chain.trace.register_input(a, f'natural-active-{index}')
                chain.trace.register_input(b, f'natural-follow-up-{index}')
                if (a['session_id'], a['load']) != (target['session_id'], target['load']) or (
                        b['session_id'], b['load']) != (a['session_id'], a['load']):
                    raise ObservationError('back-to-back natural operations changed target')
                def installed(receipt):
                    return next((row for row in chain.trace.installs
                                 if row['input_id'] == receipt['input_id'] and row['session_id'] == receipt['session_id']
                                 and row['load'] == receipt['load']), None)
                chain.wait_for('actual queued successor installed without further operation', lambda: installed(b))
                row_a, row_b = installed(a), installed(b)
                if row_a is None:
                    chain.wait_for('predecessor canonical installation', lambda: installed(a))
                    row_a = installed(a)
                start = index * 5 + 1
                expected_a = [tool_part(f'call_tool_{start + request:04d}', request) for request in range(3)]
                expected_a.append(text_part(provider.answer(start + 3), 3))
                verify_install(chain, row_a['loop_id'], expected_a)
                verify_install(chain, row_b['loop_id'], [text_part(provider.answer(start + 4))])
                def completion_a():
                    return next((row for row in chain.agent_trace.completed() if row['loop_id'] == row_a['loop_id']), None)
                chain.wait_for('actual completion boundary next identity', completion_a)
                completed = completion_a()
                if completed.get('next_loop_id') != row_b['loop_id'] or completed.get('persistence') != 'persisted':
                    raise ObservationError('not an automatic successor: actual predecessor next does not name queued B')
                if row_b['completion_seq'] != row_a['completion_seq'] + 1 or peer.seq != operation_count:
                    raise ObservationError('successor order required another frontend operation')
                links.append({'predecessor': row_a['loop_id'], 'successor': row_b['loop_id'],
                              'input_id': row_b['input_id'], 'completion_seq': row_b['completion_seq']})
                latencies.append(time.monotonic() - started)
            chain.shutdown()
            report = chain.observation_report(pairs * 2 + 1)
            if report['agent_observation']['fault_events']:
                raise ObservationError('natural successor run unexpectedly activated a fault hook')
            return {'automatic_successors': pairs, 'logical_completions': pairs * 2 + 1,
                    'provider_requests': provider.request_limit, 'latency_seconds': latencies,
                    'actual_direct_successor_links': links, **report}
        finally:
            peer.close()


def successor_batches(binary, count, natural):
    base = Path(os.environ['MINICORE_E2E_EVIDENCE_DIR'])
    remaining, results = count, []
    while remaining:
        batch = min(10, remaining)
        directory = base / f'batch-{len(results):03d}'
        directory.mkdir()
        os.environ['MINICORE_E2E_EVIDENCE_DIR'] = str(directory)
        try:
            result = run_batch(binary, batch)
            (directory / 'result.json').write_text(json.dumps(result, sort_keys=True, indent=2) + '\n')
            results.append(result)
        finally:
            os.environ['MINICORE_E2E_EVIDENCE_DIR'] = str(base)
        remaining -= batch
    latency = [value for row in results for value in row['latency_seconds']]
    return {'class': 'automatic_successor', 'mode': 'natural_fixture' if natural else 'correctness_no_fault',
            'automatic_successors': count, 'completions': sum(row['logical_completions'] for row in results),
            'canonical_installs_verified': sum(row['canonical_installs_verified'] for row in results),
            'injected_faults': [], 'latency_samples': len(latency),
            'seconds_p50': percentile(latency, .5), 'seconds_p95': percentile(latency, .95),
            'seconds_p99': percentile(latency, .99), 'seconds_max': max(latency),
            'batches': len(results)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary')
    parser.add_argument('case', choices=('successor',))
    parser.add_argument('--count', type=int, default=2)
    parser.add_argument('--natural', action='store_true')
    args = parser.parse_args()
    if not 1 <= args.count <= 200 or (args.natural and args.count < 100):
        parser.error('bounded count 1..200; natural needs at least 100 actual successors')
    if os.environ.get('MINICORE_E2E_CAPTURE_AGENT') != '1':
        parser.error('actual Agent completion-boundary observation is required')
    print(json.dumps(successor_batches(args.binary, args.count, args.natural), sort_keys=True))


def alarm(_signum, _frame):
    raise TimeoutError('natural successor outer deadline')


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, alarm)
    signal.signal(signal.SIGALRM, alarm)
    signal.alarm(600)
    try:
        main()
    except Exception as error:
        print('real-chain natural successor: ' + type(error).__name__ + ': ' + str(error), file=sys.stderr)
        sys.exit(1)
    finally:
        signal.alarm(0)
