#!/usr/bin/env python3
"""Real-server 512-message overflow via paced legitimate provider projections."""

import argparse
import json
import signal
import sys
import tempfile
import threading
from pathlib import Path

from canonical_observer import ObservationError, text_part
from real_chain_load import Chain, Plan, completed, delta, sse
from real_chain_scenarios import phone, phone_submit, new_loop, verify_install, assert_phone_turn


class PacedPlan(Plan):
    def __init__(self):
        super().__init__()
        self.request_limit = 1
        self.gate = threading.Event()
        self.pieces = [f'projection-{index:04d}-' + 'x' * 48 + '\n' for index in range(600)]
        self.advance = [threading.Event() for _ in self.pieces]

    def close(self):
        self.gate.set()
        for event in self.advance:
            event.set()

    def respond(self, handler, index, request):
        if index != 0 or not self.gate.wait(20):
            raise AssertionError('paced provider setup did not complete')
        frames = [sse([delta(piece)]) for piece in self.pieces]
        terminal = sse([completed()])
        handler.send_response(200)
        handler.send_header('Content-Type', 'text/event-stream')
        handler.send_header('Content-Length', str(sum(map(len, frames)) + len(terminal)))
        handler.send_header('Connection', 'close')
        handler.end_headers()
        for frame, event in zip(frames, self.advance):
            handler.wfile.write(frame)
            handler.wfile.flush()
            handler.server.notify()
            if not event.wait(15):
                raise AssertionError('fast primary did not observe preceding legitimate projection')
        handler.wfile.write(terminal)
        handler.wfile.flush()
        return True


def pressure(binary):
    provider = PacedPlan()
    with tempfile.TemporaryDirectory(prefix='minicore-phone-drain-') as temporary:
        gate = Path(temporary)
        with Chain(binary, None, provider, remote=True,
                   extra_env={'MINICORE_E2E_PHONE_GATE': str(gate)}) as chain:
            fast = phone(chain, 'fast-primary')
            slow = replacement = None
            try:
                receipt = phone_submit(chain, fast, 'paced-real-phone-pressure')
                chain.wait_for('real request held for secondary setup', lambda: chain.provider.count() == 1)
                loop = new_loop(chain)
                (gate / 'arm').touch(exist_ok=False)
                slow = phone(chain, 'paused-secondary', receive_buffer=1024)
                chain.wait_for('actual real-server writer-drain gate ready', lambda: (gate / 'ready').exists())
                ready = json.loads((gate / 'ready').read_text())
                if ready['session_id'] != receipt['session_id'] or ready['load'] != receipt['load']:
                    raise ObservationError('writer gate paused another actual session/load')
                if ready['view_epoch'] != slow.view_epoch:
                    raise ObservationError('writer gate paused another phone projection')
                slow.paused = True
                start_messages = fast.messages
                provider.gate.set()
                seen_bytes = 0
                for index, piece in enumerate(provider.pieces):
                    seen_bytes += len(piece.encode())
                    def advanced(seen_bytes=seen_bytes):
                        return any(block and block.get('type') == 'assistant' and block.get('loop_id') == loop
                                   and len(block.get('text', '').encode()) >= seen_bytes for block in fast.blocks)
                    chain.wait_for('fast primary projected preceding provider delta', advanced, timeout=10)
                    provider.advance[index].set()
                actual_changes = fast.messages - start_messages
                if actual_changes <= 512:
                    raise ObservationError('fixture did not produce more than the REAL 512-message capacity')
                closed = lambda: next((row for row in chain.trace.phone_events
                                       if row['event'] == 'phone_overflow_closed'
                                       and row['connection_id'] == ready['connection_id']), None)
                chain.wait_for('actual queue Full closes selected real connection', closed, timeout=5)
                if closed()['view_epoch'] != ready['view_epoch'] or closed().get('reason') != 'overflow':
                    raise ObservationError('overflow close lost exact writer identity')
                body = ''.join(provider.pieces)
                verify_install(chain, loop, [text_part(body)])
                token = slow.token
                slow.close()
                replacement = phone(chain, token=token)
                assert_phone_turn(chain, replacement, loop, 'completed', body)
                chain.shutdown()
                return {'case': 'real_phone_writer_overflow_and_full_reconnect', 'mode': 'injected_correctness',
                        'production_queue_capacity': 512, 'fast_projection_messages': actual_changes,
                        'overflow_connection_id': ready['connection_id'], 'reconnect_snapshots': replacement.snapshots,
                        **chain.observation_report(1)}
            finally:
                (gate / 'release').touch(exist_ok=True)
                provider.close()
                fast.close()
                if slow:
                    slow.close()
                if replacement:
                    replacement.close()


def alarm(_signum, _frame):
    raise TimeoutError('real-phone pressure outer bound')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('binary')
    parser.add_argument('case', choices=('phone-overflow',))
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, alarm)
    signal.signal(signal.SIGALRM, alarm)
    signal.alarm(90)
    try:
        print(json.dumps(pressure(args.binary), sort_keys=True))
    except Exception as error:
        print('real-phone pressure: ' + type(error).__name__ + ': ' + str(error), file=sys.stderr)
        sys.exit(1)
    finally:
        signal.alarm(0)
