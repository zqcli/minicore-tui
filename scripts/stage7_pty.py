#!/usr/bin/env python3
"""Run the real MiniCore TUI and Agent through a real PTY.

The harness uses only local binaries and a separate loopback Responses
process. It forwards the child PTY bytes to the current terminal emulator,
captures those bytes verbatim, and takes screenshots of the actual emulator
window at four deterministic checkpoints.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
import pathlib
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from typing import Any, BinaryIO


TUI_REV = "2b8268dbba81c162b30e984b9b31a58ebc3bba65"
AGENT_REV = "2d16f554796861a21a49afcd77f4eab74022bf92"
RAIL_REV = "1d0dd1611a4d9546c64fe9f5b5c966253fb88eba"
PI_VERSION = "0.84.4"
INITIAL_SIZE = (100, 30)
NARROW_SIZE = (62, 18)
SCREENSHOT_NAMES = (
    "01-ready-editor.png",
    "02-working-tool.png",
    "03-final-tool.png",
    "04-narrow-final.png",
)


class HarnessError(RuntimeError):
    pass


def run_text(command: list[str], cwd: pathlib.Path | None = None) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return result.stdout.strip()


def git_metadata(root: pathlib.Path) -> dict[str, Any]:
    try:
        head = run_text(["git", "-C", str(root), "rev-parse", "HEAD"])
        status = run_text(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=all"])
    except (OSError, subprocess.CalledProcessError) as error:
        raise HarnessError(f"cannot read git metadata for {root}: {error}") from error
    return {"root": str(root), "head": head, "dirty": bool(status), "status": status.splitlines()}


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def require_file(path: pathlib.Path, label: str) -> pathlib.Path:
    path = path.expanduser().resolve()
    if not path.is_file():
        raise HarnessError(f"{label} does not exist: {path}")
    if not os.access(path, os.X_OK):
        raise HarnessError(f"{label} is not executable: {path}")
    return path


def reference_metadata(reference_root: pathlib.Path) -> dict[str, Any]:
    reference_root = reference_root.expanduser().resolve()
    metadata = git_metadata(reference_root)
    if metadata["head"] != RAIL_REV:
        raise HarnessError(
            f"reference checkout is not pinned to Rail {RAIL_REV}: {metadata['head']}"
        )
    if metadata["dirty"]:
        raise HarnessError("reference checkout is dirty; refusing to use it")
    package = reference_root / "node_modules/@earendil-works/pi-coding-agent/package.json"
    if not package.is_file():
        raise HarnessError(f"Pi package metadata is missing: {package}")
    package_data = json.loads(package.read_text(encoding="utf-8"))
    if package_data.get("version") != PI_VERSION:
        raise HarnessError(
            f"reference Pi package is {package_data.get('version')!r}, expected {PI_VERSION}"
        )
    return {
        "rail": metadata,
        "pi": {"version": package_data["version"], "package": str(package)},
    }


def applescript(script: str) -> str:
    try:
        return run_text(["osascript", "-e", script])
    except (OSError, subprocess.CalledProcessError) as error:
        raise HarnessError(f"AppleScript failed: {error}") from error


def iterm_window_id() -> int:
    raw = os.environ.get("STAGE7_SCREEN_WINDOW_ID")
    if raw:
        try:
            return int(raw)
        except ValueError as error:
            raise HarnessError("STAGE7_SCREEN_WINDOW_ID must be an integer") from error
    for application in ("iTerm", "iTerm2"):
        try:
            return int(applescript(f'tell application "{application}" to get id of current window'))
        except HarnessError:
            continue
    raise HarnessError(
        "cannot identify the terminal-emulator window; set STAGE7_SCREEN_WINDOW_ID"
    )


def iterm_size() -> tuple[int, int] | None:
    try:
        raw = applescript(
            'tell application "iTerm" to tell current session of current window '
            "to get {columns, rows}"
        )
    except HarnessError:
        return None
    parts = [part.strip() for part in raw.split(",")]
    if len(parts) != 2:
        raise HarnessError(f"unexpected iTerm size response: {raw!r}")
    return int(parts[0]), int(parts[1])


def set_iterm_size(size: tuple[int, int]) -> None:
    columns, rows = size
    applescript(
        'tell application "iTerm" to tell current session of current window '
        f"to set columns to {columns}"
    )
    applescript(
        'tell application "iTerm" to tell current session of current window '
        f"to set rows to {rows}"
    )


def iterm_bounds() -> tuple[int, int, int, int]:
    raw = applescript('tell application "iTerm" to get bounds of current window')
    parts = [int(part.strip()) for part in raw.split(",")]
    if len(parts) != 4 or parts[2] <= 0 or parts[3] <= 0:
        raise HarnessError(f"unexpected iTerm bounds response: {raw!r}")
    return parts[0], parts[1], parts[2], parts[3]


def iterm_contents() -> str:
    try:
        return applescript(
            'tell application "iTerm" to tell current session of current window '
            "to get contents"
        ).lower()
    except HarnessError as error:
        raise HarnessError(f"cannot read iTerm screen contents: {error}") from error


def capture_window(
    window_id: int,
    path: pathlib.Path,
    ocr_script: pathlib.Path,
    markers: tuple[str, ...],
) -> None:
    # AppleScript's iTerm window id is not the CoreGraphics window number
    # accepted by screencapture -l on all iTerm builds. Capture the current
    # frontmost iTerm window by its live screen bounds instead; this remains a
    # screenshot of the actual emulator window, not a reconstructed PTY frame.
    _ = window_id
    x, y, width, height = iterm_bounds()
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        subprocess.run(
            ["/usr/sbin/screencapture", "-x", "-R", f"{x},{y},{width},{height}", str(path)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        screen = iterm_contents()
        missing = [marker for marker in markers if marker.lower() not in screen]
        if missing:
            raise HarnessError(
                "the frontmost iTerm contents do not match the checkpoint: "
                + ", ".join(missing)
            )
        subprocess.run(
            ["swift", str(ocr_script), str(path), *markers],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        path.unlink(missing_ok=True)
        detail = getattr(error, "stderr", None)
        raise HarnessError(
            f"terminal screenshot failed or did not contain TUI markers for {path}: "
            f"{detail.strip() if isinstance(detail, str) and detail.strip() else error}"
        ) from error
    except HarnessError:
        path.unlink(missing_ok=True)
        raise
    if not path.is_file() or path.stat().st_size == 0:
        raise HarnessError(f"terminal window screenshot is empty: {path}")


def set_winsize(fd: int, size: tuple[int, int]) -> None:
    columns, rows = size
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def write_config(
    path: pathlib.Path,
    data_dir: pathlib.Path,
    workspace: pathlib.Path,
    port: int,
) -> None:
    def quote(value: pathlib.Path | str) -> str:
        return json.dumps(str(value))

    path.write_text(
        "\n".join(
            [
                f"data_dir = {quote(data_dir)}",
                "event_capacity = 128",
                'default_profile = "coding"',
                "",
                "[profiles.coding]",
                'model = "deep"',
                'reasoning = "high"',
                'system_prompt = "Stage 7 local loopback verification."',
                'tools = ["read"]',
                "max_tool_rounds = 4",
                'approval = "auto"',
                "",
                "[models.deep]",
                'provider = "open_ai_responses"',
                'model = "stage7-loopback"',
                f'base_url = {quote(f"http://127.0.0.1:{port}/v1")}',
                'api_key_env = "MINICORE_STAGE7_KEY"',
                "physical_context_window = 32000",
                "output_budget_tokens = 2048",
                "safety_margin_tokens = 1000",
                'supported_reasoning = ["auto", "low", "medium", "high"]',
                "supports_tools = true",
                "request_timeout_seconds = 30",
                "",
            ]
        ),
        encoding="utf-8",
    )
    # Keep the workspace argument in the generated setup visible to a human
    # inspecting an artifact, while the Agent receives it through RPC.
    _ = workspace


def start_loopback(
    script: pathlib.Path,
    port_file: pathlib.Path,
    request_log: pathlib.Path,
    first_delay_ms: int,
    second_delay_ms: int,
) -> tuple[subprocess.Popen[bytes], int]:
    process = subprocess.Popen(
        [
            sys.executable,
            str(script),
            "--port-file",
            str(port_file),
            "--request-log",
            str(request_log),
            "--first-delay-ms",
            str(first_delay_ms),
            "--second-delay-ms",
            str(second_delay_ms),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if port_file.is_file():
            try:
                port = int(port_file.read_text(encoding="ascii"))
            except ValueError as error:
                raise HarnessError(f"loopback port file is invalid: {error}") from error
            return process, port
        if process.poll() is not None:
            stderr = process.stderr.read().decode(errors="replace") if process.stderr else ""
            raise HarnessError(f"loopback model exited before startup: {stderr}")
        time.sleep(0.05)
    process.terminate()
    raise HarnessError("timed out waiting for loopback model")


class PtyCapture:
    def __init__(self, master: int, raw: BinaryIO) -> None:
        self.master = master
        self.raw = raw
        self.text = ""
        self.closed = False

    def read_once(self, timeout: float) -> bool:
        if self.closed:
            return False
        ready, _, _ = select.select([self.master], [], [], max(0.0, timeout))
        if not ready:
            return False
        try:
            data = os.read(self.master, 65536)
        except OSError:
            self.closed = True
            return False
        if not data:
            self.closed = True
            return False
        self.raw.write(data)
        self.raw.flush()
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
        self.text += data.decode("utf-8", errors="replace")
        return True

    def pump(self, duration: float) -> None:
        deadline = time.monotonic() + duration
        while not self.closed and time.monotonic() < deadline:
            self.read_once(min(0.05, deadline - time.monotonic()))

    def pump_until(self, marker: str, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while marker not in self.text and not self.closed and time.monotonic() < deadline:
            self.read_once(min(0.05, deadline - time.monotonic()))
        if marker not in self.text:
            raise HarnessError(f"PTY output did not contain expected marker {marker!r}")


def send(master: int, data: bytes) -> None:
    view = memoryview(data)
    while view:
        written = os.write(master, view)
        view = view[written:]


def wait_child(pid: int, timeout: float) -> int | None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result, status = os.waitpid(pid, os.WNOHANG)
        if result == pid:
            if os.WIFEXITED(status):
                return os.WEXITSTATUS(status)
            if os.WIFSIGNALED(status):
                return 128 + os.WTERMSIG(status)
            return status
        time.sleep(0.05)
    return None


def terminate_child(pid: int) -> None:
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    wait_child(pid, 2)


def parse_args() -> argparse.Namespace:
    root = pathlib.Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tui-bin",
        type=pathlib.Path,
        default=root / "target/debug/minicore-tui",
    )
    parser.add_argument(
        "--agent-bin",
        type=pathlib.Path,
        default=root.parent / "minicore-agent/target/debug/minicore-agent",
    )
    parser.add_argument(
        "--reference-root",
        type=pathlib.Path,
        default=root.parent / "pi-rail-ui-ref-r1",
    )
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        default=root / "artifacts/stage7" / dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ"),
    )
    parser.add_argument("--first-delay-ms", type=int, default=900)
    parser.add_argument("--second-delay-ms", type=int, default=1600)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    tui = require_file(args.tui_bin, "TUI binary")
    agent = require_file(args.agent_bin, "Agent binary")
    loopback_script = pathlib.Path(__file__).with_name("stage7_loopback_model.py").resolve()
    ocr_script = pathlib.Path(__file__).with_name("stage7_ocr.swift").resolve()
    if not loopback_script.is_file():
        raise HarnessError(f"loopback script is missing: {loopback_script}")
    if not ocr_script.is_file():
        raise HarnessError(f"OCR validator is missing: {ocr_script}")

    output = args.output.expanduser().resolve()
    if output.exists() and any(output.iterdir()):
        raise HarnessError(f"output directory is not empty: {output}")
    output.mkdir(parents=True, exist_ok=True)
    screenshots = output / "screenshots"
    raw_path = output / "tui.pty.raw"
    request_log = output / "loopback.requests.jsonl"
    port_file = output / "loopback.port"

    window_id = iterm_window_id()
    original_size = iterm_size()
    resized_terminal = original_size is not None
    child_pid: int | None = None
    loopback: subprocess.Popen[bytes] | None = None
    exit_code: int | None = None
    captured = False
    completed = False
    workspace_path: pathlib.Path | None = None
    temp_dir: tempfile.TemporaryDirectory[str] | None = None
    try:
        if resized_terminal:
            set_iterm_size(INITIAL_SIZE)

        loopback, port = start_loopback(
            loopback_script,
            port_file,
            request_log,
            max(0, args.first_delay_ms),
            max(0, args.second_delay_ms),
        )
        temp_dir = tempfile.TemporaryDirectory(prefix="minicore-stage7-")
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
                    "--agent-bin",
                    str(agent),
                    "--agent-config",
                    str(config),
                    "--workspace",
                    str(workspace_path),
                    "--theme",
                    "dark",
                ],
                env,
            )
            raise AssertionError("execve returned")

        set_winsize(master, INITIAL_SIZE)
        with raw_path.open("wb") as raw:
            capture = PtyCapture(master, raw)
            capture.pump_until("Create session", 12)
            # Move from Workspace through Profile, Model, Reasoning, Title to
            # the Create action, then wait for the real Agent-backed session.
            send(master, b"\t\t\t\t\t\r")
            # The new-session form also has a ready footer. The context
            # marker only appears after the real session.open/presentation
            # path has completed.
            capture.pump_until("ctx ?", 12)
            capture_window(
                window_id,
                screenshots / SCREENSHOT_NAMES[0],
                ocr_script,
                ("workspace", "ready"),
            )

            send(master, b"inspect fixture\r")
            capture.pump(0.35)
            capture_window(
                window_id,
                screenshots / SCREENSHOT_NAMES[1],
                ocr_script,
                ("inspect", "working"),
            )
            capture.pump(3.2)
            capture_window(
                window_id,
                screenshots / SCREENSHOT_NAMES[2],
                ocr_script,
                ("loopback", "ready"),
            )

            if resized_terminal:
                set_iterm_size(NARROW_SIZE)
            set_winsize(master, NARROW_SIZE)
            capture.pump(0.8)
            capture_window(
                window_id,
                screenshots / SCREENSHOT_NAMES[3],
                ocr_script,
                ("ctx", "ready"),
            )
            captured = True

            # Ctrl-C twice follows the production shutdown path. The first
            # press arms the notice; the second requests Agent shutdown.
            send(master, b"\x03")
            capture.pump(0.25)
            send(master, b"\x03")
            capture.pump(3.0)

        exit_code = wait_child(child_pid, 8)
        if exit_code is None:
            terminate_child(child_pid)
            raise HarnessError("TUI did not exit after the clean Ctrl-C shutdown")
        child_pid = None
        if exit_code != 0:
            raise HarnessError(f"TUI exited with status {exit_code}")
        if not captured:
            raise HarnessError("PTY run ended before all screenshots were captured")

        for name in SCREENSHOT_NAMES:
            screenshot = screenshots / name
            if not screenshot.is_file() or screenshot.stat().st_size == 0:
                raise HarnessError(f"missing screenshot artifact: {screenshot}")

        metadata = {
            "schema": "minicore-stage7-evidence-v1",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
            "command": " ".join(str(part) for part in sys.argv),
            "platform": {
                "system": sys.platform,
                "machine": os.uname().machine,
                "release": os.uname().release,
                "terminal_emulator": "iTerm",
                "window_id": window_id,
                "initial_size": {"columns": INITIAL_SIZE[0], "rows": INITIAL_SIZE[1]},
                "narrow_size": {"columns": NARROW_SIZE[0], "rows": NARROW_SIZE[1]},
                "original_size": (
                    {"columns": original_size[0], "rows": original_size[1]}
                    if original_size
                    else None
                ),
            },
            "provenance": {
                "tui": git_metadata(pathlib.Path(__file__).resolve().parents[1]),
                "agent": git_metadata(pathlib.Path(__file__).resolve().parents[1].parent / "minicore-agent"),
                **reference_metadata(args.reference_root),
            },
            "binaries": {
                "tui": {"path": str(tui), "sha256": sha256(tui)},
                "agent": {"path": str(agent), "sha256": sha256(agent)},
            },
            "fixture_corpus": {"generated_cases": 114, "json_files_including_provenance": 115},
            "artifacts": {
                "pty_raw": {"path": str(raw_path), "bytes": raw_path.stat().st_size, "sha256": sha256(raw_path)},
                "loopback_requests": {
                    "path": str(request_log),
                    "bytes": request_log.stat().st_size,
                    "sha256": sha256(request_log),
                },
                "screenshots": [
                    {"path": str(screenshots / name), "bytes": (screenshots / name).stat().st_size, "sha256": sha256(screenshots / name)}
                    for name in SCREENSHOT_NAMES
                ],
            },
            "result": {"tui_exit": exit_code, "all_screenshots_captured": True},
        }
        (output / "PROVENANCE.json").write_text(
            json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        completed = True
        print(f"stage7 evidence: {output}")
        return 0
    finally:
        if not completed:
            import shutil

            shutil.rmtree(screenshots, ignore_errors=True)
            (output / "PROVENANCE.json").unlink(missing_ok=True)
        if child_pid is not None:
            terminate_child(child_pid)
        if loopback is not None and loopback.poll() is None:
            loopback.terminate()
            try:
                loopback.wait(timeout=3)
            except subprocess.TimeoutExpired:
                loopback.kill()
                loopback.wait()
        if resized_terminal and original_size is not None:
            try:
                set_iterm_size(original_size)
            except HarnessError as error:
                print(f"warning: could not restore iTerm size: {error}", file=sys.stderr)
        if temp_dir is not None:
            temp_dir.cleanup()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except HarnessError as error:
        print(f"stage7: {error}", file=sys.stderr)
        sys.exit(1)
