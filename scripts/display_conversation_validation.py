#!/usr/bin/env python3
"""Validate durable conversation display through a real kernel PTY.

The harness uses the real TUI and Agent, a deterministic local HTTP Responses
model, and pyte's mature VT parser.  It exercises two ordinary turns (ASCII
and CJK), a tool turn with text before and after the tool, a restart with
``--continue``, and one expected-negative run against an old TUI binary.

All workspaces, Agent data, model traffic, and PTY artifacts are isolated under
one output directory.  No provider credentials or external network calls are
used by the model.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
import pathlib
import pty
import select
import sys
import tempfile
import threading
import time
from typing import Any

import pyte

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))
from stage7_pty import set_winsize, terminate_child, write_config  # noqa: E402


WIDTH, HEIGHT = 120, 40

MARKERS = {
    "ascii_user": "ascii-normal-unique",
    "ascii_assistant": "assistant-ascii-unique",
    "cjk_user": "中文普通消息-唯一",
    "cjk_assistant": "助手中文-回复-唯一",
    "tool_user": "tool-round-unique",
    "tool_before": "tool-before-unique",
    "tool_result": "tool-result-unique",
    "tool_after": "tool-after-unique",
}
MESSAGE_ORDER = [
    "ascii_user",
    "ascii_assistant",
    "cjk_user",
    "cjk_assistant",
    "tool_user",
    "tool_before",
    "tool_after",
]
ORDER = [
    "ascii_user",
    "ascii_assistant",
    "cjk_user",
    "cjk_assistant",
    "tool_user",
    "tool_before",
    "tool_result",
    "tool_after",
]
ALT_RE = __import__("re").compile(rb"\x1b\[\?(1047|1049)([hl])")
ALT_PREFIXES = {
    pattern[:index]
    for pattern in (b"\x1b[?1047h", b"\x1b[?1047l", b"\x1b[?1049h", b"\x1b[?1049l")
    for index in range(1, len(pattern))
}


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def completed_event(output_items: list[dict[str, Any]] | None = None) -> dict[str, Any]:
    response: dict[str, Any] = {
        "status": "completed",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 12,
            "total_tokens": 22,
            "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0},
        },
    }
    if output_items is not None:
        response["output"] = output_items
    return {"type": "response.completed", "response": response}


def text_events(parts: list[str]) -> list[dict[str, Any]]:
    return [
        *({"type": "response.output_text.delta", "delta": part} for part in parts),
        completed_event(),
    ]


def tool_events() -> list[dict[str, Any]]:
    call_id = "stage7-read-1"
    function_call = {
        "type": "function_call",
        "call_id": call_id,
        "name": "read",
        "arguments": '{"path":"fixture.txt"}',
    }
    return [
        {"type": "response.output_text.delta", "delta": "tool-before-unique\n"},
        {
            "type": "response.output_item.done",
            "output_index": 0,
            "item": function_call,
        },
        completed_event(output_items=[function_call]),
    ]


class ModelState:
    def __init__(self, output: pathlib.Path) -> None:
        self.output = output
        self.count = 0
        self.lock = threading.Lock()
        self.requests: list[dict[str, Any]] = []

    def events(self) -> list[dict[str, Any]]:
        if self.count == 1:
            return text_events(["assistant-ascii-", "unique\n"])
        if self.count == 2:
            return text_events(["助手中文-", "回复-唯一\n"])
        if self.count == 3:
            return tool_events()
        return text_events(["tool-after-", "unique\n"])


class ModelHandler(http.server.BaseHTTPRequestHandler):
    server_version = "minicore-display-loopback/1"

    def log_message(self, _format: str, *_args: Any) -> None:
        return

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        try:
            request = json.loads(body)
        except json.JSONDecodeError:
            self.send_error(400, "invalid JSON")
            return
        state: ModelState = self.server.state  # type: ignore[attr-defined]
        with state.lock:
            state.count += 1
            count = state.count
            items = request.get("input") if isinstance(request, dict) else []
            if not isinstance(items, list):
                items = []
            record = {
                "count": count,
                "path": self.path,
                "model": request.get("model") if isinstance(request, dict) else None,
                "input_item_types": [
                    item.get("type")
                    for item in items
                    if isinstance(item, dict) and isinstance(item.get("type"), str)
                ],
                "has_tools": isinstance(request.get("tools"), list)
                if isinstance(request, dict)
                else False,
            }
            state.requests.append(record)
            events = state.events()
        body_bytes = b"".join(
            b"data: " + json.dumps(event, separators=(",", ":"), ensure_ascii=False).encode()
            + b"\n\n"
            for event in events
        )
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.send_header("Content-Length", str(len(body_bytes)))
        self.end_headers()
        cursor = 0
        while cursor < len(body_bytes):
            boundary = body_bytes.find(b"\n\n", cursor) + 2
            if boundary <= 1:
                boundary = len(body_bytes)
            self.wfile.write(body_bytes[cursor:boundary])
            self.wfile.flush()
            cursor = boundary
            time.sleep(0.45 if count == 1 else 0.12)


class ModelServer:
    def __init__(self, output: pathlib.Path) -> None:
        self.state = ModelState(output)
        server_type = type("DisplayHTTPServer", (http.server.ThreadingHTTPServer,), {})
        self.server = server_type(("127.0.0.1", 0), ModelHandler)
        self.server.state = self.state  # type: ignore[attr-defined]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def port(self) -> int:
        return int(self.server.server_port)

    def start(self) -> None:
        self.thread.start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


class PyteTerminal:
    """pyte parser with explicit xterm 1047/1049 alternate-screen buffers."""

    def __init__(self, columns: int = WIDTH, lines: int = HEIGHT) -> None:
        self.columns = columns
        self.lines = lines
        self.primary = pyte.Screen(columns, lines)
        self.alternate = pyte.Screen(columns, lines)
        self.active = self.primary
        self.active_name = "primary"
        self.stream = pyte.ByteStream(self.active, strict=False)
        self.pending = bytearray()
        self.alt_enters = 0
        self.alt_leaves = 0

    def _attach(self, screen: pyte.Screen, name: str) -> None:
        self.active = screen
        self.active_name = name
        self.stream.attach(screen)

    def _switch(self, mode: int, enabled: bool) -> None:
        if enabled:
            self.alternate = pyte.Screen(self.columns, self.lines)
            self._attach(self.alternate, "alternate")
            self.alt_enters += 1
        elif self.active_name == "alternate":
            self._attach(self.primary, "primary")
            self.alt_leaves += 1
        elif mode == 1047:
            self.alt_leaves += 1

    def _feed_plain(self, data: bytes) -> None:
        if data:
            self.stream.feed(data)

    def _partial_prefix_length(self) -> int:
        for length in range(min(len(self.pending), 9), 0, -1):
            if bytes(self.pending[-length:]) in ALT_PREFIXES:
                return length
        return 0

    def feed(self, data: bytes) -> None:
        self.pending.extend(data)
        while True:
            match = ALT_RE.search(self.pending)
            if match is None:
                keep = self._partial_prefix_length()
                if keep:
                    plain = self.pending[:-keep]
                    del self.pending[:-keep]
                else:
                    plain = bytes(self.pending)
                    self.pending.clear()
                self._feed_plain(plain)
                return
            self._feed_plain(bytes(self.pending[: match.start()]))
            mode = int(match.group(1))
            self._switch(mode, match.group(2) == b"h")
            del self.pending[: match.end()]

    def finish(self) -> None:
        self._feed_plain(bytes(self.pending))
        self.pending.clear()

    def lines_text(self) -> list[str]:
        return list(self.active.display)

    def flow(self) -> str:
        return "\n".join(line.rstrip() for line in self.lines_text())

    def contains(self, marker: str) -> bool:
        return marker in self.flow()


class PtySession:
    def __init__(self, binary: pathlib.Path, agent: pathlib.Path, config: pathlib.Path,
                 workspace: pathlib.Path, raw_path: pathlib.Path, continue_recent: bool) -> None:
        self.binary = binary
        self.agent = agent
        self.config = config
        self.workspace = workspace
        self.raw_path = raw_path
        self.continue_recent = continue_recent
        self.pid: int | None = None
        self.master: int | None = None
        self.screen = PyteTerminal()
        self.raw = raw_path.open("wb")

    def start(self) -> None:
        self.pid, self.master = pty.fork()
        if self.pid == 0:
            os.chdir(self.workspace)
            env = os.environ.copy()
            env.update({"TERM": "xterm-256color", "MINICORE_STAGE7_KEY": "display-loopback-key"})
            args = [
                str(self.binary),
                "--agent-bin", str(self.agent),
                "--agent-config", str(self.config),
                "--workspace", str(self.workspace),
                "--theme", "dark",
            ]
            if self.continue_recent:
                args.append("--continue")
            os.execve(str(self.binary), args, env)
            raise AssertionError("execve returned")
        assert self.master is not None
        set_winsize(self.master, (WIDTH, HEIGHT))

    def read_once(self, timeout: float = 0.05) -> None:
        assert self.master is not None
        ready, _, _ = select.select([self.master], [], [], timeout)
        if not ready:
            return
        try:
            data = os.read(self.master, 65536)
        except OSError:
            return
        if not data:
            return
        self.raw.write(data)
        self.raw.flush()
        self.screen.feed(data)

    def wait_for(self, markers: list[str], timeout: float = 15.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if all(self.screen.contains(marker) for marker in markers):
                return
            self.read_once(max(0.0, min(0.05, deadline - time.monotonic())))
        raise RuntimeError(
            f"{self.binary} did not render {markers!r};\n{self.screen.flow()}"
        )

    def send(self, data: bytes) -> None:
        assert self.master is not None
        view = memoryview(data)
        while view:
            written = os.write(self.master, view)
            view = view[written:]

    def _wait_and_drain(self, timeout: float) -> int | None:
        assert self.pid is not None
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read_once(0.05)
            waited, status = os.waitpid(self.pid, os.WNOHANG)
            if waited == self.pid:
                return os.waitstatus_to_exitcode(status)
        return None

    def shutdown(self) -> int:
        # Ctrl-C twice follows the production graceful shutdown path and lets
        # the Agent persist the current session before a later --continue.
        self.send(b"\x03")
        self._wait_and_drain(0.5)
        self.send(b"\x03")
        assert self.pid is not None
        status = self._wait_and_drain(8)
        if status is None:
            # Ctrl-D remains a bounded fallback for a cross-built binary whose
            # signal path is unavailable under the local execution layer.
            self.send(b"\x04")
            status = self._wait_and_drain(5)
        if status is None:
            terminate_child(self.pid)
            raise RuntimeError(f"{self.binary} did not shut down")
        self.screen.finish()
        self.raw.close()
        if self.master is not None:
            os.close(self.master)
            self.master = None
        self.pid = None
        return status

    def abort(self) -> None:
        if self.pid is not None:
            terminate_child(self.pid)
            self.pid = None
        if not self.raw.closed:
            self.raw.close()
        if self.master is not None:
            os.close(self.master)
            self.master = None


def save_screen(path: pathlib.Path, screen: PyteTerminal) -> None:
    path.write_text(screen.flow() + "\n", encoding="utf-8")


def assert_final_screen(screen: PyteTerminal) -> dict[str, Any]:
    flow = screen.flow()
    counts = {name: flow.count(marker) for name, marker in MARKERS.items()}
    message_counts = {name: counts[name] for name in MESSAGE_ORDER}
    missing_or_duplicate = {
        name: count for name, count in message_counts.items() if count != 1
    }
    if missing_or_duplicate or counts["tool_result"] < 1:
        raise RuntimeError(f"final screen marker counts are invalid: {counts}\n{flow}")
    positions = {name: flow.index(MARKERS[name]) for name in ORDER}
    message_positions = {name: positions[name] for name in MESSAGE_ORDER}
    if any(
        message_positions[left] >= message_positions[right]
        for left, right in zip(MESSAGE_ORDER, MESSAGE_ORDER[1:])
    ):
        raise RuntimeError(f"final screen message order is wrong: {message_positions}\n{flow}")
    if not positions["tool_before"] < positions["tool_result"] < positions["tool_after"]:
        raise RuntimeError(f"tool result order is wrong: {positions}\n{flow}")
    if screen.alt_enters < 1:
        raise RuntimeError(
            f"alternate-screen entry evidence missing: enters={screen.alt_enters} leaves={screen.alt_leaves}"
        )
    return {
        "message_marker_counts": message_counts,
        "tool_result_occurrences": counts["tool_result"],
        "marker_order": positions,
        "alt_enters": screen.alt_enters,
        "alt_leaves": screen.alt_leaves,
        "active_buffer_before_shutdown": screen.active_name,
    }


def create_fixture(root: pathlib.Path, port: int) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
    root.mkdir(parents=True, exist_ok=True)
    workspace = (root / "workspace").resolve()
    data_dir = (root / "agent-data").resolve()
    config = (root / "agent.toml").resolve()
    workspace.mkdir()
    data_dir.mkdir()
    (workspace / "AGENTS.md").write_text("isolated display conversation fixture\n", encoding="utf-8")
    (workspace / "fixture.txt").write_text("tool-result-unique\n", encoding="utf-8")
    write_config(config, data_dir, workspace, port)
    return workspace, data_dir, config


def run_fixed_conversation(tui: pathlib.Path, agent: pathlib.Path, root: pathlib.Path,
                           server: ModelServer, output: pathlib.Path) -> dict[str, Any]:
    workspace, _data_dir, config = create_fixture(root, server.port)
    first = PtySession(tui, agent, config, workspace, output / "first.pty.raw", False)
    try:
        first.start()
        first.wait_for(["Create session"])
        first.send(b"\t\t\t\t\t\r")
        first.wait_for(["ctx ?", "ready"])

        first.send((MARKERS["ascii_user"] + "\r").encode())
        first.wait_for([MARKERS["ascii_user"], "assistant-ascii-"])
        streaming_flow = first.screen.flow()
        if "assistant-ascii-unique" in streaming_flow:
            raise RuntimeError("ASCII screen assertion observed only completed, not streaming state")
        save_screen(output / "streaming-ascii.screen.txt", first.screen)
        first.wait_for([MARKERS["ascii_assistant"], "ready"])

        first.send((MARKERS["cjk_user"] + "\r").encode())
        first.wait_for([MARKERS["cjk_assistant"], "ready"])

        first.send((MARKERS["tool_user"] + "\r").encode())
        first.wait_for([MARKERS["tool_before"]])
        first.wait_for([MARKERS["tool_after"], MARKERS["tool_result"], "ready"], 20)
        # Agent completion and the TUI ready footer can precede the final
        # authoritative history replacement by a few reducer passes.
        for _ in range(100):
            first.read_once(0.05)
        final_assertions = assert_final_screen(first.screen)
        save_screen(output / "final.screen.txt", first.screen)
        first_status = first.shutdown()
        if first_status != 0:
            raise RuntimeError(f"fixed first TUI exited {first_status}")
    except Exception:
        first.abort()
        raise

    resumed = PtySession(tui, agent, config, workspace, output / "continue.pty.raw", True)
    try:
        resumed.start()
        resumed.wait_for([MARKERS["tool_after"], MARKERS["tool_result"], "ready"], 20)
        for _ in range(100):
            resumed.read_once(0.05)
        continue_assertions = assert_final_screen(resumed.screen)
        save_screen(output / "continue.screen.txt", resumed.screen)
        continue_status = resumed.shutdown()
        if continue_status != 0:
            raise RuntimeError(f"fixed --continue TUI exited {continue_status}")
    except Exception:
        resumed.abort()
        raise

    if server.state.count != 4:
        raise RuntimeError(f"expected four model requests, observed {server.state.count}")
    (output / "loopback.requests.jsonl").write_text(
        "".join(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n"
                for record in server.state.requests),
        encoding="utf-8",
    )
    return {
        "first_exit": first_status,
        "continue_exit": continue_status,
        "requests": server.state.requests,
        "streaming_ascii_marker": "assistant-ascii-" in streaming_flow,
        "final": final_assertions,
        "continue": continue_assertions,
    }


def run_old_negative(tui: pathlib.Path, agent: pathlib.Path, root: pathlib.Path,
                     server: ModelServer, output: pathlib.Path) -> dict[str, Any]:
    workspace, _data_dir, config = create_fixture(root, server.port)
    session = PtySession(tui, agent, config, workspace, output / "old.pty.raw", False)
    try:
        session.start()
        session.wait_for(["Create session"])
        session.send(b"\t\t\t\t\t\r")
        session.wait_for(["ctx ?", "ready"])
        session.send((MARKERS["ascii_user"] + "\r").encode())
        session.wait_for([MARKERS["ascii_assistant"], "ready"])
        session.send((MARKERS["cjk_user"] + "\r").encode())
        session.wait_for([MARKERS["cjk_assistant"], "ready"])
        session.send((MARKERS["tool_user"] + "\r").encode())
        session.wait_for([MARKERS["tool_before"]])
        time.sleep(3.0)
        for _ in range(80):
            session.read_once(0.025)
        flow = session.screen.flow()
        save_screen(output / "old-final.screen.txt", session.screen)
        missing = MARKERS["tool_after"] not in flow
        status = session.shutdown()
        if status != 0:
            raise RuntimeError(f"old binary exited {status}")
    except Exception:
        session.abort()
        raise
    if not missing:
        raise RuntimeError("old binary unexpectedly rendered the assistant marker")
    return {
        "binary": str(tui),
        "binary_sha256": sha256(tui),
        "exit": status,
        "expected_missing_marker": MARKERS["tool_after"],
        "missing": missing,
        "alt_enters": session.screen.alt_enters,
        "alt_leaves": session.screen.alt_leaves,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tui-bin", type=pathlib.Path, required=True)
    parser.add_argument("--old-tui-bin", type=pathlib.Path, required=True)
    parser.add_argument("--agent-bin", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    for path in (args.tui_bin, args.old_tui_bin, args.agent_bin):
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"executable does not exist or is not executable: {path}")
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error(f"output directory is not empty: {output}")
    output.mkdir(parents=True)
    fixed_dir = output / "fixed"
    old_dir = output / "old-negative"
    fixed_dir.mkdir()
    old_dir.mkdir()
    temp_root = pathlib.Path(tempfile.mkdtemp(prefix="minicore-display-conversation-"))
    try:
        fixed_server = ModelServer(fixed_dir)
        fixed_server.start()
        try:
            fixed = run_fixed_conversation(args.tui_bin, args.agent_bin, temp_root / "fixed", fixed_server, fixed_dir)
        finally:
            fixed_server.close()
        old_server = ModelServer(old_dir)
        old_server.start()
        try:
            old = run_old_negative(args.old_tui_bin, args.agent_bin, temp_root / "old", old_server, old_dir)
        finally:
            old_server.close()
        provenance = {
            "schema": "minicore-display-conversation-v1",
            "tui": {"path": str(args.tui_bin), "sha256": sha256(args.tui_bin)},
            "old_tui": {"path": str(args.old_tui_bin), "sha256": sha256(args.old_tui_bin)},
            "agent": {"path": str(args.agent_bin), "sha256": sha256(args.agent_bin)},
            "parser": {"library": "pyte.ByteStream", "alternate_screen": "explicit 1047/1049 buffers", "columns": WIDTH, "lines": HEIGHT},
            "fixture": {"workspace_and_data_are_temporary": True, "provider": "loopback HTTP only", "requests_fixed": 4},
            "fixed": fixed,
            "old_negative": old,
            "artifacts": {
                "fixed_final_screen": str(fixed_dir / "final.screen.txt"),
                "fixed_continue_screen": str(fixed_dir / "continue.screen.txt"),
                "fixed_streaming_screen": str(fixed_dir / "streaming-ascii.screen.txt"),
                "old_final_screen": str(old_dir / "old-final.screen.txt"),
            },
        }
        (output / "PROVENANCE.json").write_text(json.dumps(provenance, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(json.dumps(provenance, indent=2, ensure_ascii=False))
        return 0
    finally:
        import shutil
        shutil.rmtree(temp_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
