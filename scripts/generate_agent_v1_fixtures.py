#!/usr/bin/env python3
"""Generate the `tests/fixtures/agent-v1/` protocol fixtures from a real
pinned minicore-agent 0.5.0 process.

The generator never talks to a real provider. It:

  * starts a loopback OpenAI-Responses mock that returns scripted SSE bodies,
  * spawns the fixed Agent binary with a synthetic temp `data_dir`/workspace,
  * drives the exact RPC methods the TUI will consume,
  * records the *result payloads only* (never prompts, tool input/output
    bodies, credentials or real paths) into JSON files.

Every captured `session.read` / `turn.result` chunk carries the real Runtime
item envelope (`{"item": <HistoryItem>, "timestamp": ...}`), not the legacy
`HistoryItemView` display DTO. Do not hand-edit the fixtures; rerun this
script against the pinned Agent.

Recorded ids (`ses_…`, `lup_…`) are opaque entropy from that Agent run. They
are not secrets and are kept verbatim so the decoder is exercised on the real
grammar. Workspace/user text is synthetic.

Usage:
    python3 scripts/generate_agent_v1_fixtures.py \
        --agent-bin /path/to/minicore-agent \
        --out tests/fixtures/agent-v1 \
        --agent-source /path/to/minicore-agent-src \
        --runtime-source /path/to/minicore-runtime-src
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

AGENT_HEAD = "061743369459299e66be97bf97d2b27352a39914"
RUNTIME_HEAD = "6cd2bdbc634437dea925495c61c7eb0be10ba171"
PROTOCOL_VERSION = 1
MOCK_KEY_ENV = "MINICORE_FIXTURE_MOCK_KEY"
MOCK_KEY_VAL = "fixture-mock-key"

# Synthetic workspace text. Never real user content.
README_TEXT = "# demo project\n\nA synthetic fixture workspace.\n"
MAIN_RS = "fn main() {\n    println!(\"hello\");\n}\n"
NEEDLE_TEXT = "let needle_one = 1;\nlet needle_two = 2;\n"
BINARY_BYTES = b"\x00\x01\x02not utf8\xff\xfe"
TOO_LARGE_BYTES = 600 * 1024


def scenario_completed(reasoning_tokens: int = 0, output_tokens: int = 12) -> dict[str, Any]:
    total = 10 + output_tokens
    return {
        "type": "response.completed",
        "response": {
            "status": "completed",
            "usage": {
                "input_tokens": 10,
                "output_tokens": output_tokens,
                "total_tokens": total,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": reasoning_tokens},
            },
        },
    }


def text_sse(text: str) -> str:
    return sse_body([{"type": "response.output_text.delta", "delta": text}, scenario_completed()])


def tool_call_sse(call_id: str, name: str, arguments: str) -> str:
    return sse_body([
        {
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
            },
        },
        scenario_completed(),
    ])


def sse_body(events: list[dict[str, Any]]) -> bytes:
    return "".join(f"data: {json.dumps(e)}\n\n" for e in events).encode()


class MockController:
    """Loopback Responses endpoint with a scripted, gated response queue."""

    def __init__(self) -> None:
        self._responses: list[tuple[bytes, threading.Event | None]] = []
        self._lock = threading.Lock()
        self.request_count = 0
        controller = self

        class Handler(BaseHTTPRequestHandler):
            server_version = "minicore-fixture-loopback/1"

            def log_message(self, *args: Any) -> None:  # noqa: N802
                return

            def do_POST(self) -> None:  # noqa: N802
                length = int(self.headers.get("Content-Length", "0"))
                self.rfile.read(length)
                with controller._lock:
                    controller.request_count += 1
                    entry = (
                        controller._responses.pop(0)
                        if controller._responses
                        else (sse_body([scenario_completed()]), None)
                    )
                body, gate = entry
                if gate is not None:
                    gate.wait(timeout=30)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def enqueue(self, body: bytes, gate: threading.Event | None = None) -> None:
        with self._lock:
            self._responses.append((body, gate))

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()


class Agent:
    """A spawned real Agent process speaking NDJSON JSON-RPC over stdio."""

    def __init__(self, agent_bin: str, config_path: Path, env: dict[str, str]) -> None:
        self.proc = subprocess.Popen(
            [agent_bin, "--config", str(config_path), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            env=env,
        )
        self._next_id = 0
        self._captured: dict[str, dict[str, Any]] = {}

    def send(self, method: str, params: dict[str, Any] | None = None) -> int:
        self._next_id += 1
        frame = {
            "jsonrpc": "2.0",
            "id": self._next_id,
            "method": method,
            "params": params or {},
        }
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()
        return self._next_id

    def read_until_response(self, rid: int, timeout: float = 30.0) -> dict[str, Any]:
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("agent stdout closed early")
            frame = json.loads(line)
            if frame.get("id") == rid:
                return frame
        raise TimeoutError(f"no response for request {rid}")

    def call(self, method: str, params: dict[str, Any] | None = None, timeout: float = 30.0) -> dict[str, Any]:
        return self.read_until_response(self.send(method, params), timeout)

    def record(self, name: str, method: str, request: dict[str, Any] | None, result: Any) -> None:
        self._captured[name] = {
            "fixture": name,
            "method": method,
            "request": request,
            "result": result,
        }

    @property
    def captured(self) -> dict[str, dict[str, Any]]:
        return self._captured

    def stop(self) -> None:
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Scenario:
    """One isolated Agent + mock + synthetic workspace/store."""

    def __init__(self, root: Path, agent_bin: str, tools: list[str], approval: str = "auto") -> None:
        self.root = root
        self.workspace = root / "workspace"
        self.data_dir = root / "agent_data"
        self.workspace.mkdir(parents=True)
        self.data_dir.mkdir(parents=True)
        self.mock = MockController()
        self.config_path = root / "agent.toml"
        self.config_path.write_text(self._config(tools, approval), encoding="utf-8")
        env = dict(os.environ)
        env[MOCK_KEY_ENV] = MOCK_KEY_VAL
        self.agent = Agent(agent_bin, self.config_path, env)

    def _config(self, tools: list[str], approval: str) -> str:
        tool_list = ", ".join(f'"{t}"' for t in tools)
        return f'''data_dir = "{self.data_dir}"
event_capacity = 64
default_profile = "coding"

[profiles.coding]
model = "deep"
reasoning = "high"
system_prompt = "synthetic fixture profile"
tools = [{tool_list}]
max_tool_rounds = 8
approval = "{approval}"

[models.deep]
provider = "open_ai_responses"
model = "deep-model"
base_url = "{self.mock.url}"
api_key_env = "{MOCK_KEY_ENV}"
physical_context_window = 32000
output_budget_tokens = 2048
safety_margin_tokens = 1000
supported_reasoning = ["auto", "disabled", "low", "medium", "high", "xhigh", "max"]
supports_tools = true
request_timeout_seconds = 30
'''

    def seed_workspace(self) -> None:
        (self.workspace / "README.md").write_text(README_TEXT, encoding="utf-8")
        (self.workspace / "src").mkdir(exist_ok=True)
        (self.workspace / "src" / "main.rs").write_text(MAIN_RS, encoding="utf-8")
        (self.workspace / "needles.txt").write_text(NEEDLE_TEXT, encoding="utf-8")
        (self.workspace / "binary.dat").write_bytes(BINARY_BYTES)
        (self.workspace / "huge.txt").write_text("x" * TOO_LARGE_BYTES, encoding="utf-8")

    def git_init(self, commit: bool = True) -> None:
        run = lambda *a: subprocess.run(a, cwd=self.workspace, check=True,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        run("git", "init", "-q")
        run("git", "config", "user.email", "fixture@example.invalid")
        run("git", "config", "user.name", "fixture")
        if commit:
            run("git", "add", "-A")
            run("git", "commit", "-qm", "fixture baseline")

    def stop(self) -> None:
        self.agent.stop()
        self.mock.stop()


def redact_path(text: str, root: Path) -> str:
    return text.replace(str(root), "<tmp>/")


def write_fixture(out: Path, entry: dict[str, Any], root: Path) -> None:
    name = entry["fixture"]
    payload = json.dumps(entry, indent=1, sort_keys=True)
    payload = redact_path(payload, root)
    (out / f"{name}.json").write_text(payload + "\n", encoding="utf-8")


def git_head(path: Path) -> str:
    result = subprocess.run(["git", "-C", str(path), "rev-parse", "HEAD"],
                            capture_output=True, text=True)
    return result.stdout.strip() or "unknown"


# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------

def capture_discovery(workdir: Path, agent_bin: str, out: Path) -> dict[str, dict[str, Any]]:
    scenario = Scenario(workdir / "discovery", agent_bin, ["read", "write"])
    scenario.seed_workspace()
    agent = scenario.agent
    captured: dict[str, dict[str, Any]] = {}

    ping = agent.call("agent.ping")
    captured["ping"] = {"fixture": "ping", "method": "agent.ping", "request": {},
                        "result": ping["result"]}
    for name, method in [("model-list", "model.list"), ("profile-list", "profile.list"),
                         ("session-list", "session.list")]:
        captured[name] = {"fixture": name, "method": method, "request": {},
                          "result": agent.call(method)["result"]}
    created = agent.call("session.create", {"workspace": str(scenario.workspace)})["result"]
    captured["session-create"] = {"fixture": "session-create", "method": "session.create",
                                  "request": {"workspace": "<workspace>"},
                                  "result": created}
    sid = created["session"]["session_id"]
    captured["session-open"] = {"fixture": "session-open", "method": "session.open",
                                "request": {"session_id": sid},
                                "result": agent.call("session.open", {"session_id": sid})["result"]}
    captured["session-state-idle"] = {"fixture": "session-state-idle", "method": "session.state",
                                      "request": {"session_id": sid},
                                      "result": agent.call("session.state", {"session_id": sid})["result"]}
    captured["session-presentation-idle"] = {
        "fixture": "session-presentation-idle", "method": "session.presentation",
        "request": {"session_id": sid},
        "result": agent.call("session.presentation", {"session_id": sid})["result"]}
    captured["session-context-idle"] = {
        "fixture": "session-context-idle", "method": "session.context",
        "request": {"session_id": sid},
        "result": agent.call("session.context", {"session_id": sid})["result"]}
    scenario.stop()
    return captured


def capture_basic_turn(workdir: Path, agent_bin: str, out: Path) -> dict[str, dict[str, Any]]:
    scenario = Scenario(workdir / "basic", agent_bin, ["read", "write"])
    scenario.seed_workspace()
    agent = scenario.agent
    captured: dict[str, dict[str, Any]] = {}

    agent.call("agent.ping")
    created = agent.call("session.create", {"workspace": str(scenario.workspace)})["result"]
    sid = created["session"]["session_id"]
    agent.call("session.open", {"session_id": sid})

    # Gate the first model response so turn.result observes availability=pending.
    gate = threading.Event()
    scenario.mock.enqueue(text_sse("hello from the fixture loopback"), gate)
    sent = agent.call("turn.send", {"session_id": sid, "text": "say hello"})["result"]
    turn = sent["turn"]
    captured["turn-send"] = {"fixture": "turn-send", "method": "turn.send",
                             "request": {"session_id": sid, "text": "<synthetic>"},
                             "result": sent}
    pending = agent.call("turn.result", {
        "session_id": turn["session_id"], "loop_id": turn["loop_id"],
        "cursor": {"item": 0, "offset": 0}, "limit": 20, "max_bytes": 262144,
    })["result"]
    captured["turn-result-pending"] = {
        "fixture": "turn-result-pending", "method": "turn.result",
        "request": {"session_id": turn["session_id"], "loop_id": turn["loop_id"],
                    "cursor": {"item": 0, "offset": 0}, "limit": 20, "max_bytes": 262144},
        "result": pending}
    captured["session-state-running-preparing"] = {
        "fixture": "session-state-running", "method": "session.state",
        "request": {"session_id": sid},
        "result": agent.call("session.state", {"session_id": sid})["result"]}
    gate.set()
    wait = agent.call("turn.wait", {"session_id": turn["session_id"], "loop_id": turn["loop_id"]})["result"]
    captured["turn-wait"] = {"fixture": "turn-wait", "method": "turn.wait",
                             "request": {"session_id": turn["session_id"], "loop_id": turn["loop_id"]},
                             "result": wait}
    time.sleep(0.3)
    # A live report is preferred over the stored record. Close then reopen so the
    # same turn is served from history.jsonl with availability=stored.
    agent.call("session.close", {"session_id": sid})
    agent.call("session.open", {"session_id": sid})
    captured["turn-result-stored"] = {
        "fixture": "turn-result-stored", "method": "turn.result",
        "request": {"session_id": turn["session_id"], "loop_id": turn["loop_id"],
                    "cursor": {"item": 0, "offset": 0}, "limit": 20, "max_bytes": 262144},
        "result": agent.call("turn.result", {
            "session_id": turn["session_id"], "loop_id": turn["loop_id"],
            "cursor": {"item": 0, "offset": 0}, "limit": 20, "max_bytes": 262144})["result"]}
    read = agent.call("session.read", {
        "session_id": sid, "cursor": {"item": 0, "offset": 0},
        "limit": 20, "max_bytes": 262144})["result"]
    captured["session-read-first-page"] = {
        "fixture": "session-read-first-page", "method": "session.read",
        "request": {"session_id": sid, "cursor": {"item": 0, "offset": 0},
                    "limit": 20, "max_bytes": 262144},
        "result": read}
    scenario.stop()
    return captured


def capture_read_chunks(workdir: Path, agent_bin: str, out: Path) -> dict[str, dict[str, Any]]:
    """Long assistant text so one item spans pages and reconstructs exactly."""
    scenario = Scenario(workdir / "chunks", agent_bin, ["read"])
    scenario.seed_workspace()
    agent = scenario.agent
    captured: dict[str, dict[str, Any]] = {}
    long_text = ("alpha βeta 🙂 gamma\n" * 400).strip()

    agent.call("agent.ping")
    sid = agent.call("session.create", {"workspace": str(scenario.workspace)})["result"]["session"]["session_id"]
    agent.call("session.open", {"session_id": sid})
    scenario.mock.enqueue(text_sse(long_text))
    turn = agent.call("turn.send", {"session_id": sid, "text": "long answer"})["result"]["turn"]
    agent.call("turn.wait", {"session_id": sid, "loop_id": turn["loop_id"]})
    time.sleep(0.3)

    # A 4 KiB page budget forces the big assistant item to span several chunks.
    pages: list[dict[str, Any]] = []
    cursor: dict[str, Any] | None = {"item": 0, "offset": 0}
    captured_end: int | None = None
    revision: str | None = None
    for _ in range(64):
        params: dict[str, Any] = {"session_id": sid, "cursor": cursor, "limit": 20,
                                  "max_bytes": 4096}
        if captured_end is not None:
            params["captured_end"] = captured_end
            params["history_revision"] = revision
        page = agent.call("session.read", params)["result"]
        pages.append(page)
        captured_end = page["captured_end"]
        revision = page["history_revision"]
        cursor = page["next_cursor"]
        if cursor is None:
            break
    captured["session-read-paged-chunks"] = {
        "fixture": "session-read-paged-chunks", "method": "session.read",
        "request": {"session_id": sid, "cursor": {"item": 0, "offset": 0},
                    "limit": 20, "max_bytes": 4096},
        "pages": pages}
    # A single huge item with a mid-item continuation cursor.
    captured["session-read-continuation-cursor"] = {
        "fixture": "session-read-continuation-cursor", "method": "session.read",
        "request": {"session_id": sid, "max_bytes": 4096},
        "cursor_offsets": [
            {"index": c["index"], "offset": c["offset"], "total_bytes": c["total_bytes"],
             "complete": c["complete"], "data_bytes": len(c["data"])}
            for page in pages for c in page["items"]
        ]}
    # Trailing incomplete JSONL tail: append a half line to the store, reopen.
    agent.call("session.close", {"session_id": sid})
    scenario.stop()
    history = scenario.data_dir / "sessions" / sid / "history.jsonl"
    with history.open("ab") as handle:
        handle.write(b'{"loop_id":"lup_truncated","outcome":{"type":"comp')
    scenario2 = Scenario(workdir / "chunks-tail", agent_bin, ["read"])
    shutil.rmtree(scenario2.workspace)
    shutil.rmtree(scenario2.data_dir)
    shutil.copytree(scenario.workspace, scenario2.workspace)
    shutil.copytree(scenario.data_dir, scenario2.data_dir)
    scenario2.config_path.write_text(scenario2._config(["read"], "auto").replace(
        str(scenario2.data_dir), str(scenario2.data_dir)), encoding="utf-8")
    agent2 = scenario2.agent
    agent2.call("agent.ping")
    tail = agent2.call("session.read", {"session_id": sid, "cursor": {"item": 0, "offset": 0},
                                        "limit": 20, "max_bytes": 262144})["result"]
    captured["session-read-trailing-incomplete"] = {
        "fixture": "session-read-trailing-incomplete", "method": "session.read",
        "request": {"session_id": sid, "cursor": {"item": 0, "offset": 0},
                    "limit": 20, "max_bytes": 262144},
        "result": tail}
    scenario2.stop()
    return captured


def capture_tool_facts(workdir: Path, agent_bin: str, out: Path) -> dict[str, dict[str, Any]]:
    scenario = Scenario(workdir / "tools", agent_bin, ["read", "bash"])
    scenario.seed_workspace()
    agent = scenario.agent
    captured: dict[str, dict[str, Any]] = {}

    agent.call("agent.ping")
    sid = agent.call("session.create", {"workspace": str(scenario.workspace)})["result"]["session"]["session_id"]
    agent.call("session.open", {"session_id": sid})

    # 1. Running bash: gate the follow-up so the command stays observable.
    gate = threading.Event()
    scenario.mock.enqueue(tool_call_sse(
        "call_fixture_bash", "bash",
        json.dumps({"command": "sleep 2; printf out; printf err >&2"})))
    turn = agent.call("turn.send", {"session_id": sid, "text": "run a command"})["result"]["turn"]
    time.sleep(1.0)  # let the command start
    ref = {"session_id": sid, "loop_id": turn["loop_id"], "request_index": 0,
           "tool_call_id": "call_fixture_bash"}
    captured["tool-read-running"] = {
        "fixture": "tool-read-running", "method": "tool.read",
        "request": dict(ref, max_bytes=262144),
        "result": agent.call("tool.read", dict(ref, max_bytes=262144))["result"]}
    captured["tool-output-stdout-pending"] = {
        "fixture": "tool-output-stdout-pending", "method": "tool.output",
        "request": dict(ref, stream="stdout", offset=0, max_bytes=65536),
        "result": agent.call("tool.output", dict(ref, stream="stdout", offset=0, max_bytes=65536))["result"]}
    scenario.mock.enqueue(text_sse("command finished"))
    gate.set()
    agent.call("turn.wait", {"session_id": sid, "loop_id": turn["loop_id"]})
    time.sleep(0.3)
    captured["tool-read-terminal"] = {
        "fixture": "tool-read-terminal", "method": "tool.read",
        "request": dict(ref, max_bytes=262144),
        "result": agent.call("tool.read", dict(ref, max_bytes=262144))["result"]}
    for stream in ["input", "output", "stdout", "stderr"]:
        captured[f"tool-output-{stream}"] = {
            "fixture": f"tool-output-{stream}", "method": "tool.output",
            "request": dict(ref, stream=stream, offset=0, max_bytes=65536),
            "result": agent.call("tool.output", dict(ref, stream=stream, offset=0, max_bytes=65536))["result"]}

    # 2. awaiting_policy: approval=ask, a write call stays before execution.
    scenario2 = Scenario(workdir / "tools-policy", agent_bin, ["write"], approval="ask")
    scenario2.seed_workspace()
    agent2 = scenario2.agent
    agent2.call("agent.ping")
    sid2 = agent2.call("session.create", {"workspace": str(scenario2.workspace)})["result"]["session"]["session_id"]
    agent2.call("session.open", {"session_id": sid2})
    scenario2.mock.enqueue(tool_call_sse(
        "call_fixture_write", "write",
        json.dumps({"path": "note.txt", "content": "synthetic"})))
    turn2 = agent2.call("turn.send", {"session_id": sid2, "text": "write a file"})["result"]["turn"]
    time.sleep(1.0)
    ref2 = {"session_id": sid2, "loop_id": turn2["loop_id"], "request_index": 0,
            "tool_call_id": "call_fixture_write"}
    captured["tool-read-awaiting-policy"] = {
        "fixture": "tool-read-awaiting-policy", "method": "tool.read",
        "request": dict(ref2, max_bytes=262144),
        "result": agent2.call("tool.read", dict(ref2, max_bytes=262144))["result"]}
    captured["session-state-waiting-for-input"] = {
        "fixture": "session-state-waiting-for-input", "method": "session.state",
        "request": {"session_id": sid2},
        "result": agent2.call("session.state", {"session_id": sid2})["result"]}
    # Interaction requested event is emitted before the tool runs.
    scenario2.agent.call("turn.cancel", {"session_id": sid2, "loop_id": turn2["loop_id"]})
    time.sleep(0.3)
    scenario2.stop()
    scenario.stop()
    return captured


def capture_workspace(workdir: Path, agent_bin: str, out: Path) -> dict[str, dict[str, Any]]:
    scenario = Scenario(workdir / "workspace", agent_bin, ["read"])
    scenario.seed_workspace()
    scenario.git_init()
    # One tracked modification for changes.list/diff.
    (scenario.workspace / "src" / "main.rs").write_text(MAIN_RS.replace("hello", "changed"), encoding="utf-8")
    agent = scenario.agent
    captured: dict[str, dict[str, Any]] = {}

    agent.call("agent.ping")
    sid = agent.call("session.create", {"workspace": str(scenario.workspace)})["result"]["session"]["session_id"]
    agent.call("session.open", {"session_id": sid})

    ok = agent.call("workspace.read", {"session_id": sid, "path": "src/main.rs",
                                       "start_line": 1, "line_byte_offset": 0,
                                       "max_lines": 400, "max_bytes": 65536,
                                       "if_revision": None})["result"]
    captured["workspace-read-ok"] = {
        "fixture": "workspace-read-ok", "method": "workspace.read",
        "request": {"session_id": sid, "path": "src/main.rs"},
        "result": ok}
    captured["workspace-read-binary"] = {
        "fixture": "workspace-read-binary", "method": "workspace.read",
        "request": {"session_id": sid, "path": "binary.dat"},
        "result": agent.call("workspace.read", {"session_id": sid, "path": "binary.dat",
                                                "start_line": 1, "line_byte_offset": 0,
                                                "max_lines": 400, "max_bytes": 65536,
                                                "if_revision": None})["result"]}
    captured["workspace-read-too-large"] = {
        "fixture": "workspace-read-too-large", "method": "workspace.read",
        "request": {"session_id": sid, "path": "huge.txt"},
        "result": agent.call("workspace.read", {"session_id": sid, "path": "huge.txt",
                                                "start_line": 1, "line_byte_offset": 0,
                                                "max_lines": 400, "max_bytes": 65536,
                                                "if_revision": None})["result"]}
    # line-partial: a small encoded budget cuts inside the requested range.
    partial = agent.call("workspace.read", {"session_id": sid, "path": "needles.txt",
                                            "start_line": 1, "line_byte_offset": 0,
                                            "max_lines": 400, "max_bytes": 1024,
                                            "if_revision": None})["result"]
    captured["workspace-read-line-partial"] = {
        "fixture": "workspace-read-line-partial", "method": "workspace.read",
        "request": {"session_id": sid, "path": "needles.txt", "max_bytes": 1024},
        "result": partial}
    # changed: continue with the revision of a *different* file version.
    captured["workspace-read-changed"] = {
        "fixture": "workspace-read-changed", "method": "workspace.read",
        "request": {"session_id": sid, "path": "src/main.rs", "if_revision": "0" * 64},
        "result": agent.call("workspace.read", {
            "session_id": sid, "path": "src/main.rs", "start_line": 1,
            "line_byte_offset": 0, "max_lines": 400, "max_bytes": 65536,
            "if_revision": "0" * 64})["result"]}
    captured["workspace-files"] = {
        "fixture": "workspace-files", "method": "workspace.files",
        "request": {"session_id": sid, "directory": "", "recursive": True},
        "result": agent.call("workspace.files", {"session_id": sid, "directory": "",
                                                 "recursive": True, "query": None,
                                                 "limit": 100, "max_bytes": 65536})["result"]}
    captured["workspace-files-paged"] = {
        "fixture": "workspace-files-paged", "method": "workspace.files",
        "request": {"session_id": sid, "directory": "", "recursive": True, "limit": 1},
        "result": agent.call("workspace.files", {"session_id": sid, "directory": "",
                                                 "recursive": True, "query": None,
                                                 "limit": 1, "max_bytes": 65536})["result"]}
    captured["workspace-search"] = {
        "fixture": "workspace-search", "method": "workspace.search",
        "request": {"session_id": sid, "query": "needle"},
        "result": agent.call("workspace.search", {"session_id": sid, "query": "needle",
                                                  "paths": None, "case_sensitive": False,
                                                  "cursor": None, "max_matches": 100,
                                                  "max_bytes": 65536})["result"]}
    captured["workspace-status"] = {
        "fixture": "workspace-status", "method": "workspace.status",
        "request": {"session_id": sid},
        "result": agent.call("workspace.status", {"session_id": sid, "max_bytes": 65536})["result"]}
    changes = agent.call("changes.list", {"session_id": sid, "scope": "workspace",
                                          "cursor": None, "limit": 100,
                                          "max_bytes": 65536})["result"]
    captured["changes-list-workspace"] = {
        "fixture": "changes-list-workspace", "method": "changes.list",
        "request": {"session_id": sid, "scope": "workspace"},
        "result": changes}
    if changes["records"]:
        ref = changes["records"][0]["change_ref"]
        captured["changes-diff-workspace"] = {
            "fixture": "changes-diff-workspace", "method": "changes.diff",
            "request": {"session_id": sid, "change_ref": "<opaque>"},
            "result": agent.call("changes.diff", {"session_id": sid, "change_ref": ref,
                                                  "comparison": None, "context_lines": 3,
                                                  "cursor": None, "max_bytes": 65536})["result"]}
        # fragments: a very small budget forces hunk line continuation.
        captured["changes-diff-fragments"] = {
            "fixture": "changes-diff-fragments", "method": "changes.diff",
            "request": {"session_id": sid, "change_ref": "<opaque>", "max_bytes": 2048},
            "result": agent.call("changes.diff", {"session_id": sid, "change_ref": ref,
                                                  "comparison": None, "context_lines": 3,
                                                  "cursor": None, "max_bytes": 2048})["result"]}
    agent.call("session.close", {"session_id": sid})

    # detached HEAD + non-git unavailable status.
    subprocess.run(["git", "-c", "user.email=f@x", "-c", "user.name=f", "add", "-A"],
                   cwd=scenario.workspace, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["git", "-c", "user.email=f@x", "-c", "user.name=f", "commit", "-qm", "second"],
                   cwd=scenario.workspace, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["git", "checkout", "-q", "--detach"], cwd=scenario.workspace, check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    sid_detached = agent.call("session.create", {"workspace": str(scenario.workspace)})["result"]["session"]["session_id"]
    agent.call("session.open", {"session_id": sid_detached})
    captured["workspace-status-detached"] = {
        "fixture": "workspace-status-detached", "method": "workspace.status",
        "request": {"session_id": sid_detached},
        "result": agent.call("workspace.status", {"session_id": sid_detached})["result"]}
    agent.call("session.close", {"session_id": sid_detached})

    # Non-git workspace: repo_available=false.
    nongit = workdir / "workspace-nongit"
    nongit.mkdir(parents=True, exist_ok=True)
    (nongit / "plain.txt").write_text("plain\n", encoding="utf-8")
    sid_nongit = agent.call("session.create", {"workspace": str(nongit)})["result"]["session"]["session_id"]
    agent.call("session.open", {"session_id": sid_nongit})
    captured["workspace-status-unavailable"] = {
        "fixture": "workspace-status-unavailable", "method": "workspace.status",
        "request": {"session_id": sid_nongit},
        "result": agent.call("workspace.status", {"session_id": sid_nongit})["result"]}
    agent.call("session.close", {"session_id": sid_nongit})
    scenario.stop()
    return captured


def write_manifest(out: Path, agent_bin: str, agent_head: str, runtime_head: str,
                   captured: dict[str, dict[str, Any]], missing: list[dict[str, str]]) -> None:
    binary_sha = hashlib.sha256(Path(agent_bin).read_bytes()).hexdigest()
    manifest = {
        "manifest_version": 1,
        "description": (
            "Desensitized protocol fixtures captured from a real pinned "
            "minicore-agent process driven over stdio JSON-RPC against a "
            "loopback OpenAI-Responses mock. No real provider, credential, "
            "user text, tool output body, or real workspace path is included."
        ),
        "agent": {"head": agent_head, "package_version": "0.5.0"},
        "runtime": {"head": runtime_head, "package_version": "0.4.1"},
        "protocol_version": PROTOCOL_VERSION,
        "generator": "scripts/generate_agent_v1_fixtures.py",
        "generator_agent_binary_sha256": binary_sha,
        "capabilities": [
            "session.read", "session.context", "turn.result", "tool.read",
            "tool.output", "session.history", "workspace.read", "workspace.files",
            "workspace.search", "workspace.status", "changes.list", "changes.diff",
            "deferred.waiter_limit",
        ],
        "redaction": {
            "workspace_paths": "absolute temp roots replaced with <tmp>",
            "prompt_text": "replaced with <synthetic> in recorded requests",
            "tool_bodies": "tool.output stdout/stderr kept only as the real base64 wire page",
            "store": "synthetic temp data_dir created by the generator, never a user store",
            "ids": "ses_/lup_/call_ ids are opaque entropy from the captured run",
        },
        "fixtures": sorted(captured.keys()),
        "not_reproducible_against_the_real_process": missing,
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n",
                                       encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent-bin", required=True)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--agent-source", type=Path)
    parser.add_argument("--runtime-source", type=Path)
    parser.add_argument("--workdir", type=Path)
    args = parser.parse_args()

    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)
    workdir = args.workdir or Path(tempfile.mkdtemp(prefix="mctui-agent-v1-"))
    workdir.mkdir(parents=True, exist_ok=True)

    captured: dict[str, dict[str, Any]] = {}
    for capture in (capture_discovery, capture_basic_turn, capture_read_chunks,
                    capture_tool_facts, capture_workspace):
        result = capture(workdir, args.agent_bin, out)
        for name, entry in result.items():
            captured[name] = entry
            write_fixture(out, entry, workdir)

    agent_head = git_head(args.agent_source) if args.agent_source else AGENT_HEAD
    runtime_head = git_head(args.runtime_source) if args.runtime_source else RUNTIME_HEAD

    missing = [
        {"fixture": "session-state-preparing", "reason":
            "automatic preparation completes too fast against the loopback mock to "
            "observe reliably; synthesize deterministically in stage B unit tests"},
        {"fixture": "session-state-compaction", "reason":
            "manual compaction requires a real utility-generation round trip; the "
            "compacted/noop/failed/unknown_write results need fault injection"},
        {"fixture": "session-state-blocked", "reason":
            "blocked requires a store append failure, which needs a fault-injected "
            "store rather than the real process"},
        {"fixture": "tool-output-gap-expired-partial", "reason":
            "stream eviction/gap/partial require the 1 MiB tail window to overflow or "
            "the owner to stop mid-stream; reproduce with a bounded tail + fault "
            "injection in stage B stream tests"},
        {"fixture": "tool-output-clean-empty-eof", "reason":
            "a genuinely empty captured stdout page is indistinguishable from an "
            "unobserved one over a single run; stage B tests the decoder directly"},
        {"fixture": "workspace-files-search-deadline", "reason":
            "the 10s scan deadline is not reachable on a small synthetic tree"},
        {"fixture": "changes-stale-partial", "reason":
            "stale cursors need a live concurrent observation change; stage B "
            "synthesizes the cursor response deterministically"},
        {"fixture": "records-truncated", "reason":
            "requires >64 stored turn summaries in one page; stage B synthesizes"},
    ]
    write_manifest(out, args.agent_bin, agent_head, runtime_head, captured, missing)
    print(f"wrote {len(captured)} fixtures + manifest.json to {out}")
    print(f"workdir: {workdir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
