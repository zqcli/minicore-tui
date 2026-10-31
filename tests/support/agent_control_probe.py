#!/usr/bin/env python3
"""Observe the Agent's raw stdio while the real TUI drives it, and classify why
control state never appeared.

This attaches nothing to the TUI: it runs the built-in Agent subcommand itself
against the composed config, performs a real turn through a loopback Responses
API, and records every JSON-RPC frame the Agent writes. It answers one question
that a terminal-only assertion cannot: does the Agent emit `session_snapshot`
at all, and with what contents?

Outcomes are distinguished explicitly:
  no_snapshot_event   -- the Agent never sends `session_snapshot` (producer gap)
  snapshot_no_control -- it does, but execution/pending_interaction stay null
                         while a turn is waiting (sampler gap)
  snapshot_ok         -- a snapshot carried the waiting interaction
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

KEY = "dummy-local-sentinel"
TOOL_CALL_ID = "probe-call-1"


def sse(events):
    return b"".join(
        b"data: " + json.dumps(event, separators=(",", ":")).encode() + b"\n\n"
        for event in events
    )


class Provider(ThreadingHTTPServer):
    daemon_threads = False
    allow_reuse_address = False

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.requests = []
        self.errors = []
        self.lock = threading.Lock()

    def close(self):
        self.server_close()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        try:
            self.connection.settimeout(30)
            if self.headers.get("Authorization") != "Bearer " + KEY:
                raise AssertionError("missing dummy authorization sentinel")
            length = int(self.headers.get("Content-Length", "-1"))
            request = json.loads(self.rfile.read(length))
            with self.server.lock:
                index = len(self.server.requests)
                self.server.requests.append(request)
            if index == 0:
                body = sse([
                    {
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": {
                            "type": "function_call",
                            "call_id": TOOL_CALL_ID,
                            "name": "write",
                            "arguments": json.dumps(
                                {"path": "probe.txt", "content": "probe"}
                            ),
                        },
                    },
                    {"type": "response.completed", "response": {"status": "completed"}},
                ])
            else:
                body = sse([
                    {"type": "response.output_text.delta", "delta": "PROBEDONE"},
                    {"type": "response.completed", "response": {"status": "completed"}},
                ])
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.wfile.flush()
        except Exception as error:
            with self.server.lock:
                if not self.server.errors:
                    self.server.errors.append(str(error))


def compose(home, workspace, port):
    """Write the Agent config in the Agent's own terms, as the TUI composes it."""
    data_dir = home / "agent"
    data_dir.mkdir(parents=True, exist_ok=True)
    body = f"""data_dir = "{data_dir}"
default_profile = "coding"

[profiles.coding]
model = "probemodel"
reasoning = "auto"
tools = ["write"]
approval = "ask"

[profiles.coding.system_prompt]
file = "{home / 'prompt' / 'SYSTEM_PROMPT.md'}"

[models.probemodel]
provider = "open_ai_responses"
model = "probemodel"
base_url = "http://127.0.0.1:{port}/v1"
api_key_env = "MINICORE_PROBE_SENTINEL"
physical_context_window = 64000
output_budget_tokens = 8000
safety_margin_tokens = 512
supported_reasoning = ["auto"]
supports_tools = true
"""
    path = home / "agent.toml"
    path.write_text(body, encoding="utf-8")
    return path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    # "subcommand" runs the TUI executable's built-in `agent` entry; "binary"
    # runs a standalone Agent executable.
    parser.add_argument("--entry", choices=["subcommand", "binary"], default="subcommand")
    args = parser.parse_args()
    provider = Provider()
    server = threading.Thread(target=provider.serve_forever, daemon=True)
    server.start()
    temporary = tempfile.TemporaryDirectory(prefix="minicore-probe-")
    process = None
    try:
        root = Path(temporary.name)
        home = root / "home"
        workspace = root / "workspace"
        (home / "prompt").mkdir(parents=True)
        workspace.mkdir()
        (home / "prompt" / "SYSTEM_PROMPT.md").write_text("Probe.\n", encoding="utf-8")
        env = {
            "HOME": str(home),
            "PATH": "/usr/bin:/bin",
            "MINICORE_PROBE_SENTINEL": KEY,
        }
        # The Agent reads the TUI-composed config, not the TUI's own files, so
        # produce it the same way the TUI does rather than hand-writing it.
        composed = compose(home, workspace, provider.server_port)
        argv = (
            [args.binary, "agent", "--config", str(composed), "--stdio"]
            if args.entry == "subcommand"
            else [args.binary, "--config", str(composed), "--stdio"]
        )
        process = subprocess.Popen(
            argv,
            cwd=workspace,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )

        frames = []
        snapshots = []

        def read_frames():
            for line in process.stdout:
                line = line.strip()
                if not line:
                    continue
                try:
                    frame = json.loads(line)
                except json.JSONDecodeError:
                    continue
                frames.append(frame)
                if frame.get("method") == "session_snapshot":
                    snapshots.append(frame.get("params") or {})

        reader = threading.Thread(target=read_frames, daemon=True)
        reader.start()

        def send(request_id, method, params):
            process.stdin.write(
                json.dumps(
                    {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
                )
                + "\n"
            )
            process.stdin.flush()

        send(1, "agent.ping", {})
        import time

        def wait_for(predicate, label, seconds=25):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if predicate():
                    return True
                time.sleep(0.02)
            raise AssertionError(f"probe timeout at {label}")

        wait_for(
            lambda: any(frame.get("id") == 1 and "result" in frame for frame in frames),
            "ping",
        )
        ping = next(f for f in frames if f.get("id") == 1)["result"]

        send(2, "session.create", {"profile": "coding", "workspace": str(workspace)})
        wait_for(
            lambda: any(frame.get("id") == 2 and "result" in frame for frame in frames),
            "session.create",
        )
        created = next(f for f in frames if f.get("id") == 2)["result"]
        session_id = created["session"]["session_id"]
        load = created.get("load")
        create_snapshot = created.get("snapshot")

        # v1 has no load; v2 requires it. Send what this Agent speaks so the
        # probe measures the snapshot producer rather than a params rejection.
        push = {"session_id": session_id, "mode": "follow_up", "text": "probe"}
        if ping.get("protocol_version", 1) >= 2:
            push["load"] = load
        send(3, "input.push", push)
        try:
            wait_for(lambda: provider.requests, "provider request", 8)
        except AssertionError:
            print(
                json.dumps(
                    {
                        "probe_stage": "provider_request",
                        "frames": frames[-8:],
                        "provider_errors": provider.errors,
                    },
                    sort_keys=True,
                ),
                file=sys.stderr,
            )
            raise
        # The write tool needs approval; wait for the snapshot that should carry it.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if any(
                (snapshot.get("snapshot") or {}).get("pending_interaction")
                for snapshot in snapshots
            ):
                break
            time.sleep(0.02)

        def control_observed():
            for snapshot in snapshots:
                body = snapshot.get("snapshot") or {}
                if body.get("pending_interaction"):
                    return True
            return False

        event_names = sorted(
            {f.get("method") for f in frames if f.get("method")}
        )
        if not snapshots:
            outcome = "no_snapshot_event"
        elif not control_observed():
            outcome = "snapshot_no_control"
        else:
            outcome = "snapshot_ok"

        interaction = None
        for snapshot in snapshots:
            body = snapshot.get("snapshot") or {}
            if body.get("pending_interaction"):
                interaction = body["pending_interaction"]
                break

        print(
            json.dumps(
                {
                    "outcome": outcome,
                    "protocol_version": ping.get("protocol_version"),
                    "agent_version": ping.get("version"),
                    "event_methods": event_names,
                    "frame_count": len(frames),
                    "snapshot_count": len(snapshots),
                    "create_has_snapshot": create_snapshot is not None,
                    "create_snapshot_keys": sorted(create_snapshot) if create_snapshot else [],
                    "create_snapshot_load": (create_snapshot or {}).get("load"),
                    "create_snapshot_execution": (create_snapshot or {}).get("execution"),
                    "create_snapshot_completions": (create_snapshot or {}).get("completions"),
                    "create_load_present": load is not None,
                    "snapshot_revisions": [s.get("snapshot", {}).get("revision") for s in snapshots][:8],
                    "interaction_seen": interaction is not None,
                    "interaction_has_tool_name": bool(
                        interaction and interaction.get("tool_name")
                    ),
                    "provider_errors": provider.errors,
                },
                sort_keys=True,
            )
        )
    finally:
        if process is not None:
            try:
                process.stdin.close()
            except OSError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        provider.shutdown()
        provider.close()
        temporary.cleanup()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"agent-probe: {type(error).__name__}: {error}", file=sys.stderr)
        sys.exit(1)
