#!/usr/bin/env python3
"""Real-main PTY driver requiring installed-canonical observer v1 for acceptance.

Generated hashes and terminal markers are NOT canonical observations.
`--screen-only` is diagnostic, never canonical acceptance. `integrity` holds
only a provider response; tail loss, eviction, save and main apply faults need
separate owner-provided legal gates. No stdio byte reordering is permitted.
"""

import argparse
import codecs
import errno
import fcntl
import hashlib
import json
import os
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import unicodedata
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from canonical_observer import ObservationError, Trace, text_part, tool_part
from agent_observer import AgentTrace
from agent_observer_checks import bind_completion_facts
from process_support import assert_group_exited, cleanup_group, register_group, run_bounded

KEY = "dummy-local-sentinel"
WIDTH, HEIGHT = 140, 48
STAGE_TIMEOUT = 60
TURN_TIMEOUT = 30


def sse(events):
    return b"".join(
        b"data: " + json.dumps(event, separators=(",", ":")).encode() + b"\n\n"
        for event in events
    )


def completed():
    return {"type": "response.completed", "response": {"status": "completed"}}


def delta(text):
    return {"type": "response.output_text.delta", "delta": text}


def text_response(text):
    return sse([delta(text), completed()])


def tool_response(name, arguments, call_id):
    return sse([
        {
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": json.dumps(arguments),
            },
        },
        completed(),
    ])


class Provider(ThreadingHTTPServer):
    daemon_threads = False
    allow_reuse_address = False

    def __init__(self, plan):
        super().__init__(("127.0.0.1", 0), Handler)
        self.plan = plan
        self.requests = []
        self.errors = []
        self.lock = threading.Lock()
        self.notify_read, self.notify_write = os.pipe()
        self.serving = threading.Event()
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.gate = threading.Event()
        self.gate.set()

    def serve(self):
        self.serving.set()
        self.serve_forever(poll_interval=0.1)

    def notify(self):
        try:
            os.write(self.notify_write, b"x")
        except OSError:
            pass

    def record_error(self, error):
        with self.lock:
            if self.errors:
                return
            self.errors.append(str(error))
        self.notify()

    def check(self):
        with self.lock:
            if self.errors:
                raise AssertionError("provider: " + self.errors[0])

    def count(self):
        with self.lock:
            return len(self.requests)

    def request(self, index):
        with self.lock:
            return self.requests[index]

    def close(self):
        self.gate.set()
        if self.thread.is_alive():
            self.shutdown()
            self.thread.join(timeout=2)
        self.server_close()
        for fd in (self.notify_read, self.notify_write):
            try:
                os.close(fd)
            except OSError:
                pass


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def body_for(self, index):
        plan = self.server.plan
        if plan.is_tool(index):
            if plan.tool_name == "write":
                arguments = {"path": plan.tool_path, "content": "fixture"}
            else:
                arguments = {"path": plan.tool_path, "limit": 32}
            return tool_response(
                plan.tool_name, arguments, f"call_tool_{index:04d}"
            )
        answer = plan.answer(index)
        if plan.width:
            return sse([delta(answer[start:start + 128]) for start in range(0, len(answer), 128)] + [completed()])
        return text_response(answer)

    def do_POST(self):
        try:
            self.connection.settimeout(5)
            if self.path != "/v1/responses":
                raise AssertionError("unexpected request path")
            if self.headers.get("Authorization") != "Bearer " + KEY:
                raise AssertionError("missing dummy authorization sentinel")
            length = int(self.headers.get("Content-Length", "-1"))
            if not 0 <= length <= 4 * 1024 * 1024:
                raise AssertionError("request length outside fixture bound")
            request = json.loads(self.rfile.read(length))
            if request.get("model") != "smokemodel" or request.get("stream") is not True:
                raise AssertionError("unexpected model or non-streaming request")
            with self.server.lock:
                index = len(self.server.requests)
                if index >= self.server.plan.request_limit:
                    raise AssertionError("unexpected extra provider request")
                self.server.requests.append(request)
            plan = self.server.plan
            if index and plan.is_tool(index - 1):
                expected_call = f"call_tool_{index - 1:04d}"
                outputs = [item for item in request.get("input", [])
                           if item.get("type") == "function_call_output" and item.get("call_id") == expected_call]
                if len(outputs) != 1:
                    raise AssertionError("tool continuation lost its actual call identity")
                output = outputs[0].get("output", "")
                valid = output == "tool denied" if plan.tool_name == "write" else "fixture read content" in output
                if not valid:
                    raise AssertionError("real tool result did not reach provider continuation")
            self.server.notify()
            if not self.server.gate.wait(STAGE_TIMEOUT):
                raise AssertionError("response gate timed out")
            if hasattr(plan, "respond") and plan.respond(self, index, request):
                return
            body = self.body_for(index)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            if not getattr(self.server.plan, "allow_cancel_close", False):
                self.server.record_error("provider connection closed before response")
        except Exception as error:
            self.server.record_error(error)


class Plan:
    """Deterministic provider behaviour per absolute request index.

    Tool loops make actual consecutive tool requests before the final text.
    This is distinct from Agent automatic queue-successor scheduling.
    """

    def __init__(
        self,
        prefix="ANS",
        width=0,
        tool_every=0,
        tool_path="fixture.txt",
        tool_name="read",
        tool_steps=3,
    ):
        self.prefix = prefix
        self.width = width
        self.tool_every = tool_every
        self.tool_steps = tool_steps if tool_every else 0
        self.tool_path = tool_path
        self.tool_name = tool_name
        self.lock = threading.Lock()
        self.answers = {}
        self.request_limit = 16

    def is_tool(self, index):
        return bool(self.tool_every) and index % (self.tool_steps + 1) < self.tool_steps

    def text_index(self, turn):
        """Absolute provider index that answers ``turn`` with text."""
        return (self.tool_steps + 1) * turn + self.tool_steps

    def answer(self, index):
        with self.lock:
            return self.answers.get(index, f"{self.prefix}OK{index:04d}")

    def register(self, text, index=None):
        """Reserve ``text`` for an absolute provider index (default: next free)."""
        with self.lock:
            if index is None:
                index = max(self.answers) + 1 if self.answers else 0
            self.answers[index] = text
            return text


class Screen:
    def __init__(self, width=WIDTH, height=HEIGHT):
        self.width, self.height = width, height
        self.cells = [[" "] * width for _ in range(height)]
        self.row = self.col = 0
        self.state = "text"
        self.control = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.raw_bytes = 0
        self.saw_restore = False
        self.raw_tail = b""

    def feed(self, data):
        self.raw_bytes += len(data)
        if self.raw_bytes > 16 * 1024 * 1024:
            raise AssertionError("terminal output exceeded fixture bound")
        raw = self.raw_tail + data
        if b"\x1b[?1049l" in raw:
            self.saw_restore = True
        self.raw_tail = raw[-64:]
        for char in self.decoder.decode(data):
            if self.state == "text":
                if char == "\x1b":
                    self.state = "esc"
                elif char == "\r":
                    self.col = 0
                elif char == "\n":
                    self.row = min(self.row + 1, self.height - 1)
                elif char >= " " and char != "\x7f":
                    width = 0 if unicodedata.combining(char) else (
                        2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                    )
                    if width:
                        if self.col >= self.width:
                            self.col = 0
                            self.row = min(self.row + 1, self.height - 1)
                        self.cells[self.row][self.col] = char
                        if width == 2 and self.col + 1 < self.width:
                            self.cells[self.row][self.col + 1] = " "
                        self.col += width
            elif self.state == "esc":
                if char == "[":
                    self.state, self.control = "csi", ""
                elif char == "]":
                    self.state = "osc"
                else:
                    self.state = "text"
            elif self.state == "osc":
                if char == "\x07":
                    self.state = "text"
                elif char == "\x1b":
                    self.state = "osc_esc"
            elif self.state == "osc_esc":
                self.state = "text" if char == "\\" else "osc"
            elif self.state == "csi":
                if "@" <= char <= "~":
                    self.csi(char, self.control)
                    self.state = "text"
                else:
                    self.control += char
                    if len(self.control) > 128:
                        raise AssertionError("invalid terminal control sequence")

    def csi(self, command, args):
        if args.startswith(("?", ">", "<", "!")):
            return
        try:
            values = [int(value) if value else 0 for value in args.split(";")]
        except ValueError:
            return
        first = values[0] or 1
        if command in ("H", "f"):
            self.row = min(first - 1, self.height - 1)
            self.col = min((values[1] or 1) - 1 if len(values) > 1 else 0, self.width - 1)
        elif command == "G":
            self.col = min(first - 1, self.width - 1)
        elif command == "d":
            self.row = min(first - 1, self.height - 1)
        elif command == "A":
            self.row = max(0, self.row - first)
        elif command == "B":
            self.row = min(self.height - 1, self.row + first)
        elif command == "C":
            self.col = min(self.width - 1, self.col + first)
        elif command == "D":
            self.col = max(0, self.col - first)
        elif command == "J" and values[0] == 2:
            self.cells = [[" "] * self.width for _ in range(self.height)]
        elif command == "K":
            start, end = (0, self.width) if values[0] == 2 else (self.col, self.width)
            self.cells[self.row][start:end] = [" "] * (end - start)

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.cells)

    def count(self, text):
        return sum("".join(row).count(text) for row in self.cells)

    def contains(self, text):
        return self.count(text) > 0


def fixture_hash(answers):
    """Hash of GENERATED fixture bodies; never an installed observation."""
    digest = hashlib.sha256()
    for answer in answers:
        body = answer.replace("\r\n", "\n").encode()
        digest.update(len(body).to_bytes(4, "big"))
        digest.update(body)
    return digest.hexdigest()


def rss_kib(pid):
    """Resident set size of one process in KiB, or 0 when it is gone."""
    try:
        with open(f"/proc/{pid}/status", encoding="ascii") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except (FileNotFoundError, ProcessLookupError):
        return 0
    return 0


def process_cpu_ticks(pid):
    """Actual Linux user+system CPU ticks, or unavailable on other hosts."""
    try:
        fields = Path(f'/proc/{pid}/stat').read_text().rsplit(') ', 1)[1].split()
        return int(fields[11]) + int(fields[12])
    except FileNotFoundError:
        return None


def thread_count(pid):
    try:
        return len(os.listdir(f"/proc/{pid}/task"))
    except (FileNotFoundError, ProcessLookupError):
        return 0


class Sampler:
    """Peak RSS/thread observations for the TUI and its Agent child."""

    def __init__(self, tui_pid, agent_pid):
        self.tui_pid, self.agent_pid = tui_pid, agent_pid
        self.tui_peak = self.agent_peak = 0
        self.thread_peak = 0
        self.samples = 0

    def sample(self):
        tui = rss_kib(self.tui_pid)
        agent = rss_kib(self.agent_pid)
        self.tui_peak = max(self.tui_peak, tui)
        self.agent_peak = max(self.agent_peak, agent)
        self.thread_peak = max(self.thread_peak, thread_count(self.agent_pid))
        self.samples += 1

    def report(self):
        return {
            "tui_rss_peak_kib": self.tui_peak,
            "agent_rss_peak_kib": self.agent_peak,
            "agent_thread_peak": self.thread_peak,
            "rss_samples": self.samples,
        }


def assert_agent_child(tui_pid):
    result = subprocess.run(
        ["ps", "-eo", "pid=,ppid=,args="], capture_output=True, text=True, timeout=5, check=True
    )
    children = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) == 3 and fields[1] == str(tui_pid):
            if " agent --config " in fields[2] and " --stdio" in fields[2]:
                children.append(int(fields[0]))
    if len(children) != 1:
        raise AssertionError("expected one built-in Agent child with --stdio")
    return children[0]


def percentile(values, fraction):
    if not values:
        return 0
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(len(ordered) * fraction))
    return ordered[index]


class Chain:
    def __init__(self, binary, root, plan, tools="", approval="auto", screen_only=False,
                 remote=False, extra_env=None, agent_plan=None, terminal_read_rate=None):
        self.binary = binary
        self.plan = plan
        self.tools = tools
        self.approval = approval
        self.screen_only = screen_only
        self.remote_enabled = remote
        self.extra_env = extra_env or {}
        self.agent_plan = agent_plan
        self.expected_incomplete = {}
        self.terminal_inputs = []
        self.pumps = []
        self.phone_readers = []
        self.terminal_paused = False
        self.terminal_read_rate = terminal_read_rate
        self.next_terminal_read = 0
        self.terminal_read_calls = self.terminal_throttled_polls = self.unread_pty_peak = 0
        self.provider = Provider(plan)
        self.temporary = tempfile.TemporaryDirectory(prefix="minicore-natural-", dir=root)
        self.process = None
        self.master = None
        self.slave = None

    def __enter__(self):
        try:
            return self.start()
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def start(self):
        root = Path(self.temporary.name)
        self.trace = Trace(root / "observer.jsonl")
        self.home = root / "home"
        self.workspace = root / "workspace"
        (self.home / "prompt").mkdir(parents=True)
        self.workspace.mkdir()
        (self.home / "config.toml").write_text(
            'default_profile = "coding"\n'
            '[profiles.coding]\nmodel = "smokemodel"\n'
            f'approval = "{self.approval}"\ntools = [{self.tools}]\n'
            "[loop]\nmodel_retry_attempts = 1\n",
            encoding="utf-8",
        )
        (self.home / "models.toml").write_text(
            '[providers.local]\napi = "open_ai_responses"\n'
            f'base_url = "http://127.0.0.1:{self.provider.server_port}/v1"\n'
            'api_key_env = "MINICORE_TUI_SMOKE_SENTINEL"\n'
            '[models.smokemodel]\nprovider = "local"\n'
            f"context_window = {getattr(self.plan, 'context_window', 64000)}\nmax_output_tokens = 8000\n",
            encoding="utf-8",
        )
        (self.home / "prompt" / "SYSTEM_PROMPT.md").write_text("Fixture.\n", encoding="utf-8")
        if self.remote_enabled:
            with (self.home / "config.toml").open("a") as config:
                config.write(f'\n[remote]\nbind = "127.0.0.1:0"\nroots = [{json.dumps(str(self.workspace))}]\n')
            (self.home / "remote.json").write_text(json.dumps({
                "password": "isolated-phone-fixture", "devices": [],
            }))
        self.provider.thread.start()
        if not self.provider.serving.wait(5):
            raise AssertionError("provider listener did not start")
        self.master, slave = pty.openpty()
        self.slave = slave
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", HEIGHT, WIDTH, 0, 0))
        env = {
            "HOME": str(self.home),
            "PATH": "/usr/bin:/bin",
            "TERM": "xterm-256color",
            "LANG": "C.UTF-8",
            "MINICORE_TUI_SMOKE_SENTINEL": KEY,
        }
        if not self.screen_only:
            env["MINICORE_E2E_TRACE"] = str(self.trace.path)
        if os.environ.get("MINICORE_E2E_CAPTURE_AGENT") == "1":
            self.agent_trace = AgentTrace(root / "agent-observer.jsonl", plan=self.agent_plan)
            env["MINICORE_AGENT_E2E_TRACE"] = str(self.agent_trace.path)
        if self.agent_plan is not None:
            if not hasattr(self, "agent_trace"):
                raise AssertionError("Agent fault plan requires combined observation feature/capture")
            self.release_dir = root / "agent-releases"
            self.release_dir.mkdir()
            plan_path = root / "agent-plan.json"
            plan_path.write_text(json.dumps(self.agent_plan, sort_keys=True))
            env["MINICORE_AGENT_E2E_PLAN"] = str(plan_path)
            env["MINICORE_AGENT_E2E_RELEASE_DIR"] = str(self.release_dir)
            evidence = os.environ.get("MINICORE_E2E_EVIDENCE_DIR")
            if evidence:
                (Path(evidence) / "fault-plan.json").write_bytes(plan_path.read_bytes())
        env.update(self.extra_env)
        self.process = subprocess.Popen(
            [sys.executable, str(Path(__file__).with_name("process_support.py")),
             self.binary, "--home", str(self.home), "--workspace", str(self.workspace)]
            + (["--remote"] if self.remote_enabled else []),
            cwd=self.workspace,
            env=env,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            close_fds=True,
        )
        register_group(self.process)
        os.close(slave)
        self.slave = None
        self.screen = Screen()
        # Sample from the first frame on; the Agent child appears shortly after.
        self.sampler = Sampler(self.process.pid, 0)
        self.wait_for("bootstrap", lambda: self.screen.contains("smokemodel"))
        self.agent_pid = assert_agent_child(self.process.pid)
        self.sampler.agent_pid = self.agent_pid
        self.sampler.sample()
        if not self.screen_only:
            self.wait_for("test-only installed-content observer ready", lambda: self.trace.ready, timeout=5)
        return self

    def wait_for(self, label, predicate, timeout=STAGE_TIMEOUT):
        deadline = time.monotonic() + timeout
        while True:
            self.provider.check()
            self.sampler.sample()
            for pump in self.pumps:
                pump()
            if not self.screen_only:
                self.trace.poll()
            if predicate():
                return time.monotonic()
            if self.process.poll() is not None:
                # A paced reader may still have the final restore in the PTY
                # after the process exits; consume actual bytes before judging.
                self.drain()
                if not self.screen_only:
                    self.trace.poll()
                if predicate():
                    return time.monotonic()
                raise AssertionError(
                    f"TUI exited during {label} (status {self.process.returncode})"
                )
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                # Never copy prompts/answers/tool arguments into evidence.
                raise AssertionError(
                    f"timeout at {label}; requests={self.provider.count()}, "
                    f"terminal_bytes={self.screen.raw_bytes}; "
                    f"observer_ready={self.trace.ready}, installs={len(self.trace.installs)}"
                )
            terminal_ready = not self.terminal_paused and time.monotonic() >= self.next_terminal_read
            if self.terminal_read_rate:
                unread = struct.unpack('I', fcntl.ioctl(self.master, termios.FIONREAD, struct.pack('I', 0)))[0]
                self.unread_pty_peak = max(self.unread_pty_peak, unread)
                self.terminal_throttled_polls += int(not terminal_ready)
            ready, _, _ = select.select(
                ([self.master] if terminal_ready else []) + [self.provider.notify_read]
                + [peer.socket for peer in self.phone_readers if peer.read_ready()],
                [], [], min(remaining, 0.05)
            )
            if self.provider.notify_read in ready:
                os.read(self.provider.notify_read, 4096)
            if self.master in ready:
                try:
                    data = os.read(self.master, 4096 if self.terminal_read_rate else 65536)
                    self.terminal_read_calls += 1
                    if self.terminal_read_rate:
                        self.next_terminal_read = time.monotonic() + len(data) / self.terminal_read_rate
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    data = b""
                if data:
                    self.screen.feed(data)
                elif self.process.poll() is None:
                    raise AssertionError("terminal closed before TUI exit")

    def verify_turn(self, index, text, denied=False):
        if self.screen_only:
            return
        start = index * (self.plan.tool_steps + 1)
        expected = [
            tool_part(f"call_tool_{start + step:04d}", step, "denied" if denied else "succeeded")
            for step in range(self.plan.tool_steps)
        ]
        expected.append(text_part(text, self.plan.tool_steps))
        self.wait_for(
            f"actual canonical installation {index}",
            lambda: self.trace.expect_install(index, expected) is not None,
            timeout=TURN_TIMEOUT,
        )

    def approval_ready(self, tool_index):
        if self.screen_only:
            return True
        if not self.trace.controls:
            return False
        pending = self.trace.controls[-1].get("interaction")
        return pending is not None and pending["tool_call_id"] == f"call_tool_{tool_index:04d}"

    def observation_report(self, count):
        if self.screen_only:
            return {"canonical_evidence": False, "canonical_installs_verified": None}
        # Captured before the temporary trace directory is cleaned up.
        if self.observation_result["canonical_installs_verified"] != count:
            raise AssertionError("expected completion count differs from actual installations")
        return self.observation_result

    def key(self, data):
        os.write(self.master, data)

    def submit(self, text):
        self.terminal_inputs.append(text)
        os.write(self.master, text.encode() + b"\r")

    def queue(self, text):
        # Alt+Enter is the production FollowUp key, not a steer/manual turn.
        self.terminal_inputs.append(text)
        os.write(self.master, text.encode() + b"\x1b\r")

    def drain(self):
        """Consume whatever the terminal has produced without waiting."""
        while True:
            ready, _, _ = select.select([self.master], [], [], 0)
            if not ready:
                return
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                return
            if not data:
                return
            self.screen.feed(data)

    def release_fault(self, step):
        if not hasattr(self, "release_dir") or not step.replace("-", "").replace("_", "").isalnum():
            raise AssertionError("invalid test fault release")
        (self.release_dir / (step + ".release")).touch(exist_ok=False)

    def shutdown(self):
        self.terminal_paused = False
        os.write(self.master, b"\x03" if self.remote_enabled else b"\x04")
        if self.remote_enabled:
            self.wait_for("remote quit confirmation armed", lambda: self.screen.contains("quit again"), timeout=5)
            os.write(self.master, b"\x03")
        self.wait_for("terminal restore", lambda: self.screen.saw_restore, timeout=TURN_TIMEOUT)
        try:
            status = self.process.wait(timeout=10)
        except subprocess.TimeoutExpired as error:
            raise AssertionError("TUI did not exit after terminal restore") from error
        if status != 0:
            raise AssertionError(f"TUI exit status {status}")
        assert_group_exited(self.process.pid, self.agent_pid, timeout=10)
        if not self.screen_only:
            if self.terminal_inputs:
                started = [row for row in self.agent_trace.records() if row['event'] == 'loop_installed']
                if len(started) > len(self.terminal_inputs):
                    raise ObservationError('actual terminal starts exceed submitted fixture inputs')
                for row, text in zip(started, self.terminal_inputs):
                    self.trace.register_input(row, text)
            self.trace.poll()
            facts = bind_completion_facts(self.agent_trace.records(), self.trace.installs, self.trace.views)
            self.trace.completion_order = {target: row['seq'] for target, row in facts.items()}
            evidence = os.environ.get('MINICORE_E2E_EVIDENCE_DIR')
            if evidence:
                with (Path(evidence) / 'fixture-oracle.json').open('x', encoding='utf-8') as handle:
                    json.dump(self.trace.oracle_document(self.expected_incomplete, self.agent_plan), handle, sort_keys=True, indent=2)
                    handle.write('\n')
            self.observation_result = {"canonical_evidence": True,
                                       "sampled_process_resources": self.sampler.report(),
                                       "terminal_reader": {"rate_bytes_per_second": self.terminal_read_rate,
                                                           "read_calls": self.terminal_read_calls,
                                                           "throttled_polls": self.terminal_throttled_polls,
                                                           "unread_pty_peak_bytes": self.unread_pty_peak},
                                       **self.trace.finish(len(self.trace.installs), self.expected_incomplete)}
            if hasattr(self, "agent_trace"):
                self.observation_result["agent_observation"] = self.agent_trace.finish(
                    self.trace.installs, self.expected_incomplete)
            evidence = os.environ.get("MINICORE_E2E_EVIDENCE_DIR")
            if evidence:
                # Only schema1 identities/hashes/counters, never fixture bodies.
                (Path(evidence) / "observer.jsonl").write_bytes(self.trace.path.read_bytes())
                if hasattr(self, "agent_trace"):
                    (Path(evidence) / "agent-observer.jsonl").write_bytes(self.agent_trace.path.read_bytes())

    def __exit__(self, *_exc):
        self.provider.gate.set()
        if hasattr(self.plan, "close"):
            self.plan.close()
        if hasattr(self, "release_dir"):
            for step in self.agent_plan.get("steps", []):
                if step.get("action") == "pause":
                    (self.release_dir / (step["id"] + ".release")).touch(exist_ok=True)
        if not self.screen_only and hasattr(self, "trace"):
            evidence = os.environ.get("MINICORE_E2E_EVIDENCE_DIR")
            if evidence and self.trace.path.exists():
                (Path(evidence) / "observer.jsonl").write_bytes(self.trace.path.read_bytes())
                if hasattr(self, "agent_trace") and self.agent_trace.path.exists():
                    (Path(evidence) / "agent-observer.jsonl").write_bytes(self.agent_trace.path.read_bytes())
        cleanup_group(self.process)
        if self.master is not None:
            try:
                os.close(self.master)
            except OSError:
                pass
        if self.slave is not None:
            os.close(self.slave)
            self.slave = None
        self.provider.close()
        self.temporary.cleanup()
        return False


def natural(binary, root, count, long_stream, tool_every=0, screen_only=False):
    """Sequential MANUAL submissions; tool loops are not automatic successors."""
    plan = Plan(width=4096 if long_stream else 0, tool_every=tool_every)
    plan.request_limit = count * (plan.tool_steps + 1)
    latencies = []
    answers = []
    anomalies = []
    with Chain(binary, root, plan, tools='"read"' if tool_every else "", screen_only=screen_only) as chain:
        if tool_every:
            (chain.workspace / "fixture.txt").write_text("fixture read content\n")
        started = time.monotonic()
        for index in range(count):
            # The marker always ends the answer so a stream longer than the
            # screen still leaves its identity and completion line visible; the
            # padding before it makes the answer span many frames.
            # Class-tagged fixture oracles are independent of observed hashes.
            tag = "TOOL" if tool_every else "LONG" if long_stream else "SHRT"
            marker = f"NAT{tag}{index:04d}END"
            padding = "Z" * (3072 if long_stream else 0)
            # Register against the absolute provider index this turn's text
            # answer will occupy; tool turns take the even indices.
            text = plan.register(padding + marker, plan.text_index(index))
            # Generated bytes are an independent oracle, not an observation.
            answers.append(text)
            before = chain.provider.count()
            submitted = time.monotonic()
            chain.submit(f"turn {index}")
            chain.wait_for(
                f"request {index}",
                lambda before=before: chain.provider.count() > before,
                timeout=TURN_TIMEOUT,
            )
            # Wait for this turn's own marker, so a previous completion cannot
            # satisfy the predicate, and for its completion line on the same
            # frame so the turn really reached the terminal state.
            chain.wait_for(
                f"completion {index}",
                lambda marker=marker: chain.screen.contains(marker)
                and chain.screen.contains("completed"),
                timeout=TURN_TIMEOUT,
            )
            chain.verify_turn(index, text)
            latencies.append(time.monotonic() - submitted)
            if chain.screen.count(marker) != 1:
                anomalies.append(f"turn {index} answer not shown exactly once")
            if chain.screen.count("completed") < 1:
                anomalies.append(f"turn {index} lost its completion line")
            # A tool turn must still terminate: its result has to reach the
            # transcript before the answer, so a lost tool record is visible.
            if tool_every and not chain.screen.contains("fixture.txt"):
                anomalies.append(f"turn {index} lost its tool record")
        elapsed = time.monotonic() - started
        chain.drain()
        final_visible = sum(1 for text in answers if text in chain.screen.text())
        chain.shutdown()
    result = {
                "mode": "screen_diagnostic" if screen_only else "natural_fixture", "manual_submissions": count,
                "automatic_successors": 0, "latency_seconds": latencies,
                "class": (
                    "long_stream"
                    if long_stream
                    else "tool_sequence"
                    if tool_every
                    else "short_turns"
                ),
                "completions": count,
                "anomalies": len(anomalies),
                "anomaly_samples": anomalies[:5],
                "provider_requests": chain.provider.count(),
                "tool_requests": plan.tool_steps * count,
                "screen_markers_confirmed": count - len(anomalies),
                "answers_on_final_screen": final_visible,
                "expected_fixture_sha256": fixture_hash(answers),
                **chain.observation_report(count),
                "turn_seconds_p50": round(percentile(latencies, 0.50), 4),
                "turn_seconds_p95": round(percentile(latencies, 0.95), 4),
                "turn_seconds_p99": round(percentile(latencies, 0.99), 4),
                "turn_seconds_max": round(max(latencies) if latencies else 0.0, 4),
                "total_seconds": round(elapsed, 3),
                "terminal_bytes": chain.screen.raw_bytes,
                "agent_pid_observed": True,
                "cleanup": "tui-zero-exit,agent-gone,group-gone",
                **chain.sampler.report(),
            }
    if anomalies:
        raise ObservationError(f"natural load found {len(anomalies)} anomalies")
    return result


def approvals(binary, root, count, screen_only=False):
    """Every turn asks for a write approval, denied with Esc, then completes.

    This is the approval-under-load class: the sheet must appear for each turn,
    Esc must deny it, the tool must not run, and the turn must still reach a
    completion. Interleaving them under repetition is what makes it load rather
    than a single gated smoke case.
    """
    plan = Plan(tool_every=1, tool_name="write", tool_path="denied.txt", tool_steps=1)
    plan.request_limit = count * 2
    latencies = []
    answers = []
    anomalies = []
    with Chain(binary, root, plan, tools='"write"', approval="ask", screen_only=screen_only) as chain:
        started = time.monotonic()
        for index in range(count):
            marker = f"NATAPPR{index:04d}END"
            text = plan.register(marker, plan.text_index(index))
            answers.append(text)
            submitted = time.monotonic()
            chain.submit(f"turn {index}")
            # The sheet from the previous turn is still in the scrollback, so the
            # presence of "Allow once" alone cannot mean this turn is asking. Wait
            # for this turn's own tool request to have been served first: that
            # both proves the new turn started and rules out the stale modal.
            chain.wait_for(
                f"tool request {index}",
                lambda index=index: chain.provider.count() >= 2 * index + 1,
                timeout=TURN_TIMEOUT,
            )
            chain.wait_for(
                f"approval sheet {index}",
                lambda marker=marker, index=index: chain.screen.contains("Allow once")
                and chain.screen.contains("Deny")
                and not chain.screen.contains(marker)
                and chain.approval_ready(2 * index),
                timeout=TURN_TIMEOUT,
            )
            chain.key(b"\x1b")
            chain.wait_for(
                f"completion {index}",
                lambda marker=marker: chain.screen.contains(marker)
                and chain.screen.contains("completed"),
                timeout=TURN_TIMEOUT,
            )
            chain.verify_turn(index, text, denied=True)
            latencies.append(time.monotonic() - submitted)
            if chain.screen.count(marker) != 1:
                anomalies.append(f"turn {index} answer not shown exactly once")
            if (chain.workspace / "denied.txt").exists():
                anomalies.append(f"turn {index} denied write still created the file")
        elapsed = time.monotonic() - started
        final_visible = sum(1 for text in answers if chain.screen.contains(text))
        chain.drain()
        chain.shutdown()
    result = {
                "mode": "screen_diagnostic" if screen_only else "natural_fixture", "manual_submissions": count,
                "automatic_successors": 0, "latency_seconds": latencies,
                "class": "approval_denied",
                "completions": count,
                "anomalies": len(anomalies),
                "anomaly_samples": anomalies[:5],
                "provider_requests": 2 * count,
                "approvals_shown": count,
                "screen_markers_confirmed": count - len(anomalies),
                "answers_on_final_screen": final_visible,
                "expected_fixture_sha256": fixture_hash(answers),
                **chain.observation_report(count),
                "turn_seconds_p50": round(percentile(latencies, 0.50), 4),
                "turn_seconds_p95": round(percentile(latencies, 0.95), 4),
                "turn_seconds_p99": round(percentile(latencies, 0.99), 4),
                "turn_seconds_max": round(max(latencies) if latencies else 0.0, 4),
                "total_seconds": round(elapsed, 3),
                "terminal_bytes": chain.screen.raw_bytes,
                "agent_pid_observed": True,
                "cleanup": "tui-zero-exit,agent-gone,group-gone",
                **chain.sampler.report(),
            }
    if anomalies:
        raise ObservationError(f"approval load found {len(anomalies)} anomalies")
    return result


def integrity(binary, root, screen_only=False):
    """Gated fault: the provider is held, then released, across two turns."""
    plan = Plan()
    plan.request_limit = 2
    results = []
    with Chain(binary, root, plan, screen_only=screen_only) as chain:
        first = plan.register("INTEGRITYA")
        chain.submit("first")
        chain.wait_for("first request", lambda: chain.provider.count() >= 1, timeout=TURN_TIMEOUT)
        chain.wait_for(
            "first completion",
            lambda: chain.screen.contains(first) and chain.screen.contains("completed"),
            timeout=TURN_TIMEOUT,
        )
        chain.verify_turn(0, first)
        results.append(chain.screen.count(first) == 1)
        second = plan.register("INTEGRITYB")
        chain.provider.gate.clear()
        chain.submit("second")
        chain.wait_for("second request held", lambda: chain.provider.count() >= 2, timeout=TURN_TIMEOUT)
        chain.wait_for("working shown", lambda: chain.screen.contains("Working"), timeout=TURN_TIMEOUT)
        chain.provider.gate.set()
        chain.wait_for(
            "second completion",
            lambda: chain.screen.contains(second),
            timeout=TURN_TIMEOUT,
        )
        chain.verify_turn(1, second)
        results.append(second in chain.screen.text())
        chain.drain()
        first_count = chain.screen.count(first)
        chain.shutdown()
    print(
        json.dumps(
            {
                "mode": "provider_gate_diagnostic" if screen_only else "integrity",
                "faults": ["provider_response_hold"],
                "turns": 2,
                "each_visible": all(results),
                "first_not_duplicated": first_count == 1,
                "expected_fixture_sha256": fixture_hash([first, second]),
                **chain.observation_report(2),
                "cleanup": "tui-zero-exit,agent-gone,group-gone",
            },
            sort_keys=True,
        )
    )




def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    parser.add_argument("mode", choices=["natural", "integrity", "approvals"])
    parser.add_argument("--count", type=int, default=100)
    parser.add_argument("--long-stream", action="store_true")
    parser.add_argument(
        "--tool-every",
        action="store_true",
        help="answer each turn with a read tool call before its text",
    )
    parser.add_argument("--root")
    parser.add_argument("--screen-only", action="store_true", help="diagnostic only; not canonical acceptance")
    args = parser.parse_args()
    root = args.root or None
    if args.mode != "integrity" and args.count < 100 and not args.screen_only:
        parser.error("natural acceptance needs >=100 completions per class")
    if args.mode == 'integrity':
        integrity(args.binary, root, args.screen_only)
        return
    if args.screen_only:
        report = natural(args.binary, root, args.count, args.long_stream, args.tool_every, True) if args.mode == 'natural' else approvals(args.binary, root, args.count, True)
        print(json.dumps(report, sort_keys=True))
        return
    if not 100 <= args.count <= 200 or os.environ.get('MINICORE_E2E_CAPTURE_AGENT') != '1':
        parser.error('natural classes require count 100..200 and actual Agent capture')
    base = Path(os.environ['MINICORE_E2E_EVIDENCE_DIR'])
    remaining, results = args.count, []
    while remaining:
        count = min(16, remaining)
        directory = base / f'batch-{len(results):03d}'
        directory.mkdir()
        os.environ['MINICORE_E2E_EVIDENCE_DIR'] = str(directory)
        try:
            report = natural(args.binary, root, count, args.long_stream, args.tool_every) if args.mode == 'natural' else approvals(args.binary, root, count)
            (directory / 'result.json').write_text(json.dumps(report, sort_keys=True, indent=2) + '\n')
            results.append(report)
        finally:
            os.environ['MINICORE_E2E_EVIDENCE_DIR'] = str(base)
        remaining -= count
    latency = [value for report in results for value in report['latency_seconds']]
    print(json.dumps({'mode': 'natural_fixture', 'class': results[0]['class'], 'completions': args.count,
                      'manual_submissions': args.count, 'automatic_successors': 0,
                      'canonical_installs_verified': sum(report['canonical_installs_verified'] for report in results),
                      'anomalies': sum(report['anomalies'] for report in results), 'injected_faults': [],
                      'batches': len(results), 'seconds_p50': percentile(latency, .5), 'seconds_p95': percentile(latency, .95),
                      'seconds_p99': percentile(latency, .99), 'seconds_max': max(latency)}, sort_keys=True))


if __name__ == "__main__":
    run_bounded(main, 3600, "natural-load")
