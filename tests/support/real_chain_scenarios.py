#!/usr/bin/env python3
"""Bounded real-main scenarios; no second Agent client or synthetic UI model.

`--natural` requires >=100 per class and forbids controlled provider/read gates.
Short scenario runs are correctness checks, NEVER frequency acceptance.
"""

import argparse
import fcntl
import json
import os
import re
import signal
import struct
import sys
import threading
import tempfile
import time
from pathlib import Path

from canonical_observer import ObservationError, parts, text_part, tool_part
from phone_client import Phone
from real_chain_load import Chain, Plan, STAGE_TIMEOUT, completed, delta, percentile, process_cpu_ticks, sse

# Runtime's legal per-channel cap is 256 KiB. With JSON escaping/envelope,
# this single canonical item exceeds the default 256-KiB page and must chunk.
PAGINATED_BYTES = 256 * 1024


class ScenarioPlan(Plan):
    def __init__(self, case, count):
        super().__init__()
        self.case, self.request_limit = case, count
        self.holds = {}
        self.sent = set()
        self.partial = set()
        self.events = {}
        self.allow_cancel_close = case == "cancel"
        self.labels = {}
        self.lock = threading.Lock()

    def hold(self, index):
        gate = threading.Event()
        self.holds[index] = gate
        return gate

    def close(self):
        for gate in self.holds.values():
            gate.set()
        for gate in self.events.values():
            gate.set()

    def text(self, index):
        marker = f"SCENARIO{self.case.upper().replace('-', '')}{index:04d}END"
        if self.case in ("pagination", "slow-terminal", "slow-phone", "paced-terminal", "paced-phone"):
            line = "canonical exact bytes including  double spaces and unicode 雪\n"
            body_bytes = 16 * 1024 if self.case.startswith('paced-') else PAGINATED_BYTES
            available = body_bytes - len(marker.encode())
            repetitions, remainder = divmod(available, len(line.encode()))
            return line * repetitions + "x" * remainder + marker
        return marker

    def respond(self, handler, index, request):
        gate = self.holds.get(index)
        if gate is not None and not gate.wait(STAGE_TIMEOUT):
            raise AssertionError("scenario provider gate exceeded bound")
        if self.case == "error":
            body = b'{"error":{"message":"isolated scenario failure","type":"invalid_request_error"}}'
            handler.send_response(400)
            handler.send_header("Content-Type", "application/json")
        else:
            text = self.text(index)
            if self.case == "cancel":
                body = sse([delta(text), completed()])
            else:
                body = sse([delta(text[start:start + 4096])
                            for start in range(0, len(text), 4096)] + [completed()])
            handler.send_response(200)
            handler.send_header("Content-Type", "text/event-stream")
        handler.send_header("Content-Length", str(len(body)))
        handler.send_header("Connection", "close")
        handler.end_headers()
        if self.case == "cancel":
            partial = sse([delta(self.text(index))])
            handler.wfile.write(partial)
            handler.wfile.flush()
            self.partial.add(index)
            handler.server.notify()
            release = self.events.setdefault(index, threading.Event())
            if not release.wait(STAGE_TIMEOUT):
                raise AssertionError("cancelled HTTP stream release exceeded bound")
            return True
        handler.wfile.write(body)
        handler.wfile.flush()
        self.sent.add(index)
        handler.server.notify()
        return True


def latest_control(chain, session=None):
    controls = chain.trace.controls
    return next((row for row in reversed(controls)
                 if session is None or row["session_id"] == session), None)


def verify_install(chain, loop, expected, persistence="persisted", uncertain=False):
    def installed():
        row = next((row for row in chain.trace.installs if row["loop_id"] == loop), None)
        if row is None:
            return None
        chain.trace.register_canonical(row, expected, persistence, uncertain)
        if parts(row["parts"]) != parts(expected):
            raise ObservationError("actual installed non-preserved part/order mismatch for " + loop
                                   + "; expected=" + json.dumps(expected, sort_keys=True)
                                   + "; actual=" + json.dumps(row["parts"], sort_keys=True))
        if (row["persistence"], row["persistence_uncertain"]) != (persistence, uncertain):
            raise ObservationError("actual installed Saving mismatch for " + loop)
        return row
    chain.wait_for("verified actual install " + loop, installed, timeout=45)
    return installed()


def new_loop(chain, session=None, excluded=()):
    def target():
        row = latest_control(chain, session)
        if row and row["execution"].get("loop_id") not in (None, *excluded):
            return row["execution"]["loop_id"]
        # A short final may already be installed when the next observation runs.
        return next((row["loop_id"] for row in chain.trace.installs
                     if row["loop_id"] not in excluded
                     and (session is None or row["session_id"] == session)), None)
    chain.wait_for("new exact execution identity", target)
    return target()


def phone(chain, name="scenario-phone", token=None, receive_buffer=None, read_rate=None):
    def port():
        match = re.search(r"http://127\.0\.0\.1:(\d+)", chain.screen.text())
        return int(match[1]) if match else None
    chain.wait_for("real loopback remote address", port)
    client = Phone(port(), name=name, token=token, receive_buffer=receive_buffer, read_rate=read_rate)
    chain.pumps.append(client.poll)
    chain.phone_readers.append(client)
    chain.wait_for("real phone full snapshot", lambda: client.snapshots > 0)
    return client


def ack(chain, client, operation, accepted=False):
    chain.wait_for("correlated phone ACK " + operation, lambda: operation in client.acks)
    row = client.acks.pop(operation)
    if row.get("ok") is not True:
        raise AssertionError("real phone operation refused: " + row.get("error", "unknown"))
    if accepted and (row.get("accepted") is not True or not row.get("input_id")):
        raise AssertionError("queue operation lacks actual accepted input identity")
    return row


def phone_submit(chain, client, text, queue=False, input_kind='prompt', target_loop=None, request_index=None):
    row = ack(chain, client, client.intent("queue" if queue else "submit", text=text), accepted=True)
    chain.trace.register_input(row, text, input_kind, target_loop, request_index)
    chain.wait_for("phone session/load adoption", lambda: client.state.get("session") is not None)
    if row["session_id"] != client.state["session"]["id"] or row["load"] != client.state["session"]["load"]:
        raise AssertionError("accepted input targets another phone projection")
    return row


def assert_phone_turn(chain, client, loop, outcome, expected_text=None):
    def terminal():
        return next((block for block in client.blocks if block and block.get("type") == "turn_end"
                     and block["loop_id"] == loop and block.get("saving") is not None), None)
    chain.wait_for("real phone terminal projection with Saving", terminal)
    row = terminal()
    if row["outcome"] != outcome or row["saving"] != "saved":
        raise AssertionError("phone terminal outcome/Saving mismatch")
    if expected_text is not None:
        found = [block for block in client.blocks if block and block.get("type") == "assistant"
                 and block.get("loop_id") == loop and block.get("text") == expected_text]
        if len(found) != 1:
            raise AssertionError("real phone projection lost/duplicated full answer")


def automatic(binary, count, application_gate=None):
    # One first turn and preaccepted FollowUps. NO submit/resume after release.
    total = count + 1
    plan = ScenarioPlan("automatic", total)
    first_gate = plan.hold(0)
    env = {"MINICORE_E2E_APPLY_GATE": str(application_gate)} if application_gate else None
    with Chain(binary, None, plan, remote=True, extra_env=env) as chain:
        client = phone(chain)
        try:
            first = phone_submit(chain, client, "automatic-first")
            chain.wait_for("held first real provider request", lambda: chain.provider.count() == 1)
            first_loop = new_loop(chain)
            queued = []
            # At most 32 queued inputs; this correctness scenario deliberately
            # tests <=16. Natural long batches must replenish BEFORE next release.
            for index in range(count):
                queued.append(phone_submit(chain, client, f"automatic-queued-{index}", queue=True))
            ids = [row["input_id"] for row in queued]
            chain.wait_for("all accepted queue IDs installed while first held", lambda:
                           latest_control(chain)["queue_ids"] == ids)
            release_seq = chain.trace.event_seq
            operations_at_release = client.seq
            if application_gate:
                (application_gate / 'arm').touch(exist_ok=False)
            first_gate.set()
            if application_gate:
                try:
                    chain.wait_for("main application gate actual readiness", lambda:
                                   (application_gate / 'ready').exists())
                    ready = json.loads((application_gate / 'ready').read_text())
                    if ready['session_id'] != first['session_id'] or ready['load'] != first['load']:
                        raise ObservationError('main gate paused wrong session/load')
                    if not hasattr(chain, 'agent_trace'):
                        raise ObservationError('main pause requires actual Agent completion observations')
                    chain.wait_for("all real Agent completions while application paused", lambda:
                                   len(chain.agent_trace.completed()) == total, timeout=20)
                finally:
                    (application_gate / 'release').touch(exist_ok=True)
            expected_loops = [first_loop]
            for index in range(total):
                # Two recovery tasks may finish out of order. Completion seq
                # and accepted input ID bind the oracle; install arrival cannot.
                def found(index=index):
                    return next((row for row in chain.trace.installs
                                 if row["completion_seq"] == index + 1
                                 and row["session_id"] == first["session_id"]
                                 and row["load"] == first["load"]), None)
                chain.wait_for("automatic completion discovery", found)
                row = found()
                verify_install(chain, row["loop_id"], [text_part(plan.text(index))])
                expected_input = first["input_id"] if index == 0 else ids[index - 1]
                if row["input_id"] != expected_input:
                    raise AssertionError("automatic installed turn has wrong accepted input ID")
                if index:
                    expected_loops.append(row["loop_id"])
            if client.seq != operations_at_release:
                raise AssertionError("automatic successor run issued a post-release operation")
            chain.wait_for("automatic queue drained in actual control", lambda:
                           latest_control(chain)["queue_ids"] == [] and
                           latest_control(chain)["execution"]["kind"] == "none")
            if chain.provider.count() != total or len(set(expected_loops)) != total:
                raise AssertionError("automatic successor count/identity mismatch")
            # Provider input confirms accepted FollowUps were actually executed
            # FIFO, not just assigned fresh identities by an unrelated loop.
            for index in range(1, total):
                request = chain.provider.request(index)
                if f"automatic-queued-{index - 1}" not in json.dumps(request):
                    raise AssertionError("automatic provider request lost queued input")
            chain.shutdown()
            return {"class": "automatic_accepted_queue", "mode": "controlled_provider_hold",
                    "completions": total, "automatic_successors": count,
                    "queued_input_ids": ids, "first_input_id": first["input_id"],
                    "release_event_seq": release_seq, "post_release_operations": 0,
                    "main_application_paused": application_gate is not None,
                    **chain.observation_report(total)}
        finally:
            client.close()


def steering(binary):
    plan = Plan(tool_every=True, tool_steps=1)
    plan.request_limit = 2
    with Chain(binary, None, plan, tools='"read"', remote=True) as chain:
        (chain.workspace / 'fixture.txt').write_text('fixture read content\n')
        chain.provider.gate.clear()
        client = phone(chain)
        try:
            initial = phone_submit(chain, client, 'actual-steering-prompt')
            chain.wait_for('first model request held', lambda: chain.provider.count() == 1)
            loop = new_loop(chain)
            steer = phone_submit(chain, client, 'actual steering  雪  bytes', input_kind='steering', target_loop=loop, request_index=1)
            chain.wait_for('actual accepted steer queued before request boundary', lambda:
                           latest_control(chain)['queue_ids'] == [steer['input_id']])
            chain.provider.gate.set()
            installed = verify_install(chain, loop, [tool_part('call_tool_0000'), text_part(plan.answer(1), 1)])
            delivery_key = (initial['session_id'], initial['load'], steer['input_id'])
            if chain.trace.delivered_facts.get(delivery_key) != dict(loop_id=loop, request_index=1):
                raise ObservationError('the actual delivered steering identity/boundary was not observed')
            if chain.trace.delivered_sequences[delivery_key] >= installed['event_seq']:
                raise ObservationError('steering fixture lacks actual pre-canonical delivery checkpoint')
            if 'actual steering  雪  bytes' not in json.dumps(chain.provider.request(1), ensure_ascii=False):
                raise ObservationError('Runtime did not deliver the accepted steer at the next model boundary')
            chain.wait_for('actual original steering completion recorded', lambda: len(chain.agent_trace.completed()) >= 1)
            if initial['input_id'] == steer['input_id'] or len(chain.agent_trace.completed()) != 1:
                raise ObservationError('steering was retyped as a separate automatic/manual turn')
            chain.shutdown()
            report = chain.observation_report(1)
            users = [entry for view in chain.trace.views.values() for entry in view['inventory'] if entry['kind'] == 'user']
            if len(users) != 2 or users[0]['input_id'] != initial['input_id'] or users[1]['loop_id'] != loop or users[1]['user_kind'] != 'steering' or users[1]['input_id'] != steer['input_id'] or users[1].get('request_index') != 1:
                raise ObservationError('actual installed prompt/steering identity order changed')
            return {'case': 'real_runtime_steer_user_identity', 'mode': 'controlled_provider_hold',
                    'canonical_steering_input_identity_available': users[1]['input_id'] is not None,
                    'actual_delivery_checkpoint_before_install': True, **report}
        finally:
            client.close()


def idle(binary):
    plan = ScenarioPlan('idle', 1)
    with Chain(binary, None, plan) as chain:
        chain.submit('one completed turn before the idle measurement')
        loop = new_loop(chain)
        verify_install(chain, loop, [text_part(plan.text(0))])
        chain.wait_for('authoritative idle execution', lambda:
                       latest_control(chain)['execution']['kind'] == 'none'
                       and latest_control(chain)['queue_ids'] == [])
        queries = lambda: sum(row['event'] == 'result_query' for row in chain.agent_trace.records())
        before = queries()
        work_calls = lambda: sum(row['event'] == 'work' and row.get('kind') == 'query_encode'
                                 for row in chain.agent_trace.records())
        before_work = work_calls()
        cpu_before = {name: process_cpu_ticks(pid) for name, pid in (('tui', chain.process.pid), ('agent', chain.agent_pid))}
        started = time.monotonic()
        # This is a measured idle interval, not a timing gate establishing
        # execution order; readiness above comes from the installed result.
        chain.wait_for('bounded idle observation window', lambda: time.monotonic() - started >= 1.25, timeout=3)
        elapsed = time.monotonic() - started
        after = queries()
        after_work = work_calls()
        cpu_after = {name: process_cpu_ticks(pid) for name, pid in (('tui', chain.process.pid), ('agent', chain.agent_pid))}
        if Path('/proc/self/stat').exists() and any(value is None for value in (*cpu_before.values(), *cpu_after.values())):
            raise ObservationError('actual live process CPU measurement unexpectedly unavailable')
        cpu_delta = {name: cpu_after[name] - cpu_before[name]
                     if cpu_before[name] is not None and cpu_after[name] is not None else None for name in cpu_before}
        if any(value is not None and value < 0 for value in cpu_delta.values()):
            raise ObservationError('actual process CPU counter regressed')
        if after != before:
            raise ObservationError('completed idle main issued additional result polling queries')
        chain.shutdown()
        return {'case': 'completed_idle_without_result_polling', 'mode': 'measured_idle_window',
                'observed_idle_seconds': elapsed, 'additional_result_queries': after - before,
                'additional_encoded_query_responses': after_work - before_work,
                'process_cpu_delta_ticks': cpu_delta,
                'cpu_clock_ticks_per_second': os.sysconf('SC_CLK_TCK'),
                **chain.observation_report(1)}


def repeated(binary, case, count):
    plan = ScenarioPlan(case, count)
    latencies = []
    with Chain(binary, None, plan, remote=True) as chain:
        client = phone(chain)
        seen = []
        try:
            for index in range(count):
                start = time.monotonic()
                receipt = phone_submit(chain, client, f"repeated-{case}-{index}")
                chain.wait_for("provider request", lambda index=index: chain.provider.count() == index + 1)
                loop = new_loop(chain, excluded=seen)
                seen.append(loop)
                if case == "cancel":
                    chain.wait_for("real partial projected to phone", lambda loop=loop:
                                   any(b and b.get("type") == "assistant" and b.get("loop_id") == loop
                                       and b.get("text") == plan.text(index) for b in client.blocks))
                    ack(chain, client, client.intent("cancel", loop_id=loop))
                    plan.events.setdefault(index, threading.Event()).set()
                # A cancellation's canonical interrupted display copy is
                # installed too; compare it rather than assuming an empty body.
                expected = [text_part(plan.text(index))] if case == "cancel" else []
                row = verify_install(chain, loop, expected)
                if row["input_id"] != receipt["input_id"]:
                    raise AssertionError("terminal install lost input identity")
                assert_phone_turn(chain, client, loop, "failed" if case == "error" else "cancelled")
                latencies.append(time.monotonic() - start)
            chain.shutdown()
            return {"class": "provider_error" if case == "error" else "cancel",
                    "mode": "correctness", "completions": count,
                    "provider_requests": chain.provider.count(),
                    "seconds_p50": percentile(latencies, .5), "seconds_p95": percentile(latencies, .95),
                    "seconds_p99": percentile(latencies, .99), "seconds_max": max(latencies),
                    "latency_seconds": latencies,
                    **chain.observation_report(count)}
        finally:
            client.close()


def pagination(binary, count):
    # Bound request-history and installed-view memory in a small independent
    # process batch; count>=100 can be orchestrated as explicit bounded batches.
    plan = ScenarioPlan("pagination", count)
    latencies = []
    with Chain(binary, None, plan) as chain:
        loops = []
        for index in range(count):
            submitted = time.monotonic()
            chain.submit(f"pagination-{index}")
            loop = new_loop(chain, excluded=loops)
            loops.append(loop)
            verify_install(chain, loop, [text_part(plan.text(index))])
            latencies.append(time.monotonic() - submitted)
        chain.shutdown()
        observed = chain.observation_report(count).get("agent_observation")
        if observed:
            rows = chain.agent_trace.records()
            for loop in loops:
                pages = [row for row in rows if row['event'] == 'result_query' and row['loop_id'] == loop
                         and row.get('error') is None and row.get('availability') in ('live', 'stored')]
                if len(pages) < 2 or pages[0]['cursor'] is not None or pages[-1]['next_cursor'] is not None or len({row['result_id'] for row in pages}) != 1:
                    raise ObservationError('legal maximum answer lacks multiple actual available pages with one identity')
                if any(left['next_cursor'] is None or left['next_cursor'] != right['cursor'] for left, right in zip(pages, pages[1:])):
                    raise ObservationError('actual pagination result pages did not form the verified advancing cursor chain')
        return {"class": "pagination_scale", "mode": "correctness", "completions": count,
                "answer_bytes_min": PAGINATED_BYTES, "default_result_page_bytes": 256 * 1024,
                "actual_page_cost_measured": "agent_observation" in chain.observation_result,
                "latency_seconds": latencies,
                **chain.observation_report(count)}


def resident(binary, count, overlap=True):
    plan = ScenarioPlan("resident", count * 2)
    gate_a = plan.hold(0) if overlap else None
    gate_b = plan.hold(1) if overlap else None
    latencies = []
    with Chain(binary, None, plan, remote=True) as chain:
        a, b = phone(chain, "resident-A"), phone(chain, "resident-B")
        try:
            started_a = time.monotonic()
            rec_a = phone_submit(chain, a, "resident-A-0")
            chain.wait_for("A request held", lambda: chain.provider.count() == 1)
            started_b = time.monotonic()
            rec_b = phone_submit(chain, b, "resident-B-0")
            chain.wait_for("B request held", lambda: chain.provider.count() == 2)
            if rec_a["session_id"] == rec_b["session_id"]:
                raise AssertionError("independent draft phones did not create two resident sessions")
            loop_a = new_loop(chain, rec_a["session_id"])
            loop_b = new_loop(chain, rec_b["session_id"])
            if loop_a == loop_b:
                raise AssertionError("resident execution identities aliased")
            if gate_b:
                gate_b.set()
            verify_install(chain, loop_b, [text_part(plan.text(1))])
            latencies.append(time.monotonic() - started_b)
            if gate_a:
                gate_a.set()
            verify_install(chain, loop_a, [text_part(plan.text(0))])
            latencies.append(time.monotonic() - started_a)
            assert_phone_turn(chain, a, loop_a, "completed", plan.text(0))
            assert_phone_turn(chain, b, loop_b, "completed", plan.text(1))
            for index in range(1, count):
                for client, session in ((a, rec_a["session_id"]), (b, rec_b["session_id"])):
                    provider_index = chain.provider.count()
                    old = [row["loop_id"] for row in chain.trace.installs]
                    started = time.monotonic()
                    phone_submit(chain, client, f"resident-{index}")
                    loop = new_loop(chain, session, old)
                    verify_install(chain, loop, [text_part(plan.text(provider_index))])
                    assert_phone_turn(chain, client, loop, "completed", plan.text(provider_index))
                    latencies.append(time.monotonic() - started)
            chain.shutdown()
            return {"class": "multi_resident", "mode": "controlled_overlap" if overlap else "natural_fixture", "sessions": 2,
                    "completions": count * 2, "latency_seconds": latencies,
                    **chain.observation_report(count * 2)}
        finally:
            a.close()
            b.close()


def slow_terminal(binary):
    plan = ScenarioPlan("slow-terminal", 1)
    gate = plan.hold(0)
    with Chain(binary, None, plan) as chain:
        chain.submit("slow-terminal")
        chain.wait_for("real provider held", lambda: chain.provider.count() == 1)
        loop = new_loop(chain)
        chain.terminal_paused = True
        gate.set()
        chain.wait_for("provider completed while terminal consumption paused", lambda: 0 in plan.sent)
        # Observe actual queued terminal bytes, not a guessed sleep interval.
        queued = lambda: struct.unpack("I", fcntl.ioctl(chain.master, termios_queue(), struct.pack("I", 0)))[0]
        chain.wait_for("PTY has unread output", lambda: queued() > 0)
        unread = queued()
        chain.terminal_paused = False
        verify_install(chain, loop, [text_part(plan.text(0))])
        chain.shutdown()
        return {"class": "slow_terminal", "mode": "controlled_read_pause", "completions": 1,
                "unread_pty_bytes_at_release": unread, **chain.observation_report(1)}


def termios_queue():
    import termios
    return termios.FIONREAD


def slow_phone(binary):
    plan = ScenarioPlan("slow-phone", 2)
    with Chain(binary, None, plan, remote=True) as chain:
        client = phone(chain, receive_buffer=1024)
        replacement = None
        try:
            phone_submit(chain, client, "slow-phone-0")
            loop = new_loop(chain)
            client.paused = True
            verify_install(chain, loop, [text_part(plan.text(0))])
            token = client.token
            if not token:
                raise AssertionError("real phone did not acquire reconnect credential")
            client.close()
            replacement = phone(chain, token=token)
            chain.wait_for("new full canonical projection after nonreading peer", lambda:
                           any(b and b.get("type") == "turn_end" and b.get("loop_id") == loop
                               for b in replacement.blocks))
            assert_phone_turn(chain, replacement, loop, "completed", plan.text(0))
            phone_submit(chain, replacement, "slow-phone-1")
            second = new_loop(chain, excluded=[loop])
            verify_install(chain, second, [text_part(plan.text(1))])
            assert_phone_turn(chain, replacement, second, "completed", plan.text(1))
            chain.shutdown()
            return {"class": "slow_phone_reconnect", "mode": "controlled_nonreading_peer",
                    "completions": 2, "reconnect_full_snapshots": replacement.snapshots,
                    "queue_overflow_proven": False, **chain.observation_report(2)}
        finally:
            client.close()
            if replacement:
                replacement.close()


def paced_reader(binary, case, count):
    """Ordinary unheld provider; only the real consuming device has a read rate."""
    plan = ScenarioPlan('paced-terminal' if case == 'slow-terminal' else 'paced-phone', count)
    rate = 64 * 1024
    latencies = []
    with Chain(binary, None, plan, remote=case == 'slow-phone',
               terminal_read_rate=rate if case == 'slow-terminal' else None) as chain:
        client = phone(chain, read_rate=rate, receive_buffer=4096) if case == 'slow-phone' else None
        try:
            seen = []
            for index in range(count):
                started = time.monotonic()
                if client:
                    receipt = phone_submit(chain, client, f'natural-paced-{index}')
                else:
                    chain.submit(f'natural-paced-{index}')
                loop = new_loop(chain, excluded=seen)
                seen.append(loop)
                row = verify_install(chain, loop, [text_part(plan.text(index))])
                if client:
                    if row['input_id'] != receipt['input_id']:
                        raise ObservationError('paced phone input identity changed')
                    assert_phone_turn(chain, client, loop, 'completed', plan.text(index))
                latencies.append(time.monotonic() - started)
            chain.shutdown()
            report = chain.observation_report(count)
            if report['agent_observation']['fault_events']:
                raise ObservationError('paced natural reader activated an Agent fault')
            if client:
                readback = {'rate_bytes_per_second': rate, 'read_calls': client.read_calls,
                            'throttled_polls': client.throttled_polls, 'received_bytes': client.bytes}
            else:
                readback = report['terminal_reader']
            if not readback['throttled_polls'] or not readback['read_calls']:
                raise ObservationError('paced class did not actually exercise device read throttling')
            return {'class': case, 'mode': 'natural_fixture', 'completions': count,
                    'device_reader': readback, 'latency_seconds': latencies, **report}
        finally:
            if client:
                client.close()


def natural_batches(binary, case, count):
    evidence = os.environ.get("MINICORE_E2E_EVIDENCE_DIR")
    if not evidence:
        raise AssertionError("natural fixture source/trace evidence directory required")
    base = Path(evidence)
    results = []
    remaining = count
    while remaining:
        # Keep actual result/source ownership bounded and do not make a 25-MiB
        # committed model history exceed its independent context/input limits.
        batch = min(2 if case in ("pagination", "resident") else 16, remaining)
        if case == "resident" and batch % 2:
            raise AssertionError("resident natural class needs an even completion count")
        directory = base / f"batch-{len(results):03d}"
        directory.mkdir()
        previous = os.environ.get("MINICORE_E2E_EVIDENCE_DIR")
        os.environ["MINICORE_E2E_EVIDENCE_DIR"] = str(directory)
        try:
            if case == "pagination":
                result = pagination(binary, batch)
            elif case == "resident":
                result = resident(binary, batch // 2, overlap=False)
            elif case in ('slow-terminal', 'slow-phone'):
                result = paced_reader(binary, case, batch)
            else:
                result = repeated(binary, case, batch)
            (directory / "result.json").write_text(json.dumps(result, sort_keys=True, indent=2) + "\n")
            results.append(result)
        finally:
            os.environ["MINICORE_E2E_EVIDENCE_DIR"] = previous
        remaining -= batch
    latencies = [value for result in results for value in result.get('latency_seconds', [])]
    return {"mode": "natural_fixture", "class": case, "completions": count,
            "seconds_p50": percentile(latencies, .5), "seconds_p95": percentile(latencies, .95),
            "seconds_p99": percentile(latencies, .99), "seconds_max": max(latencies) if latencies else None,
            "latency_samples": len(latencies),
            "batches": len(results), "canonical_installs_verified": sum(r["canonical_installs_verified"] for r in results),
            "injected_faults": [], "batch_results": [str(base / f"batch-{i:03d}" / "result.json") for i in range(len(results))]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    parser.add_argument("case", choices=("automatic", "main-pause", "steering", "idle", "error", "cancel", "pagination", "resident", "slow-terminal", "slow-phone"))
    parser.add_argument("--count", type=int, default=2)
    parser.add_argument("--natural", action="store_true", help=">=100 new-class fixture completions, no fault hooks")
    args = parser.parse_args()
    if args.natural:
        if args.case not in ("error", "cancel", "pagination", "resident", "slow-terminal", "slow-phone") or not 100 <= args.count <= 200:
            parser.error("natural fixture classes: error/cancel/pagination/resident/slow-terminal/slow-phone with count 100..200")
        if os.environ.get("MINICORE_E2E_CAPTURE_AGENT") != "1":
            parser.error("natural runs require recorded current Agent/core observation hooks")
        report = natural_batches(args.binary, args.case, args.count)
        print(json.dumps(report, sort_keys=True))
        return
    if not 1 <= args.count <= 16:
        parser.error("correctness batches must be bounded to 1..16, not natural statistics")
    if args.case == 'idle':
        result = idle(args.binary)
    elif args.case == "steering":
        result = steering(args.binary)
    elif args.case == "automatic":
        result = automatic(args.binary, args.count)
    elif args.case == "main-pause":
        with tempfile.TemporaryDirectory(prefix='minicore-main-pause-') as root:
            result = automatic(args.binary, max(args.count, 10), Path(root))
    elif args.case in ("error", "cancel"):
        result = repeated(args.binary, args.case, args.count)
    elif args.case == "pagination":
        result = pagination(args.binary, args.count)
    elif args.case == "resident":
        result = resident(args.binary, args.count)
    elif args.case == "slow-terminal":
        result = slow_terminal(args.binary)
    else:
        result = slow_phone(args.binary)
    print(json.dumps(result, sort_keys=True))


def alarm_handler(_signum, _frame):
    raise TimeoutError("real-chain scenario outer deadline")


if __name__ == "__main__":
    signal.signal(signal.SIGALRM, alarm_handler)
    signal.signal(signal.SIGTERM, alarm_handler)
    signal.alarm(360)
    try:
        main()
    except Exception as error:
        print(f"real-chain scenario: {type(error).__name__}: {error}", file=sys.stderr)
        sys.exit(1)
    finally:
        signal.alarm(0)
