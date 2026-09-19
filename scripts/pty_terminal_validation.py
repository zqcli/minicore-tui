#!/usr/bin/env python3
"""Remote OS-PTY validation for the v0.3 terminal and event-loop contract.

This driver intentionally does not use a TestBackend.  It attaches the Rust
terminal test binary and, when requested, the real TUI binary to a kernel PTY,
changes the slave window size with TIOCSWINSZ, injects bytes, and records the
raw terminal stream.  The result is evidence for Linux OS-PTY behavior only;
it is not iTerm2, IME, native macOS/Windows, or hosted-CI evidence.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import resource
import select
import signal
import struct
import tempfile
import shutil
import time
import fcntl
import termios
import errno
from pathlib import Path
from typing import Iterable

MARKER = b"UNFLUSHED_TUI_FRAME_MUST_NOT_LEAK"


def set_size(fd: int, columns: int, rows: int) -> None:
    packed = struct.pack("HHHH", rows, columns, 0, 0)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, packed)


def cooked_mode_restored(slave: int) -> bool:
    """Read the exact slave FD shared with the child, not `/dev/pts/ptmx`."""
    local_flags = termios.tcgetattr(slave)[3]
    return bool(local_flags & termios.ICANON) and bool(local_flags & termios.ECHO)


def restore_cooked_mode(slave: int) -> None:
    """Repair a deliberately failing negative fixture before the next case."""
    attributes = termios.tcgetattr(slave)
    attributes[3] |= termios.ICANON | termios.ECHO
    termios.tcsetattr(slave, termios.TCSANOW, attributes)


def drain_master(master: int, output: bytearray) -> None:
    """Drain bytes already queued on the master without waiting for EIO."""
    while True:
        try:
            ready, _, _ = select.select([master], [], [], 0)
        except OSError:
            return
        if not ready:
            return
        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            if error.errno in (errno.EIO, errno.EBADF):
                return
            raise
        if not chunk:
            return
        output.extend(chunk)


def kill_and_reap(pid: int) -> tuple[int, resource.struct_rusage]:
    """Kill the PTY session and wait4 it exactly once."""
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except OSError:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    waited, status, usage = os.wait4(pid, 0)
    if waited != pid:
        raise RuntimeError(f"wait4 returned pid {waited}, expected {pid}")
    return os.waitstatus_to_exitcode(status), usage


def run_pty(
    argv: list[str],
    environment: dict[str, str],
    *,
    timeout: float = 20.0,
    input_events: Iterable[tuple[float, bytes | tuple[int, int]]] = (),
    initial_size: tuple[int, int] = (80, 24),
) -> tuple[int, bytes, resource.struct_rusage, bool]:
    master, slave = os.openpty()
    pid = os.fork()
    if pid == 0:
        try:
            os.close(master)
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            for target in (0, 1, 2):
                os.dup2(slave, target)
            if slave > 2:
                os.close(slave)
            os.environ.update(environment)
            os.execv(argv[0], argv)
        except BaseException:
            os._exit(127)

    set_size(master, *initial_size)
    events = iter(sorted(input_events, key=lambda item: item[0]))
    next_event = next(events, None)
    output = bytearray()
    deadline = time.monotonic() + timeout
    reaped = False
    try:
        while True:
            now = time.monotonic()
            if now >= deadline:
                kill_and_reap(pid)
                reaped = True
                raise RuntimeError(
                    f"PTY child exceeded {timeout:.1f}s: {' '.join(argv)}; "
                    f"output_tail={bytes(output[-4096:])!r}"
                )

            if next_event is not None and now - (deadline - timeout) >= next_event[0]:
                event = next_event[1]
                if isinstance(event, tuple):
                    set_size(master, event[0], event[1])
                else:
                    os.write(master, event)
                next_event = next(events, None)
                continue

            ready, _, _ = select.select([master], [], [], 0.02)
            if ready:
                try:
                    output.extend(os.read(master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
            waited, status, usage = os.wait4(pid, os.WNOHANG)
            if waited == pid:
                reaped = True
                # The parent deliberately keeps the original slave FD open,
                # so the master cannot report a misleading EIO before the
                # queued bytes have been drained. Inspect the same slave FD
                # before closing it, then do one final nonblocking drain.
                drain_master(master, output)
                pty_restored = cooked_mode_restored(slave)
                if not pty_restored:
                    restore_cooked_mode(slave)
                os.close(slave)
                drain_master(master, output)
                return (
                    os.waitstatus_to_exitcode(status),
                    bytes(output),
                    usage,
                    pty_restored,
                )
    finally:
        if not reaped:
            try:
                kill_and_reap(pid)
            except ChildProcessError:
                pass
        try:
            os.close(slave)
        except OSError:
            pass
        try:
            os.close(master)
        except OSError:
            pass


def test_binary_case(test_binary: Path, name: str, *, timeout: float = 20.0,
                     input_events: Iterable[tuple[float, bytes | tuple[int, int]]] = ()) -> dict:
    env = {
        "TERM": "xterm-256color",
        "MINICORE_TUI_REQUIRE_PTY": "1",
        "RUST_BACKTRACE": "0",
    }
    code, output, usage, pty_restored = run_pty(
        [str(test_binary), "--exact", name, "--ignored", "--nocapture"],
        env,
        timeout=timeout,
        input_events=input_events,
    )
    return {
        "name": name,
        "exit": code,
        "bytes": len(output),
        "cpu_user_ms": round(usage.ru_utime * 1000, 3),
        "cpu_sys_ms": round(usage.ru_stime * 1000, 3),
        "max_rss_kib": usage.ru_maxrss,
        "pty_cooked_after_exit": pty_restored,
        "output": output,
    }


def assert_terminal_stream(result: dict, *, min_alt_pairs: int = 1) -> None:
    output = result["output"]
    if result["exit"] != 0:
        raise RuntimeError(f"{result['name']} failed with exit {result['exit']}")
    if not result["pty_cooked_after_exit"]:
        raise RuntimeError(f"{result['name']} left the PTY in raw/non-echo mode")
    if MARKER in output:
        raise RuntimeError(f"{result['name']} leaked the unflushed frame marker")
    required = [
        b"\x1b[?1049h",  # alternate screen enter
        b"\x1b[?1049l",  # alternate screen leave
        b"\x1b[?2004h",  # bracketed paste on
        b"\x1b[?2004l",  # bracketed paste off
        b"\x1b[?25h",    # cursor restored
    ]
    missing = [sequence.hex() for sequence in required if sequence not in output]
    if missing:
        raise RuntimeError(f"{result['name']} missed ANSI sequences: {missing}")
    if output.count(b"\x1b[?1049h") < min_alt_pairs:
        raise RuntimeError(f"{result['name']} did not enter the alternate screen enough times")


def run_tui_probe(tui_binary: Path, fake_agent: Path, root: Path) -> dict:
    config = root / "fake-agent-mode.toml"
    config.write_text("normal\n", encoding="utf-8")
    env = {
        "TERM": "xterm-256color",
        "RUST_BACKTRACE": "0",
        "MINICORE_TUI_PERF_STATS": "1",
    }
    events: list[tuple[float, bytes | tuple[int, int]]] = [
        (0.50, b"\x1bOP"),       # F1: open Help
        (0.80, b"\x1b"),         # close Help
        (1.00, b"abc"),          # normal input
        (1.10, b"\x7f"),         # backspace
        (1.20, (100, 30)),        # resize while input is active
        (1.35, b"\x03"),         # clear the non-empty draft
        (1.60, b"\x04"),         # Ctrl-D requests shutdown while empty/idle
    ]
    code, output, usage, pty_restored = run_pty(
        [
            str(tui_binary),
            "--agent-bin", str(fake_agent),
            "--agent-config", str(config),
            "--workspace", str(root),
        ],
        env,
        timeout=20.0,
        input_events=events,
    )
    if code != 0:
        raise RuntimeError(f"real TUI PTY probe exited {code}")
    if MARKER in output:
        raise RuntimeError("real TUI PTY probe contained the terminal marker")
    return tui_result("real_tui_input_resize_shutdown", code, output, usage, pty_restored)


def run_idle_tui_probe(tui_binary: Path, fake_agent: Path, root: Path) -> dict:
    config = root / "fake-agent-idle.toml"
    config.write_text("normal\n", encoding="utf-8")
    env = {
        "TERM": "xterm-256color",
        "RUST_BACKTRACE": "0",
        "MINICORE_TUI_PERF_STATS": "1",
    }
    code, output, usage, pty_restored = run_pty(
        [
            str(tui_binary),
            "--agent-bin", str(fake_agent),
            "--agent-config", str(config),
            "--workspace", str(root),
        ],
        env,
        timeout=45.0,
        input_events=[(30.0, b"\x04")],
    )
    if code != 0:
        raise RuntimeError(f"30-second idle TUI PTY probe exited {code}")
    return tui_result("real_tui_idle_30s", code, output, usage, pty_restored)


def run_main_clipboard_probe(main_test_binary: Path, root: Path) -> dict:
    helper_dir = root / "clipboard-bin"
    helper_dir.mkdir()
    pid_path = root / "clipboard-helper.pid"
    helper = helper_dir / "xclip"
    helper.write_text(
        "#!/bin/sh\n"
        f"printf '%s' \"$$\" > '{pid_path}'\n"
        "exec /bin/sleep 2\n",
        encoding="utf-8",
    )
    helper.chmod(0o700)
    old_path = os.environ.get("PATH", "")
    env = {
        "TERM": "xterm-256color",
        "RUST_BACKTRACE": "0",
        "MINICORE_TUI_REQUIRE_PTY": "1",
        "MINICORE_TUI_CLIPBOARD_PID_FILE": str(pid_path),
        "PATH": f"{helper_dir}:{old_path}",
    }
    code, output, usage, pty_restored = run_pty(
        [
            str(main_test_binary),
            "--exact",
            "tests::real_run_commands_keeps_rpc_and_draft_live_during_native_clipboard",
            "--ignored",
            "--nocapture",
        ],
        env,
        timeout=15.0,
    )
    result = {
        "name": "real_run_commands_native_clipboard",
        "exit": code,
        "bytes": len(output),
        "cpu_user_ms": round(usage.ru_utime * 1000, 3),
        "cpu_sys_ms": round(usage.ru_stime * 1000, 3),
        "max_rss_kib": usage.ru_maxrss,
        "pty_cooked_after_exit": pty_restored,
        "output": output,
    }
    assert_terminal_stream(result)
    if not pid_path.exists():
        raise RuntimeError("real clipboard helper did not leave its PID record")
    pid = int(pid_path.read_text(encoding="utf-8").strip())
    if Path(f"/proc/{pid}").exists():
        raise RuntimeError("real clipboard helper survived without being reaped")
    return {key: value for key, value in result.items() if key != "output"}


def tui_result(
    name: str,
    code: int,
    output: bytes,
    usage: resource.struct_rusage,
    pty_restored: bool,
) -> dict:
    match = re.search(rb"minicore-tui perf: draws=(\d+)[^\r\n]*", output)
    if match is None:
        raise RuntimeError(f"{name} did not emit opt-in draw counters")
    draw_calls = int(match.group(1))
    if name == "real_tui_idle_30s" and draw_calls > 30:
        raise RuntimeError(f"idle TUI drew {draw_calls} frames in 30 seconds")
    return {
        "name": name,
        "exit": code,
        "bytes": len(output),
        "cpu_user_ms": round(usage.ru_utime * 1000, 3),
        "cpu_sys_ms": round(usage.ru_stime * 1000, 3),
        "max_rss_kib": usage.ru_maxrss,
        "pty_cooked_after_exit": pty_restored,
        "draw_calls": draw_calls,
        "perf": match.group().decode("utf-8", "replace"),
        "output": output,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--terminal-test-bin", type=Path, required=True)
    parser.add_argument("--tui-bin", type=Path)
    parser.add_argument("--fake-agent-bin", type=Path)
    parser.add_argument("--main-test-bin", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    for path in (args.terminal_test_bin, args.tui_bin, args.fake_agent_bin, args.main_test_bin):
        if path is not None and not path.is_file():
            parser.error(f"executable does not exist: {path}")

    results: list[dict] = []
    enter = test_binary_case(args.terminal_test_bin, "real_pty_enter_and_restore_round_trip")
    assert_terminal_stream(enter)
    results.append({key: value for key, value in enter.items() if key != "output"})

    editor = test_binary_case(args.terminal_test_bin, "real_pty_editor_suspend_and_resume_round_trip",
                              timeout=20.0)
    assert_terminal_stream(editor, min_alt_pairs=2)
    results.append({key: value for key, value in editor.items() if key != "output"})

    raw = test_binary_case(args.terminal_test_bin, "real_pty_raw_mode_is_restored_across_suspend_and_exit")
    assert_terminal_stream(raw, min_alt_pairs=2)
    results.append({key: value for key, value in raw.items() if key != "output"})

    negative = test_binary_case(
        args.terminal_test_bin,
        "real_pty_negative_leaves_raw_mode",
        timeout=10.0,
    )
    if negative["exit"] != 0 or negative["pty_cooked_after_exit"]:
        raise RuntimeError(
            "the negative raw-mode fixture was not detected on the child's slave"
        )
    results.append(
        {
            key: value
            for key, value in negative.items()
            if key != "output"
        }
        | {"expected_raw_detection": True}
    )

    input_probe = test_binary_case(
        args.terminal_test_bin,
        "real_pty_delivers_input_resize_and_shutdown_signal",
        timeout=10.0,
        input_events=[(0.30, b"x"), (0.45, (96, 28)), (0.60, b"\x03")],
    )
    if input_probe["exit"] != 0:
        raise RuntimeError(f"PTY input/resize test failed with exit {input_probe['exit']}")
    results.append({key: value for key, value in input_probe.items() if key != "output"})

    panic = test_binary_case(args.terminal_test_bin, "real_pty_panic_child_restores_terminal_before_exit")
    assert_terminal_stream(panic)
    results.append({key: value for key, value in panic.items() if key != "output"})

    root = Path(tempfile.mkdtemp(prefix="minicore-tui-pty-"))
    try:
        if args.tui_bin is not None or args.fake_agent_bin is not None:
            if args.tui_bin is None or args.fake_agent_bin is None:
                parser.error("--tui-bin and --fake-agent-bin must be provided together")
            tui = run_tui_probe(args.tui_bin, args.fake_agent_bin, root)
            results.append({key: value for key, value in tui.items() if key != "output"})
            idle = run_idle_tui_probe(args.tui_bin, args.fake_agent_bin, root)
            results.append({key: value for key, value in idle.items() if key != "output"})
        if args.main_test_bin is not None:
            results.append(run_main_clipboard_probe(args.main_test_bin, root))
    finally:
        for child in root.iterdir():
            if child.is_dir():
                shutil.rmtree(child)
            else:
                child.unlink()
        root.rmdir()

    report = {
        "status": "PASS",
        "pty": "Linux kernel PTY via pty.openpty/fork/setsid/TIOCSCTTY/TIOCSWINSZ",
        "native_manual_iTerm2": "Not run",
        "hosted_ci": "Not run",
        "macos_windows_native": "Not run",
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
