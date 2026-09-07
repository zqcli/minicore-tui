#!/usr/bin/env python3
"""Stage 7 terminal-emulator capture: real TUI/Agent/loopback through a real
PTY, rendered by a real xterm.js terminal in headless Edge (Playwright).

Unlike the iTerm-based `stage7_pty.py`, this harness does not depend on a
physical GUI or screen-capture permission. The child PTY bytes are genuine raw
output from the running TUI/Agent pair; they are streamed to xterm.js over a
local WebSocket, and xterm.js (a real terminal emulator) renders the terminal
and reports its buffer. Screenshots are element captures of that rendered
terminal, and assertions run against the emulator's decoded buffer, not
against hardcoded pixels. Input events (keys and a mouse click) are delivered
through the emulator/PTY path.

Artifacts: four PNG screenshots, the raw PTY byte stream, a portable cast
JSONL, an input log, the loopback request log, and a machine PROVENANCE.json.

All tool calls run in an isolated temporary workspace. The model is the
deterministic loopback mock (labeled as such); the Agent and its tools are
real local binaries.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import fcntl
import hashlib
import json
import os
import pathlib
import pty
import queue
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
from typing import Any, BinaryIO

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
XTERM_TOOL_DIR = REPO_ROOT / "tools" / "stage7-xtermjs"

# Reuse the proven session/loopback helpers from the iTerm variant.
sys.path.insert(0, str(SCRIPT_DIR))
from stage7_pty import (  # noqa: E402
    AGENT_REV,
    HarnessError,
    PtyCapture,
    require_file,
    run_text,
    send,
    set_winsize,
    sha256,
    start_loopback,
    terminate_child,
    wait_child,
    write_config,
)


def git_metadata_compact(root: pathlib.Path) -> dict[str, Any]:
    """Like stage7_pty.git_metadata but never embeds the full `git status`
    listing. Dirty agent checkouts can have thousands of changed files, which
    bloated PROVENANCE.json past 500 KB; keep the machine-verifiable facts
    (head, dirty flag, counts) and a 25-line sample of the status."""
    head = run_text(["git", "-C", str(root), "rev-parse", "HEAD"])
    status = run_text(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=all"])
    lines = status.splitlines()
    counts = {"modified": 0, "untracked": 0, "staged": 0, "deleted": 0, "renamed": 0, "others": 0}
    for line in lines:
        if line.startswith("??"):
            counts["untracked"] += 1
            continue
        index, worktree = line[:1], line[1:2]
        if index != " ":
            counts["staged" if index != "D" else "deleted"] += 1
        if worktree != " ":
            counts["deleted" if worktree == "D" else "modified"] += 1
        if "R" in (index, worktree):
            counts["renamed"] += 1
    return {
        "root": str(root),
        "head": head,
        "dirty": bool(lines),
        "status_lines": len(lines),
        "counts": counts,
        "sample": lines[:25],
    }


TUI_REV = "2b8268dbba81c162b30e984b9b31a58ebc3bba65"
RAIL_REV = "1d0dd1611a4d9546c64fe9f5b5c966253fb88eba"
PI_VERSION = "0.84.4"
XTERM_VERSION = "5.5.0"
PLAYWRIGHT_VERSION = "1.63.0"

INITIAL_SIZE = (80, 24)
MID_SIZE = (120, 40)
NARROW_SIZE = (62, 18)
SCREENSHOT_NAMES = (
    "01-mixed-loop-running-80x24.png",
    "02-same-loop-completed-80x24.png",
    "03-card-expanded-anchor-80x24.png",
    "04-multiline-editor-footer-80x24.png",
    "05-same-loop-completed-120x40.png",
    "06-multiline-editor-footer-120x40.png",
    "07-narrow-final.png",
)

CONTENT_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json; charset=utf-8",
}


# ---------------------------------------------------------------------------
# Minimal RFC 6455 WebSocket server + static file serving (stdlib only)
# ---------------------------------------------------------------------------

def ws_accept_key(key: str) -> str:
    magic = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
    return base64.b64encode(hashlib.sha1(key.encode() + magic).digest()).decode()


class Bridge:
    """Owns WS clients, PTY emission, and control (resize)."""

    def __init__(self, master_fd: int) -> None:
        self.master_fd = master_fd
        self.clients: set[socket.socket] = set()
        self.lock = threading.Lock()
        self.read_callback = None
        # Bounded catch-up buffer: a late-connecting viewer replays the recent
        # stream before going live, so startup screens are never lost.
        self.replay_frames: list[bytes] = []
        self.replay_bytes = 0
        self.REPLAY_MAX_BYTES = 512 * 1024

    def register(self, client: socket.socket) -> None:
        with self.lock:
            self.clients.add(client)
            replay = list(self.replay_frames)
        for frame in replay:
            try:
                client.sendall(ws_encode(0x2, frame))
            except OSError:
                self.unregister(client)
                return

    def unregister(self, client: socket.socket) -> None:
        with self.lock:
            self.clients.discard(client)

    def broadcast(self, data: bytes) -> None:
        with self.lock:
            clients = list(self.clients)
            self.replay_frames.append(data)
            self.replay_bytes += len(data)
            while self.replay_bytes > self.REPLAY_MAX_BYTES:
                self.replay_bytes -= len(self.replay_frames.pop(0))
        for client in clients:
            try:
                client.sendall(ws_encode(0x2, data))
            except OSError:
                self.unregister(client)

    def handle_text(self, text: str) -> None:
        if text.startswith("{"):
            try:
                control = json.loads(text)
            except json.JSONDecodeError:
                pass
            else:
                if control.get("type") == "resize":
                    cols = int(control["cols"])
                    rows = int(control["rows"])
                    set_winsize(self.master_fd, (cols, rows))
                    return
        # Raw emulator input (keys, mouse events) -> PTY.
        send(self.master_fd, text.encode())


def encode_len(length: int) -> bytes:
    if length < 126:
        return bytes([length])
    if length < 65536:
        return b"\x7e" + struct.pack(">H", length)
    return b"\x7f" + struct.pack(">Q", length)


def ws_encode(opcode: int, payload: bytes) -> bytes:
    return bytes([0x80 | opcode]) + encode_len(len(payload)) + payload


def recv_exact(conn: socket.socket, n: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < n:
        chunk = conn.recv(n - len(chunks))
        if not chunk:
            raise OSError("connection closed")
        chunks.extend(chunk)
    return bytes(chunks)


def websocket_loop(conn: socket.socket, headers: dict[str, str], bridge: Bridge) -> None:
    key = headers.get("sec-websocket-key")
    if not key:
        conn.close()
        return
    accept = ws_accept_key(key)
    response = (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )
    conn.sendall(response.encode())
    bridge.register(conn)
    try:
        while True:
            header = recv_exact(conn, 2)
            fin = bool(header[0] & 0x80)
            opcode = header[0] & 0x0F
            masked = bool(header[1] & 0x80)
            length = header[1] & 0x7F
            if length == 126:
                length = struct.unpack(">H", recv_exact(conn, 2))[0]
            elif length == 127:
                length = struct.unpack(">Q", recv_exact(conn, 8))[0]
            payload = bytearray()
            if masked:
                mask = recv_exact(conn, 4)
                coded = recv_exact(conn, length)
                payload = bytearray(b ^ mask[i % 4] for i, b in enumerate(coded))
            else:
                payload = recv_exact(conn, length)
            if opcode == 0x8:  # close
                break
            if opcode == 0x9:  # ping
                conn.sendall(ws_encode(0xA, bytes(payload)))
                continue
            if opcode == 0xA:  # pong
                continue
            if opcode in (0x1, 0x2) and fin:
                bridge.handle_text(bytes(payload).decode("utf-8", errors="replace"))
    except OSError:
        pass
    finally:
        bridge.unregister(conn)
        try:
            conn.close()
        except OSError:
            pass


STATIC_FILES = {
    "/index.html": XTERM_TOOL_DIR / "page.html",
    "/vendor/xterm.js": XTERM_TOOL_DIR / "node_modules/@xterm/xterm/lib/xterm.js",
    "/vendor/xterm.css": XTERM_TOOL_DIR / "node_modules/@xterm/xterm/css/xterm.css",
}


def serve_static(conn: socket.socket, path: str, _serve_dir: pathlib.Path) -> None:
    path = path.split("?", 1)[0]
    if path == "/":
        path = "/index.html"
    template = STATIC_FILES.get(path)
    if template is None:
        body = b"not found\n"
        conn.sendall(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body)
            + body
        )
        return
    try:
        data = template.read_bytes()
    except OSError:
        body = b"not found\n"
        conn.sendall(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body)
            + body
        )
        return
    content_type = CONTENT_TYPES.get(template.suffix, "application/octet-stream")
    conn.sendall(
        (
            "HTTP/1.1 200 OK\r\n"
            f"Content-Type: {content_type}\r\n"
            f"Content-Length: {len(data)}\r\n"
            "Connection: close\r\n\r\n"
        ).encode()
        + data
    )


def serve_http(listener: socket.socket, _serve_dir: pathlib.Path, bridge: Bridge) -> None:
    while bridge_threads_alive:
        try:
            conn, _ = listener.accept()
        except OSError:
            return
        threading.Thread(
            target=handle_http_client, args=(conn, _serve_dir, bridge), daemon=True
        ).start()


def handle_http_client(
    conn: socket.socket, serve_dir: pathlib.Path, bridge: Bridge
) -> None:
    conn.settimeout(20)
    try:
        request = b""
        while b"\r\n\r\n" not in request:
            chunk = conn.recv(4096)
            if not chunk:
                return
            request += chunk
            if len(request) > 65536:
                return
        head, _, _ = request.partition(b"\r\n\r\n")
        lines = head.decode("latin1").split("\r\n")
        parts = lines[0].split(" ", 2)
        if len(parts) != 3:
            return
        method, path, _ = parts
        headers: dict[str, str] = {}
        for line in lines[1:]:
            if ":" in line:
                key, value = line.split(":", 1)
                headers[key.strip().lower()] = value.strip()
        if method == "GET" and path == "/ws":
            websocket_loop(conn, headers, bridge)
        else:
            serve_static(conn, path, serve_dir)
    except (OSError, ValueError):
        pass
    finally:
        try:
            conn.close()
        except OSError:
            pass


# ---------------------------------------------------------------------------
# Node/Playwright driver process
# ---------------------------------------------------------------------------

class Driver:
    def __init__(self, node: str, workdir: pathlib.Path) -> None:
        self.proc = subprocess.Popen(
            [node, str(workdir / "driver.mjs")],
            cwd=str(workdir),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.replies: queue.Queue[tuple[bool, dict[str, Any]]] = queue.Queue()
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()
        self.stderr_lines: list[str] = []

    def _read(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                self.stderr_lines.append(line)
                continue
            if message.get("ok"):
                self.replies.put((True, message))
            else:
                self.replies.put((False, message))

    def call(self, op: str, **params: Any) -> dict[str, Any]:
        if self.proc.poll() is not None:
            raise HarnessError(f"driver exited: {''.join(self._drain_stderr())}")
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps({"op": op, **params}) + "\n")
        self.proc.stdin.flush()
        try:
            ok, message = self.replies.get(timeout=90)
        except queue.Empty:
            raise HarnessError(f"driver timed out on op {op}; stderr: {''.join(self._drain_stderr())}")
        if not ok:
            raise HarnessError(f"driver op {op} failed: {message.get('error')}")
        return message

    def _drain_stderr(self) -> list[str]:
        if self.proc.stderr:
            try:
                return self.proc.stderr.readlines()
            except OSError:
                return []
        return []

    def close(self) -> None:
        if self.proc.poll() is None:
            try:
                self.call("close")
            except HarnessError:
                pass
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def verify(self) -> str:
        result = self.call("verify")
        return str(result.get("versions") or "")


def buffer_lines(driver: Driver) -> list[str]:
    result = driver.call("buffer")
    return result.get("text", {}).get("lines", [])


BOX_GLYPHS = "┌┐└┘─│╭╮╰╯"


def flow_text(lines: list[str]) -> str:
    """Reconstruct a terminal flow from visual rows.

    Soft-wrapped assistant text splits between words, so trimming each row and
    joining with a single space recovers the exact sentence. Every other
    character is preserved verbatim: box glyphs, punctuation, and CJK are kept
    so a stale/partial frame can never "match" by losing characters.
    """
    return " ".join(" ".join(line.split()) for line in lines)


def marker_in_lines(lines: list[str], marker: str) -> bool:
    wanted = " ".join(marker.split()).strip()
    if not wanted:
        return False
    return wanted in flow_text(lines)


def assert_clean_flow(lines: list[str], marker: str) -> None:
    """The marker's exact sentence must appear in the flow with no box glyphs
    in the rows that carry it (a capture artifact or mid-render frame leaves a
    glyph and the sentence broken)."""
    flow = flow_text(lines)
    wanted = " ".join(marker.split()).strip()
    if wanted not in flow:
        raise HarnessError(f"plain-message sentence missing: {marker!r}\nflow:\n{flow}")
    token = marker.split()[0]
    for row, line in enumerate(lines):
        if token and token in line:
            if any(glyph in line for glyph in BOX_GLYPHS):
                raise HarnessError(
                    f"box glyph inside plain message row {row}: {line!r}"
                )


def debug_buffer_state(driver: Driver) -> str:
    try:
        value = driver.call(
            "termEval",
            expr="JSON.stringify((()=>{const b=term.buffer.active;const out={type:b.type,length:b.length,baseY:b.baseY,viewportY:b.viewportY,rows:term.rows,cols:term.cols,cursorCol:b.cursorX,cursorRow:b.cursorY};out.last6=[];for(let i=0;i<6;i++){const y=(b.viewportY||0)+term.rows-6+i;out.last6.push(y+':'+(b.getLine(y)?.translateToString(true)??'U').slice(0,40));}return out;})())",
        )
        return f"\nbuffer-internals: {value.get('value', '')}"
    except HarnessError:
        return "\nbuffer-internals: (unavailable)"


def wait_buffer(
    driver: Driver, markers: list[str], timeout: float, reject: list[str] | None = None
) -> list[str]:
    deadline = time.monotonic() + timeout
    seen = ""
    while time.monotonic() < deadline:
        lines = buffer_lines(driver)
        seen = "\n".join(lines)
        if all(marker_in_lines(lines, marker) for marker in markers):
            if reject and any(marker_in_lines(lines, marker) for marker in reject):
                time.sleep(0.05)
                continue
            return lines
        time.sleep(0.1)
    raise HarnessError(
        f"emulator buffer did not contain {markers!r}; last buffer:\n{seen}"
        + "\nmatch: "
        + json.dumps({m: marker_in_lines(seen.splitlines(), m) for m in markers})
        + debug_buffer_state(driver)
    )


def row_of(driver: Driver, needle: str) -> int:
    for index, line in enumerate(buffer_lines(driver)):
        if needle in line:
            return index
    raise HarnessError(f"row not found in buffer for {needle!r}")


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------

def init_workspace_git(root: pathlib.Path, workspace: pathlib.Path) -> None:
    """Makes the isolated workspace a real git work tree on branch `dev` so the
    footer branch marker is genuine Agent data (not a fixture value)."""
    subprocess.run(["git", "init", "-b", "dev"], cwd=str(workspace), check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["git", "add", "."], cwd=str(workspace), check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    env = os.environ.copy()
    env["GIT_AUTHOR_NAME"] = "minicore-stage7"
    env["GIT_AUTHOR_EMAIL"] = "stage7@localhost"
    env["GIT_COMMITTER_NAME"] = "minicore-stage7"
    env["GIT_COMMITTER_EMAIL"] = "stage7@localhost"
    subprocess.run(
        ["git", "commit", "-m", "stage7 fixture"],
        cwd=str(workspace), check=True, env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tui-bin", type=pathlib.Path, default=REPO_ROOT / "target/debug/minicore-tui"
    )
    parser.add_argument(
        "--agent-bin",
        type=pathlib.Path,
        default=REPO_ROOT.parent / "minicore-agent/target/debug/minicore-agent",
    )
    parser.add_argument(
        "--node-bin",
        type=pathlib.Path,
        default=pathlib.Path(subprocess.run(["which", "node"], capture_output=True, text=True).stdout.strip()),
    )
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        default=REPO_ROOT / "artifacts/stage7-xtermjs" / dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ"),
    )
    parser.add_argument("--first-delay-ms", type=int, default=2600)
    parser.add_argument("--second-delay-ms", type=int, default=1400)
    return parser.parse_args()


bridge_threads_alive = True


def main() -> int:
    global bridge_threads_alive
    args = parse_args()
    tui = require_file(args.tui_bin, "TUI binary")
    agent = require_file(args.agent_bin, "Agent binary")
    node = require_file(args.node_bin, "node binary")
    loopback_script = SCRIPT_DIR / "stage7_loopback_model.py"
    if not loopback_script.is_file():
        raise HarnessError(f"loopback script is missing: {loopback_script}")
    if not (XTERM_TOOL_DIR / "driver.mjs").is_file():
        raise HarnessError(f"driver is missing: {XTERM_TOOL_DIR / 'driver.mjs'}")
    if not (XTERM_TOOL_DIR / "node_modules" / "@xterm" / "xterm" / "lib" / "xterm.js").is_file():
        raise HarnessError("pinned @xterm/xterm is missing; run npm install in tools/stage7-xtermjs")

    output = args.output.expanduser().resolve()
    if output.exists() and any(output.iterdir()):
        raise HarnessError(f"output directory is not empty: {output}")
    output.mkdir(parents=True, exist_ok=True)
    screenshots = output / "screenshots"
    screenshots.mkdir()
    raw_path = output / "tui.pty.raw"
    cast_path = output / "tui.pty.cast.jsonl"
    inputs_path = output / "inputs.jsonl"
    request_log = output / "loopback.requests.jsonl"
    port_file = output / "loopback.port"
    assertions: dict[str, Any] = {}

    child_pid: int | None = None
    loopback: subprocess.Popen[bytes] | None = None
    driver: Driver | None = None
    bridge: Bridge | None = None
    listener: socket.socket | None = None
    temp_dir: tempfile.TemporaryDirectory[str] | None = None
    completed = False
    exit_code: int | None = None
    resized = False
    pump_thread: threading.Thread | None = None
    pump_stop = threading.Event()
    try:
        loopback, port = start_loopback(
            loopback_script, port_file, request_log,
            max(0, args.first_delay_ms), max(0, args.second_delay_ms),
        )
        temp_dir = tempfile.TemporaryDirectory(prefix="minicore-stage7-xtermjs-")
        temp_root = pathlib.Path(temp_dir.name)
        workspace_path = temp_root / "workspace"
        data_dir = temp_root / "agent-data"
        workspace_path.mkdir()
        data_dir.mkdir()
        (workspace_path / "AGENTS.md").write_text(
            "This workspace is an isolated Stage 7 fixture.\n", encoding="utf-8"
        )
        (workspace_path / "fixture.txt").write_text(
            "stage7 fixture line one\nstage7 fixture line two\n", encoding="utf-8"
        )
        init_workspace_git(temp_root, workspace_path)
        config = temp_root / "agent.toml"
        write_config(config, data_dir, workspace_path, port)

        env = os.environ.copy()
        env["MINICORE_STAGE7_KEY"] = "stage7-loopback-key"
        child_pid, master = pty.fork()
        if child_pid == 0:
            os.chdir(workspace_path)
            os.execve(
                str(tui),
                [
                    str(tui),
                    "--agent-bin", str(agent),
                    "--agent-config", str(config),
                    "--workspace", str(workspace_path),
                    "--theme", "dark",
                ],
                env,
            )
            raise AssertionError("execve returned")
        set_winsize(master, INITIAL_SIZE)
        resized = True

        # Bridge: static page + WS + PTY emission pump.
        bridge = Bridge(master)
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 0))
        listener.listen(8)
        bridge_port = listener.getsockname()[1]
        threading.Thread(
            target=serve_http, args=(listener, XTERM_TOOL_DIR, bridge), daemon=True
        ).start()

        start_time = time.monotonic()
        pump_stop = threading.Event()
        with raw_path.open("wb") as raw, cast_path.open("w", encoding="utf-8") as cast:
            def pump_loop() -> None:
                while not pump_stop.is_set():
                    ready, _, _ = select.select([master], [], [], 0.5)
                    if not ready:
                        continue
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        return
                    if not data:
                        return
                    raw.write(data)
                    raw.flush()
                    offset = time.monotonic() - start_time
                    cast.write(
                        json.dumps(
                            {"t": round(offset, 4), "data": data.hex()}, separators=(",", ":")
                        )
                        + "\n"
                    )
                    cast.flush()
                    bridge.broadcast(data)

            pump_thread = threading.Thread(target=pump_loop, daemon=True)
            pump_thread.start()

            def pty_input(label: str, data: bytes) -> None:
                inputs_path.parent.mkdir(parents=True, exist_ok=True)
                with inputs_path.open("a", encoding="utf-8") as inputs:
                    inputs.write(
                        json.dumps(
                            {"label": label, "t": round(time.monotonic() - start_time, 4), "data": data.hex()},
                            separators=(",", ":"),
                        )
                        + "\n"
                    )
                send(master, data)

            # Browser session.
            driver = Driver(args.node_bin, XTERM_TOOL_DIR)
            page_url = f"http://127.0.0.1:{bridge_port}/?ws=ws://127.0.0.1:{bridge_port}/ws"
            driver.call("open", url=page_url, viewport={"width": 1480, "height": 860})
            driver.call("resize", cols=INITIAL_SIZE[0], rows=INITIAL_SIZE[1])

            # Wait for the Workspace selector, then create the session.
            print("stage7: waiting for workspace selector", flush=True)
            wait_buffer(driver, ["Create session"], 20)
            print("stage7: session created", flush=True)
            pty_input("create-session", b"\t\t\t\t\t\r")
            wait_buffer(driver, ["ctx ?"], 20)

            # Checkpoint 1: mixed loop running (thinking + tool in flight).
            print("stage7: checkpoint 1 (mixed loop running)", flush=True)
            pty_input("prompt", b"inspect fixture\r")
            wait_buffer(driver, ["working", "Step one:"], 20)
            driver.call("flush")
            time.sleep(0.3)
            lines_1 = buffer_lines(driver)
            assertions["01"] = {
                "markers": ["working", "Step one:", "inspect"],
                "found": {m: marker_in_lines(lines_1, m) for m in ["working", "Step one:", "inspect"]},
            }
            screenshot_taken = {}
            driver.call("screenshot", path=str(screenshots / SCREENSHOT_NAMES[0]))
            screenshot_taken["01"] = INITIAL_SIZE
            print("stage7: checkpoint 1 done", flush=True)

            # Checkpoint 2: the same loop completes. The exact final sentence
            print("stage7: checkpoint 2 (same loop completed)", flush=True)
            # must be present across the soft wrap with no box glyph in its
            # rows (proves a clean, fully-rendered frame, not a mid-backlog
            # capture).
            FINAL_SENTENCE = (
                "The loopback fixture is present and readable. "
                "It contains two fixture lines as expected. "
                "This completes the same-loop verification."
            )
            wait_buffer(driver, ["ready", "same-loop verification", "fixture.txt"], 30)
            driver.call("flush")
            time.sleep(0.3)
            lines_2 = buffer_lines(driver)
            assert_clean_flow(lines_2, FINAL_SENTENCE)
            rendered_2 = driver.call("screenText").get("text", "")
            assert_clean_flow(rendered_2.splitlines(), FINAL_SENTENCE)
            assertions["02"] = {
                "markers": ["ready", "same-loop verification", "fixture.txt"],
                "found": {m: marker_in_lines(lines_2, m) for m in
                          ["ready", "same-loop verification", "fixture.txt"]},
                "full_final_sentence_clean": True,
                "dom_final_sentence_clean": True,
            }
            driver.call("screenshot", path=str(screenshots / SCREENSHOT_NAMES[1]))
            screenshot_taken["02"] = INITIAL_SIZE
            print("stage7: checkpoint 2 done", flush=True)

            # Checkpoint 3: single-card expand with the scroll anchor kept.
            print("stage7: checkpoint 3 (card expand anchor)", flush=True)
            driver.call("flush")
            row_before = row_of(driver, "inspect fixture")
            top_before = buffer_lines(driver)[:3]
            click_row = row_of(driver, "Step one:")
            driver.call("click", col=2, row=click_row)
            time.sleep(0.5)
            driver.call("flush")
            lines_3 = buffer_lines(driver)
            assert_markers = ["Step one:", "Step four:"]
            seen_3 = "\n".join(lines_3)
            ok_3 = all(m in seen_3 for m in assert_markers) and "earlier lines" not in seen_3
            row_after = row_of(driver, "inspect fixture")
            top_after = buffer_lines(driver)[:3]
            anchor_ok = row_before == row_after and top_before == top_after
            assertions["03"] = {
                "expanded": ok_3,
                "collapsed_hint_removed": "earlier lines" not in seen_3,
                "anchor_row_before": row_before,
                "anchor_row_after": row_after,
                "anchor_top_before": top_before,
                "anchor_top_after": top_after,
                "anchor_preserved": anchor_ok,
            }
            driver.call("screenshot", path=str(screenshots / SCREENSHOT_NAMES[2]))
            screenshot_taken["03"] = INITIAL_SIZE
            print("stage7: checkpoint 3 done", flush=True)

            # Checkpoint 4: multiline editor and a single footer row.
            print("stage7: checkpoint 4 (multiline editor + footer)", flush=True)
            pty_input("editor-line-1", b"first editor line")
            pty_input("editor-newline", b"\x0a")
            pty_input("editor-line-2", b"second editor line")
            time.sleep(0.4)
            driver.call("flush")
            lines_4 = buffer_lines(driver)
            bottom = lines_4[-2:]
            footer_only_last_row = "▸" in lines_4[-1] and (len(lines_4) < 2 or "▸" not in lines_4[-2])
            assertions["04"] = {
                "editor_has_two_lines": any("first editor line" in line for line in lines_4)
                and any("second editor line" in line for line in lines_4),
                "footer_single_row": footer_only_last_row,
                "footer_markers": {
                    m: any(m in lines_4[-1] for m in ["▸", "ctx ?", "@dev"])
                    for m in ["▸", "ctx ?", "@dev"]
                },
                "last_two_rows": bottom,
            }
            footer_ok = (
                footer_only_last_row
                and "▸" in lines_4[-1]
                and "ctx ?" in lines_4[-1]
                and "@dev" in lines_4[-1]
            )
            assertions["04"]["footer_ok"] = footer_ok
            driver.call("screenshot", path=str(screenshots / SCREENSHOT_NAMES[3]))
            screenshot_taken["04"] = INITIAL_SIZE
            print("stage7: checkpoint 4 done", flush=True)

            # Spec-size reframe: the same live session at 120x40 (TUI reflows
            print("stage7: reframe 120x40", flush=True)
            # on the real resize; the completed loop, editor, and footer must
            # stay clean).
            driver.call("resize", cols=MID_SIZE[0], rows=MID_SIZE[1])
            driver.call("flush")
            time.sleep(0.6)
            driver.call("flush")
            assert_clean_flow(buffer_lines(driver), FINAL_SENTENCE)
            rendered_5 = driver.call("screenText").get("text", "")
            assert_clean_flow(rendered_5.splitlines(), FINAL_SENTENCE)
            driver.call("screenshot", path=str(screenshots / SCREENSHOT_NAMES[4]))
            screenshot_taken["05"] = MID_SIZE
            footer_wide = buffer_lines(driver)[-1]
            if "▸" not in footer_wide:
                raise HarnessError("120x40 reframe lost the footer")
            driver.call("screenshot", path=str(screenshots / SCREENSHOT_NAMES[5]))
            screenshot_taken["06"] = MID_SIZE
            print("stage7: reframe done", flush=True)

            # Bonus narrow capture at the same instant (density check).
            print("stage7: narrow 62x18", flush=True)
            driver.call("resize", cols=NARROW_SIZE[0], rows=NARROW_SIZE[1])
            driver.call("flush")
            time.sleep(0.6)
            driver.call("flush")
            driver.call("screenshot", path=str(screenshots / SCREENSHOT_NAMES[6]))
            screenshot_taken["07"] = NARROW_SIZE
            print("stage7: narrow done; shutdown", flush=True)

            # Production shutdown path: Ctrl-C clears the leftover editor text,
            # the next arms the double-press, and a third quits (spec 22.1).
            pty_input("first-ctrl-c", b"\x03")
            time.sleep(0.3)
            pty_input("second-ctrl-c", b"\x03")
            time.sleep(0.3)
            pty_input("third-ctrl-c", b"\x03")

            pump_deadline = time.monotonic() + 3.0
            while time.monotonic() < pump_deadline and driver is not None:
                time.sleep(0.1)
            driver.call("close")
            pump_stop.set()
            if pump_thread is not None:
                pump_thread.join(timeout=3)

        exit_code = wait_child(child_pid, 8)
        if exit_code is None:
            terminate_child(child_pid)
            raise HarnessError("TUI did not exit after the clean Ctrl-C shutdown")
        child_pid = None
        if exit_code != 0:
            raise HarnessError(f"TUI exited with status {exit_code}")

        for name in SCREENSHOT_NAMES:
            screenshot = screenshots / name
            if not screenshot.is_file() or screenshot.stat().st_size == 0:
                raise HarnessError(f"missing screenshot artifact: {screenshot}")

        node_version = subprocess.run(
            [str(args.node_bin), "--version"], capture_output=True, text=True
        ).stdout.strip()

        metadata: dict[str, Any] = {
            "schema": "minicore-stage7-xtermjs-evidence-v1",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
            "command": " ".join(str(part) for part in sys.argv),
            "capture": {
                "mode": "real PTY -> xterm.js (Edge headless) element screenshots",
                "terminal_emulator": f"@xterm/xterm {XTERM_VERSION}",
                "browser_channel": "msedge",
                "playwright": PLAYWRIGHT_VERSION,
                "fonts": "Menlo / SF Mono / Consolas / Liberation Mono (14px, line-height 1.12)",
                "page_background": "#2D2A2E (xterm theme + CSS)",
                "sizes": [
                    {"shot": name, "columns": screenshot_taken[key][0], "rows": screenshot_taken[key][1]}
                    for key, name in [
                        ("01", SCREENSHOT_NAMES[0]),
                        ("02", SCREENSHOT_NAMES[1]),
                        ("03", SCREENSHOT_NAMES[2]),
                        ("04", SCREENSHOT_NAMES[3]),
                        ("05", SCREENSHOT_NAMES[4]),
                        ("06", SCREENSHOT_NAMES[5]),
                        ("07", SCREENSHOT_NAMES[6]),
                    ]
                ],
                "valid": True,
            },
            "provenance": {
                "tui": git_metadata_compact(REPO_ROOT),
                "agent": git_metadata_compact(REPO_ROOT.parent / "minicore-agent"),
                "node": node_version,
                "xterm": XTERM_VERSION,
                "playwright": PLAYWRIGHT_VERSION,
                "loopback_model": "deterministic stage7_loopback_model.py (labeled mock; no external model)",
            },
            "binaries": {
                "tui": {"path": str(tui), "sha256": sha256(tui)},
                "agent": {"path": str(agent), "sha256": sha256(agent)},
            },
            "assertions": assertions,
            "artifacts": {
                "pty_raw": {"path": str(raw_path), "bytes": raw_path.stat().st_size, "sha256": sha256(raw_path)},
                "cast": {"path": str(cast_path), "bytes": cast_path.stat().st_size, "sha256": sha256(cast_path)},
                "inputs": {"path": str(inputs_path), "bytes": inputs_path.stat().st_size, "sha256": sha256(inputs_path)},
                "loopback_requests": {
                    "path": str(request_log), "bytes": request_log.stat().st_size, "sha256": sha256(request_log),
                },
                "screenshots": [
                    {
                        "path": str(screenshots / name),
                        "bytes": (screenshots / name).stat().st_size,
                        "sha256": sha256(screenshots / name),
                    }
                    for name in sorted(p.name for p in screenshots.glob("*.png"))
                ],
            },
            "result": {
                "tui_exit": exit_code,
                "all_screenshots_captured": True,
                "footer_ok": footer_ok,
            },
        }
        if not (assertions.get("03", {}).get("anchor_preserved") and assertions["03"]["expanded"]):
            raise HarnessError("card-expand anchor assertions failed; not writing evidence as passing")
        (output / "PROVENANCE.json").write_text(
            json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        completed = True
        print(f"stage7-xtermjs evidence: {output}")
        return 0
    finally:
        pump_stop.set()
        if pump_thread is not None and pump_thread.is_alive():
            pump_thread.join(timeout=3)
        bridge_threads_alive = False
        if listener is not None:
            try:
                listener.close()
            except OSError:
                pass
        if not completed:
            import shutil
            shutil.rmtree(screenshots, ignore_errors=True)
            (output / "PROVENANCE.json").unlink(missing_ok=True)
        if driver is not None:
            driver.close()
        if child_pid is not None:
            terminate_child(child_pid)
        if loopback is not None and loopback.poll() is None:
            loopback.terminate()
            try:
                loopback.wait(timeout=3)
            except subprocess.TimeoutExpired:
                loopback.kill()
                loopback.wait()
        if temp_dir is not None:
            temp_dir.cleanup()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except HarnessError as error:
        print(f"stage7-xtermjs: {error}", file=sys.stderr)
        sys.exit(1)