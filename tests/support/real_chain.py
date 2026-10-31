#!/usr/bin/env python3
"""Drive the real Unix TUI in a PTY against a finite loopback Responses API."""

import codecs
import contextlib
import errno
import fcntl
import json
import os
import pty
import select
import signal
import socket
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

from process_support import assert_group_exited, cleanup_group, register_group


KEY = "dummy-local-sentinel"
PROMPT = "Exercise the isolated smoke fixture."
ANSWER = "SMOKEANSWEROK"
PARTIAL = "SMOKEPARTIALOK"
TOOL_CONTENT = "SMOKEREADCONTENT"
CALL_ID = "smoke-call-1"
WIDTH, HEIGHT = 120, 36
STAGE_TIMEOUT = 20


def sse(events):
    return b"".join(
        b"data: " + json.dumps(event, separators=(",", ":")).encode() + b"\n\n"
        for event in events
    )


def completed():
    return {"type": "response.completed", "response": {"status": "completed"}}


def text_response(text):
    return sse([{"type": "response.output_text.delta", "delta": text}, completed()])


def tool_response(name, arguments):
    return sse([
        {
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": CALL_ID,
                "name": name,
                "arguments": json.dumps(arguments),
            },
        },
        completed(),
    ])


class Provider(ThreadingHTTPServer):
    daemon_threads = False
    allow_reuse_address = False

    def __init__(self, case):
        super().__init__(("127.0.0.1", 0), Handler)
        self.case = case
        self.requests = []
        self.errors = []
        self.lock = threading.Lock()
        self.release = threading.Event()
        self.cancel_release = threading.Event()
        self.notify_read, self.notify_write = os.pipe()
        self.serving = threading.Event()
        self.thread = threading.Thread(target=self.serve, daemon=True)

    def serve(self):
        self.serving.set()
        self.serve_forever(poll_interval=0.1)

    def notify(self):
        os.write(self.notify_write, b"x")

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
        self.release.set()
        self.cancel_release.set()
        if self.thread.is_alive():
            self.shutdown()
            self.thread.join(timeout=2)
        self.server_close()
        os.close(self.notify_read)
        os.close(self.notify_write)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        try:
            self.connection.settimeout(STAGE_TIMEOUT)
            if self.path != "/v1/responses":
                raise AssertionError("unexpected request path")
            if self.headers.get("Authorization") != "Bearer " + KEY:
                raise AssertionError("missing dummy authorization sentinel")
            length = int(self.headers.get("Content-Length", "-1"))
            if not 0 <= length <= 1024 * 1024:
                raise AssertionError("request length outside fixture bound")
            request = json.loads(self.rfile.read(length))
            if request.get("model") != "smokemodel" or request.get("stream") is not True:
                raise AssertionError("unexpected model or non-streaming request")
            with self.server.lock:
                index = len(self.server.requests)
                limit = 2 if self.server.case in ("tool", "approval") else 1
                if index >= limit:
                    raise AssertionError("unexpected extra provider request")
                self.server.requests.append(request)
            self.server.notify()
            if not self.server.release.wait(STAGE_TIMEOUT):
                raise AssertionError("response gate timed out")
            case = self.server.case
            if case in ("tool", "approval") and index == 0:
                name = "read" if case == "tool" else "write"
                arguments = (
                    {"path": "fixture.txt", "limit": 32}
                    if case == "tool"
                    else {"path": "denied.txt", "content": "fixture"}
                )
                body = tool_response(name, arguments)
            elif index != (1 if case in ("tool", "approval") else 0):
                raise AssertionError("unexpected extra provider request")
            elif case == "error":
                body = b'{"error":{"message":"fixture failure","type":"invalid_request_error"}}'
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()
                return
            elif case == "cancel":
                partial = sse([{"type": "response.output_text.delta", "delta": PARTIAL}])
                body = partial + sse([completed()])
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(partial)
                self.wfile.flush()
                if not self.server.cancel_release.wait(STAGE_TIMEOUT):
                    raise AssertionError("cancel gate timed out")
                return
            else:
                body = text_response(ANSWER)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            if self.server.case != "cancel":
                self.server.record_error("provider connection closed before response")
        except Exception as error:
            self.server.record_error(error)


class Screen:
    def __init__(self):
        self.cells = [[" "] * WIDTH for _ in range(HEIGHT)]
        self.row = self.col = 0
        self.state = "text"
        self.control = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.raw_bytes = 0
        self.saw_restore = False
        self.raw_tail = b""

    def feed(self, data):
        self.raw_bytes += len(data)
        if self.raw_bytes > 4 * 1024 * 1024:
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
                    self.row = min(self.row + 1, HEIGHT - 1)
                elif char >= " " and char != "\x7f":
                    width = 0 if unicodedata.combining(char) else (
                        2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                    )
                    if width:
                        if self.col >= WIDTH:
                            self.col = 0
                            self.row = min(self.row + 1, HEIGHT - 1)
                        self.cells[self.row][self.col] = char
                        if width == 2 and self.col + 1 < WIDTH:
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
            self.row = min(first - 1, HEIGHT - 1)
            self.col = min((values[1] or 1) - 1 if len(values) > 1 else 0, WIDTH - 1)
        elif command == "G":
            self.col = min(first - 1, WIDTH - 1)
        elif command == "d":
            self.row = min(first - 1, HEIGHT - 1)
        elif command == "A":
            self.row = max(0, self.row - first)
        elif command == "B":
            self.row = min(HEIGHT - 1, self.row + first)
        elif command == "C":
            self.col = min(WIDTH - 1, self.col + first)
        elif command == "D":
            self.col = max(0, self.col - first)
        elif command == "J" and values[0] == 2:
            self.cells = [[" "] * WIDTH for _ in range(HEIGHT)]
        elif command == "K":
            start, end = (0, WIDTH) if values[0] == 2 else (self.col, WIDTH)
            self.cells[self.row][start:end] = [" "] * (end - start)

    def count(self, text):
        return sum("".join(row).count(text) for row in self.cells)

    def contains(self, text):
        return self.count(text) > 0


def child_tty():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


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


def main(binary, case):
    if case not in {"answer", "tool", "approval", "error", "cancel"}:
        raise ValueError("unknown smoke case")
    provider = Provider(case)
    temporary = tempfile.TemporaryDirectory(prefix="minicore-real-chain-")
    process = None
    master = slave = None
    start = time.monotonic()
    try:
        with contextlib.nullcontext(temporary.name) as root:
            home = Path(root) / "home"
            workspace = Path(root) / "workspace"
            (home / "prompt").mkdir(parents=True)
            workspace.mkdir()
            (workspace / "fixture.txt").write_text(TOOL_CONTENT, encoding="utf-8")
            tools = '["read"]' if case == "tool" else '["write"]' if case == "approval" else "[]"
            approval = "ask" if case == "approval" else "auto"
            (home / "config.toml").write_text(
                f'default_profile = "coding"\n[profiles.coding]\nmodel = "smokemodel"\n'
                f'approval = "{approval}"\ntools = {tools}\n[loop]\nmodel_retry_attempts = 1\n',
                encoding="utf-8",
            )
            (home / "models.toml").write_text(
                '[providers.local]\napi = "open_ai_responses"\n'
                f'base_url = "http://127.0.0.1:{provider.server_port}/v1"\n'
                'api_key_env = "MINICORE_TUI_SMOKE_SENTINEL"\n'
                '[models.smokemodel]\nprovider = "local"\n'
                'context_window = 64000\nmax_output_tokens = 8000\n',
                encoding="utf-8",
            )
            (home / "prompt" / "SYSTEM_PROMPT.md").write_text("Fixture.\n", encoding="utf-8")
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", HEIGHT, WIDTH, 0, 0))
            env = {
                "HOME": str(home),
                "PATH": "/usr/bin:/bin",
                "TERM": "xterm-256color",
                "LANG": "C.UTF-8",
                "MINICORE_TUI_SMOKE_SENTINEL": KEY,
            }
            process = subprocess.Popen(
                [binary, "--home", str(home), "--workspace", str(workspace)],
                cwd=workspace,
                env=env,
                stdin=slave,
                stdout=slave,
                stderr=slave,
                preexec_fn=child_tty,
                close_fds=True,
            )
            register_group(process)
            os.close(slave)
            slave = None
            provider.thread.start()
            if not provider.serving.wait(2):
                raise AssertionError("provider listener did not start")
            screen = Screen()

            def wait_for(label, predicate):
                deadline = time.monotonic() + STAGE_TIMEOUT
                while True:
                    provider.check()
                    if predicate():
                        return
                    if process.poll() is not None:
                        raise AssertionError(f"TUI exited during {label} (status {process.returncode})")
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        # Say why an interaction never appeared, so a missing
                        # control snapshot is not reported as a bare timeout.
                        hint = ""
                        if label == "approval":
                            hint = (
                                "; the agent requested a tool but no approval sheet "
                                "was drawn, which means no control snapshot carrying "
                                "pending_interaction reached the TUI"
                            )
                        raise AssertionError(
                            f"timeout at {label}; provider_requests={provider.count()}, "
                            f"terminal_bytes={screen.raw_bytes}, "
                            f"saw_working={screen.contains('Working')}{hint}"
                        )
                    ready, _, _ = select.select([master, provider.notify_read], [], [], remaining)
                    if provider.notify_read in ready:
                        os.read(provider.notify_read, 4096)
                    if master in ready:
                        try:
                            data = os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            data = b""
                        if data:
                            screen.feed(data)
                        elif process.poll() is None:
                            raise AssertionError("terminal closed before TUI exit")

            wait_for("bootstrap", lambda: screen.contains("smokemodel"))
            os.write(master, PROMPT.encode() + b"\r")
            wait_for("provider request and busy display", lambda: provider.count() >= 1 and screen.contains("Working"))
            first = provider.request(0)
            if PROMPT not in json.dumps(first):
                raise AssertionError("provider did not receive the fixture input")
            agent_pid = assert_agent_child(process.pid)
            provider.release.set()
            if case == "cancel":
                wait_for("streamed partial", lambda: screen.contains(PARTIAL))
                os.write(master, b"\x1b")
                wait_for("cancel outcome", lambda: screen.contains("interrupted"))
                if not screen.contains(PARTIAL):
                    raise AssertionError("cancelled stream lost its visible partial")
                provider.cancel_release.set()
            elif case == "approval":
                wait_for("approval", lambda: screen.contains("Allow once") and screen.contains("Deny"))
                os.write(master, b"\x1b")
                wait_for("denied tool continuation", lambda: provider.count() >= 2)
                if (workspace / "denied.txt").exists():
                    raise AssertionError("denied tool wrote a file")
            if case == "tool":
                wait_for("tool continuation", lambda: provider.count() >= 2)
            if case in ("tool", "approval"):
                continuation = provider.request(1)
                results = [
                    item for item in continuation.get("input", [])
                    if item.get("type") == "function_call_output" and item.get("call_id") == CALL_ID
                ]
                if len(results) != 1:
                    raise AssertionError("tool continuation is missing its result identity")
                if case == "tool" and TOOL_CONTENT not in results[0]["output"]:
                    raise AssertionError("read result did not reach the provider")
                if case == "approval" and results[0]["output"] != "tool denied":
                    raise AssertionError("approval denial did not reach the provider")
            if case == "error":
                wait_for("provider failure", lambda: screen.contains("failed") and screen.contains("400"))
            elif case != "cancel":
                wait_for("answer and completion", lambda: screen.contains(ANSWER) and screen.contains("completed"))
                if screen.count(ANSWER) != 1:
                    raise AssertionError("answer is duplicated on the terminal")
                if case in ("tool", "approval") and not screen.contains("1 tool"):
                    raise AssertionError("tool completion is not visible")
            expected_requests = 2 if case in ("tool", "approval") else 1
            if provider.count() != expected_requests:
                raise AssertionError("unexpected provider request count")
            os.write(master, b"\x04")
            wait_for("terminal restore", lambda: screen.saw_restore)
            try:
                status = process.wait(timeout=5)
            except subprocess.TimeoutExpired as error:
                raise AssertionError("TUI did not exit after terminal restore") from error
            if status != 0:
                raise AssertionError(f"TUI exit status {status}")
            assert_group_exited(process.pid, agent_pid)
            provider.check()
            print(
                f"real-chain {case}: requests={expected_requests}, "
                f"agent=stdio-child, terminal=restored, exit=0, "
                f"elapsed_ms={int((time.monotonic() - start) * 1000)}"
            )
    finally:
        provider.release.set()
        provider.cancel_release.set()
        cleanup_group(process)
        if master is not None:
            os.close(master)
        if slave is not None:
            os.close(slave)
        provider.close()
        temporary.cleanup()


def alarm_handler(_signum, _frame):
    raise TimeoutError("real-chain helper exceeded its deadline")


if __name__ == "__main__":
    signal.signal(signal.SIGALRM, alarm_handler)
    signal.signal(signal.SIGTERM, alarm_handler)
    signal.alarm(75)
    try:
        main(sys.argv[1], sys.argv[2])
    except Exception as error:
        print(f"real-chain {sys.argv[2]}: {type(error).__name__}: {error}", file=sys.stderr)
        sys.exit(1)
    finally:
        signal.alarm(0)
